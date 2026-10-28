//! The Markdown vault: the knowledge graph rendered as one page per entity, with user edits
//! merged back.
//!
//! # Layout
//! ```text
//! <root>/README.md                          index of pages by kind
//! <root>/<Kind>/<Name> (<short id>).md      one page per active entity
//! <root>/.nugget/state.json                 page manifest: path, write fingerprint, conflicts
//! <root>/.nugget/base/entities/<id>.md      last rendered version of each page (merge base)
//! <root>/.nugget/base/README.md             last rendered index
//! <root>/.nugget/conflicts/<page path>      generated version of a page whose edit conflicted
//! ```
//!
//! `.nugget/` holds plain files, like the vault itself. The merge base is the previous *render*
//! and the manifest holds paths, hashes and ids, so `.nugget/` never contains anything the vault
//! pages do not already show; it needs no more protection than the vault (which the user owns and
//! may sync or back up as they like). The encrypted store remains the source of truth.
//!
//! # Page format
//! ```markdown
//! ---
//! nugget_id: "ent_000001"
//! kind: "person"
//! aliases: ["Sam"]
//! updated: "2026-10-28T09:00:00Z"
//! ---
//! <!-- nugget:begin header -->
//! # Samantha Lee
//! <!-- nugget:end header -->
//!
//! <!-- nugget:begin facts -->
//! ## Facts
//!
//! ### Works at <!-- p:works_at -->
//! - Samantha works at [[Organization/Acme (ent_000002)|Acme]]. [^1] <!-- c:1 -->
//! <!-- nugget:end facts -->
//!
//! <!-- nugget:begin related -->
//! ## Related
//!
//! - [[Organization/Acme (ent_000002)|Acme]]: works at
//! <!-- nugget:end related -->
//!
//! <!-- nugget:begin sources -->
//! [^1]: [Lunch on Friday](https://mail.example/1): "Sam from Acme"
//! <!-- nugget:end sources -->
//!
//! ## Notes
//! ```
//! Links name the target page by its path stem, which ends in the entity's stable short id; they
//! are regenerated on every render, and links in user text are rewritten when a page moves.
//!
//! # Merging
//! See [`merge`](crate::merge) for the three-way merge and [`edits`](crate::edits) for how edits
//! to fact lines become graph changes. A page is never written from stale input: the user's file
//! is re-read just before the atomic rename, and the render is retried if it changed.

mod config;
mod doc;
mod edits;
mod items;
mod merge;
mod paths;
mod render;
mod validate;
mod watch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use nugget_core::ids::EntityId;
use nugget_core::ipc::VaultStatus;
use nugget_core::{Error, Result};
use nugget_graph::{
    ClaimId, ClaimObject, ClaimStatus, Entity, EntityKind, EntityQuery, GraphStore, NewClaim,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use config::VaultConfig;
pub use items::{ItemLookup, NoItems, SourceInfo};
pub use paths::{META_DIR, page_rel_path, page_stem, sanitize_name, short_id};
pub use render::item_url;
pub use validate::{IssueKind, ValidationIssue, ValidationReport, validate_page};

use doc::{Doc, Segment, collapse_ws, link_page, strip_links, wikilinks};
use edits::{EditPlan, plan_edits};
use merge::{MergeOpts, Merged, merge};
use paths::{SafeFs, fingerprint};
use render::Ctx;

const STATE_FILE: &str = ".nugget/state.json";
const INDEX_PAGE: &str = "README.md";
const INDEX_BASE: &str = ".nugget/base/README.md";
const STATE_VERSION: u32 = 1;
/// How often a page is re-rendered when the user keeps changing it under us.
const WRITE_ATTEMPTS: usize = 3;

/// Graph changes made from user edits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EditReport {
    /// New (or confirmed) user-asserted claims.
    pub asserted: Vec<ClaimId>,
    /// Claims whose line the user deleted. They are now `rejected` (not `retracted`), so
    /// re-extracting their sources never brings them back.
    pub retracted: Vec<ClaimId>,
    /// Claims superseded because their line was edited.
    pub superseded: Vec<ClaimId>,
    pub aliases_added: Vec<String>,
}

