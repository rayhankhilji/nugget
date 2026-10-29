//! `nugget_proactive::Drafter` backed by the Double Tap engine (`nugget-draft`): the same
//! provider choice, budgeted retrieval with the sensitive gate, voice and post-processing as a
//! draft typed into a mail field. The card's evidence quotes are passed as the thread, which the
//! engine fences as untrusted data.
//!
//! The voice is the user's profile as the draft feature last saved it ([`SnapshotVoice`], for
//! the identity in `[draft.voice]`); neutral until one exists.
//!
//! The engine runs with the user's Double Tap settings from `[draft]` ([`proactive_config`]):
//! tier, budget, temperature, timeouts, and the provider mapped to Mail in `app_providers`.
//! `[draft]` is read once at daemon start (nothing rewrites it at runtime; the desktop's own
//! per-app providers travel with each Double Tap request instead), so a startup copy is current.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    AppInfo, DraftContext, DraftRequest as FieldDraft, FieldInfo, ThreadMessage,
};
use nugget_core::{Error, Result};
use nugget_infer::ChatEvent;
use nugget_proactive::{DraftRequest, Drafter};
use nugget_store::Store;

use crate::daemon::blocking;
use crate::features::draft::draft_config_from_table;
use crate::features::draft::voice::SnapshotVoice;

/// Longest a proactive draft may take end to end.
const DRAFT_TIMEOUT: Duration = Duration::from_secs(180);
/// Evidence quotes passed as the thread at most.
const MAX_THREAD: usize = 12;
/// A loaded voice snapshot is read again after this long.
const VOICE_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// `app_providers` keys that name the mail app proactive replies are drafted for (compared
/// trimmed and case-insensitively): Apple Mail's bundle id and the app's name.
const MAIL_APP_KEYS: &[&str] = &["com.apple.mail", "mail"];

/// The engine config for proactive drafts: the user's `[draft]` settings, or the defaults (with
/// a warning) when the table does not parse.
pub(crate) fn proactive_config(draft: &toml::Table) -> nugget_draft::DraftConfig {
    draft_config_from_table(draft).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "proactive drafts: invalid [draft], using the defaults");
        nugget_draft::DraftConfig::default()
    })
}

/// The provider the user mapped to Mail in `app_providers`, if any.
pub(crate) fn mail_provider(config: &nugget_draft::DraftConfig) -> Option<ProviderId> {
    MAIL_APP_KEYS.iter().find_map(|key| {
        config
            .app_providers
            .iter()
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
            .map(|(_, v)| v.clone())
    })
}

pub(crate) struct EngineDrafter {
    engine: nugget_draft::Drafter,
    voice: Option<Arc<SnapshotVoice>>,
    /// The user's provider for Mail, named on every request (the field context has no app id).
    provider: Option<ProviderId>,
}

impl EngineDrafter {
    /// A drafter over `store`, `retriever` and `router`, with the user's `[draft]` (`draft`)
    /// settings and voice when it parses (defaults and a neutral voice otherwise, logged).
    pub(crate) fn new(
        store: Store,
        retriever: nugget_retrieve::Retriever,
        router: Arc<nugget_infer::Router>,
        draft: &toml::Table,
    ) -> Self {
        let voice = match SnapshotVoice::from_draft_table(store.clone(), draft, VOICE_MAX_AGE) {
            Ok(v) => Some(Arc::new(v)),
            Err(e) => {
                tracing::warn!(error = %e, "proactive drafts: no voice profile");
                None
            }
        };
        let source: Arc<dyn nugget_draft::VoiceSource> = match &voice {
            Some(v) => Arc::new(nugget_draft::ProfileVoice::from_arc(v.clone())),
            None => Arc::new(nugget_draft::NoVoice),
        };
        let config = proactive_config(draft);
        let provider = mail_provider(&config);
        let engine = nugget_draft::Drafter::new(store, retriever, router, source, config);
        Self {
            engine,
            voice,
            provider,
        }
    }
}

