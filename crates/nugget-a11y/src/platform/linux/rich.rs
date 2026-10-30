//! Rich text over AT-SPI:
//!
//! - Web contenteditable regions (Firefox, Chromium): the editable root's text holds a U+FFFC
//!   for each child object (paragraph, list, link...). The text is flattened by replacing each
//!   one with its object's text, recursively and bounded, with block objects on lines of their
//!   own; the caret and selection are mapped into the flattened text.
//! - LibreOffice Writer: the focus is on a `paragraph` whose parent `document text` holds one
//!   object per paragraph. The field's text is the parent's paragraphs joined with `\n`.
//!
//! Every read here is best effort and bounded (objects, characters, wall clock); callers fall
//! back to the element's own text.

use std::time::{Duration, Instant};

use atspi::Interface;
use atspi::proxy::hyperlink::HyperlinkProxyBlocking;
use atspi::proxy::hypertext::HypertextProxyBlocking;
use zbus::proxy::CacheProperties;

use super::{AtspiEl, role_name};
use crate::platform::hints;
use crate::platform::text_map::{self, EMBED, Embedded, Flat, TextMap};
use crate::tree::Element;

/// Objects read while flattening one field.
const FLATTEN_NODES: usize = 400;
/// Nesting followed while flattening.
const FLATTEN_DEPTH: usize = 8;
const FLATTEN_TIME: Duration = Duration::from_millis(500);
/// Ancestors climbed to find a web document.
const WEB_DEPTH: usize = 40;
/// Paragraphs read from an office document.
const OFFICE_PARAGRAPHS: usize = 4000;
const OFFICE_TIME: Duration = Duration::from_millis(800);

/// The live objects behind a [`Flat`], in the same shape: `kids[i]` is the object of
/// `flat.embeds[i]`.
pub(super) struct Tree {
    pub el: AtspiEl,
    kids: Vec<Option<Tree>>,
}

impl Tree {
    fn at(&self, path: &[usize]) -> Option<&Tree> {
        let mut t = self;
        for &k in path {
            t = t.kids.get(k)?.as_ref()?;
        }
        Some(t)
    }
}

struct Budget {
    nodes: usize,
    chars: usize,
    deadline: Instant,
    exhausted: bool,
}

impl Budget {
    fn take(&mut self) -> bool {
        if self.nodes == 0 || self.chars == 0 || Instant::now() >= self.deadline {
            self.exhausted = true;
            return false;
        }
        self.nodes -= 1;
        true
    }
}

impl AtspiEl {
    fn hypertext(&self) -> Option<HypertextProxyBlocking<'static>> {
        HypertextProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    fn hyperlink(&self) -> Option<HyperlinkProxyBlocking<'static>> {
        HyperlinkProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    /// The objects behind the `n` U+FFFC characters of this element's text, in order:
    /// `Hypertext.GetLink(i)` → `Hyperlink.GetObject(0)` when the link count matches, else the
    /// children in order (Gecko and Chromium expose no text leaves as children, so children
    /// and embedded characters line up).
    fn embedded_objects(&self, n: usize) -> Vec<Option<AtspiEl>> {
        if self.interfaces().contains(Interface::Hypertext)
            && let Some(h) = self.hypertext()
            && h.get_n_links().is_ok_and(|l| usize::try_from(l) == Ok(n))
        {
            return (0..n)
                .map(|i| {
                    let r = h.get_link(i32::try_from(i).ok()?).ok()?;
                    let link = AtspiEl::from_ref(&self.conn, &r)?;
                    // Gecko and Chromium answer with the child itself, which is its own link.
                    Some(
                        link.hyperlink()
                            .and_then(|l| l.get_object(0).ok())
                            .and_then(|o| AtspiEl::from_ref(&self.conn, &o))
                            .unwrap_or(link),
                    )
                })
                .collect();
        }
        let mut kids: Vec<Option<AtspiEl>> = self.children(n).into_iter().map(Some).collect();
        kids.resize_with(n, || None);
        kids
    }