impl EditReport {
    pub fn is_empty(&self) -> bool {
        self.asserted.is_empty()
            && self.retracted.is_empty()
            && self.superseded.is_empty()
            && self.aliases_added.is_empty()
    }

    fn extend(&mut self, o: EditReport) {
        self.asserted.extend(o.asserted);
        self.retracted.extend(o.retracted);
        self.superseded.extend(o.superseded);
        self.aliases_added.extend(o.aliases_added);
    }
}

/// What a render pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RenderReport {
    /// Pages whose file content changed (vault-relative).
    pub written: Vec<String>,
    /// `(from, to)` page moves (renames and kind changes).
    pub moved: Vec<(String, String)>,
    /// Pages turned into redirect or retraction stubs.
    pub stubs: Vec<String>,
    /// Pages with an unresolved conflict after this pass.
    pub conflicts: Vec<String>,
    /// Pages left untouched because they could not be read safely.
    pub skipped: Vec<String>,
    pub edits: EditReport,
    /// Change-feed high-water mark covered by this pass (`render_changed`).
    pub high_water: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Stub {
    Merged { into: EntityId },
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PageState {
    path: String,
    kind: EntityKind,
    name: String,
    /// SHA-256 of what we last wrote (the watcher ignores events for this content).
    fingerprint: String,
    #[serde(default)]
    conflicts: Vec<String>,
    #[serde(default)]
    links: Vec<EntityId>,
    #[serde(default)]
    stub: Option<Stub>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct IndexState {
    fingerprint: String,
    #[serde(default)]
    conflicts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    pages: BTreeMap<EntityId, PageState>,
    #[serde(default)]
    index: Option<IndexState>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    last_render: Option<OffsetDateTime>,
    #[serde(default)]
    invalid: Vec<String>,
}

/// Per-pass bookkeeping.
#[derive(Default)]
struct Pass {
    report: RenderReport,
    renames: Vec<(String, String)>,
    notes: BTreeMap<EntityId, Vec<String>>,
    rendered: BTreeSet<EntityId>,
    /// Entities whose pages need a re-render because user edits changed their claims.
    affected: BTreeSet<EntityId>,
}

/// The Markdown vault. All operations are synchronous and serialised on an internal lock; async
/// callers should use `spawn_blocking` (as [`Vault::watch`] does).
pub struct Vault {
    fs: SafeFs,
    graph: Arc<dyn GraphStore>,
    items: Arc<dyn ItemLookup>,
    config: VaultConfig,
    state: Mutex<State>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("root", &self.fs.root())
            .finish_non_exhaustive()
    }
}

fn base_rel(id: &EntityId) -> String {
    format!(".nugget/base/entities/{}.md", short_id(id))
}

fn conflict_rel(page: &str) -> String {
    format!(".nugget/conflicts/{page}")
}

fn stem_of(rel: &str) -> &str {
    rel.strip_suffix(".md").unwrap_or(rel)
}

fn basename(stem: &str) -> &str {
    stem.rsplit('/').next().unwrap_or(stem)
}

impl Vault {
    /// Open (or create) a vault at `root`. The root is created if missing and canonicalised; all
    /// later file access stays inside it.
    pub fn new(
        root: impl AsRef<Path>,
        graph: Arc<dyn GraphStore>,
        store: Arc<dyn ItemLookup>,
        config: VaultConfig,
    ) -> Result<Self> {
        let fs = SafeFs::new(root.as_ref())?;
        let state = match fs.read(STATE_FILE) {
            Ok(Some(text)) => match serde_json::from_str::<State>(&text) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "vault: unreadable state; starting fresh");
                    State::default()
                }
            },
            Ok(None) => State::default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            fs,
            graph,
            items: store,
            config,
            state: Mutex::new(state),
        })
    }

    /// The canonical vault root.
    pub fn root(&self) -> &Path {
        self.fs.root()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Render every active entity (and stub pages of merged or removed ones), merging user edits.
    pub fn render_all(&self) -> Result<RenderReport> {
        let mut st = self.lock();
        self.run(&mut st, None)
    }

    /// Render the entities changed after `since_seq` in the graph's change feed. The report's
    /// `high_water` is the mark to pass next time.
    pub fn render_changed(&self, since_seq: u64) -> Result<RenderReport> {
        let mut st = self.lock();
        let mut ids = Vec::new();
        let mut cur = since_seq;
        loop {
            let (batch, high) = self.graph.changes_since(cur, 512)?;
            if batch.is_empty() || high <= cur {
                break;
            }
            ids.extend(batch);
            cur = high;
        }
        let mut report = if ids.is_empty() {
            RenderReport::default()
        } else {
            self.run(&mut st, Some(ids))?
        };
        report.high_water = cur;
        Ok(report)
    }

    /// Apply the user's edits in the page at `path` (absolute, or relative to the root) to the
    /// graph, then re-render it and any page the edits affect.
    pub fn apply_user_edits(&self, path: impl AsRef<Path>) -> Result<EditReport> {
        let mut st = self.lock();
        let rel = self.fs.rel_of(path.as_ref()).ok_or_else(|| {
            Error::Invalid(format!("{} is not in the vault", path.as_ref().display()))
        })?;
        let id = st
            .pages
            .iter()
            .find(|(_, p)| p.path == rel)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| Error::NotFound(format!("vault page {rel}")))?;
        if st.pages[&id].stub.is_some() {
            return Ok(EditReport::default());
        }
        Ok(self.run(&mut st, Some(vec![id]))?.edits)
    }

    /// Pages whose files differ from what we last wrote (edited while nobody was watching).
    fn edited_pages(&self, st: &State) -> Vec<EntityId> {
        st.pages
            .iter()
            .filter(|(_, p)| p.stub.is_none())
            .filter(|(_, p)| match self.fs.read(&p.path) {
                Ok(Some(t)) => fingerprint(&t) != p.fingerprint,
                _ => false,
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Apply edits in every page changed since we wrote it.
    pub fn apply_pending_edits(&self) -> Result<EditReport> {
        let mut st = self.lock();
        let ids = self.edited_pages(&st);
        if ids.is_empty() {
            return Ok(EditReport::default());
        }
        Ok(self.run(&mut st, Some(ids))?.edits)
    }

    /// Handle changed paths reported by the watcher: our own writes are recognised by their
    /// fingerprint and ignored.
    pub(crate) fn on_paths_changed(&self, paths: &[PathBuf]) -> Result<EditReport> {
        let mut st = self.lock();
        let mut ids = Vec::new();
        for p in paths {
            let Some(rel) = self.fs.rel_of(p) else {
                continue;
            };
            if rel.starts_with(".nugget/") || !rel.ends_with(".md") {
                continue;
            }
            let Some((id, page)) = st.pages.iter().find(|(_, s)| s.path == rel) else {
                continue;
            };
            if page.stub.is_some() {
                continue;
            }
            match self.fs.read(&rel) {
                Ok(Some(t)) if fingerprint(&t) != page.fingerprint => ids.push(id.clone()),
                _ => {}
            }
        }
        if ids.is_empty() {
            return Ok(EditReport::default());
        }
        Ok(self.run(&mut st, Some(ids))?.edits)
    }

    /// Absolute path of the page for `id` (following merges), if it has been rendered.
    pub fn page_path(&self, id: &EntityId) -> Result<Option<PathBuf>> {
        let st = self.lock();
        let live = self.graph.get_entity(id)?.map(|e| e.id);
        let page = live
            .as_ref()
            .and_then(|l| st.pages.get(l))
            .or_else(|| st.pages.get(id));
        page.map(|p| self.fs.abs(&p.path)).transpose()
    }

    /// Vault status for IPC.
    pub fn status(&self) -> VaultStatus {
        let st = self.lock();
        let mut conflicts: Vec<String> = st
            .pages
            .values()
            .filter(|p| !p.conflicts.is_empty())
            .map(|p| p.path.clone())
            .collect();
        if st.index.as_ref().is_some_and(|i| !i.conflicts.is_empty()) {
            conflicts.push(INDEX_PAGE.into());
        }
        conflicts.sort();
        VaultStatus {
            path: Some(self.fs.root().display().to_string()),
            pages: st.pages.values().filter(|p| p.stub.is_none()).count() as u64,
            last_render: st.last_render,
            conflicts,
            invalid: st.invalid.clone(),
        }
    }

    /// Check every page: citations, links and frontmatter.
    pub fn validate(&self) -> Result<ValidationReport> {
        let mut st = self.lock();
        let mut report = ValidationReport::default();
        let exists = |target: &str| self.fs.is_file(&format!("{target}.md"));
        let status = |id: ClaimId| self.graph.get_claim(id).ok().flatten().map(|c| c.status);
        let mut pages: Vec<String> = st.pages.values().map(|p| p.path.clone()).collect();
        if st.index.is_some() {
            pages.push(INDEX_PAGE.into());
        }
        for rel in pages {
            match self.fs.read(&rel) {
                Ok(Some(text)) => report.merge(validate::validate_page_with(
                    &rel,
                    &text,
                    &exists,
                    Some(&status),
                )),
                Ok(None) => report.issues.push(ValidationIssue {
                    page: rel,
                    line: None,
                    kind: IssueKind::MissingPage,
                    message: "page file is missing".into(),
                }),
                Err(e) => report.issues.push(ValidationIssue {
                    page: rel,
                    line: None,
                    kind: IssueKind::UnreadablePage,
                    message: e.to_string(),
                }),
            }
        }
        st.invalid = report.invalid_pages();
        self.save_state(&st)?;
        Ok(report)
    }

    // ---- passes ----

    fn all_entity_ids(&self) -> Result<Vec<EntityId>> {
        const PAGE: usize = 500;
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let batch = self.graph.list_entities(&EntityQuery {
                text: None,
                kind: None,
                limit: PAGE,
                offset,
            })?;
            let n = batch.len();
            out.extend(batch.into_iter().map(|s| s.id));
            if n < PAGE {
                break;
            }
            offset += n;
        }
        Ok(out)
    }

    fn run(&self, st: &mut State, ids: Option<Vec<EntityId>>) -> Result<RenderReport> {
        let ctx = Ctx::new(self.graph.as_ref(), self.items.as_ref(), &self.config);
        let mut pass = Pass::default();
        let ids = match ids {
            Some(v) => v,
            None => {
                let mut v = self.all_entity_ids()?;
                v.extend(st.pages.keys().cloned());
                v
            }
        };
        let result = (|| {
            self.process(st, &ctx, &mut pass, ids)?;
            for _ in 0..8 {
                let mut next = std::mem::take(&mut pass.affected);
                for p in st.pages.values().filter(|p| p.stub.is_none()) {
                    for l in &p.links {
                        if !st.pages.contains_key(l) {
                            next.insert(l.clone());
                        }
                    }
                }
                if next.is_empty() {
                    break;
                }
                ctx.forget();
                self.process(st, &ctx, &mut pass, next.into_iter().collect())?;
            }
            if !pass.renames.is_empty() {
                // Pages rendered before a move was found may hold links to the old path in user
                // text: one more pass over everything rewrites them.
                ctx.forget();
                let all: Vec<EntityId> = st.pages.keys().cloned().collect();
                self.process(st, &ctx, &mut pass, all)?;
            }
            self.render_index(st, &ctx, &mut pass)
        })();
        st.last_render = Some(OffsetDateTime::now_utc());
        st.version = STATE_VERSION;
        self.save_state(st)?;
        result?;
        let mut report = pass.report;
        report.conflicts = st
            .pages
            .values()
            .filter(|p| !p.conflicts.is_empty())
            .map(|p| p.path.clone())
            .collect();
        Ok(report)
    }

    fn process(
        &self,
        st: &mut State,
        ctx: &Ctx,
        pass: &mut Pass,
        ids: Vec<EntityId>,
    ) -> Result<()> {
        let mut live: Vec<Entity> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut tombs: Vec<(EntityId, Option<Entity>)> = Vec::new();
        let mut tomb_seen = BTreeSet::new();
        for id in ids {
            match ctx.entity(&id)? {
                Some(e) if e.id == id => {
                    if seen.insert(e.id.clone()) {
                        live.push(e);
                    }
                }
                Some(e) => {
                    if st.pages.contains_key(&id) && tomb_seen.insert(id.clone()) {
                        tombs.push((id, Some(e.clone())));
                    }
                    if seen.insert(e.id.clone()) {
                        live.push(e);
                    }
                }
                None => {
                    if st.pages.contains_key(&id) && tomb_seen.insert(id.clone()) {
                        tombs.push((id, None));
                    }
                }
            }
        }

        // 1. Notes on pages of merged entities move to the surviving page.
        for (id, target) in &tombs {
            let Some(t) = target else { continue };
            let prev = &st.pages[id];
            if prev.stub.is_some() {
                continue;
            }
            let user = match self.fs.read(&prev.path) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(page = %prev.path, error = %e, "vault: cannot read page");
                    continue;
                }
            };
            let Some(user) = user.map(|u| Doc::parse(&u)) else {
                continue;
            };
            let base = self.read_base(id).map(|b| Doc::parse(&b));
            let mut content = user.user_content(base.as_ref());
            content.retain(|l| l.trim() != "## Notes");
            while content.first().is_some_and(|l| l.trim().is_empty()) {
                content.remove(0);
            }
            while content.last().is_some_and(|l| l.trim().is_empty()) {
                content.pop();
            }
            if content.is_empty() {
                continue;
            }
            let mut block = vec![
                String::new(),
                format!("### Notes moved from {}", render::inline(&prev.name)),
                String::new(),
            ];
            block.extend(content);
            pass.notes.entry(t.id.clone()).or_default().extend(block);
        }

        // 2. Live pages (receiving moved notes first, so a crash duplicates notes, never loses them).
        for e in &live {
            self.render_page(st, ctx, pass, e)?;
        }

        // 3. Stubs.
        for (id, target) in tombs {
            self.render_stub_page(st, ctx, pass, &id, target.as_ref())?;
        }
        Ok(())
    }

    fn read_base(&self, id: &EntityId) -> Option<String> {
        self.fs.read(&base_rel(id)).ok().flatten()
    }

    fn save_state(&self, st: &State) -> Result<()> {
        self.fs
            .write(STATE_FILE, &serde_json::to_string_pretty(st)?)
    }

    /// Resolve a link target written by the user to an entity id.
    fn resolve_link(&self, st: &State, target: &str) -> Option<EntityId> {
        let t = link_page(target);
        let t = t.strip_suffix(".md").unwrap_or(t);
        let mut by_base = None;
        for (id, p) in &st.pages {
            let stem = stem_of(&p.path);
            if stem == t {
                return Some(id.clone());
            }
            if basename(stem) == t {
                by_base = Some(id.clone());
            }
        }
        if by_base.is_some() {
            return by_base;
        }
        let sid = t.rsplit_once(" (")?.1.strip_suffix(')')?;
        st.pages.keys().find(|id| short_id(id) == sid).cloned()
    }

    fn user_claim(&self, st: &State, subject: &Entity, predicate: &str, content: &str) -> NewClaim {
        let object = wikilinks(content)
            .into_iter()
            .filter_map(|(t, _)| self.resolve_link(st, &t))
            .find(|id| *id != subject.id);
        let text = collapse_ws(&strip_links(content));
        NewClaim {
            subject: subject.id.clone(),
            predicate: predicate.to_owned(),
            object: match object {
                Some(o) => ClaimObject::Entity(o),
                None => ClaimObject::Literal(text.clone()),
            },
            text,
            confidence: 1.0,
            status: ClaimStatus::UserAsserted,
            evidence: Vec::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    fn apply_plan(&self, st: &State, e: &Entity, plan: &EditPlan, pass: &mut Pass) -> Result<()> {
        let g = self.graph.as_ref();
        let mine = |c: &nugget_graph::Claim| c.subject == e.id || e.merged_ids.contains(&c.subject);
        let mut rep = EditReport::default();
        let touch = |pass: &mut Pass, o: &ClaimObject| {
            if let ClaimObject::Entity(id) = o {
                pass.affected.insert(id.clone());
            }
        };
        // A line gone because its claim moved to another entity (a split) is not a deletion:
        // the fact now lives on the other page.
        for &id in &plan.retract {
            if let Some(c) = g.get_claim(id)?
                && mine(&c)
                && c.status.is_active()
            {
                g.set_claim_status(id, ClaimStatus::Rejected)?;
                rep.retracted.push(id);
                touch(pass, &c.object);
            }
        }
        for (id, nf) in &plan.supersede {
            let Some(c) = g.get_claim(*id)? else { continue };
            // An edited line whose claim was split off to another entity since the last render:
            // the edit applies there.
            let owner = if mine(&c) {
                e.clone()
            } else {
                match g.get_entity(&c.subject)? {
                    Some(o) => o,
                    None => continue,
                }
            };
            if c.status.is_active() {
                g.set_claim_status(*id, ClaimStatus::Superseded)?;
                rep.superseded.push(*id);
                touch(pass, &c.object);
            }
            let new = self.user_claim(st, &owner, &nf.predicate, &nf.content);
            touch(pass, &new.object);
            if owner.id != e.id {
                pass.affected.insert(owner.id.clone());
            }
            rep.asserted.push(g.add_claim(&new)?);
        }
        for nf in &plan.add {
            let new = self.user_claim(st, e, &nf.predicate, &nf.content);
            touch(pass, &new.object);
            rep.asserted.push(g.add_claim(&new)?);
        }
        for a in &plan.aliases {
            g.add_alias(&e.id, a)?;
            rep.aliases_added.push(a.clone());
        }
        pass.affected.remove(&e.id);
        pass.report.edits.extend(rep);
        Ok(())
    }

    fn render_page(&self, st: &mut State, ctx: &Ctx, pass: &mut Pass, e: &Entity) -> Result<()> {
        let id = e.id.clone();
        let prev = st.pages.get(&id).cloned();
        let notes = pass.notes.remove(&id).unwrap_or_default();
        let mut entity = e.clone();
        for _ in 0..WRITE_ATTEMPTS {
            let target = page_rel_path(entity.kind, &entity.name, &id, self.config.max_name_chars);
            let src = prev
                .as_ref()
                .map(|p| p.path.clone())
                .filter(|p| self.fs.entry_exists(p))
                .or_else(|| self.fs.entry_exists(&target).then(|| target.clone()));
            let user_text = match src.as_deref().map(|s| self.fs.read(s)) {
                Some(Ok(t)) => t,
                Some(Err(err)) => {
                    tracing::warn!(page = ?src, error = %err, "vault: leaving page alone");
                    pass.report.skipped.extend(src);
                    pass.notes.insert(id, notes); // keep them for a later pass
                    return Ok(());
                }
                None => None,
            };
            let base = self.read_base(&id).map(|b| Doc::parse(&b));
            let user = user_text.as_deref().map(Doc::parse);

            if let (Some(b), Some(u)) = (&base, &user)
                && prev.as_ref().is_none_or(|p| p.stub.is_none())
            {
                let plan = plan_edits(b, u);
                if !plan.is_empty() {
                    self.apply_plan(st, &entity, &plan, pass)?;
                    ctx.forget();
                    if let Some(fresh) = ctx.entity(&id)? {
                        entity = fresh;
                    }
                }
            }

            let claims = self.graph.claims_for(&id, false)?;
            let rendered = ctx.render_entity(&entity, &claims)?;
            let gen_text = rendered.doc.render();
            let mut rel = page_rel_path(entity.kind, &entity.name, &id, self.config.max_name_chars);
            if src.as_deref() != Some(rel.as_str()) && self.fs.is_file(&rel) {
                // Something else already sits at the new path: stay where we are.
                if let Some(s) = &src {
                    tracing::warn!(page = %rel, "vault: target path taken; not moving page");
                    rel = s.clone();
                }
            }
            let prev_conflicts = prev
                .as_ref()
                .map(|p| p.conflicts.clone())
                .unwrap_or_default();
            let Merged { mut doc, conflicts } = match &user {
                Some(u) => merge(
                    base.as_ref(),
                    u,
                    &rendered.doc,
                    &MergeOpts {
                        prev_conflicts: &prev_conflicts,
                        renames: &pass.renames,
                        append: notes.clone(),
                    },
                ),
                None => Merged {
                    doc: rendered.doc.clone(),
                    conflicts: Vec::new(),
                },
            };
            if user.is_none() && !notes.is_empty() {
                doc.segs.push(Segment::User(notes.clone()));
            }
            let text = doc.render();

            // Never write over a change we have not seen.
            if let Some(s) = &src
                && self.fs.read(s).ok().flatten() != user_text
            {
                continue;
            }
            let current = if src.as_deref() == Some(rel.as_str()) {
                user_text.clone()
            } else {
                self.fs.read(&rel).ok().flatten()
            };
            if current.as_deref() != Some(text.as_str()) {
                self.fs.write(&rel, &text)?;
                pass.report.written.push(rel.clone());
            }
            if let Some(s) = &src
                && *s != rel
            {
                self.fs.remove(s)?;
                if let Some((dir, _)) = s.rsplit_once('/') {
                    self.fs.prune_empty_dir(dir);
                }
                pass.report.moved.push((s.clone(), rel.clone()));
            }
            if let Some(p) = &prev
                && p.path != rel
            {
                let (old, new) = (stem_of(&p.path), stem_of(&rel));
                pass.renames.push((old.to_owned(), new.to_owned()));
                if basename(old) != basename(new) {
                    pass.renames
                        .push((basename(old).to_owned(), basename(new).to_owned()));
                }
                let _ = self.fs.remove(&conflict_rel(&p.path));
            }
            self.fs.write(&base_rel(&id), &gen_text)?;
            if conflicts.is_empty() {
                let _ = self.fs.remove(&conflict_rel(&rel));
            } else {
                self.write_sidecar(&rel, &conflicts, &gen_text)?;
            }
            st.pages.insert(
                id.clone(),
                PageState {
                    path: rel,
                    kind: entity.kind,
                    name: entity.name.clone(),
                    fingerprint: fingerprint(&text),
                    conflicts,
                    links: rendered.links.into_iter().collect(),
                    stub: None,
                },
            );
            pass.rendered.insert(id);
            return Ok(());
        }
        tracing::warn!(entity = %id, "vault: page kept changing during render; will retry later");
        pass.report
            .skipped
            .push(prev.map(|p| p.path).unwrap_or_default());
        pass.notes.insert(id, notes);
        Ok(())
    }

    fn write_sidecar(&self, rel: &str, conflicts: &[String], generated: &str) -> Result<()> {
        let header = format!(
            "<!-- nugget: generated version of {rel}. Your version was kept; these blocks conflicted: {} -->\n",
            conflicts.join(", ")
        );
        self.fs.write(&conflict_rel(rel), &(header + generated))
    }

    fn render_stub_page(
        &self,
        st: &mut State,
        ctx: &Ctx,
        pass: &mut Pass,
        id: &EntityId,
        target: Option<&Entity>,
    ) -> Result<()> {
        let prev = st.pages[id].clone();
        let want = match target {
            Some(t) => Stub::Merged { into: t.id.clone() },
            None => Stub::Gone,
        };
        let transition = prev.stub.as_ref() != Some(&want);
        if transition
            && let Some(t) = target
            && (!pass.rendered.contains(&t.id) || pass.notes.contains_key(&t.id))
        {
            // The surviving page did not take the notes yet; keep this page for a later pass.
            return Ok(());
        }
        let rel = prev.path.clone();
        let user_text = match self.fs.read(&rel) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(page = %rel, error = %e, "vault: leaving page alone");
                pass.report.skipped.push(rel);
                return Ok(());
            }
        };
        let base = self.read_base(id).map(|b| Doc::parse(&b));
        let user = user_text.as_deref().map(Doc::parse);
        let rendered = ctx.render_stub(id, prev.kind, &prev.name, target);
        let gen_text = rendered.doc.render();
        let (doc, conflicts) = match (&user, transition) {
            (Some(u), false) => {
                let m = merge(
                    base.as_ref(),
                    u,
                    &rendered.doc,
                    &MergeOpts {
                        prev_conflicts: &prev.conflicts,
                        renames: &pass.renames,
                        append: Vec::new(),
                    },
                );
                (m.doc, m.conflicts)
            }
            (Some(u), true) if target.is_none() => {
                let mut d = rendered.doc.clone();
                let mut content = u.user_content(base.as_ref());
                if content.iter().any(|l| !l.trim().is_empty()) {
                    if content.first().is_some_and(|l| !l.trim().is_empty()) {
                        content.insert(0, String::new());
                    }
                    d.segs.push(Segment::User(content));
                }
                (d, Vec::new())
            }
            _ => (rendered.doc.clone(), Vec::new()),
        };
        let text = doc.render();
        if user_text.as_deref() != Some(text.as_str()) {
            if self.fs.read(&rel).ok().flatten() != user_text {
                return Ok(()); // changed under us; next pass
            }
            self.fs.write(&rel, &text)?;
            pass.report.written.push(rel.clone());
        }
        self.fs.write(&base_rel(id), &gen_text)?;
        if conflicts.is_empty() {
            let _ = self.fs.remove(&conflict_rel(&rel));
        } else {
            self.write_sidecar(&rel, &conflicts, &gen_text)?;
        }
        if transition {
            pass.report.stubs.push(rel.clone());
        }
        st.pages.insert(
            id.clone(),
            PageState {
                fingerprint: fingerprint(&text),
                conflicts,
                links: rendered.links.into_iter().collect(),
                stub: Some(want),
                ..prev
            },
        );
        Ok(())
    }

    fn render_index(&self, st: &mut State, ctx: &Ctx, pass: &mut Pass) -> Result<()> {
        let pages: Vec<(EntityKind, String, EntityId)> = st
            .pages
            .iter()
            .filter(|(_, p)| p.stub.is_none())
            .map(|(id, p)| (p.kind, p.name.clone(), id.clone()))
            .collect();
        let new = ctx.render_index(&pages);
        let gen_text = new.render();
        let user_text = match self.fs.read(INDEX_PAGE) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "vault: leaving README.md alone");
                pass.report.skipped.push(INDEX_PAGE.into());
                return Ok(());
            }
        };
        let base = self
            .fs
            .read(INDEX_BASE)
            .ok()
            .flatten()
            .map(|b| Doc::parse(&b));
        let prev_conflicts = st
            .index
            .as_ref()
            .map(|i| i.conflicts.clone())
            .unwrap_or_default();
        let Merged { doc, conflicts } = match user_text.as_deref().map(Doc::parse) {
            Some(u) => merge(
                base.as_ref(),
                &u,
                &new,
                &MergeOpts {
                    prev_conflicts: &prev_conflicts,
                    renames: &[],
                    append: Vec::new(),
                },
            ),
            None => Merged {
                doc: new.clone(),
                conflicts: Vec::new(),
            },
        };
        let text = doc.render();
        if user_text.as_deref() != Some(text.as_str()) {
            self.fs.write(INDEX_PAGE, &text)?;
            pass.report.written.push(INDEX_PAGE.into());
        }
        self.fs.write(INDEX_BASE, &gen_text)?;
        if conflicts.is_empty() {
            let _ = self.fs.remove(&conflict_rel(INDEX_PAGE));
        } else {
            self.write_sidecar(INDEX_PAGE, &conflicts, &gen_text)?;
        }
        st.index = Some(IndexState {
            fingerprint: fingerprint(&text),
            conflicts,
        });
        Ok(())
    }
}
