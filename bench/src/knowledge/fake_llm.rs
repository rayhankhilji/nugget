//! A deterministic fake LLM: [`ScriptedLlm`] answers every chat request with the scripted
//! extraction (`llm_script.json`) of the item the request is about.
//!
//! The item is found from the request's `items` (the egress-provenance refs), or else by the
//! longest scripted fingerprint (a line only that item contains) found in the request text.
//! A request that matches no item gets an empty extraction and is counted as unmatched.
//!
//! The answer is one JSON text in `nugget-knowledge`'s extraction schema (`nugget_knowledge::llm`):
//!
//! ```json
//! {"entities": [{"ref": "e1", "kind": "person", "name": "Maggie", "aliases": []},
//!               {"ref": "e2", "kind": "preference", "name": "Vegetarian food",
//!                "aliases": ["vegetarian food"]}],
//!  "claims": [{"subject": "e1", "predicate": "prefers", "object": "e2", "text": "...",
//!              "confidence": 0.9, "quotes": [{"chunk": 0, "text": "I prefer vegetarian food"}]}]}
//! ```
//!
//! - An entity named like the user (the system prompt's "The user is …") gets the ref `user`.
//! - An entity's `aliases` hold the surface form the item uses, when it differs from the name.
//! - A quote's `chunk` is the `[chunk N]` section of the prompt that contains it (else the
//!   first one).
//!
//! Hallucinated and injected claims are returned like any other (their flags are not sent):
//! the pipeline has to catch them by checking each quote against the source chunk.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream;
use nugget_core::capability::Locality;
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Part, Provider, ProviderKind, ProviderMeta, Router,
    StopReason,
};
use serde_json::{Value, json};

use super::fixture::{Fixture, ScriptEntry, normalize_quote};

/// Provider id of the fake.
pub const FAKE_LLM_ID: &str = "bench-scripted-llm";

pub struct ScriptedLlm {
    id: ProviderId,
    by_ref: HashMap<ItemRef, usize>,
    /// (normalised fingerprint, script index), longest first.
    fingerprints: Vec<(String, usize)>,
    script: Vec<ScriptEntry>,
    calls: AtomicUsize,
    unmatched: AtomicUsize,
}

/// System prompt and message text of a request.
fn request_text(req: &ChatRequest) -> String {
    let mut text = req.system.clone().unwrap_or_default();
    for m in &req.messages {
        for p in &m.content {
            if let Part::Text { text: t } = p {
                text.push('\n');
                text.push_str(t);
            }
        }
    }
    text
}

/// The user's name from a system prompt containing "The user is <name>.".
fn user_name(system: Option<&str>) -> Option<String> {
    let s = system?;
    let at = s.find("The user is ")? + "The user is ".len();
    let rest = &s[at..];
    let end = rest
        .find(".\n")
        .or_else(|| rest.find(". "))
        .unwrap_or(rest.len());
    let name = rest[..end].trim_end_matches('.').trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// `[chunk N]` sections of a prompt: (N, normalised text after the marker).
fn chunk_sections(text: &str) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("[chunk ") {
        let after = &rest[at + "[chunk ".len()..];
        let Some(close) = after.find(']') else {
            break;
        };
        let body = &after[close + 1..];
        let next = body.find("[chunk ").unwrap_or(body.len());
        if let Ok(n) = after[..close].trim().parse::<i64>() {
            out.push((n, normalize_quote(&body[..next])));
        }
        rest = &body[next..];
    }
    out
}

impl ScriptedLlm {
    pub fn new(fx: &Fixture) -> Self {
        let mut by_ref = HashMap::new();
        let mut fingerprints = Vec::new();
        for (i, s) in fx.script.iter().enumerate() {
            if let Some(spec) = fx.item(&s.item) {
                by_ref.insert(spec.item_ref(), i);
            }
            fingerprints.push((normalize_quote(&s.fingerprint), i));
        }
        fingerprints.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.1.cmp(&b.1)));
        Self {
            id: ProviderId::new(FAKE_LLM_ID),
            by_ref,
            fingerprints,
            script: fx.script.clone(),
            calls: AtomicUsize::new(0),
            unmatched: AtomicUsize::new(0),
        }
    }

    /// Chat requests answered so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Requests that matched no scripted item.
    pub fn unmatched(&self) -> usize {
        self.unmatched.load(Ordering::SeqCst)
    }

    /// The scripted entry a request is about, if any.
    pub fn entry_for(&self, req: &ChatRequest) -> Option<&ScriptEntry> {
        if let Some(i) = req.items.iter().find_map(|r| self.by_ref.get(r)) {
            return Some(&self.script[*i]);
        }
        let text = normalize_quote(&request_text(req));
        self.fingerprints
            .iter()
            .find(|(fp, _)| !fp.is_empty() && text.contains(fp.as_str()))
            .map(|(_, i)| &self.script[*i])
    }

    /// The JSON answer for a request (an empty extraction when no item matches).
    pub fn response_for(&self, req: &ChatRequest) -> Value {
        let Some(e) = self.entry_for(req) else {
            self.unmatched.fetch_add(1, Ordering::SeqCst);
            return json!({"entities": [], "claims": []});
        };
        let user = user_name(req.system.as_deref());
        let sections = chunk_sections(&request_text(req));
        let mut refs: HashMap<&str, String> = HashMap::new();
        let mut entities = Vec::new();
        for (i, x) in e.entities.iter().enumerate() {
            let r = if user.as_deref() == Some(x.name.as_str()) {
                "user".to_owned()
            } else {
                format!("e{}", i + 1)
            };
            refs.insert(&x.name, r.clone());
            let aliases: Vec<&str> = x
                .surface
                .as_deref()
                .filter(|s| *s != x.name)
                .into_iter()
                .collect();
            entities.push(json!({"ref": r, "kind": x.kind, "name": x.name, "aliases": aliases}));
        }
        let claims: Vec<Value> = e
            .claims
            .iter()
            .map(|c| {
                let q = normalize_quote(&c.quote);
                let chunk = sections
                    .iter()
                    .find(|(_, t)| t.contains(&q))
                    .or(sections.first())
                    .map_or(0, |(n, _)| *n);
                let subject = refs
                    .get(c.subject.as_str())
                    .cloned()
                    .unwrap_or_else(|| c.subject.clone());
                let object = if c.object_type == "entity" {
                    refs.get(c.object.as_str())
                        .cloned()
                        .unwrap_or_else(|| c.object.clone())
                } else {
                    c.object.clone()
                };
                json!({
                    "subject": subject, "predicate": c.predicate, "object": object,
                    "text": c.text, "confidence": c.confidence,
                    "quotes": [{"chunk": chunk, "text": c.quote}],
                })
            })
            .collect();
        json!({"entities": entities, "claims": claims})
    }

    /// A router with this fake as the probed `Synthesis` (and `Light`) tier, which is where
    /// `nugget-knowledge` sends LLM extraction.
    pub fn router(self: &Arc<Self>) -> Router {
        super::system::synthesis_router(self.clone())
    }
}