    fn caret(&self) -> Option<u32> {
        let c = self.text()?.caret_offset().ok()?;
        u32::try_from(c).ok()
    }

    fn selection_range(&self) -> Option<(u32, u32)> {
        let t = self.text()?;
        if t.get_n_selections().ok()? < 1 {
            return None;
        }
        let (s, e) = t.get_selection(0).ok()?;
        let (s, e) = (u32::try_from(s).ok()?, u32::try_from(e).ok()?);
        Some((s.min(e), s.max(e)))
    }
}

fn flatten_el(el: &AtspiEl, text: String, depth: usize, b: &mut Budget) -> (Flat, Tree) {
    let n = text.chars().filter(|&c| c == EMBED).count();
    if n == 0 || depth == 0 {
        return (
            Flat::leaf(&text),
            Tree {
                el: el.clone(),
                kids: (0..n).map(|_| None).collect(),
            },
        );
    }
    let mut embeds = Vec::with_capacity(n);
    let mut kids = Vec::with_capacity(n);
    for obj in el.embedded_objects(n) {
        let got = obj.filter(|_| b.take()).and_then(|o| {
            let max = i32::try_from(b.chars).unwrap_or(i32::MAX);
            let (t, count) = o.text_contents(max)?;
            if usize::try_from(count).unwrap_or(0) > b.chars {
                b.exhausted = true;
            }
            b.chars = b.chars.saturating_sub(t.chars().count());
            let block = text_map::atspi_block_role(&role_name(&o.acc()));
            let (flat, tree) = flatten_el(&o, t, depth - 1, b);
            Some((Embedded { text: flat, block }, tree))
        });
        match got {
            Some((e, t)) => {
                embeds.push(Some(e));
                kids.push(Some(t));
            }
            None => {
                embeds.push(None);
                kids.push(None);
            }
        }
    }
    (
        text_map::flatten_embedded(&text, embeds),
        Tree {
            el: el.clone(),
            kids,
        },
    )
}

/// A field whose text holds embedded objects, flattened.
pub(super) struct Rich {
    pub flat: Flat,
    pub tree: Tree,
    /// A budget ran out: some content is missing.
    pub truncated: bool,
}

/// Flatten `el`'s text `raw` when it holds embedded objects (`None` otherwise).
pub(super) fn flatten(el: &AtspiEl, raw: &str, max_chars: usize) -> Option<Rich> {
    if !raw.contains(EMBED) {
        return None;
    }
    let mut b = Budget {
        nodes: FLATTEN_NODES,
        chars: max_chars.max(1),
        deadline: Instant::now() + FLATTEN_TIME,
        exhausted: false,
    };
    let (flat, tree) = flatten_el(el, raw.to_string(), FLATTEN_DEPTH, &mut b);
    Some(Rich {
        flat,
        tree,
        truncated: b.exhausted,
    })
}

impl Rich {
    fn inner<'a>(
        &'a self,
        get: impl Fn(&AtspiEl) -> Option<u32> + 'a,
    ) -> impl FnMut(&[usize]) -> Option<u32> + 'a {
        move |p: &[usize]| self.tree.at(p).and_then(|t| get(&t.el))
    }

    /// Selection (or caret) as offsets into the flattened text.
    pub fn selection(&self) -> Option<(u32, u32)> {
        let root = &self.tree.el;
        if let Some((s, e)) = root.selection_range()
            && s != e
        {
            let start = self.flat.resolve(
                Some(s),
                false,
                &mut self.inner(|el| el.selection_range().map(|r| r.0)),
            )?;
            let end = self.flat.resolve(
                Some(e),
                true,
                &mut self.inner(|el| el.selection_range().map(|r| r.1)),
            )?;
            return Some((start.min(end), start.max(end)));
        }
        let c = self
            .flat
            .resolve(root.caret(), false, &mut self.inner(AtspiEl::caret))?;
        Some((c, c))
    }

    /// The innermost object holding the caret and the caret offset in its own text.
    pub fn caret_leaf(&self) -> Option<(AtspiEl, u32)> {
        let (path, off) =
            self.flat
                .descend(self.tree.el.caret(), false, &mut self.inner(AtspiEl::caret))?;
        Some((self.tree.at(&path)?.el.clone(), off))
    }

    /// The root's own selection, in its own offsets (for extents).
    pub fn root_selection(&self) -> Option<(u32, u32)> {
        self.tree.el.selection_range()
    }
}

