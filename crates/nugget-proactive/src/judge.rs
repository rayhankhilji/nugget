//! The optional LLM judge. It **enriches** the top-scored candidates (a summary, a suggested
//! action, a confidence); it never creates candidates and never ranks on its own.
//!
//! Local by default: a `Cloud` provider is used only when allowed in config, and never for a
//! candidate whose evidence is sensitive ([`judge_allowed`]).

use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::capability::Locality;
use nugget_core::ids::ItemRef;
use nugget_core::{Error, Result};
use nugget_infer::{ChatRequest, ChatResponse, Message, Provider};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use time::OffsetDateTime;

use crate::candidate::{Candidate, CandidateKind};
use crate::text::truncate;

/// The judge's additions to one candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Enrichment {
    pub dedup_key: String,
    pub summary: String,
    /// An action name offered by the candidate (`draft_reply`, `reminder`, ...).
    pub suggested_action: Option<String>,
    /// 0–1.
    pub confidence: f64,
    /// When it was judged (a summary written before the due date is redone after it).
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub at: Option<OffsetDateTime>,
}

impl Enrichment {
    /// Written before `due` passed, now that it has.
    pub(crate) fn stale(&self, due: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
        due.is_some_and(|d| d <= now && self.at.is_none_or(|a| a < d))
    }
}

/// What the judge sees of a candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeInput {
    pub key: String,
    pub kind: String,
    /// Who owes what to whom, in words.
    pub direction: String,
    /// The other person (an address), when there is one.
    pub person: Option<String>,
    /// The user's date today.
    pub today: String,
    pub title: String,
    pub detail: String,
    /// `YYYY-MM-DD`, with `(overdue)` once past.
    pub due: Option<String>,
    pub evidence: Vec<String>,
    pub actions: Vec<String>,
    /// The candidate's evidence items, for the egress ledger (not sent to the model).
    #[serde(skip)]
    pub items: Vec<ItemRef>,
}

impl JudgeInput {
    /// `local_now` is now in the user's offset.
    pub fn from_candidate(c: &Candidate, local_now: OffsetDateTime) -> Self {
        let direction = match c.kind {
            CandidateKind::UnansweredThread => "they wrote to the user, who has not replied yet",
            CandidateKind::FollowUp => "the user promised this to them",
            CandidateKind::Commitment => "a commitment in the user's notes and mail",
            CandidateKind::MeetingPrep => "an upcoming meeting of the user's",
            CandidateKind::Renewal => "a subscription the user pays for",
        };
        Self {
            key: c.dedup_key.clone(),
            kind: c.kind.as_str().into(),
            direction: direction.into(),
            person: c.people.first().cloned(),
            today: local_now.date().to_string(),
            title: c.title.clone(),
            detail: c.detail.clone(),
            due: c.due.map(|d| {
                let day = d.to_offset(local_now.offset()).date();
                if d <= local_now {
                    format!("{day} (overdue)")
                } else {
                    day.to_string()
                }
            }),
            evidence: c
                .evidence
                .iter()
                .filter_map(|e| e.quote.clone())
                .take(4)
                .collect(),
            actions: c.actions.iter().map(|a| a.name().to_owned()).collect(),
            items: c.evidence.iter().map(|e| e.item.clone()).collect(),
        }
    }
}

#[async_trait]
pub trait Judge: Send + Sync {
    /// Where the judge's model runs (decides the sensitive gate).
    fn locality(&self) -> Locality;
    /// Enrichments for some of `inputs` (keys not in `inputs` are ignored by the caller).
    async fn enrich(&self, inputs: &[JudgeInput]) -> Result<Vec<Enrichment>>;
}

/// Whether a candidate may be shown to a judge at `locality`.
pub fn judge_allowed(locality: Locality, allow_cloud: bool, sensitive: bool) -> bool {
    match locality {
        Locality::OnDevice | Locality::LocalNetwork => true,
        Locality::Cloud => allow_cloud && !sensitive,
    }
}

/// A judge backed by a chat model through `nugget-infer`.
pub struct LlmJudge {
    provider: Arc<dyn Provider>,
    max_tokens: u32,
}

pub const FEATURE_JUDGE: &str = "proactive_judge";

const SYSTEM: &str = "You review suggestions a personal assistant found in the user's own mail, \
calendar and notes. For each suggestion, write a one-sentence summary of why it matters today (`today` is the \
current date; `direction` says who owes whom; a due date marked overdue has passed), \
pick the most useful action from the ones it lists (or none), and give your confidence from 0 to \
1 that the user will want to act on it. Use only the evidence given. Do not add suggestions. \
Reply with JSON only.";

impl LlmJudge {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            max_tokens: 1200,
        }
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "key": {"type": "string"},
                            "summary": {"type": "string"},
                            "suggested_action": {"type": ["string", "null"]},
                            "confidence": {"type": "number"}
                        },
                        "required": ["key", "summary", "confidence"]
                    }
                }
            },
            "required": ["items"]
        })
    }
}

#[derive(Deserialize)]
struct Reply {
    items: Vec<ReplyItem>,
}

#[derive(Deserialize)]
struct ReplyItem {
    key: String,
    summary: String,
    #[serde(default)]
    suggested_action: Option<String>,
    confidence: f64,
}

/// Parse the model's JSON, tolerating a Markdown code fence around it.
pub(crate) fn parse_reply(text: &str) -> Result<Vec<Enrichment>> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .map_or(t, |s| s.trim_end().trim_end_matches("```"))
        .trim();
    let start = t
        .find('{')
        .ok_or_else(|| Error::Provider("judge: no JSON".into()))?;
    let end = t
        .rfind('}')
        .ok_or_else(|| Error::Provider("judge: no JSON".into()))?;
    let reply: Reply = serde_json::from_str(&t[start..=end])
        .map_err(|e| Error::Provider(format!("judge: bad JSON: {e}")))?;
    Ok(reply
        .items
        .into_iter()
        .map(|i| Enrichment {
            dedup_key: i.key,
            summary: truncate(&i.summary, 300),
            suggested_action: i.suggested_action.filter(|s| !s.is_empty()),
            confidence: if i.confidence.is_finite() {
                i.confidence.clamp(0.0, 1.0)
            } else {
                0.0
            },
            at: None,
        })
        .collect())
}

#[async_trait]
impl Judge for LlmJudge {
    fn locality(&self) -> Locality {
        self.provider.meta().locality
    }

    async fn enrich(&self, inputs: &[JudgeInput]) -> Result<Vec<Enrichment>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let payload = serde_json::to_string_pretty(&json!({ "suggestions": inputs }))?;
        let mut req = ChatRequest::new(vec![Message::user_text(payload)])
            .with_system(SYSTEM)
            .with_json_schema(Self::schema())
            .with_max_tokens(self.max_tokens)
            .with_feature(FEATURE_JUDGE);
        // Every item whose content (title, detail, quotes) is in the payload, for the ledger.
        for item in inputs.iter().flat_map(|i| &i.items) {
            if !req.items.contains(item) {
                req.items.push(item.clone());
            }
        }
        let stream = self.provider.chat(req).await?;
        let resp = ChatResponse::collect(stream).await?;
        parse_reply(&resp.text())
    }
}
