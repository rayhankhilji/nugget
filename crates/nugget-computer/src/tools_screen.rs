//! The new screen after an action. When an action changes its window beyond the element acted
//! on (a new view, a dialog, rows that went away) the result carries the window as `observe`
//! would show it, so the model reads the fresh ids instead of reusing ones that are gone.
//!
//! The fresh snapshot is compared with the cached one (taken by the before-observation or the
//! last `observe`) and then cached in its place, exactly as `observe` caches it: the unknown-id
//! rule (an id missing from the cache is Send) is untouched. The rendering is the same compact
//! view `observe` returns, which prints secure values as `value=hidden`.

use nugget_a11y::{ElementId, UiNode, WindowId, WindowSel};
use nugget_core::Error;

use super::view;
use crate::render::render;
use crate::state::Shared;

/// Take a fresh snapshot of `window` (the front window when `None`) and, if it differs from the
/// cached one in more than `target`'s own subtree or focus, the text to append to the result.
/// Blocking; call from a blocking task.
pub(super) fn after_action(
    s: &Shared,
    window: Option<&WindowId>,
    target: Option<&ElementId>,
) -> Option<String> {
    let limits = s.config.tree_limits();
    let sel = window.map_or(WindowSel::Focused, |w| WindowSel::Id(w.clone()));
    let (tree, closed) = match s.cu.snapshot(&sel, &limits) {
        Ok(t) => (t, false),
        // The closed window stays cached: the after-observation reads it as gone (and drops
        // it), which is how a click that closes its window verifies.
        Err(Error::NotFound(_)) if window.is_some() => {
            match s.cu.snapshot(&WindowSel::Focused, &limits) {
                Ok(t) => (t, true),
                Err(_) => return Some("\nthe window closed".into()),
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "snapshot after the action failed");
            return None;
        }
    };
    let before = s
        .find(&tree.root.id)
        .filter(|f| f.window.id == tree.window.id)
        .map(|f| f.node);
    let changed = closed || before.is_none_or(|b| differs(&b, &tree.root, target));
    let opts = s.config.render_options();
    let text = changed.then(|| render(&view::compact(&tree, &opts), &opts));
    s.remember(tree);
    let lead = if closed {
        "the window closed; the front window now shows"
    } else {
        "the window changed; it now shows"
    };
    text.map(|t| format!("\n{lead}:\n{}", t.trim_end()))
}

/// The trees differ in more than `target`'s subtree and keyboard focus.
fn differs(before: &UiNode, after: &UiNode, target: Option<&ElementId>) -> bool {
    let (mut b, mut a) = (before.clone(), after.clone());
    mask(&mut b, target);
    mask(&mut a, target);
    b != a
}

fn mask(node: &mut UiNode, target: Option<&ElementId>) {
    if target == Some(&node.id) {
        *node = UiNode {
            id: node.id.clone(),
            ..UiNode::default()
        };
        return;
    }
    node.states.focused = false;
    for c in &mut node.children {
        mask(c, target);
    }
}