/// Whether `el` is, or is inside, a web document (bounded walk up).
pub(super) fn in_web_document(el: &AtspiEl) -> bool {
    let mut cur = Some(el.clone());
    for _ in 0..WEB_DEPTH {
        let Some(e) = cur else { return false };
        let role = role_name(&e.acc());
        if hints::atspi_web_document_role(&role) {
            return true;
        }
        if matches!(role.as_str(), "frame" | "window" | "application" | "dialog") {
            return false;
        }
        cur = e.parent();
    }
    false
}

/// A LibreOffice-style document: the focused paragraph's parent's paragraphs, joined.
pub(super) struct Office {
    pub text: String,
    /// Where the focused paragraph starts in `text`.
    pub start: u32,
    /// The focused paragraph's own text (embedded objects dropped), with its offset map.
    pub own: TextMap,
    /// Paragraphs past a budget were left out.
    pub truncated: bool,
}

impl Office {
    /// The focused paragraph's own selection (or caret), as offsets into `text`.
    pub fn selection(&self, el: &AtspiEl) -> Option<(u32, u32)> {
        let (s, e) = el
            .selection_range()
            .or_else(|| el.caret().map(|c| (c, c)))?;
        Some((
            self.start + self.own.to_text(s),
            self.start + self.own.to_text(e),
        ))
    }

    /// An offset into `text` as an offset into the focused paragraph's own text, when it
    /// falls inside that paragraph.
    pub fn to_own(&self, offset: u32) -> Option<u32> {
        let local = offset.checked_sub(self.start)?;
        (local <= self.own.len()).then(|| self.own.to_source(local))
    }
}

/// Read the whole document when `el` is a paragraph of an office document (LibreOffice
/// Writer), bounded; `None` for anything else.
pub(super) fn office_document(el: &AtspiEl, max_chars: usize) -> Option<Office> {
    let role = role_name(&el.acc());
    if !matches!(role.as_str(), "paragraph" | "heading" | "list item") {
        return None;
    }
    let parent = el.parent()?;
    if !hints::atspi_office_paragraph(&role, &role_name(&parent.acc())) {
        return None;
    }
    let deadline = Instant::now() + OFFICE_TIME;
    let mut text = String::new();
    let mut chars = 0u32;
    let mut mine: Option<(u32, TextMap)> = None;
    let mut truncated = false;
    for (i, k) in parent.children(OFFICE_PARAGRAPHS).iter().enumerate() {
        let is_me = k.same(el);
        if !is_me && (Instant::now() >= deadline || chars as usize >= max_chars) {
            truncated = true;
            if mine.is_some() {
                break;
            }
            continue;
        }
        if i > 0 {
            text.push('\n');
            chars += 1;
        }
        let max = i32::try_from(max_chars).unwrap_or(i32::MAX);
        let t = k.text_contents(max).map(|(s, _)| s).unwrap_or_default();
        let leaf = Flat::leaf(&t);
        if is_me {
            mine = Some((chars, leaf.map.clone()));
        }
        chars += leaf.map.len();
        text.push_str(leaf.text());
    }
    let (start, own) = mine?;
    Some(Office {
        text,
        start,
        own,
        truncated,
    })
}