/// The field context a mail reply would have: an email body in a mail app, with subject,
/// recipients and the evidence as the thread.
pub(crate) fn field_request(req: &DraftRequest) -> FieldDraft {
    let thread = req
        .context
        .iter()
        .filter(|q| !q.trim().is_empty())
        .take(MAX_THREAD)
        .map(|q| ThreadMessage {
            author: None,
            text: q.clone(),
            from_me: Some(false),
            timestamp: None,
        })
        .collect();
    let intent = req.intent.trim();
    FieldDraft {
        context: DraftContext {
            app: AppInfo {
                id: None,
                name: "Mail".into(),
                window_title: None,
                url: None,
            },
            field: FieldInfo {
                role: "text_area".into(),
                label: Some("Message body".into()),
                ..Default::default()
            },
            thread,
            recipients: req.to.clone(),
            subject: Some(req.subject.clone()),
            screenshot_png_base64: None,
        },
        instruction: (!intent.is_empty()).then(|| intent.to_owned()),
        provider: None,
        max_tokens: None,
    }
}

#[async_trait]
impl Drafter for EngineDrafter {
    async fn draft_reply(&self, req: &DraftRequest) -> Result<String> {
        // Read the snapshot first, outside the engine's short voice-lookup timeout.
        if let Some(v) = self.voice.clone() {
            let _ = blocking(move || Ok(v.load_blocking())).await;
        }
        let run = async {
            let mut field = field_request(req);
            field.provider = self.provider.clone();
            let (_, mut stream) = self.engine.begin(field).await?;
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                match ev? {
                    ChatEvent::TextDelta(t) => text.push_str(&t),
                    ChatEvent::Done { .. } => return Ok(text),
                    _ => {}
                }
            }
            Err(Error::Provider(
                "the draft ended before it was complete".into(),
            ))
        };
        let text = tokio::time::timeout(DRAFT_TIMEOUT, run)
            .await
            .map_err(|_| Error::Provider("drafting the reply timed out".into()))??;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err(Error::Provider(
                "the drafter returned an empty reply".into(),
            ));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use nugget_core::capability::{Locality, Tier};
    use nugget_core::ids::{ItemKey, ItemRef, ProviderId, SourceId};
    use nugget_draft::FieldKind;
    use nugget_infer::{ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, StopReason};
    use nugget_store::{NewChunk, NewItem, content_hash};
    use nugget_voice::{MeIdentity, VoiceConfig, VoiceProfile};

    /// Answers every chat with a fixed reply and keeps the requests.
    struct Fake {
        id: ProviderId,
        requests: Mutex<Vec<ChatRequest>>,
    }

    #[async_trait]
    impl Provider for Fake {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn meta(&self) -> ProviderMeta {
            ProviderMeta {
                kind: ProviderKind::OpenaiCompatible,
                model: "fake".into(),
                locality: Locality::OnDevice,
                context_tokens: None,
            }
        }

