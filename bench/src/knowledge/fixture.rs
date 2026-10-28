//! The labelled fixture corpus (`bench/fixtures/knowledge/`): loading, validation and ingest.
//!
//! Files (all JSON except the items themselves):
//!
//! | File | Holds |
//! |---|---|
//! | `manifest.json` | One entry per item: id, kind, file, MIME, source id, item key, title, modification time, version and the connector metadata ingest would store |
//! | `items/{mail,calendar,notes}/…` | RFC 822 messages, iCalendar events and Markdown notes (frontmatter, wikilinks, tags) |
//! | `gold/entities.json` | Every entity with its kind, name, surface forms and identifiers (`optional` ones are not counted either way) |
//! | `gold/claims.json` | `(subject, predicate, object)` triples with the items (and quotes) that support them; `status` is `active` or `superseded` |
//! | `gold/mentions.json` | Resolution gold: which entity each mention in each item refers to, with the surface forms used |
//! | `llm_script.json` | What the fake LLM returns for each item, including hallucinated and injected claims |
//! | `scenario.json` | Items to delete, the prompt-injection item, the hard cases, user edits and MCP queries |

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_graph::{EntityKind, Identifier, normalize_name};
use nugget_store::{ChunkId, NewChunk, NewItem, Store, content_hash};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::runner::BenchError;

/// The fixture directory shipped in the source tree.
pub const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/knowledge");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Mail,
    Calendar,
    Note,
}

/// One item of the corpus, as the manifest describes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSpec {
    pub id: String,
    pub kind: ItemKind,
    /// Relative to the fixture directory.
    pub path: String,
    pub mime: String,
    /// Store source id, e.g. `bench:knowledge:mail`.
    pub source: String,
    /// Connector that would have produced it (`imap`, `caldav`, `files`).
    pub connector: String,
    /// Store item key (the connector's key format).
    pub key: String,
    pub title: String,
    /// RFC 3339.
    pub modified: String,
    pub version: String,
    /// Connector metadata, stored under `metadata.source` like ingest does.
    pub metadata: Value,
}

