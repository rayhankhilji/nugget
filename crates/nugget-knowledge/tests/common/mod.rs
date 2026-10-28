//! Shared fixtures: items as ingest stores them, a scripted fake provider, graph queries.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_core::Result;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_graph::{Claim, ClaimObject, Entity, EntityKind, EntityQuery, GraphStore, MemGraph};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Part, Provider, ProviderKind, ProviderMeta, Router,
    StopReason,
};
use nugget_knowledge::{Knowledge, KnowledgeConfig, RunBudget, RunStats};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde_json::{Value, json};

pub struct Env {
    pub store: Store,
    pub graph: Arc<MemGraph>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            store: Store::open_in_memory().unwrap(),
            graph: Arc::new(MemGraph::new()),
        }
    }

    pub fn knowledge(&self, cfg: KnowledgeConfig, router: Option<Arc<Router>>) -> Knowledge {
        Knowledge::new(self.store.clone(), self.graph.clone(), router, cfg)
    }

    pub fn put(&self, item: NewItem, chunks: &[&str]) {
        let chunks: Vec<NewChunk> = chunks.iter().map(|c| NewChunk::text(*c)).collect();
        self.store.upsert_item_with_chunks(&item, &chunks).unwrap();
    }

    pub fn g(&self) -> &dyn GraphStore {
        self.graph.as_ref()
    }

    /// The one entity of `kind` named (or aliased) `name`.
    pub fn entity(&self, kind: EntityKind, name: &str) -> Entity {
        let ids = self.g().find_by_name(name, Some(kind)).unwrap();
        assert_eq!(
            ids.len(),
            1,
            "expected one {kind:?} named {name}, got {ids:?}"
        );
        self.g().get_entity(&ids[0]).unwrap().unwrap()
    }

    pub fn try_entity(&self, kind: EntityKind, name: &str) -> Option<Entity> {
        let ids = self.g().find_by_name(name, Some(kind)).unwrap();
        ids.first()
            .map(|id| self.g().get_entity(id).unwrap().unwrap())
    }

    pub fn entities(&self, kind: EntityKind) -> Vec<Entity> {
        self.g()
            .list_entities(&EntityQuery {
                kind: Some(kind),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|s| self.g().get_entity(&s.id).unwrap().unwrap())
            .collect()
    }

    /// Claims (any status) with `subject` and `predicate`.
    pub fn claims(&self, subject: &Entity, predicate: &str) -> Vec<Claim> {
        self.g()
            .claims_for(&subject.id, true)
            .unwrap()
            .into_iter()
            .filter(|c| c.subject == subject.id && c.predicate == predicate)
            .collect()
    }

    pub fn active(&self, subject: &Entity, predicate: &str) -> Vec<Claim> {
        self.claims(subject, predicate)
            .into_iter()
            .filter(|c| c.status.is_active())
            .collect()
    }

    pub fn me(&self) -> Entity {
        let people = self.entities(EntityKind::Person);
        let me: Vec<Entity> = people
            .into_iter()
            .filter(|e| e.attributes.get("is_me") == Some(&Value::Bool(true)))
            .collect();
        assert_eq!(me.len(), 1, "expected one 'me' entity");
        me.into_iter().next().unwrap()
    }

    /// Every active claim in the graph.
    pub fn all_claims(&self) -> Vec<Claim> {
        let mut out: Vec<Claim> = Vec::new();
        for kind in EntityKind::ALL {
            for e in self.entities(kind) {
                for c in self.g().claims_for(&e.id, false).unwrap() {
                    if !out.iter().any(|x| x.id == c.id) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }
}

pub fn obj(c: &Claim) -> String {
    match &c.object {
        ClaimObject::Entity(e) => e.to_string(),
        ClaimObject::Literal(l) => l.clone(),
    }
}

pub fn item_ref(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: nugget_core::ids::SourceId::new(source),
        key: nugget_core::ids::ItemKey::new(key),
    }
}

pub async fn run(k: &Knowledge) -> RunStats {
    k.run_once(RunBudget::unlimited()).await.unwrap()
}

/// A mail item as ingest stores it (`metadata.extracted` holds the parsed headers).
pub struct Mail<'a> {
    pub source: &'a str,
    pub key: &'a str,
    pub from: &'a str,
    pub to: &'a [&'a str],
    pub cc: &'a [&'a str],
    pub date: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
    /// Connector metadata (`folder`, `labels`).
    pub source_meta: Value,
    pub sensitive: bool,
}

impl Default for Mail<'_> {
    fn default() -> Self {
        Self {
            source: "imap:me@corp.example",
            key: "INBOX/1",
            from: "Alice Smith <alice@acme.com>",
            to: &["Tanuj <tanuj@trylumi.co>"],
            cc: &[],
            date: "2026-03-01T10:00:00Z",
            subject: "Hello",
            body: "Hi there, see you at the offsite.",
            source_meta: json!({"folder": "INBOX"}),
            sensitive: false,
        }
    }
}

