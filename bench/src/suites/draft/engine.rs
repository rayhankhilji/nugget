//! The Nugget arm through the real drafting engine ([`nugget_draft::Drafter`]), for real models.
//!
//! The in-suite pipeline ([`super::arms::NuggetArm`]) renders the suite's own prompt format,
//! which the deterministic fake model reads; it has no grounding switch. With a real model the
//! Nugget arm instead drafts the way `nuggetd` does: one [`Drafter`] per persona over the
//! persona's store (the same stores as the in-suite arm), routing to the drafting model on the
//! Synthesis tier, with the product's [`DraftConfig`] except for the settings the run varies
//! (`grounding`, `temperature`). Whatever `nugget-draft` does (prompt, grounding, streaming
//! post-processing) is therefore what is scored.
//!
//! - **Voice**: [`EngineVoice`] answers the engine's [`VoiceQuery`] with the in-suite voice
//!   logic ([`NuggetArm::voice`]): recipient statistics when the query's recipients match
//!   enough of the persona's messages, channel statistics otherwise; exemplars by recipient,
//!   then topic.
//! - **Prompt size**: [`Capture`] wraps the provider and keeps the last request's text, so the
//!   prompt is scored (size, secret exposure) like the other arms' prompts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use nugget_core::capability::Tier;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::DraftRequest;
use nugget_draft::{DraftConfig, Drafter, VoiceQuery, VoiceSource};
use nugget_infer::{ChatEvent, ChatRequest, ChatStream, Part, Provider, ProviderMeta, Router};
use nugget_retrieve::{RetrieveConfig, Retriever};
use nugget_voice::{Channel, VoiceHints};

use super::arms::{NuggetArm, World, reference_time};
use super::fake::content_words;
use super::fixtures::{Persona, Task};
use super::{Generation, RequestFailure, timed};

/// Tier the engine routes by (its default), and the only tier the bench router serves.
const TIER: Tier = Tier::Synthesis;

/// Everything a request sends as text: the system prompt, then each text part.
pub fn request_text(req: &ChatRequest) -> String {
    let mut out = req.system.clone().unwrap_or_default();
    for m in &req.messages {
        for p in &m.content {
            if let Part::Text { text } = p {
                out.push('\n');
                out.push_str(text);
            }
        }
    }
    out
}

/// A provider that remembers the text of the last request it was sent.
pub struct Capture {
    inner: Arc<dyn Provider>,
    last: Mutex<Option<String>>,
}

impl Capture {
    pub fn new(inner: Arc<dyn Provider>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            last: Mutex::new(None),
        })
    }

    /// The last request's text, cleared.
    pub fn take(&self) -> Option<String> {
        self.last.lock().ok().and_then(|mut l| l.take())
    }
}

#[async_trait]
impl Provider for Capture {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }
    fn meta(&self) -> ProviderMeta {
        self.inner.meta()
    }
    async fn chat(&self, req: ChatRequest) -> nugget_core::Result<ChatStream> {
        if let Ok(mut l) = self.last.lock() {
            *l = Some(request_text(&req));
        }
        self.inner.chat(req).await
    }
    async fn embed(&self, texts: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
        self.inner.embed(texts).await
    }
    async fn prewarm(&self) -> nugget_core::Result<()> {
        self.inner.prewarm().await
    }
}

/// The engine's voice source: the in-suite voice logic over one persona's sent messages.
pub struct EngineVoice {
    pub persona: Persona,
}

