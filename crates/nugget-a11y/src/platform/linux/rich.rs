//! Rich text over AT-SPI:
//!
//! - Web contenteditable regions (Firefox, Chromium): the editable root's text holds a U+FFFC
//!   for each child object (paragraph, list, link...). The text is flattened by replacing each
//!   one with its object's text, recursively and bounded, with block objects on lines of their
//!   own; the caret and selection are mapped into the flattened text. Only for editable
//!   elements inside a web document: native toolkits (a GTK text view with a child anchor)
//!   keep their own text and offsets.
//! - LibreOffice Writer: the focus is on a `paragraph` whose parent `document text` holds one
//!   object per paragraph. The field's text is the parent's paragraphs joined with `\n`.
//!
//! Every read here is best effort and bounded (objects, characters, wall clock); callers fall
//! back to the element's own text.

use std::time::{Duration, Instant};

use atspi::proxy::hyperlink::HyperlinkProxyBlocking;
use atspi::proxy::hypertext::HypertextProxyBlocking;
use atspi::proxy::value::ValueProxyBlocking;
use atspi::{Interface, State};

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
/// Siblings of an office document looked at for its scroll bar.
const OFFICE_SCROLL_SIBLINGS: usize = 32;

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
    /// The objects behind the `n` U+FFFC characters of this element's text, in order:
    /// `Hypertext.GetLink(i)` → `Hyperlink.GetObject(0)` when the link count matches, else the
    /// children in order (Gecko and Chromium expose no text leaves as children, so children
    /// and embedded characters line up). Each object costs one node of the budget; once it
    /// runs out the rest are `None`.
    fn embedded_objects(&self, n: usize, b: &mut Budget) -> Vec<Option<AtspiEl>> {
        let mut out: Vec<Option<AtspiEl>> = Vec::with_capacity(n);
        if self.interfaces().contains(Interface::Hypertext)
            && let Some(h) = self.proxy::<HypertextProxyBlocking<'static>>()
            && h.get_n_links().is_ok_and(|l| usize::try_from(l) == Ok(n))
        {
            for i in 0..n {
                if !b.take() {
                    break;
                }
                out.push((|| {
                    let r = h.get_link(i32::try_from(i).ok()?).ok()?;
                    let link = AtspiEl::from_ref(&self.conn, &r)?;
                    // Gecko and Chromium answer with the child itself, which is its own link.
                    Some(
                        link.proxy::<HyperlinkProxyBlocking<'static>>()
                            .and_then(|l| l.get_object(0).ok())
                            .and_then(|o| AtspiEl::from_ref(&self.conn, &o))
                            .unwrap_or(link),
                    )
                })());
            }
        } else {
            for k in self.children(n) {
                if !b.take() {
                    break;
                }
                out.push(Some(k));
            }
        }
        out.resize_with(n, || None);
        out
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

    /// The own selection, else the caret, in this element's own offsets.
    pub(super) fn live_selection(&self) -> Option<(u32, u32)> {
        self.selection_range()
            .or_else(|| self.caret().map(|c| (c, c)))
    }
}

fn flatten_el(el: &AtspiEl, text: String, depth: usize, b: &mut Budget) -> (Flat, Tree) {
    let n = text.chars().filter(|&c| c == EMBED).count();
    if n == 0 || depth == 0 {
        if n > 0 {
            b.exhausted = true;
        }
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
    for obj in el.embedded_objects(n, b) {
        let got = obj.and_then(|o| {
            let role = role_name(&o.acc());
            // Never the text of a password field, even inside an editor.
            if role == "password text" {
                return None;
            }
            let max = i32::try_from(b.chars).unwrap_or(i32::MAX);
            let (t, count) = o.text_contents(max)?;
            if usize::try_from(count).unwrap_or(0) > b.chars {
                b.exhausted = true;
            }
            b.chars = b.chars.saturating_sub(t.chars().count());
            let block = text_map::atspi_block_role(&role);
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

/// Whether `el` is an editable element inside a web document: the only place its embedded
/// objects are flattened.
pub(super) fn is_web_editable(el: &AtspiEl) -> bool {
    el.states()
        .is_ok_and(|s| s.contains(State::Editable) && !s.contains(State::ReadOnly))
        && in_web_document(el)
}

/// Flatten `el`'s text `raw` when it holds embedded objects (`None` otherwise). The caller
/// checks [`is_web_editable`].
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
                &mut self.inner(|el| el.selection_range().map(|r| r.0)),
            )?;
            let end = self.flat.resolve(
                Some(e),
                &mut self.inner(|el| el.selection_range().map(|r| r.1)),
            )?;
            return Some((start.min(end), start.max(end)));
        }
        let c = self
            .flat
            .resolve(root.caret(), &mut self.inner(AtspiEl::caret))?;
        Some((c, c))
    }

    /// The innermost object holding the caret and the caret offset in its own text.
    pub fn caret_leaf(&self) -> Option<(AtspiEl, u32)> {
        let (path, off) = self
            .flat
            .descend(self.tree.el.caret(), &mut self.inner(AtspiEl::caret))?;
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
    /// Only part of the document was read: a budget ran out, or the document has content
    /// outside the window, whose paragraphs LibreOffice does not expose.
    pub truncated: bool,
}

impl Office {
    /// The focused paragraph's own selection (or caret), as offsets into `text`.
    pub fn selection(&self, el: &AtspiEl) -> Option<(u32, u32)> {
        let (s, e) = el.live_selection()?;
        Some((
            self.start + self.own.to_text(s),
            self.start + self.own.to_text(e),
        ))
    }
}

/// The parent document when `el` is a paragraph of an office document (LibreOffice Writer).
fn office_parent(el: &AtspiEl) -> Option<AtspiEl> {
    let role = role_name(&el.acc());
    if !matches!(role.as_str(), "paragraph" | "heading" | "list item") {
        return None;
    }
    let parent = el.parent()?;
    hints::atspi_office_paragraph(&role, &role_name(&parent.acc())).then_some(parent)
}

/// Whether `el` is a paragraph of an office document, without reading the document.
pub(super) fn is_office_paragraph(el: &AtspiEl) -> bool {
    office_parent(el).is_some()
}

/// Whether the document's window scrolls (a scroll bar next to it with a range): LibreOffice
/// then exposes only the paragraphs laid out in the window. Best effort: no scroll bar found
/// reads as "all exposed".
fn office_scrolls(doc: &AtspiEl) -> bool {
    let Some(container) = doc.parent() else {
        return false;
    };
    container
        .children(OFFICE_SCROLL_SIBLINGS)
        .iter()
        .filter(|k| role_name(&k.acc()) == "scroll bar")
        .filter_map(|k| k.proxy::<ValueProxyBlocking<'static>>())
        .any(|v| match (v.minimum_value(), v.maximum_value()) {
            (Ok(min), Ok(max)) => max > min,
            _ => false,
        })
}

/// Read the whole document when `el` is a paragraph of an office document (LibreOffice
/// Writer), bounded; `None` for anything else.
pub(super) fn office_document(el: &AtspiEl, max_chars: usize) -> Option<Office> {
    let parent = office_parent(el)?;
    let deadline = Instant::now() + OFFICE_TIME;
    let mut text = String::new();
    let mut chars = 0u32;
    let mut mine: Option<(u32, TextMap)> = None;
    let mut truncated = office_scrolls(&parent);
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
