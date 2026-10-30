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
    /// The work package that added it (`ee1`); absent for the original M2 corpus.
    #[serde(default)]
    pub added: Option<String>,
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
    /// The work package that added it (`ee1`); absent for the original M2 corpus.
    #[serde(default)]
    pub added: Option<String>,
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

/// One job change: claims that must end up inactive, and the ones that replace them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobChange {
    pub superseded: Vec<String>,
    pub current: Vec<String>,
    /// The work package that added the case (`ee1`); absent for the original M2 cases.
    #[serde(default)]
    pub added: Option<String>,
}

/// A name in an item that nothing in the item ties to a known person (a bare first name or
/// nickname with no corroborating header, thread, organisation or context). Its gold mention, if
/// any, is of an `optional` entity; the hard case requires that the system does not attribute it
/// to the case's people.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unattributed {
    pub item: String,
    pub surface: String,
}

/// One person written under a nickname, display-name variants, initials or several addresses.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NicknameCase {
    pub entity: String,
    /// The same nickname used for somebody the corpus cannot identify.
    #[serde(default)]
    pub unattributed: Vec<Unattributed>,
    #[serde(default)]
    pub added: Option<String>,
}

/// People who share a first name.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SameFirstNameCase {
    pub entities: Vec<String>,
    /// The bare first name where nothing says which of them (if either) is meant.
    #[serde(default)]
    pub unattributed: Vec<Unattributed>,
    #[serde(default)]
    pub added: Option<String>,
}

/// The resolution and time-awareness hard cases. Each metric (`hard_cases.<kind>`) passes when
/// every case of its kind in scope is handled. The first case of each kind is the original M2
/// case; a real-model subset always keeps its items (see [`Fixture::select_items`]).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardCases {
    pub nickname_identity: Vec<NicknameCase>,
    pub same_first_name: Vec<SameFirstNameCase>,
    pub job_change: Vec<JobChange>,
}

impl HardCases {
    /// Every claim id a job-change case names.
    pub fn job_change_claims(&self) -> impl Iterator<Item = &String> {
        self.job_change
            .iter()
            .flat_map(|j| j.superseded.iter().chain(&j.current))
    }

    /// Every unattributed mention, of either kind.
    pub fn unattributed(&self) -> impl Iterator<Item = &Unattributed> {
        self.nickname_identity
            .iter()
            .flat_map(|c| &c.unattributed)
            .chain(self.same_first_name.iter().flat_map(|c| &c.unattributed))
    }
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

/// Which corpus a run uses (`--knowledge-corpus`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Corpus {
    /// Every item and hard case.
    #[default]
    All,
    /// The corpus as it was before EE1 (93 items): everything marked `added` is left out, so
    /// item selection and the scored gold equal the pre-EE1 bench's, for before/after runs.
    Original,
}