impl ItemSpec {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef {
            source: SourceId::new(self.source.clone()),
            key: ItemKey::new(self.key.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    name: String,
    version: u32,
    items: Vec<ItemSpec>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldEntity {
    pub id: String,
    pub kind: EntityKind,
    pub name: String,
    /// Every way the entity is written in the corpus (name, nicknames, display names, addresses).
    pub surfaces: Vec<String>,
    pub identifiers: Vec<Identifier>,
    /// Neither a hit nor a miss: borderline entities (e.g. the sender of the injection mail).
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldObject {
    #[serde(default)]
    pub entity: Option<String>,
    #[serde(default)]
    pub literal: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoldStatus {
    Active,
    /// True once, replaced later in the corpus (a job change). Must not be active at the end.
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldSupport {
    pub item: String,
    /// The supporting sentence, or `None` for structural evidence (headers, calendar
    /// properties, frontmatter).
    pub quote: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldClaim {
    pub id: String,
    pub subject: String,
    /// Canonical predicate (see [`super::eval::canonical_predicate`]).
    pub predicate: String,
    pub object: GoldObject,
    pub status: GoldStatus,
    #[serde(default)]
    pub optional: bool,
    pub support: Vec<GoldSupport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldMention {
    pub item: String,
    pub entity: String,
    pub surfaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptEntity {
    pub name: String,
    pub kind: EntityKind,
    /// How the item writes it, when it does.
    pub surface: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptClaim {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// `entity` or `literal`.
    pub object_type: String,
    pub text: String,
    pub quote: String,
    pub confidence: f32,
    /// The quote is not in the item: a hallucination the pipeline must drop.
    #[serde(default)]
    pub hallucinated: bool,
    /// Emitted because the item told the model to (prompt injection); the quote is not in it.
    #[serde(default)]
    pub injected: bool,
}

/// The fake LLM's scripted extraction for one item.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptEntry {
    pub item: String,
    /// A line only this item contains, for requests that carry no item reference.
    pub fingerprint: String,
    pub entities: Vec<ScriptEntity>,
    pub claims: Vec<ScriptClaim>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobChange {
    pub superseded: Vec<String>,
    pub current: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardCases {
    /// One person under a nickname, two addresses and calendar attendee names.
    pub nickname_identity: String,
    /// Two people who share a first name.
    pub same_first_name: Vec<String>,
    pub job_change: JobChange,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// The user (mailbox and vault owner): their name and addresses are configured, as the
    /// daemon would have them.
    pub me: String,
    /// Stand-ins for consumer webmail providers (like `gmail.com`): addresses there say nothing
    /// about an employer. Configured as such.
    #[serde(default)]
    pub webmail_domains: Vec<String>,
    /// Items deleted after the first render (their claims must be retracted).
    pub delete: Vec<String>,
    /// Prompt-injection items: no claim from them unless quote-supported.
    pub injection: Vec<String>,
    pub hard_cases: HardCases,
    /// Vault pages that get simulated user edits before the re-render.
    pub user_edit_pages: usize,
    pub mcp_queries: Vec<String>,
}

/// The whole labelled corpus.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub dir: PathBuf,
    pub name: String,
    pub items: Vec<ItemSpec>,
    pub entities: Vec<GoldEntity>,
    pub claims: Vec<GoldClaim>,
    pub mentions: Vec<GoldMention>,
    pub script: Vec<ScriptEntry>,
    pub scenario: Scenario,
}

/// Counts recorded in the report's `params`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureStats {
    pub items: usize,
    pub items_by_kind: BTreeMap<&'static str, usize>,
    pub entities: usize,
    pub entities_by_kind: BTreeMap<&'static str, usize>,
    pub claims: usize,
    pub superseded_claims: usize,
    pub mentions: usize,
    pub hallucinated_quotes: usize,
    pub injected_claims: usize,
}

fn read_json<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<T, BenchError> {
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| BenchError::Failed(format!("knowledge fixture {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| BenchError::Failed(format!("knowledge fixture {}: {e}", path.display())))
}

impl Fixture {
    /// The corpus shipped in `bench/fixtures/knowledge/`.
    pub fn builtin() -> Result<Self, BenchError> {
        Self::load(Path::new(FIXTURE_DIR))
    }

    pub fn load(dir: &Path) -> Result<Self, BenchError> {
        let manifest: Manifest = read_json(dir, "manifest.json")?;
        if manifest.version != 1 {
            return Err(BenchError::Failed(format!(
                "knowledge fixture manifest version {} (expected 1)",
                manifest.version
            )));
        }
        let fx = Self {
            dir: dir.to_owned(),
            name: manifest.name,
            items: manifest.items,
            entities: read_json(dir, "gold/entities.json")?,
            claims: read_json(dir, "gold/claims.json")?,
            mentions: read_json(dir, "gold/mentions.json")?,
            script: read_json(dir, "llm_script.json")?,
            scenario: read_json(dir, "scenario.json")?,
        };
        fx.validate()
            .map_err(|e| BenchError::Failed(format!("knowledge fixture {}: {e}", dir.display())))?;
        Ok(fx)
    }

    /// Referential integrity: unique ids, every reference resolves, files exist.
    pub fn validate(&self) -> Result<(), String> {
        let mut items = HashSet::new();
        let mut refs = HashSet::new();
        for it in &self.items {
            if !items.insert(it.id.as_str()) {
                return Err(format!("duplicate item id {}", it.id));
            }
            if !refs.insert(it.item_ref()) {
                return Err(format!("duplicate item ref for {}", it.id));
            }
            if OffsetDateTime::parse(&it.modified, &Rfc3339).is_err() {
                return Err(format!("item {}: bad modified time", it.id));
            }
            if !self.dir.join(&it.path).is_file() {
                return Err(format!("item {}: missing file {}", it.id, it.path));
            }
        }
        let mut ents = HashSet::new();
        for e in &self.entities {
            if !ents.insert(e.id.as_str()) {
                return Err(format!("duplicate entity id {}", e.id));
            }
            if e.surfaces.is_empty() || normalize_name(&e.name).is_empty() {
                return Err(format!("entity {} has no name", e.id));
            }
        }
        let entity = |id: &str, what: &str| {
            ents.contains(id)
                .then_some(())
                .ok_or_else(|| format!("{what}: unknown entity {id}"))
        };
        let item = |id: &str, what: &str| {
            items
                .contains(id)
                .then_some(())
                .ok_or_else(|| format!("{what}: unknown item {id}"))
        };
        let mut claim_ids = HashSet::new();
        for c in &self.claims {
            if !claim_ids.insert(c.id.as_str()) {
                return Err(format!("duplicate claim id {}", c.id));
            }
            entity(&c.subject, &c.id)?;
            match (&c.object.entity, &c.object.literal) {
                (Some(o), None) => entity(o, &c.id)?,
                (None, Some(l)) if !l.trim().is_empty() => {}
                _ => {
                    return Err(format!(
                        "claim {}: object must be one entity or literal",
                        c.id
                    ));
                }
            }
            if c.support.is_empty() {
                return Err(format!("claim {} has no support", c.id));
            }
            for s in &c.support {
                item(&s.item, &c.id)?;
            }
        }
        for m in &self.mentions {
            item(&m.item, "mention")?;
            entity(&m.entity, "mention")?;
            if m.surfaces.is_empty() {
                return Err(format!(
                    "mention of {} in {} has no surface",
                    m.entity, m.item
                ));
            }
        }
        let mut scripted = HashSet::new();
        for s in &self.script {
            item(&s.item, "llm script")?;
            if !scripted.insert(s.item.as_str()) {
                return Err(format!("llm script: item {} twice", s.item));
            }
            for c in &s.claims {
                for name in [&c.subject, &c.object] {
                    if name.trim().is_empty() {
                        return Err(format!("llm script {}: empty name", s.item));
                    }
                }
                if c.object_type != "entity" && c.object_type != "literal" {
                    return Err(format!("llm script {}: bad object_type", s.item));
                }
            }
        }
        let sc = &self.scenario;
        for id in sc.delete.iter().chain(&sc.injection) {
            item(id, "scenario")?;
        }
        entity(&sc.me, "scenario")?;
        entity(&sc.hard_cases.nickname_identity, "scenario")?;
        for id in &sc.hard_cases.same_first_name {
            entity(id, "scenario")?;
        }
        for id in sc
            .hard_cases
            .job_change
            .superseded
            .iter()
            .chain(&sc.hard_cases.job_change.current)
        {
            if !claim_ids.contains(id.as_str()) {
                return Err(format!("scenario: unknown claim {id}"));
            }
        }
        Ok(())
    }

    pub fn item(&self, id: &str) -> Option<&ItemSpec> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn entity(&self, id: &str) -> Option<&GoldEntity> {
        self.entities.iter().find(|e| e.id == id)
    }

    pub fn stats(&self) -> FixtureStats {
        let mut items_by_kind = BTreeMap::new();
        for it in &self.items {
            let k = match it.kind {
                ItemKind::Mail => "mail",
                ItemKind::Calendar => "calendar",
                ItemKind::Note => "note",
            };
            *items_by_kind.entry(k).or_default() += 1;
        }
        let mut entities_by_kind = BTreeMap::new();
        for e in self.entities.iter().filter(|e| !e.optional) {
            *entities_by_kind.entry(e.kind.as_str()).or_default() += 1;
        }
        let scripted = self.script.iter().flat_map(|s| &s.claims);
        FixtureStats {
            items: self.items.len(),
            items_by_kind,
            entities: self.entities.iter().filter(|e| !e.optional).count(),
            entities_by_kind,
            claims: self.claims.iter().filter(|c| !c.optional).count(),
            superseded_claims: self
                .claims
                .iter()
                .filter(|c| c.status == GoldStatus::Superseded)
                .count(),
            mentions: self.mentions.len(),
            hallucinated_quotes: scripted.clone().filter(|c| c.hallucinated).count(),
            injected_claims: scripted.filter(|c| c.injected).count(),
        }
    }
}

/// One item as stored: its ref, extracted text and chunks.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredItem {
    pub id: String,
    pub item: ItemRef,
    pub text: String,
    pub chunks: Vec<(ChunkId, String)>,
    /// The item's extracted fields (mail headers, frontmatter, ...) as `key: value` lines,
    /// against which evidence that names no chunk (structural evidence) is checked.
    pub fields: String,
}

/// The corpus after ingest into a [`Store`].
#[derive(Debug, Clone, Default)]
pub struct Ingested {
    pub items: Vec<StoredItem>,
    by_id: HashMap<String, usize>,
    by_ref: HashMap<ItemRef, usize>,
    chunk_owner: HashMap<ChunkId, usize>,
}

impl Ingested {
    pub fn get(&self, id: &str) -> Option<&StoredItem> {
        self.by_id.get(id).map(|&i| &self.items[i])
    }

    pub fn by_ref(&self, r: &ItemRef) -> Option<&StoredItem> {
        self.by_ref.get(r).map(|&i| &self.items[i])
    }

    /// The stored item a chunk belongs to.
    pub fn chunk_item(&self, chunk: ChunkId) -> Option<&StoredItem> {
        self.chunk_owner.get(&chunk).map(|&i| &self.items[i])
    }

    pub fn chunk_count(&self) -> usize {
        self.chunk_owner.len()
    }

    pub fn refs(&self) -> Vec<ItemRef> {
        self.items.iter().map(|i| i.item.clone()).collect()
    }
}

/// Write every fixture item into `store` the way `nugget-ingest` would: extract with
/// `nugget-extract`, chunk with the default options, and store the item with ingest's metadata
/// layout (`{source, mime, extracted?, links?, tags?, sensitive, junk}`), its text and chunks.
/// No embeddings (the knowledge pipeline reads text).
pub fn ingest(store: &Store, fx: &Fixture) -> Result<Ingested, BenchError> {
    let opts = nugget_extract::ExtractOptions::default();
    let chunk_opts = nugget_extract::ChunkOptions::default();
    let mut out = Ingested::default();
    for spec in &fx.items {
        let bytes = std::fs::read(fx.dir.join(&spec.path))?;
        let ex = nugget_extract::extract(&spec.mime, nugget_extract::Source::Bytes(&bytes), &opts)
            .map_err(|e| BenchError::Failed(format!("extract {}: {e}", spec.id)))?;
        let chunks: Vec<String> = nugget_extract::chunk(&ex, &chunk_opts)
            .into_iter()
            .map(|c| c.text)
            .filter(|t| !t.trim().is_empty())
            .collect();
        let mut meta = Map::new();
        meta.insert("source".into(), spec.metadata.clone());
        meta.insert("mime".into(), json!(spec.mime));
        if ex.metadata.as_object().is_some_and(|o| !o.is_empty()) {
            meta.insert("extracted".into(), ex.metadata.clone());
        }
        if !ex.links.is_empty() {
            meta.insert("links".into(), json!(ex.links));
        }
        if !ex.tags.is_empty() {
            meta.insert("tags".into(), json!(ex.tags));
        }
        meta.insert("sensitive".into(), json!(false));
        meta.insert("junk".into(), json!(false));
        let hash = content_hash(format!("nugget-ingest/v1\0{}\0{}", spec.mime, ex.text));
        let mut new = NewItem::new(spec.source.as_str(), spec.key.as_str(), hash)
            .with_mime(spec.mime.clone())
            .with_metadata(Value::Object(meta));
        new.title = ex.title.clone().or_else(|| Some(spec.title.clone()));
        new.version = Some(spec.version.clone());
        new.source_modified_at = OffsetDateTime::parse(&spec.modified, &Rfc3339).ok();
        new.text = Some(ex.text.clone());
        let item = spec.item_ref();
        store.upsert_item(&new)?;
        let new_chunks: Vec<NewChunk> = chunks.iter().cloned().map(NewChunk::text).collect();
        let ids = store.replace_chunks(&item, &new_chunks)?;
        let idx = out.items.len();
        for id in &ids {
            out.chunk_owner.insert(*id, idx);
        }
        out.by_id.insert(spec.id.clone(), idx);
        out.by_ref.insert(item.clone(), idx);
        out.items.push(StoredItem {
            id: spec.id.clone(),
            item,
            fields: field_lines(&ex.metadata),
            text: ex.text,
            chunks: ids.into_iter().zip(chunks).collect(),
        });
    }
    Ok(out)
}

/// `key: value` lines for every scalar (or list of scalars) in extracted metadata and in its
/// `frontmatter` object, each also with `[[wikilink]]` brackets removed.
pub fn field_lines(meta: &Value) -> String {
    fn scalar(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut push = |k: &str, v: &Value| {
        let values: Vec<String> = match v {
            Value::Array(a) => a.iter().filter_map(scalar).collect(),
            v => scalar(v).into_iter().collect(),
        };
        for v in values {
            out.push(format!("{k}: {v}"));
            let bare = v.replace("[[", "").replace("]]", "");
            if bare != v {
                out.push(format!("{k}: {bare}"));
            }
        }
    };
    let Some(obj) = meta.as_object() else {
        return String::new();
    };
    for (k, v) in obj {
        push(k, v);
    }
    if let Some(fm) = obj.get("frontmatter").and_then(Value::as_object) {
        for (k, v) in fm {
            push(k, v);
        }
    }
    out.join("\n")
}

/// Quote matching: whitespace collapsed, case folded, typographic quotes and dashes unified.
pub fn normalize_quote(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{2032}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            c => c,
        })
        .collect();
    mapped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether `quote` occurs in `text` under [`normalize_quote`]. An empty quote never does.
pub fn quote_in(quote: &str, text: &str) -> bool {
    let q = normalize_quote(quote);
    !q.is_empty() && normalize_quote(text).contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_fixture_loads_and_has_the_promised_shape() {
        let fx = Fixture::builtin().unwrap();
        let s = fx.stats();
        assert!((60..=120).contains(&s.items), "{s:?}");
        assert_eq!(s.entities_by_kind["person"], 25);
        assert_eq!(s.entities_by_kind["organization"], 8);
        assert_eq!(s.entities_by_kind["project"], 10);
        for k in ["mail", "calendar", "note"] {
            assert!(s.items_by_kind[k] > 0, "{k}");
        }
        assert!(s.superseded_claims > 0 && s.hallucinated_quotes > 0 && s.injected_claims > 0);
        // Fictional domains only.
        for e in &fx.entities {
            for i in &e.identifiers {
                if i.scheme == "email" || i.scheme == "domain" {
                    let host = i.value.rsplit('@').next().unwrap();
                    assert!(
                        host.ends_with(".test") || host.ends_with("example.com"),
                        "{}",
                        i.value
                    );
                }
            }
        }
    }

    #[test]
    fn validation_catches_broken_references() {
        let fx = Fixture::builtin().unwrap();
        let mut bad = fx.clone();
        bad.claims[0].subject = "p-nobody".into();
        assert!(bad.validate().unwrap_err().contains("unknown entity"));
        let mut bad = fx.clone();
        bad.scenario.delete.push("mail-999".into());
        assert!(bad.validate().unwrap_err().contains("unknown item"));
        let mut bad = fx;
        bad.items.push(bad.items[0].clone());
        assert!(bad.validate().unwrap_err().contains("duplicate"));
    }

    /// The labels agree with what ingest actually stores: every gold and scripted quote is in its
    /// item's chunks, every hallucinated or injected quote is not, and every gold surface form is
    /// in its raw item.
    #[test]
    fn labels_agree_with_ingested_text() {
        let fx = Fixture::builtin().unwrap();
        let store = Store::open_in_memory().unwrap();
        let ing = ingest(&store, &fx).unwrap();
        assert_eq!(ing.items.len(), fx.items.len());
        let chunk_text = |id: &str| {
            let it = ing.get(id).unwrap();
            it.chunks
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        for c in &fx.claims {
            for s in &c.support {
                if let Some(q) = &s.quote {
                    assert!(
                        quote_in(q, &chunk_text(&s.item)),
                        "{} in {}: {q:?}",
                        c.id,
                        s.item
                    );
                }
            }
        }
        for e in &fx.script {
            let text = chunk_text(&e.item);
            for c in &e.claims {
                let found = quote_in(&c.quote, &text);
                assert_eq!(
                    found,
                    !(c.hallucinated || c.injected),
                    "{}: {:?}",
                    e.item,
                    c.quote
                );
            }
            assert!(quote_in(&e.fingerprint, &text), "fingerprint of {}", e.item);
        }
        for m in &fx.mentions {
            let raw =
                std::fs::read_to_string(fx.dir.join(&fx.item(&m.item).unwrap().path)).unwrap();
            let raw = raw.to_lowercase();
            for s in &m.surfaces {
                assert!(
                    raw.contains(&s.to_lowercase()),
                    "{} {} {s}",
                    m.item,
                    m.entity
                );
            }
        }
        // Every stored item has text and at least one chunk.
        for it in &ing.items {
            assert!(!it.chunks.is_empty(), "{}", it.id);
            assert_eq!(ing.by_ref(&it.item).unwrap().id, it.id);
        }
    }

    #[test]
    fn quote_normalisation() {
        assert!(quote_in(
            "I  prefer\nmorning",
            "Yes, i prefer morning meetings"
        ));
        assert!(quote_in("Robin\u{2019}s plan", "robin's plan"));
        assert!(!quote_in("", "anything"));
        assert!(!quote_in("black coffee", "vegetarian food"));
    }
}