impl Mail<'_> {
    pub fn item(&self) -> NewItem {
        let mut extracted = json!({
            "from": self.from,
            "date": self.date,
            "subject": self.subject,
            "message_id": format!("<{}@mail>", self.key),
        });
        if !self.to.is_empty() {
            extracted["to"] = json!(self.to);
        }
        if !self.cc.is_empty() {
            extracted["cc"] = json!(self.cc);
        }
        NewItem::new(
            self.source,
            self.key,
            content_hash(format!(
                "{}{}{}{:?}",
                self.from, self.date, self.body, self.to
            )),
        )
        .with_title(self.subject)
        .with_mime("message/rfc822")
        .with_version(content_hash(format!("{}{}", self.date, self.body)))
        .with_metadata(json!({
            "source": self.source_meta,
            "mime": "message/rfc822",
            "extracted": extracted,
            "sensitive": self.sensitive,
            "junk": false,
        }))
    }

    pub fn put(&self, env: &Env) {
        env.put(self.item(), &[self.body]);
    }
}

/// A markdown note as ingest stores it.
pub fn note(
    source: &str,
    key: &str,
    title: &str,
    frontmatter: Value,
    tags: &[&str],
    wikilinks: &[&str],
    body: &str,
) -> NewItem {
    let mut extracted = json!({});
    if !frontmatter.is_null() {
        extracted["frontmatter"] = frontmatter;
    }
    if !wikilinks.is_empty() {
        extracted["wikilinks"] = Value::Array(
            wikilinks
                .iter()
                .map(|w| json!({"target": w, "embed": false}))
                .collect(),
        );
    }
    let mut meta = json!({
        "source": {},
        "mime": "text/markdown",
        "extracted": extracted,
        "sensitive": false,
        "junk": false,
    });
    if !tags.is_empty() {
        meta["tags"] = json!(tags);
    }
    NewItem::new(source, key, content_hash(format!("{body}{meta}")))
        .with_title(title)
        .with_mime("text/markdown")
        .with_version(content_hash(format!("{body}{meta}")))
        .with_metadata(meta)
}

/// A `text/calendar` item with `metadata.extracted.events` as the ics extractor writes them.
pub fn ics(source: &str, key: &str, events: Value, text: &str) -> NewItem {
    NewItem::new(source, key, content_hash(format!("{events}{text}")))
        .with_mime("text/calendar")
        .with_version(content_hash(format!("{events}")))
        .with_metadata(json!({
            "source": {"calendar": "Work"},
            "mime": "text/calendar",
            "extracted": {"events": events},
            "sensitive": false,
            "junk": false,
        }))
}

// ---------------------------------------------------------------- fake providers

type Script = dyn Fn(&str) -> String + Send + Sync;

/// A chat provider replying with scripted JSON, recording every prompt.
pub struct FakeProvider {
    pub id: ProviderId,
    pub locality: Locality,
    script: Box<Script>,
    pub prompts: Mutex<Vec<String>>,
    pub systems: Mutex<Vec<String>>,
    pub embeddings: HashMap<String, Vec<f32>>,
}

impl FakeProvider {
    pub fn new(
        id: &str,
        locality: Locality,
        script: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new(id),
            locality,
            script: Box::new(script),
            prompts: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
            embeddings: HashMap::new(),
        })
    }

    pub fn embedder(id: &str, vectors: &[(&str, Vec<f32>)]) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new(id),
            locality: Locality::OnDevice,
            script: Box::new(|_| "{}".into()),
            prompts: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
            embeddings: vectors
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        })
    }

    pub fn calls(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "fake".into(),
            locality: self.locality,
            context_tokens: None,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let text: String = req
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.prompts.lock().unwrap().push(text.clone());
        self.systems
            .lock()
            .unwrap()
            .push(req.system.clone().unwrap_or_default());
        let reply = (self.script)(&text);
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ChatEvent::TextDelta(reply)),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ])))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                self.embeddings
                    .get(t)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0, 0.0, 1.0])
            })
            .collect())
    }
}

/// A router with these providers on `tier`, probed for JSON-schema output (and embeddings).
pub fn router(tier: Tier, providers: Vec<Arc<FakeProvider>>) -> Arc<Router> {
    let ids: Vec<ProviderId> = providers.iter().map(|p| p.id.clone()).collect();
    let dyns: Vec<Arc<dyn Provider>> = providers
        .iter()
        .map(|p| p.clone() as Arc<dyn Provider>)
        .collect();
    let r = Router::new(dyns, HashMap::from([(tier, ids.clone())]));
    for id in ids {
        r.set_capabilities(
            &id,
            Capabilities {
                streaming: true,
                json_schema: true,
                embeddings: true,
                ..Default::default()
            },
        );
    }
    Arc::new(r)
}
