//! Rendering entities, redirect stubs and the index into page documents.
//!
//! Output is a pure function of the graph (and the item lookup): claims are grouped by predicate
//! in byte order, sorted by id within a group, footnotes are numbered in order of first use, and
//! neighbours are sorted by kind, name and id.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{Claim, ClaimObject, ClaimStatus, Entity, EntityKind, GraphStore, Identifier};
use time::UtcOffset;
use time::format_description::well_known::Rfc3339;

use crate::config::VaultConfig;
use crate::doc::*;
use crate::items::{ItemLookup, SourceInfo};
use crate::paths::{kind_dir, page_stem};

pub(crate) struct Ctx<'a> {
    pub graph: &'a dyn GraphStore,
    pub items: &'a dyn ItemLookup,
    pub cfg: &'a VaultConfig,
    sources: RefCell<HashMap<ItemRef, SourceInfo>>,
    entities: RefCell<HashMap<EntityId, Option<Entity>>>,
    /// [`Ctx::entity_sensitive`] per entity (for this pass).
    sensitive: RefCell<HashMap<EntityId, bool>>,
    /// A source lookup failed in this pass (those items count as sensitive).
    lookup_failed: std::cell::Cell<bool>,
}

/// Mentions read per entity to decide whether it is sensitive-only (as `nuggetd`'s graph views
/// and MCP do).
const SENSITIVE_MENTIONS: usize = 5000;

pub(crate) struct Rendered {
    pub doc: Doc,
    /// Entities this page links to.
    pub links: BTreeSet<EntityId>,
}

fn json(v: impl serde::Serialize) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "\"\"".into())
}

/// Make arbitrary text safe on one Markdown line inside a generated region: no newlines, no HTML
/// comment delimiters (they would break our markers), no wikilink or footnote syntax.
pub(crate) fn inline(s: &str) -> String {
    collapse_ws(s)
        .replace("<!--", "&lt;!--")
        .replace("-->", "--&gt;")
        .replace("[[", "[ [")
        .replace("]]", "] ]")
        .replace("[^", "[ ^")
}

/// Text shown inside `[[target|display]]`.
pub(crate) fn display(name: &str) -> String {
    let s = inline(name).replace(['|', '[', ']'], "-");
    if s.is_empty() { "Untitled".into() } else { s }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `nugget://item/<source>/<key>`, percent-encoded.
pub fn item_url(item: &ItemRef) -> String {
    format!(
        "nugget://item/{}/{}",
        pct(item.source.as_str()),
        pct(item.key.as_str())
    )
}

/// An item url we are willing to put in a link: http(s) or mailto, encoded for a Markdown link.
fn safe_url(url: &str) -> Option<String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    if !(lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:"))
    {
        return None;
    }
    if u.chars().any(|c| c.is_control()) {
        return None;
    }
    let mut out = String::new();
    for c in u.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '[' => out.push_str("%5B"),
            ']' => out.push_str("%5D"),
            '|' => out.push_str("%7C"),
            '"' => out.push_str("%22"),
            c => out.push(c),
        }
    }
    Some(out)
}

/// First occurrence of `word` in `hay` on word boundaries.
fn find_word(hay: &str, word: &str) -> Option<usize> {
    if word.is_empty() {
        return None;
    }
    let mut from = 0;
    while let Some(i) = hay[from..].find(word) {
        let at = from + i;
        let before = hay[..at].chars().next_back();
        let after = hay[at + word.len()..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return Some(at);
        }
        from = at + word.len();
    }
    None
}

fn rfc3339(t: time::OffsetDateTime) -> String {
    t.to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_default()
}

impl<'a> Ctx<'a> {
    pub fn new(graph: &'a dyn GraphStore, items: &'a dyn ItemLookup, cfg: &'a VaultConfig) -> Self {
        Self {
            graph,
            items,
            cfg,
            sources: RefCell::default(),
            entities: RefCell::default(),
            sensitive: RefCell::default(),
            lookup_failed: std::cell::Cell::new(false),
        }
    }

    pub fn stem(&self, kind: EntityKind, name: &str, id: &EntityId) -> String {
        page_stem(kind, name, id, self.cfg.max_name_chars)
    }

    pub fn link(&self, e: &Entity) -> String {
        format!(
            "[[{}|{}]]",
            self.stem(e.kind, &e.name, &e.id),
            display(&e.name)
        )
    }