impl Corpus {
    pub fn as_str(self) -> &'static str {
        match self {
            Corpus::All => "all",
            Corpus::Original => "original",
        }
    }
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

    /// The corpus restricted to `corpus`. `Original` drops the items, entities and hard cases
    /// marked `added`, the mentions and scripted answers of dropped items, their support on
    /// the remaining claims, and the claims left with no support (which only they stated).
    pub fn corpus(&self, corpus: Corpus) -> Fixture {
        let mut fx = self.clone();
        if corpus == Corpus::All {
            return fx;
        }
        let dropped: HashSet<String> = fx
            .items
            .iter()
            .filter(|i| i.added.is_some())
            .map(|i| i.id.clone())
            .collect();
        fx.items.retain(|i| i.added.is_none());
        fx.entities.retain(|e| e.added.is_none());
        fx.mentions.retain(|m| !dropped.contains(&m.item));
        fx.script.retain(|s| !dropped.contains(&s.item));
        for c in &mut fx.claims {
            c.support.retain(|s| !dropped.contains(&s.item));
        }
        fx.claims.retain(|c| !c.support.is_empty());
        let hc = &mut fx.scenario.hard_cases;
        hc.nickname_identity.retain(|c| c.added.is_none());
        hc.same_first_name.retain(|c| c.added.is_none());
        hc.job_change.retain(|c| c.added.is_none());
        fx
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
        let hc = &sc.hard_cases;
        if hc.nickname_identity.is_empty()
            || hc.same_first_name.is_empty()
            || hc.job_change.is_empty()
        {
            return Err("scenario: every hard-case kind needs at least one case".into());
        }
        for c in &hc.nickname_identity {
            entity(&c.entity, "scenario")?;
        }
        for c in &hc.same_first_name {
            if c.entities.len() < 2 {
                return Err("scenario: a same-first-name case needs two people".into());
            }
            for id in &c.entities {
                entity(id, "scenario")?;
            }
        }
        for u in hc.unattributed() {
            item(&u.item, "scenario")?;
        }
        for id in hc.job_change_claims() {
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

    /// Up to `n` item ids for a real-model run, in manifest order, deterministic: first the items
    /// the scenario needs (the deleted items, the prompt-injection items, and for the first case
    /// of each hard-case kind: the items supporting the job-change claims (other than those of
    /// a further case), two items mentioning the nickname person and one per same-first-name
    /// person), then each further hard case as a whole (every item mentioning its people or
    /// supporting its claims, plus its unattributed mentions) while it fits, then items evenly
    /// spaced through the rest of the manifest (which keeps the mail / calendar / note mix). The
    /// first group is always kept, even when it alone exceeds `n`. `n >= items` keeps
    /// everything.
    pub fn select_items(&self, n: usize) -> Vec<String> {
        if n >= self.items.len() {
            return self.items.iter().map(|i| i.id.clone()).collect();
        }
        let sc = &self.scenario;
        let mut keep: Vec<&str> = Vec::new();
        fn add<'a>(fx: &'a Fixture, id: &str, keep: &mut Vec<&'a str>) {
            if let Some(it) = fx.item(id)
                && !keep.contains(&it.id.as_str())
            {
                keep.push(&it.id);
            }
        }
        for id in sc.delete.iter().chain(&sc.injection) {
            add(self, id, &mut keep);
        }
        let hc = &sc.hard_cases;
        let supporting = |jc: &JobChange| -> Vec<&str> {
            self.claims
                .iter()
                .filter(|c| jc.superseded.contains(&c.id) || jc.current.contains(&c.id))
                .flat_map(|c| c.support.iter().map(|s| s.item.as_str()))
                .collect()
        };
        let mentioned = |entity: &str| -> Vec<&str> {
            let mut v: Vec<&str> = self
                .mentions
                .iter()
                .filter(|m| m.entity == entity)
                .map(|m| m.item.as_str())
                .collect();
            v.sort_by_key(|id| self.items.iter().position(|i| i.id == *id));
            v.dedup();
            v
        };
        // Every further case, whole: all the items mentioning its people or supporting its
        // claims, and its unattributed mentions.
        let mut further: Vec<Vec<&str>> = Vec::new();
        for c in hc.nickname_identity.iter().skip(1) {
            let mut v = mentioned(&c.entity);
            v.extend(c.unattributed.iter().map(|u| u.item.as_str()));
            further.push(v);
        }
        for c in hc.same_first_name.iter().skip(1) {
            let mut v: Vec<&str> = c.entities.iter().flat_map(|e| mentioned(e)).collect();
            v.extend(c.unattributed.iter().map(|u| u.item.as_str()));
            further.push(v);
        }
        for jc in hc.job_change.iter().skip(1) {
            further.push(supporting(jc));
        }
        // The original case of each kind (the first): always kept. For the job change, the
        // items supporting its claims, except those of a further case (which come whole or not
        // at all).
        if let Some(jc) = hc.job_change.first() {
            for id in supporting(jc) {
                if !further.iter().flatten().any(|f| *f == id) {
                    add(self, id, &mut keep);
                }
            }
        }
        if let Some(c) = hc.nickname_identity.first() {
            let nick = mentioned(&c.entity);
            for id in &nick {
                if nick.iter().filter(|i| keep.contains(i)).count() >= 2 {
                    break;
                }
                add(self, id, &mut keep);
            }
        }
        if let Some(c) = hc.same_first_name.first() {
            for e in &c.entities {
                let items = mentioned(e);
                if !items.iter().any(|i| keep.contains(i))
                    && let Some(first) = items.first()
                {
                    add(self, first, &mut keep);
                }
            }
        }
        // Then each further case, while it fits in `n`.
        for case in further {
            let mut new: Vec<&str> = case.into_iter().filter(|id| !keep.contains(id)).collect();
            new.sort_unstable();
            new.dedup();
            if keep.len() + new.len() <= n {
                for id in new {
                    add(self, id, &mut keep);
                }
            }
        }
        let rest: Vec<&str> = self
            .items
            .iter()
            .map(|i| i.id.as_str())
            .filter(|id| !keep.contains(id))
            .collect();
        let want = n.saturating_sub(keep.len()).min(rest.len());
        for k in 0..want {
            keep.push(rest[k * rest.len() / want]);
        }
        self.items
            .iter()
            .filter(|i| keep.contains(&i.id.as_str()))
            .map(|i| i.id.clone())
            .collect()
    }

    /// The corpus restricted to `keep` (item ids), for scoring a run that processed only those
    /// items. Precision and recall are then over what those items support:
    ///
    /// - items, scripted extractions and gold mentions: those of kept items;
    /// - a gold claim is in scope when at least one of its supporting items is kept (its support
    ///   is cut to those items); a superseded claim stays superseded only while every current
    ///   job-change claim is in scope too (otherwise nothing in the subset supersedes it);
    /// - a gold entity is in scope when a kept item mentions it or an in-scope claim names it;
    /// - out-of-scope claims and entities become `optional`: neither a hit nor a miss, so they
    ///   leave the recall denominators and a system entity matching one is not a false positive;
    /// - the scenario's deleted and injection items: the kept ones.
    pub fn subset(&self, keep: &[String]) -> Fixture {
        let kept: HashSet<&str> = keep.iter().map(String::as_str).collect();
        let mut fx = self.clone();
        fx.items.retain(|i| kept.contains(i.id.as_str()));
        fx.script.retain(|s| kept.contains(s.item.as_str()));
        fx.mentions.retain(|m| kept.contains(m.item.as_str()));
        for c in &mut fx.claims {
            let support: Vec<GoldSupport> = c
                .support
                .iter()
                .filter(|s| kept.contains(s.item.as_str()))
                .cloned()
                .collect();
            if support.is_empty() {
                c.optional = true;
            } else {
                c.support = support;
            }
        }
        // A superseded claim stays scored only while a job change that replaces it (one naming
        // it, else one about the same person) has all its current claims in scope.
        let active: HashSet<&str> = fx
            .claims
            .iter()
            .filter(|c| !c.optional && c.status == GoldStatus::Active)
            .map(|c| c.id.as_str())
            .collect();
        let subject: HashMap<&str, &str> = fx
            .claims
            .iter()
            .map(|c| (c.id.as_str(), c.subject.as_str()))
            .collect();
        let jcs = &fx.scenario.hard_cases.job_change;
        let unreplaced: Vec<String> = fx
            .claims
            .iter()
            .filter(|c| c.status == GoldStatus::Superseded)
            .filter(|c| {
                let naming: Vec<&JobChange> = jcs
                    .iter()
                    .filter(|j| j.superseded.contains(&c.id))
                    .collect();
                let cases = if naming.is_empty() {
                    jcs.iter()
                        .filter(|j| {
                            j.current
                                .iter()
                                .any(|id| subject.get(id.as_str()) == Some(&c.subject.as_str()))
                        })
                        .collect()
                } else {
                    naming
                };
                !cases
                    .iter()
                    .any(|j| j.current.iter().all(|id| active.contains(id.as_str())))
            })
            .map(|c| c.id.clone())
            .collect();
        for c in &mut fx.claims {
            if unreplaced.contains(&c.id) {
                c.optional = true;
            }
        }
        let mut in_scope: HashSet<String> = fx.mentions.iter().map(|m| m.entity.clone()).collect();
        for c in fx.claims.iter().filter(|c| !c.optional) {
            in_scope.insert(c.subject.clone());
            in_scope.extend(c.object.entity.clone());
        }
        for e in &mut fx.entities {
            if !in_scope.contains(&e.id) {
                e.optional = true;
            }
        }
        fx.scenario.delete.retain(|id| kept.contains(id.as_str()));
        fx.scenario
            .injection
            .retain(|id| kept.contains(id.as_str()));
        fx
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
        assert_eq!(s.entities_by_kind["person"], 30);
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
        // An unattributed name is in its item, and the gold labels do not give it to the people
        // it must not be attributed to (it is an optional entity's mention, if anyone's).
        let hc = &fx.scenario.hard_cases;
        let cases = hc
            .nickname_identity
            .iter()
            .map(|c| (vec![c.entity.clone()], &c.unattributed))
            .chain(
                hc.same_first_name
                    .iter()
                    .map(|c| (c.entities.clone(), &c.unattributed)),
            );
        for (people, unattributed) in cases {
            for u in unattributed {
                let raw =
                    std::fs::read_to_string(fx.dir.join(&fx.item(&u.item).unwrap().path)).unwrap();
                assert!(raw.contains(&u.surface), "{} {}", u.item, u.surface);
                for m in fx.mentions.iter().filter(|m| m.item == u.item) {
                    let named = m
                        .surfaces
                        .iter()
                        .any(|s| normalize_name(s) == normalize_name(&u.surface));
                    if named {
                        assert!(!people.contains(&m.entity), "{} {}", u.item, m.entity);
                        assert!(fx.entity(&m.entity).unwrap().optional, "{}", m.entity);
                    }
                }
            }
        }
        // Every stored item has text and at least one chunk.
        for it in &ing.items {
            assert!(!it.chunks.is_empty(), "{}", it.id);
            assert_eq!(ing.by_ref(&it.item).unwrap().id, it.id);
        }
    }

    #[test]
    fn real_model_subsets_keep_the_scenario_and_spread_the_rest() {
        let fx = Fixture::builtin().unwrap();
        let all = fx.select_items(usize::MAX);
        assert_eq!(all.len(), fx.items.len());
        let small = fx.select_items(8);
        assert_eq!(small, fx.select_items(8), "deterministic");
        assert_eq!(small.len(), 8);
        for id in fx.scenario.delete.iter().chain(&fx.scenario.injection) {
            assert!(small.contains(id), "{id}");
        }
        let quick = fx.select_items(30);
        assert_eq!(quick.len(), 30);
        assert!(small.iter().all(|id| quick.contains(id)));
        for kind in [ItemKind::Mail, ItemKind::Calendar, ItemKind::Note] {
            assert!(
                quick.iter().any(|id| fx.item(id).unwrap().kind == kind),
                "{kind:?}"
            );
        }
        // The quick subset carries the further hard cases whole: each item mentioning their
        // people, holding an unattributed name or supporting a further job change's claims.
        let hc = &fx.scenario.hard_cases;
        let people: Vec<&String> = hc
            .nickname_identity
            .iter()
            .skip(1)
            .map(|c| &c.entity)
            .chain(hc.same_first_name.iter().skip(1).flat_map(|c| &c.entities))
            .collect();
        let mut needed: Vec<&str> = fx
            .mentions
            .iter()
            .filter(|m| people.contains(&&m.entity))
            .map(|m| m.item.as_str())
            .collect();
        needed.extend(hc.unattributed().map(|u| u.item.as_str()));
        let jc: Vec<&String> = hc
            .job_change
            .iter()
            .skip(1)
            .flat_map(|j| j.superseded.iter().chain(&j.current))
            .collect();
        needed.extend(
            fx.claims
                .iter()
                .filter(|c| jc.contains(&&c.id))
                .flat_map(|c| c.support.iter().map(|s| s.item.as_str())),
        );
        assert!(needed.len() >= 10);
        for id in needed {
            assert!(quick.contains(&id.to_owned()), "{id}");
        }
        // Manifest order, no duplicates.
        let pos: Vec<usize> = quick
            .iter()
            .map(|id| fx.items.iter().position(|i| &i.id == id).unwrap())
            .collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]));
    }

    /// `--knowledge-corpus original` is the pre-EE1 corpus (d643a5a): the same counts, and the
    /// same real-model item selection for every size. The lists were computed with the pre-EE1
    /// `select_items` on the pre-EE1 fixture.
    #[test]
    fn the_original_corpus_reproduces_the_pre_ee1_selection() {
        let fx = Fixture::builtin().unwrap().corpus(Corpus::Original);
        fx.validate().unwrap();
        let s = fx.stats();
        assert_eq!(s.items, 93);
        assert_eq!(
            (s.items_by_kind["mail"], s.items_by_kind["calendar"]),
            (48, 15)
        );
        assert_eq!(s.items_by_kind["note"], 30);
        assert_eq!(s.entities, 85);
        assert_eq!(s.entities_by_kind["person"], 25);
        assert_eq!(fx.entities.len(), 88);
        assert_eq!(s.claims, 166);
        assert_eq!(fx.claims.len(), 181);
        assert_eq!(s.superseded_claims, 3);
        assert_eq!(s.mentions, 515);
        let scripted: usize = fx.script.iter().map(|e| e.claims.len()).sum();
        assert_eq!(scripted, 144);
        assert_eq!((s.hallucinated_quotes, s.injected_claims), (4, 4));
        let hc = &fx.scenario.hard_cases;
        assert_eq!(hc.nickname_identity.len(), 1);
        assert_eq!(hc.same_first_name.len(), 1);
        assert_eq!(hc.job_change.len(), 1);
        assert!(fx.items.iter().all(|i| i.added.is_none()));
        assert!(
            fx.claims
                .iter()
                .flat_map(|c| &c.support)
                .all(|s| fx.item(&s.item).is_some())
        );

        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let smoke = ids(&[
            "mail-002", "mail-004", "mail-006", "mail-008", "mail-022", "mail-025", "note-002",
            "note-025",
        ]);
        let quick = ids(&[
            "mail-001", "mail-002", "mail-004", "mail-006", "mail-007", "mail-008", "mail-012",
            "mail-016", "mail-020", "mail-022", "mail-025", "mail-026", "mail-030", "mail-034",
            "mail-037", "mail-041", "mail-045", "cal-001", "cal-005", "cal-009", "cal-013",
            "note-001", "note-002", "note-006", "note-010", "note-014", "note-018", "note-022",
            "note-025", "note-027",
        ]);
        let forty = ids(&[
            "mail-001", "mail-002", "mail-004", "mail-005", "mail-006", "mail-008", "mail-010",
            "mail-012", "mail-015", "mail-018", "mail-020", "mail-022", "mail-024", "mail-025",
            "mail-028", "mail-030", "mail-033", "mail-036", "mail-038", "mail-041", "mail-044",
            "mail-046", "cal-001", "cal-004", "cal-006", "cal-009", "cal-012", "cal-014",
            "note-002", "note-003", "note-006", "note-008", "note-011", "note-014", "note-016",
            "note-019", "note-022", "note-024", "note-025", "note-028",
        ]);
        assert_eq!(fx.select_items(8), smoke);
        assert_eq!(fx.select_items(30), quick);
        assert_eq!(fx.select_items(40), forty);
        assert_eq!(fx.select_items(93).len(), 93);
        assert_eq!(fx.select_items(usize::MAX).len(), 93);
        // The default corpus keeps everything.
        let all = Fixture::builtin().unwrap();
        assert_eq!(all.corpus(Corpus::All).items.len(), all.items.len());
        assert!(all.items.len() > 93);
    }

    /// Scoring a subset: only what the kept items support counts.
    #[test]
    fn subsets_restrict_the_gold_to_the_kept_items() {
        let fx = Fixture::builtin().unwrap();
        let keep = fx.select_items(8);
        let sub = fx.subset(&keep);
        assert_eq!(sub.items.len(), 8);
        assert!(sub.mentions.iter().all(|m| keep.contains(&m.item)));
        assert!(sub.script.iter().all(|s| keep.contains(&s.item)));
        for c in sub.claims.iter().filter(|c| !c.optional) {
            assert!(c.support.iter().all(|s| keep.contains(&s.item)), "{}", c.id);
        }
        // Every in-scope entity is mentioned in a kept item or named by an in-scope claim.
        for e in sub.entities.iter().filter(|e| !e.optional) {
            let mentioned = sub.mentions.iter().any(|m| m.entity == e.id);
            let named =
                sub.claims.iter().filter(|c| !c.optional).any(|c| {
                    c.subject == e.id || c.object.entity.as_deref() == Some(e.id.as_str())
                });
            assert!(mentioned || named, "{}", e.id);
        }
        let (s, f) = (sub.stats(), fx.stats());
        assert!(s.entities > 0 && s.entities < f.entities);
        assert!(s.claims > 0 && s.claims < f.claims);
        // The job change is in the smoke subset, so the superseded claims stay superseded.
        assert!(
            sub.claims
                .iter()
                .any(|c| c.status == GoldStatus::Superseded && !c.optional)
        );
        // Without the new job, nothing supersedes the old one: not scored either way.
        let old_only = fx.subset(&["mail-002".to_owned()]);
        assert!(
            old_only
                .claims
                .iter()
                .filter(|c| c.status == GoldStatus::Superseded)
                .all(|c| c.optional)
        );
        assert!(old_only.scenario.delete.is_empty());
        // Each job change on its own: the second one's new job (mail-053) keeps its old claims
        // superseded, while the first one's (no new job in scope) are not scored.
        let second = fx.subset(&["mail-053".to_owned(), "mail-055".to_owned()]);
        let superseded_scored = |sub: &Fixture, who: &str| {
            sub.claims
                .iter()
                .filter(|c| c.status == GoldStatus::Superseded && c.subject == who)
                .all(|c| !c.optional)
        };
        assert!(superseded_scored(&second, "p-delphine"));
        assert!(!superseded_scored(&second, "p-priya"));
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