        async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
            self.requests.lock().unwrap().push(req);
            Ok(futures::stream::iter(vec![
                Ok(ChatEvent::TextDelta("Sure, attached.".into())),
                Ok(ChatEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    input_tokens: None,
                    output_tokens: None,
                }),
            ])
            .boxed())
        }

        async fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
            Err(Error::Unsupported("no embeddings".into()))
        }
    }

    const ME: &str = "alex@morgan.example";
    const DRAFT_TOML: &str = "[voice]\nemails = [\"alex@morgan.example\"]";

    /// Mail the user sent (shaped as ingest stores Gmail items).
    fn put_sent(store: &Store, key: &str, body: &str) {
        let meta = serde_json::json!({
            "source": { "labels": ["SENT"], "thread_id": key, "from": ME, "subject": "Hi" },
            "mime": "message/rfc822",
            "extracted": {
                "from": format!("Alex Morgan <{ME}>"),
                "to": ["Sam Lee <sam@acme.example>"],
                "date": "2026-09-01T10:00:00Z",
                "subject": "Hi",
            },
            "sensitive": false,
            "junk": false,
        });
        let item = NewItem::new("gmail:alex", key, content_hash(body))
            .with_title("Hi")
            .with_text(body)
            .with_mime("message/rfc822")
            .with_metadata(meta);
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text(body)])
            .unwrap();
    }

    fn request() -> DraftRequest {
        DraftRequest {
            reply_to: ItemRef {
                source: SourceId::new("mail"),
                key: ItemKey::new("1"),
            },
            to: vec!["sam@acme.example".into()],
            subject: "Re: Pricing".into(),
            intent: "Send the pricing sheet".into(),
            context: vec!["Could you send pricing?".into()],
        }
    }

    /// Draft a reply over `store`; returns the system prompt the provider got.
    async fn draft_system(store: Store) -> String {
        let fake = Arc::new(Fake {
            id: ProviderId::new("fake"),
            requests: Mutex::new(Vec::new()),
        });
        let router = Arc::new(nugget_infer::Router::new(
            vec![fake.clone() as Arc<dyn Provider>],
            HashMap::from([(Tier::Synthesis, vec![fake.id.clone()])]),
        ));
        let retriever = nugget_retrieve::Retriever::new(
            store.clone(),
            None,
            nugget_retrieve::RetrieveConfig::default(),
        );
        let table: toml::Table = toml::from_str(DRAFT_TOML).unwrap();
        let drafter = EngineDrafter::new(store, retriever, router, &table);
        let text = drafter.draft_reply(&request()).await.unwrap();
        assert_eq!(text, "Sure, attached.");
        let req = fake.requests.lock().unwrap().pop().unwrap();
        req.system.unwrap_or_default()
    }

    /// Draft one reply with the `[draft]` table `draft_toml` over fakes `(id, tier)` (a provider
    /// with no tier is only reachable by name); returns the drafter and the requests per fake.
    async fn draft_with(
        draft_toml: &str,
        fakes: &[(&str, Option<Tier>)],
    ) -> (EngineDrafter, HashMap<String, Vec<ChatRequest>>) {
        let fakes: Vec<(Arc<Fake>, Option<Tier>)> = fakes
            .iter()
            .map(|(id, tier)| {
                let fake = Arc::new(Fake {
                    id: ProviderId::new(*id),
                    requests: Mutex::new(Vec::new()),
                });
                (fake, *tier)
            })
            .collect();
        let mut tiers: HashMap<Tier, Vec<ProviderId>> = HashMap::new();
        for (f, tier) in &fakes {
            if let Some(t) = tier {
                tiers.entry(*t).or_default().push(f.id.clone());
            }
        }
        let router = Arc::new(nugget_infer::Router::new(
            fakes
                .iter()
                .map(|(f, _)| f.clone() as Arc<dyn Provider>)
                .collect(),
            tiers,
        ));
        let store = Store::open_in_memory().unwrap();
        let retriever = nugget_retrieve::Retriever::new(
            store.clone(),
            None,
            nugget_retrieve::RetrieveConfig::default(),
        );
        let table: toml::Table = toml::from_str(draft_toml).unwrap();
        let drafter = EngineDrafter::new(store, retriever, router, &table);
        let text = drafter.draft_reply(&request()).await.unwrap();
        assert_eq!(text, "Sure, attached.");
        let requests = fakes
            .iter()
            .map(|(f, _)| {
                let reqs = std::mem::take(&mut *f.requests.lock().unwrap());
                (f.id.as_str().to_owned(), reqs)
            })
            .collect();
        (drafter, requests)
    }

    #[test]
    fn the_proactive_config_is_the_users_draft_settings() {
        let table: toml::Table = toml::from_str(
            "tier = \"light\"\nbudget_tokens = 900\ntemperature = 0.2\n\
             retrieval_timeout_ms = 4000\nvoice_timeout_ms = 900\n\
             app_providers = { \"Mail\" = \"mailp\" }\n[voice]\nemails = [\"a@b.example\"]",
        )
        .unwrap();
        let c = proactive_config(&table);
        assert_eq!(c.tier, Tier::Light);
        assert_eq!(c.budget_tokens, 900);
        assert_eq!(c.temperature, Some(0.2));
        assert_eq!(c.retrieval_timeout_ms, 4000);
        assert_eq!(c.voice_timeout_ms, 900);
        assert_eq!(mail_provider(&c), Some(ProviderId::new("mailp")));

        // The budget is capped as for Double Tap.
        let table: toml::Table = toml::from_str("budget_tokens = 999999999").unwrap();
        assert_eq!(
            proactive_config(&table).budget_tokens,
            nugget_draft::MAX_BUDGET_TOKENS
        );
    }

    #[test]
    fn an_invalid_draft_section_falls_back_to_the_defaults() {
        let table: toml::Table = toml::from_str("tier = \"light\"\nbogus = 1").unwrap();
        let c = proactive_config(&table);
        let d = nugget_draft::DraftConfig::default();
        assert_eq!(c.tier, d.tier);
        assert_eq!(c.budget_tokens, d.budget_tokens);
        assert_eq!(c.temperature, d.temperature);
        assert!(c.app_providers.is_empty());
        assert_eq!(proactive_config(&toml::Table::new()).tier, d.tier);
    }

    #[test]
    fn the_mail_provider_is_found_by_bundle_id_or_name() {
        let mut c = nugget_draft::DraftConfig::default();
        assert_eq!(mail_provider(&c), None);
        c.app_providers
            .insert("com.tinyspeck.slackmacgap".into(), ProviderId::new("slack"));
        assert_eq!(mail_provider(&c), None);
        c.app_providers
            .insert(" COM.APPLE.MAIL ".into(), ProviderId::new("apple"));
        assert_eq!(mail_provider(&c), Some(ProviderId::new("apple")));
    }

    #[tokio::test]
    async fn drafts_use_the_users_tier_budget_and_temperature() {
        let (drafter, reqs) = draft_with(
            "tier = \"light\"\nbudget_tokens = 900\ntemperature = 0.2",
            &[("syn", Some(Tier::Synthesis)), ("lite", Some(Tier::Light))],
        )
        .await;
        assert!(reqs["syn"].is_empty());
        assert_eq!(reqs["lite"].len(), 1);
        assert_eq!(reqs["lite"][0].temperature, Some(0.2));
        assert_eq!(drafter.engine.config().budget_tokens, 900);
        assert_eq!(drafter.engine.config().tier, Tier::Light);
    }

    #[tokio::test]
    async fn drafts_use_the_provider_mapped_to_mail() {
        let (_, reqs) = draft_with(
            "app_providers = { \"Mail\" = \"mailp\" }",
            &[("syn", Some(Tier::Synthesis)), ("mailp", None)],
        )
        .await;
        assert!(reqs["syn"].is_empty());
        assert_eq!(reqs["mailp"].len(), 1);
    }

    #[tokio::test]
    async fn drafts_use_the_defaults_when_draft_is_invalid() {
        let (drafter, reqs) = draft_with(
            "tier = \"light\"\nbogus = 1",
            &[("syn", Some(Tier::Synthesis)), ("lite", Some(Tier::Light))],
        )
        .await;
        assert_eq!(reqs["syn"].len(), 1);
        assert!(reqs["lite"].is_empty());
        assert_eq!(
            drafter.engine.config().budget_tokens,
            nugget_draft::DraftConfig::default().budget_tokens
        );
    }

    #[tokio::test]
    async fn drafts_use_the_saved_voice_profile() {
        let store = Store::open_in_memory().unwrap();
        for i in 0..4 {
            put_sent(
                &store,
                &format!("s{i}"),
                "hey Sam! the deck is in the shared folder, lmk 🙂\n\ncheers,\nAlex",
            );
        }
        let table: toml::Table = toml::from_str(DRAFT_TOML).unwrap();
        let me = MeIdentity::new([ME], Vec::<String>::new());
        let p = VoiceProfile::load_or_build(&store, me, VoiceConfig::default()).unwrap();
        assert!(nugget_voice::VoiceSource::status(&p).samples > 0);
        // The snapshot was saved for the identity in `[draft.voice]`.
        let v = SnapshotVoice::from_draft_table(store.clone(), &table, VOICE_MAX_AGE).unwrap();
        assert!(v.load_blocking());

        let system = draft_system(store).await;
        assert!(system.contains("Learned from 4"), "{system}");
        assert!(system.to_lowercase().contains("cheers"), "{system}");
    }

    #[tokio::test]
    async fn drafts_work_without_a_voice_profile() {
        let system = draft_system(Store::open_in_memory().unwrap()).await;
        assert!(system.contains("No writing samples yet"), "{system}");
    }

    #[test]
    fn the_field_context_is_an_email_reply() {
        let req = DraftRequest {
            reply_to: ItemRef {
                source: SourceId::new("mail"),
                key: ItemKey::new("1"),
            },
            to: vec!["ana@acme.io".into()],
            subject: "Re: Pricing".into(),
            intent: "Send the pricing sheet".into(),
            context: vec!["Could you send pricing?".into(), " ".into()],
        };
        let f = field_request(&req);
        assert_eq!(
            nugget_draft::gate(&f.context, &[]).unwrap(),
            FieldKind::EmailBody
        );
        assert_eq!(f.context.thread.len(), 1);
        assert_eq!(f.context.recipients, vec!["ana@acme.io".to_string()]);
        assert_eq!(f.instruction.as_deref(), Some("Send the pricing sheet"));
    }
}