    /// Live entity by id (cached for this pass).
    pub fn entity(&self, id: &EntityId) -> Result<Option<Entity>> {
        if let Some(e) = self.entities.borrow().get(id) {
            return Ok(e.clone());
        }
        let e = self.graph.get_entity(id)?;
        self.entities.borrow_mut().insert(id.clone(), e.clone());
        Ok(e)
    }

    /// Forget cached entities (after the graph changed under us).
    pub fn forget(&self) {
        self.entities.borrow_mut().clear();
        self.sensitive.borrow_mut().clear();
    }

    /// Look up every item in `items` not looked up yet in this pass, in one batch
    /// ([`ItemLookup::source_infos`]), so the per-item checks below hit the cache.
    pub fn prefetch<'i>(&self, items: impl IntoIterator<Item = &'i ItemRef>) {
        let want: Vec<ItemRef> = {
            let cached = self.sources.borrow();
            let mut seen = std::collections::HashSet::new();
            items
                .into_iter()
                .filter(|i| !cached.contains_key(*i) && seen.insert(*i))
                .cloned()
                .collect()
        };
        if want.is_empty() {
            return;
        }
        let missing = || SourceInfo {
            sensitive: self.items.missing_is_sensitive(),
            ..SourceInfo::default()
        };
        // A failed lookup counts as sensitive.
        let failed = || SourceInfo {
            sensitive: true,
            ..SourceInfo::default()
        };
        let infos: Vec<(ItemRef, SourceInfo)> = match self.items.source_infos(&want) {
            Ok(mut f) => want
                .into_iter()
                .map(|i| {
                    let info = f.remove(&i).unwrap_or_else(missing);
                    (i, info)
                })
                .collect(),
            Err(e) => {
                // One bad item must not count the whole batch as sensitive: ask one by one.
                tracing::warn!(error = %e, "vault: batched source lookup failed; retrying per item");
                want.into_iter()
                    .map(|i| {
                        let info = match self.items.source_info(&i) {
                            Ok(Some(info)) => info,
                            Ok(None) => missing(),
                            Err(e) => {
                                tracing::warn!(error = %e, "vault: source lookup failed");
                                self.lookup_failed.set(true);
                                failed()
                            }
                        };
                        (i, info)
                    })
                    .collect()
            }
        };
        self.sources.borrow_mut().extend(infos);
    }

    /// Whether a source lookup failed in this pass: its items counted as sensitive, so nothing
    /// is deleted on that answer.
    pub fn lookup_failed(&self) -> bool {
        self.lookup_failed.get()
    }

    /// Whether the item is sensitive. An item the lookup does not know counts as sensitive when
    /// the lookup says so ([`ItemLookup::missing_is_sensitive`]); a failed lookup always does.
    pub fn item_sensitive(&self, item: &ItemRef) -> bool {
        self.source(item).sensitive
    }

    /// A claim is sensitive-only when it has evidence and every piece of it is from a sensitive
    /// item. A claim without evidence (the user's own) is not.
    pub fn claim_sensitive(&self, c: &Claim) -> bool {
        !c.evidence.is_empty() && c.evidence.iter().all(|ev| self.item_sensitive(&ev.item))
    }

    /// An entity is sensitive-only when it has at least one active claim or mention, every
    /// active claim (as subject or object) is sensitive-only and every mention is from a
    /// sensitive item. Such an entity gets no page.
    pub fn entity_sensitive(&self, id: &EntityId) -> Result<bool> {
        if let Some(s) = self.sensitive.borrow().get(id) {
            return Ok(*s);
        }
        let claims = self.graph.claims_for(id, false)?;
        self.prefetch(
            claims
                .iter()
                .flat_map(|c| c.evidence.iter().map(|e| &e.item)),
        );
        let mut any = !claims.is_empty();
        let mut sensitive = claims.iter().all(|c| self.claim_sensitive(c));
        if sensitive {
            let mentions = self.graph.mentions_for(id, SENSITIVE_MENTIONS)?;
            self.prefetch(mentions.iter().map(|m| &m.item));
            any |= !mentions.is_empty();
            sensitive = mentions.iter().all(|m| self.item_sensitive(&m.item));
        }
        let out = any && sensitive;
        self.sensitive.borrow_mut().insert(id.clone(), out);
        Ok(out)
    }

    /// The aliases and identifiers the page may show: all but those whose recorded provenance
    /// ([`GraphStore::entity_provenance`]) is only sensitive items. A detail without recorded
    /// provenance (one the user added in the vault, or from a merge) is shown: the page is the
    /// user's own file.
    fn shown_details(&self, e: &Entity) -> Result<(Vec<String>, Vec<Identifier>)> {
        let Some(p) = self.graph.entity_provenance(&e.id)? else {
            return Ok((e.aliases.clone(), e.identifiers.clone()));
        };
        self.prefetch(
            p.aliases
                .iter()
                .flat_map(|(_, items)| items)
                .chain(p.identifiers.iter().flat_map(|(_, items)| items)),
        );
        let hidden =
            |items: &[ItemRef]| !items.is_empty() && items.iter().all(|i| self.item_sensitive(i));
        let hidden_aliases: Vec<&String> = p
            .aliases
            .iter()
            .filter(|(_, items)| hidden(items))
            .map(|(a, _)| a)
            .collect();
        let hidden_ids: Vec<&Identifier> = p
            .identifiers
            .iter()
            .filter(|(_, items)| hidden(items))
            .map(|(i, _)| i)
            .collect();
        let aliases = e
            .aliases
            .iter()
            .filter(|a| !hidden_aliases.contains(a))
            .cloned()
            .collect();
        let identifiers = e
            .identifiers
            .iter()
            .filter(|i| !hidden_ids.contains(i))
            .cloned()
            .collect();
        Ok((aliases, identifiers))
    }

    fn source(&self, item: &ItemRef) -> SourceInfo {
        if let Some(s) = self.sources.borrow().get(item) {
            return s.clone();
        }
        self.prefetch([item]);
        self.sources
            .borrow()
            .get(item)
            .cloned()
            .unwrap_or_else(|| SourceInfo {
                sensitive: true,
                ..SourceInfo::default()
            })
    }

    fn frontmatter(&self, e: &Entity) -> Result<Vec<String>> {
        let (aliases, identifiers) = self.shown_details(e)?;
        let mut fm = vec![
            format!("nugget_id: {}", json(e.id.as_str())),
            format!("kind: {}", json(e.kind.as_str())),
            format!("aliases: {}", json(&aliases)),
        ];
        if self.cfg.include_identifiers && !identifiers.is_empty() {
            let mut ids: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for i in &identifiers {
                ids.entry(i.scheme.as_str())
                    .or_default()
                    .push(i.value.as_str());
            }
            fm.push(format!("identifiers: {}", json(ids)));
        }
        fm.push(format!("updated: {}", json(rfc3339(e.updated_at))));
        Ok(fm)
    }

    pub fn render_entity(&self, e: &Entity, claims: &[Claim]) -> Result<Rendered> {
        let mut links = BTreeSet::new();
        self.prefetch(
            claims
                .iter()
                .flat_map(|c| c.evidence.iter().map(|e| &e.item)),
        );

        // ---- facts ----
        let mut groups: BTreeMap<&str, Vec<&Claim>> = BTreeMap::new();
        // Facts supported only by sensitive items are left out (and so are their footnotes).
        let claims: Vec<&Claim> = claims
            .iter()
            .filter(|c| c.status.is_active() && !self.claim_sensitive(c))
            .collect();
        for &c in &claims {
            if c.subject == e.id {
                groups.entry(c.predicate.as_str()).or_default().push(c);
            }
        }
        let mut notes: Vec<String> = Vec::new();
        let mut note_ids: HashMap<(ItemRef, Option<String>), usize> = HashMap::new();
        let mut facts = vec!["## Facts".to_owned(), String::new()];
        if groups.is_empty() {
            facts.push("_No facts yet._".into());
        }
        for (pred, mut cs) in groups {
            cs.sort_by_key(|c| c.id);
            if facts.len() > 2 {
                facts.push(String::new());
            }
            facts.push(format!(
                "### {} {PRED_MARK}{} -->",
                inline(&humanize(pred)),
                inline(pred).replace("--", "-")
            ));
            for c in cs {
                facts.push(self.fact_line(e, c, &mut links, &mut notes, &mut note_ids)?);
            }
        }

        // ---- related ----
        let mut nb: BTreeMap<EntityId, (BTreeSet<&str>, BTreeSet<&str>)> = BTreeMap::new();
        for c in &claims {
            let ClaimObject::Entity(o) = &c.object else {
                continue;
            };
            if c.subject == e.id && *o != e.id {
                nb.entry(o.clone()).or_default().0.insert(&c.predicate);
            } else if *o == e.id && c.subject != e.id {
                nb.entry(c.subject.clone())
                    .or_default()
                    .1
                    .insert(&c.predicate);
            }
        }
        let mut rows = Vec::new();
        for (id, (out, inc)) in nb {
            let Some(other) = self.entity(&id)? else {
                continue;
            };
            if other.id == e.id {
                continue;
            }
            let mut preds: Vec<String> = out.iter().map(|p| inline(&p.replace('_', " "))).collect();
            preds.extend(
                inc.iter()
                    .map(|p| format!("← {}", inline(&p.replace('_', " ")))),
            );
            let key = (
                EntityKind::ALL.iter().position(|k| *k == other.kind),
                other.name.to_lowercase(),
                other.id.clone(),
            );
            // A sensitive-only entity has no page: name it without a link.
            let name = if self.entity_sensitive(&other.id)? {
                display(&other.name)
            } else {
                links.insert(other.id.clone());
                self.link(&other)
            };
            rows.push((key, format!("- {name}: {}", preds.join(", "))));
        }
        rows.sort();
        rows.dedup_by(|a, b| a.1 == b.1);
        let mut related = vec!["## Related".to_owned(), String::new()];
        if rows.is_empty() {
            related.push("_No related entities yet._".into());
        }
        related.extend(rows.into_iter().map(|(_, l)| l));

        let doc = Doc {
            fm: Some(self.frontmatter(e)?),
            segs: vec![
                Segment::Region {
                    name: R_HEADER.into(),
                    body: vec![format!("# {}", inline(&e.name))],
                },
                Segment::User(vec![String::new()]),
                Segment::Region {
                    name: R_FACTS.into(),
                    body: facts,
                },
                Segment::User(vec![String::new()]),
                Segment::Region {
                    name: R_RELATED.into(),
                    body: related,
                },
                Segment::User(vec![String::new()]),
                Segment::Region {
                    name: R_SOURCES.into(),
                    body: notes,
                },
                Segment::User(vec![String::new(), "## Notes".into(), String::new()]),
            ],
        };
        Ok(Rendered { doc, links })
    }

    fn fact_line(
        &self,
        e: &Entity,
        c: &Claim,
        links: &mut BTreeSet<EntityId>,
        notes: &mut Vec<String>,
        note_ids: &mut HashMap<(ItemRef, Option<String>), usize>,
    ) -> Result<String> {
        let mut body = inline(&c.text);
        let object = match &c.object {
            ClaimObject::Entity(o) if *o != e.id => self.entity(o)?,
            _ => None,
        };
        if body.is_empty() {
            let obj = match (&c.object, &object) {
                (_, Some(o)) => inline(&o.name),
                (ClaimObject::Literal(l), _) => inline(l),
                _ => String::new(),
            };
            body = format!("{} {obj}", inline(&humanize(&c.predicate)))
                .trim()
                .to_owned();
        }
        let object_has_page = match &object {
            Some(o) => !self.entity_sensitive(&o.id)?,
            None => false,
        };
        if let Some(o) = object.as_ref().filter(|_| !object_has_page) {
            // A sensitive-only entity has no page to link to: keep the sentence, and add the
            // name as plain text when the sentence lacks it.
            if find_word(&body, &inline(&o.name)).is_none() {
                body = format!("{body} ({})", display(&o.name));
            }
        } else if let Some(o) = object {
            links.insert(o.id.clone());
            // Link the entity where the sentence names it (by name, else by its longest alias).
            let mut names: Vec<String> = o.aliases.iter().map(|a| inline(a)).collect();
            names.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
            names.insert(0, inline(&o.name));
            let hit = names
                .iter()
                .filter(|n| !n.is_empty())
                .find_map(|n| find_word(&body, n).map(|at| (at, n.clone())));
            match hit {
                Some((at, n)) => {
                    let stem = self.stem(o.kind, &o.name, &o.id);
                    body.replace_range(at..at + n.len(), &format!("[[{stem}|{}]]", display(&n)));
                }
                None => body = format!("{body} ({})", self.link(&o)),
            }
        }
        let mut cites = String::new();
        // Never cite (or quote) a sensitive item, even next to other evidence.
        for ev in c
            .evidence
            .iter()
            .filter(|ev| !self.item_sensitive(&ev.item))
        {
            let key = (ev.item.clone(), ev.quote.clone());
            let n = match note_ids.get(&key) {
                Some(n) => *n,
                None => {
                    let n = notes.len() + 1;
                    notes.push(self.footnote(n, &ev.item, ev.quote.as_deref()));
                    note_ids.insert(key, n);
                    n
                }
            };
            cites.push_str(&format!("[^{n}]"));
        }
        let mut line = format!("- {body}");
        if !cites.is_empty() {
            line.push(' ');
            line.push_str(&cites);
        }
        if c.status == ClaimStatus::UserAsserted {
            line.push_str(" (you)");
        }
        line.push_str(&format!(" {CLAIM_MARK}{} -->", c.id));
        Ok(line)
    }

    fn footnote(&self, n: usize, item: &ItemRef, quote: Option<&str>) -> String {
        let info = self.source(item);
        let title = info
            .title
            .as_deref()
            .map(|t| inline(t).replace('[', "(").replace(']', ")"))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                inline(item.key.as_str())
                    .replace('[', "(")
                    .replace(']', ")")
            });
        let title = truncate(&title, 120);
        let url = info
            .url
            .as_deref()
            .and_then(safe_url)
            .unwrap_or_else(|| item_url(item));
        let mut line = format!("[^{n}]: [{title}]({url})");
        if let Some(q) = quote.map(|q| inline(q).replace('"', "'"))
            && !q.is_empty()
        {
            line.push_str(&format!(": \"{}\"", truncate(&q, 300)));
        }
        line
    }

    /// Stub for a page whose entity was merged away (`target`) or no longer exists. With
    /// `hide_name` (the entity is now known only from sensitive items) the stub does not name
    /// it: the heading is neutral.
    pub fn render_stub(
        &self,
        id: &EntityId,
        kind: EntityKind,
        name: &str,
        target: Option<&Entity>,
        hide_name: bool,
    ) -> Rendered {
        let mut fm = vec![
            format!("nugget_id: {}", json(id.as_str())),
            format!("kind: {}", json(kind.as_str())),
        ];
        let mut links = BTreeSet::new();
        let line = match target {
            Some(t) => {
                fm.push(format!("merged_into: {}", json(t.id.as_str())));
                links.insert(t.id.clone());
                format!(
                    "This page moved: the entity was merged into {}. Your notes from here were moved there too.",
                    self.link(t)
                )
            }
            None => {
                fm.push("status: \"retracted\"".into());
                "This entity is no longer in your knowledge graph. Your notes below are kept."
                    .into()
            }
        };
        let heading = if hide_name {
            "# Hidden page".to_owned()
        } else {
            format!("# {}", inline(name))
        };
        Rendered {
            doc: Doc {
                fm: Some(fm),
                segs: vec![Segment::Region {
                    name: R_REDIRECT.into(),
                    body: vec![heading, String::new(), line],
                }],
            },
            links,
        }
    }

    /// The `README.md` index: live pages by kind.
    pub fn render_index(&self, pages: &[(EntityKind, String, EntityId)]) -> Doc {
        let mut body = vec![
            "# Nugget vault".to_owned(),
            String::new(),
            "Generated from your Nugget knowledge graph. Every fact cites its sources; facts marked \
             (you) came from your own edits. Text outside the generated blocks, including each \
             page's Notes section, is yours and survives re-renders. Edit a fact line to correct \
             it, or delete it to retract it."
                .to_owned(),
        ];
        for kind in EntityKind::ALL {
            let mut rows: Vec<(String, &EntityId, &String)> = pages
                .iter()
                .filter(|(k, _, _)| *k == kind)
                .map(|(_, n, id)| (n.to_lowercase(), id, n))
                .collect();
            if rows.is_empty() {
                continue;
            }
            rows.sort();
            body.push(String::new());
            body.push(format!("## {} ({})", kind_dir(kind), rows.len()));
            body.push(String::new());
            for (_, id, name) in rows {
                body.push(format!(
                    "- [[{}|{}]]",
                    self.stem(kind, name, id),
                    display(name)
                ));
            }
        }
        Doc {
            fm: None,
            segs: vec![
                Segment::Region {
                    name: R_INDEX.into(),
                    body,
                },
                Segment::User(vec![String::new()]),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{ItemKey, SourceId};

    #[test]
    fn urls() {
        let it = ItemRef {
            source: SourceId::new("files:~/Docs"),
            key: ItemKey::new("a b/c.md"),
        };
        assert_eq!(item_url(&it), "nugget://item/files%3A~%2FDocs/a%20b%2Fc.md");
        assert_eq!(safe_url("javascript:alert(1)"), None);
        assert_eq!(
            safe_url("https://x.test/a (b)").as_deref(),
            Some("https://x.test/a%20%28b%29")
        );
    }

    #[test]
    fn inline_text_cannot_break_structure() {
        assert_eq!(
            inline("a\nb <!-- c:1 --> [[x]] [^1]"),
            "a b &lt;!-- c:1 --&gt; [ [x] ] [ ^1]"
        );
        assert_eq!(find_word("Alan met Al", "Al"), Some(9));
    }
}