#[async_trait]
impl Provider for ScriptedLlm {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "scripted-extractions".into(),
            locality: Locality::OnDevice,
            context_tokens: Some(32_768),
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.response_for(&req);
        let events = vec![
            Ok(ChatEvent::TextDelta(answer.to_string())),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ];
        Ok(Box::pin(stream::iter(events)))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("the scripted LLM does not embed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::capability::{Capabilities, Tier};
    use nugget_infer::{ChatResponse, Message, Role};

    fn request(system: &str, text: &str, items: Vec<ItemRef>) -> ChatRequest {
        let mut req: ChatRequest =
            serde_json::from_value(json!({"system": system, "messages": []})).unwrap();
        req.messages.push(Message {
            role: Role::User,
            content: vec![Part::Text { text: text.into() }],
        });
        req.items = items;
        req
    }

    #[tokio::test]
    async fn answers_by_item_ref_and_by_fingerprint_in_the_pipeline_schema() {
        let fx = Fixture::builtin().unwrap();
        let llm = Arc::new(ScriptedLlm::new(&fx));
        let spec = fx.item("mail-015").unwrap();
        let by_ref = request("x", "whatever", vec![spec.item_ref()]);
        assert_eq!(llm.entry_for(&by_ref).unwrap().item, "mail-015");

        let raw = std::fs::read_to_string(fx.dir.join(&spec.path)).unwrap();
        let prompt = format!("<<<DOC-1>>>\nTitle: coffee?\n[chunk 0]\n{raw}\n<<<END-1>>>");
        let by_text = request(
            "Extract. The user is Robin Ashgrove.\nOUTPUT: JSON",
            &prompt,
            vec![],
        );
        assert_eq!(llm.entry_for(&by_text).unwrap().item, "mail-015");

        let resp = ChatResponse::collect(llm.chat(by_text).await.unwrap())
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&resp.text()).unwrap();
        let ents = v["entities"].as_array().unwrap();
        let reference = |name: &str| {
            ents.iter().find(|e| e["name"] == name).unwrap()["ref"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(reference("Robin Ashgrove"), "user");
        let pref = ents
            .iter()
            .find(|e| e["name"] == "Vegetarian food")
            .unwrap();
        assert_eq!(pref["aliases"], json!(["vegetarian food"]));
        let claims = v["claims"].as_array().unwrap();
        let veg = claims
            .iter()
            .find(|c| c["quotes"][0]["text"] == "I prefer vegetarian food")
            .unwrap();
        assert_eq!(veg["subject"], reference("Maggie"));
        assert_eq!(veg["object"], reference("Vegetarian food"));
        assert_eq!(veg["quotes"][0]["chunk"], 0);
        // The hallucination is served without its flag.
        let coffee = claims
            .iter()
            .find(|c| c["quotes"][0]["text"] == "Maggie only drinks black coffee")
            .unwrap();
        assert_eq!(coffee["object"], "Black coffee");
        assert!(claims.iter().all(|c| c.get("hallucinated").is_none()));
        assert_eq!(llm.calls(), 1);

        let none = llm.response_for(&request("x", "nothing to see here", vec![]));
        assert_eq!(none["claims"], json!([]));
        assert_eq!(llm.unmatched(), 1);

        // Routed as the synthesis tier.
        let router = llm.router();
        let p = router
            .route(Tier::Synthesis, &Capabilities::default())
            .unwrap();
        assert_eq!(p.id().as_str(), FAKE_LLM_ID);
    }

    #[test]
    fn prompt_parsing() {
        assert_eq!(
            user_name(Some("Hi. The user is Robin Ashgrove.\nMore")).as_deref(),
            Some("Robin Ashgrove")
        );
        assert_eq!(user_name(Some("No user here")), None);
        let s = chunk_sections("x [chunk 0]\nfirst part\n[chunk 3]\nSecond  part\n<<<END>>>");
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].0, 3);
        assert!(s[1].1.starts_with("second part"));
    }
}