impl EngineVoice {
    /// The persona's correspondent among the query's recipients (matched by address or name
    /// on this channel), if any.
    fn recipient(&self, channel: &str, recipients: &[String]) -> String {
        let recipients: Vec<String> = recipients.iter().map(|r| r.to_lowercase()).collect();
        self.persona
            .sent
            .iter()
            .filter(|m| m.channel == channel)
            .find(|m| {
                let to = m.to.to_lowercase();
                let name = m.to_name.to_lowercase();
                recipients.iter().any(|r| {
                    (!to.is_empty() && r.contains(&to)) || (!name.is_empty() && r.contains(&name))
                })
            })
            .map(|m| m.to_name.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl VoiceSource for EngineVoice {
    async fn hints(&self, query: &VoiceQuery) -> nugget_core::Result<VoiceHints> {
        let channel = match query.channel {
            Channel::Chat | Channel::Comment => "chat",
            _ => "email",
        };
        let recipient = self.recipient(channel, &query.recipients);
        let ask = content_words(&query.topic);
        let (mut hints, _) = NuggetArm::voice(&self.persona, channel, &recipient, &ask);
        hints.channel = Some(query.channel);
        Ok(hints)
    }
}

/// One Nugget arm through the engine: a drafter per persona with one [`DraftConfig`].
pub struct Engine {
    drafters: HashMap<String, Drafter>,
    capture: Arc<Capture>,
    /// Output cap passed with each request (`None`: the engine's own, from the length band).
    max_tokens: Option<u32>,
    timeout: Duration,
    pub config: DraftConfig,
}

impl Engine {
    pub fn new(
        world: &World,
        provider: Arc<dyn Provider>,
        config: DraftConfig,
        max_tokens: Option<u32>,
        timeout: Duration,
    ) -> Self {
        let capture = Capture::new(provider);
        let routed: Arc<dyn Provider> = capture.clone();
        let tiers = HashMap::from([(TIER, vec![routed.id().clone()])]);
        let router = Arc::new(Router::new(vec![routed], tiers));
        let mut drafters = HashMap::new();
        for p in &world.corpus.personas {
            let Some(store) = world.store(&p.id) else {
                continue;
            };
            let retriever = Retriever::new(store.clone(), None, RetrieveConfig::default())
                .with_reference_time(reference_time());
            let drafter = Drafter::new(
                store.clone(),
                retriever,
                router.clone(),
                Arc::new(EngineVoice { persona: p.clone() }),
                config.clone(),
            );
            drafters.insert(p.id.clone(), drafter);
        }
        Self {
            drafters,
            capture,
            max_tokens,
            timeout,
            config,
        }
    }

    /// Draft one task: `begin` with the task's context, then the stream to its end. Times are
    /// from `started`. Returns the outcome and the text of the prompt that was sent (empty if
    /// none was).
    pub(crate) async fn draft(
        &self,
        task: &Task,
        started: Instant,
    ) -> (Result<Generation, RequestFailure>, String) {
        let _ = self.capture.take();
        let Some(drafter) = self.drafters.get(&task.persona) else {
            let f = RequestFailure {
                kind: "error",
                message: format!("no drafter for persona {}", task.persona),
                elapsed: started.elapsed(),
            };
            return (Err(f), String::new());
        };
        let req = DraftRequest {
            context: task.context.clone(),
            instruction: None,
            provider: None,
            max_tokens: self.max_tokens,
        };
        let work = async {
            let (_, mut stream) = drafter.begin(req).await?;
            let mut text = String::new();
            let mut ttft = None;
            while let Some(ev) = stream.next().await {
                match ev? {
                    ChatEvent::TextDelta(d) => {
                        if ttft.is_none() && !d.trim().is_empty() {
                            ttft = Some(started.elapsed());
                        }
                        text.push_str(&d);
                    }
                    ChatEvent::Done { .. } => break,
                    _ => {}
                }
            }
            Ok((text, ttft))
        };
        let out = timed(work, self.timeout, started).await;
        (out, self.capture.take().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suites::draft::fixtures::Corpus;

    #[test]
    fn engine_voice_matches_recipients_by_address_or_name() {
        let corpus = Corpus::load().unwrap();
        let p = &corpus.personas[0];
        let m = p.sent.iter().find(|m| !m.to.is_empty()).unwrap();
        let v = EngineVoice { persona: p.clone() };
        assert_eq!(
            v.recipient(&m.channel, &[format!("Someone <{}>", m.to.to_uppercase())]),
            m.to_name
        );
        assert_eq!(v.recipient(&m.channel, &[]), "");
    }

    #[test]
    fn request_text_has_system_and_user_parts() {
        let req =
            ChatRequest::new(vec![nugget_infer::Message::user_text("hello")]).with_system("rules");
        assert_eq!(request_text(&req), "rules\nhello");
    }
}
