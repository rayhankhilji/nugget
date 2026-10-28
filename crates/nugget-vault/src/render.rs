//! Rendering entities, redirect stubs and the index into page documents.
//!
//! Output is a pure function of the graph (and the item lookup): claims are grouped by predicate
//! in byte order, sorted by id within a group, footnotes are numbered in order of first use, and
//! neighbours are sorted by kind, name and id.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{Claim, ClaimObject, ClaimStatus, Entity, EntityKind, GraphStore};
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
}

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
    }

    fn source(&self, item: &ItemRef) -> SourceInfo {
        if let Some(s) = self.sources.borrow().get(item) {
            return s.clone();
        }
        let info = match self.items.source_info(item) {
            Ok(i) => i.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "vault: source lookup failed");
                SourceInfo::default()
            }
        };
        self.sources.borrow_mut().insert(item.clone(), info.clone());
        info
    }

    fn frontmatter(&self, e: &Entity) -> Vec<String> {
        let mut fm = vec![
            format!("nugget_id: {}", json(e.id.as_str())),
            format!("kind: {}", json(e.kind.as_str())),
            format!("aliases: {}", json(&e.aliases)),
        ];
        if self.cfg.include_identifiers && !e.identifiers.is_empty() {
            let mut ids: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for i in &e.identifiers {
                ids.entry(i.scheme.as_str())
                    .or_default()
                    .push(i.value.as_str());
            }
            fm.push(format!("identifiers: {}", json(ids)));
        }
        fm.push(format!("updated: {}", json(rfc3339(e.updated_at))));
        fm
    }

    pub fn render_entity(&self, e: &Entity, claims: &[Claim]) -> Result<Rendered> {
        let mut links = BTreeSet::new();

        // ---- facts ----
        let mut groups: BTreeMap<&str, Vec<&Claim>> = BTreeMap::new();
        for c in claims {
            if c.subject == e.id && c.status.is_active() {
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
        for c in claims.iter().filter(|c| c.status.is_active()) {
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
            links.insert(other.id.clone());
            rows.push((
                key,
                format!("- {}: {}", self.link(&other), preds.join(", ")),
            ));
        }
        rows.sort();
        rows.dedup_by(|a, b| a.1 == b.1);
        let mut related = vec!["## Related".to_owned(), String::new()];
        if rows.is_empty() {
            related.push("_No related entities yet._".into());
        }
        related.extend(rows.into_iter().map(|(_, l)| l));

        let doc = Doc {
            fm: Some(self.frontmatter(e)),
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
        if let Some(o) = object {
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
        for ev in &c.evidence {
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

    /// Stub for a page whose entity was merged away (`target`) or no longer exists.
    pub fn render_stub(
        &self,
        id: &EntityId,
        kind: EntityKind,
        name: &str,
        target: Option<&Entity>,
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
        Rendered {
            doc: Doc {
                fm: Some(fm),
                segs: vec![Segment::Region {
                    name: R_REDIRECT.into(),
                    body: vec![format!("# {}", inline(name)), String::new(), line],
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
