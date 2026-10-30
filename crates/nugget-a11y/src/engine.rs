//! The platform-neutral half of every backend: [`Backend`] implements [`Accessibility`] on top
//! of a small [`Platform`] trait (find the focused element, read a field, insert, paste,
//! clipboard), and owns the policy: role normalisation, secure fields, the deny list, the
//! thread walk, read-back verification and the clipboard fallback.

use std::time::{Duration, Instant};

use nugget_core::ipc::{AppInfo, FieldInfo};
use nugget_core::{Error, Result};

use crate::clipboard::{self, Clipboard, PasteCheck, PasteError, PasteTiming};
use crate::deny::DenyList;
use crate::role::{FieldHints, FieldRole, Role, field_role};
use crate::sanitize;
use crate::thread::{self, ThreadLimits};
use crate::tree::{
    EMBEDDED_OBJECT, Element, FlatText, FlattenLimits, Node, Snapshot, WalkLimits,
    editable_ancestor, flatten_text_with, snapshot_around,
};
use crate::verify::{self, FieldState};
use crate::{Accessibility, Focused, InsertMethod, InsertOutcome, Permission, Rect};

/// How `insert` may put text into the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsertStrategy {
    /// Accessibility insertion, falling back to the clipboard when the field did not change.
    /// Rich web editors get the clipboard first for multi-line or long text (see
    /// [`RichPaste`]), falling back to accessibility insertion when the paste left the field
    /// unchanged.
    #[default]
    Auto,
    AccessibilityOnly,
    ClipboardOnly,
}

/// When [`InsertStrategy::Auto`] pastes first into a rich web editor (a contenteditable root
/// such as ProseMirror/TipTap, Quill, Draft.js, Lexical, CKEditor or a mail compose body).
///
/// These editors handle `insertFromPaste` themselves, as one undo step with paragraphs intact.
/// Accessibility insertion reaches them as `insertText` or per-character input, which loses
/// paragraph breaks or is reverted by the framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RichPaste {
    /// Paste first when the text contains a line break.
    pub on_newline: bool,
    /// Paste first when the text has at least this many characters (`usize::MAX`: never).
    pub min_chars: usize,
}

impl RichPaste {
    /// Never paste first: accessibility insertion, then the clipboard, as for other fields.
    pub const OFF: RichPaste = RichPaste {
        on_newline: false,
        min_chars: usize::MAX,
    };
}

impl Default for RichPaste {
    fn default() -> Self {
        RichPaste {
            on_newline: true,
            min_chars: 1000,
        }
    }
}

/// Backend configuration.
#[derive(Debug, Clone)]
pub struct A11yConfig {
    pub deny: DenyList,
    pub thread: ThreadLimits,
    pub walk: WalkLimits,
    pub insert: InsertStrategy,
    pub paste: PasteTiming,
    /// Cap on a field value read for drafting (bytes).
    pub max_value_bytes: usize,
    /// How long to poll the field after an accessibility insertion before calling it
    /// unverified.
    pub verify_timeout: Duration,
    /// When `Auto` insertion pastes first into rich web editors.
    pub rich_paste: RichPaste,
}

impl Default for A11yConfig {
    fn default() -> Self {
        A11yConfig {
            deny: DenyList::defaults(),
            thread: ThreadLimits::default(),
            walk: WalkLimits::default(),
            insert: InsertStrategy::Auto,
            paste: PasteTiming::default(),
            max_value_bytes: sanitize::MAX_VALUE,
            verify_timeout: Duration::from_millis(600),
            rich_paste: RichPaste::default(),
        }
    }
}

/// The focused field as read by a platform.
#[derive(Debug, Clone, Default)]
pub struct FieldRead {
    /// The element itself (role, name, placeholder, states, bounds, attrs).
    pub node: Node,
    /// The label from a separate labelling element, when the platform has one.
    pub label: Option<String>,
    /// Value and selection (character offsets). Empty for secure fields.
    pub state: FieldState,
}

/// The focused field as the backend uses it (see `Backend::resolve_field`).
struct Field<E> {
    /// The editor root: its value, role, label and surroundings are what `focused()` reports and
    /// what verification reads back.
    root: E,
    /// The element with keyboard focus when it is a descendant of `root` (a paragraph inside a
    /// contenteditable). Writes go to it.
    caret: Option<E>,
    /// `root` as read, its value rebuilt from its children when it held embedded objects, with
    /// the selection mapped into that value (`None` when it could not be).
    read: FieldRead,
    /// The state of the element written to, as the platform read it (its own offsets).
    write_state: FieldState,
}

impl<E> Field<E> {
    /// Accessibility insertion, the caret bounds and the paste target use the caret's own
    /// element: platforms address text by offsets into the element they are given, and the
    /// selection lives there.
    fn write_el(&self) -> &E {
        self.caret.as_ref().unwrap_or(&self.root)
    }
}

/// How an accessibility insertion ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Verified,
    /// The field changed, but not as expected, or it could not be read back completely: never
    /// insert a second copy.
    Changed,
    Unchanged,
}

/// What the frontmost app has focused.
pub struct Target<E> {
    /// Id, name and window title (the URL is filled in from the element tree).
    pub app: AppInfo,
    pub element: E,
}

/// A top-level window as a platform enumerates it (M4 computer use).
pub struct PlatformWindow<E> {
    pub app: crate::computer::AppEntry,
    /// A handle that is stable while the window exists (CGWindowID / AX pid+index, HWND, AT-SPI
    /// bus name + object path). Hashed into the [`WindowId`](crate::computer::WindowId).
    pub native_id: String,
    pub title: Option<String>,
    pub bounds: Option<Rect>,
    pub focused: bool,
    pub minimized: bool,
    /// The window element; its subtree is what [`Backend`] snapshots.
    pub element: E,
}

fn no_computer_use<T>() -> Result<T> {
    Err(Error::Unsupported(
        "computer use is not implemented for this platform".into(),
    ))
}

/// The per-OS half of a backend.
pub trait Platform: Send + Sync {
    type Element: Element;

    fn permission(&self) -> Permission;
    fn request_permission(&self) -> Result<()>;
    /// The frontmost app and the element focused in its focused window, or `None` when nothing
    /// is focused. Must not return an element from any other window.
    fn focus(&self) -> Result<Option<Target<Self::Element>>>;
    /// Read the focused field. Must not read the value of a secure field.
    fn read_field(&self, el: &Self::Element, max_value_bytes: usize) -> Result<FieldRead>;
    /// Caret (or selection) bounds in screen points, top-left origin.
    fn caret_bounds(&self, el: &Self::Element, state: &FieldState) -> Option<Rect>;
    /// Insert `text` replacing the selection through the accessibility API.
    fn insert_text(&self, el: &Self::Element, text: &str, before: &FieldState) -> Result<()>;
    /// The system clipboard.
    fn clipboard(&self) -> Result<Box<dyn Clipboard>>;
    /// Paste into `el` (Cmd/Ctrl+V or a toolkit paste action). Never sends Return.
    fn paste(&self, el: &Self::Element) -> Result<()>;

    // --- M4 computer use. Defaults refuse, so a platform can grow into them. ---

    /// Running apps that have windows, frontmost first.
    fn apps(&self) -> Result<Vec<crate::computer::AppEntry>> {
        no_computer_use()
    }
    /// Top-level windows of every app, the focused window first.
    fn windows(&self) -> Result<Vec<PlatformWindow<Self::Element>>> {
        no_computer_use()
    }
    /// Synthesise key input to whatever has keyboard focus. Text is typed as Unicode (never as
    /// shortcuts); chords press modifiers, the key, and release in reverse.
    fn key_input(&self, _input: &crate::computer::KeyInput) -> Result<()> {
        no_computer_use()
    }
    /// Launch the app by name or id, or activate it when it is running.
    fn open_app(&self, _name: &str) -> Result<()> {
        no_computer_use()
    }
    /// A PNG of the window, for the screenshot fallback.
    fn screenshot(&self, _window: &Self::Element) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// [`Accessibility`] over a [`Platform`].
pub struct Backend<P: Platform> {
    platform: P,
    config: A11yConfig,
    /// Element ids handed out by recent computer-use snapshots.
    pub(crate) computer: crate::computer::backend::Cache,
}

impl<P: Platform> Backend<P> {
    pub fn new(platform: P, config: A11yConfig) -> Self {
        Backend {
            platform,
            config,
            computer: Default::default(),
        }
    }

    pub fn config(&self) -> &A11yConfig {
        &self.config
    }

    pub fn platform(&self) -> &P {
        &self.platform
    }

    /// The frontmost app with its URL, and whether it is denied, without reading any field.
    pub fn frontmost_app(&self) -> Result<Option<(AppInfo, bool)>> {
        let Some(t) = self.platform.focus()? else {
            return Ok(None);
        };
        let app = self.with_url(t.app, &t.element);
        let denied = self.config.deny.is_denied(&app);
        Ok(Some((app, denied)))
    }

    /// A snapshot of the focused element's surroundings, for recording test trees and for
    /// computer use. `None` when nothing is focused or the app is denied.
    pub fn snapshot(&self) -> Result<Option<(AppInfo, Snapshot)>> {
        let Some(t) = self.platform.focus()? else {
            return Ok(None);
        };
        let app = self.with_url(t.app, &t.element);
        if self.config.deny.is_denied(&app) {
            return Ok(None);
        }
        let snap = snapshot_around(&t.element, &self.config.walk);
        Ok(Some((app, snap)))
    }

    fn with_url(&self, mut app: AppInfo, el: &P::Element) -> AppInfo {
        app.id = sanitize::clean_opt(app.id.as_deref(), sanitize::MAX_SHORT);
        app.name = sanitize::clean_line(&app.name, sanitize::MAX_SHORT);
        app.window_title =
            sanitize::clean_opt(app.window_title.as_deref(), sanitize::MAX_SHORT * 2);
        if app.url.is_none() {
            app.url = find_url(el);
        }
        app.url = app.url.as_deref().and_then(clean_url);
        app
    }

    fn field_role_of(read: &FieldRead) -> Option<FieldRole> {
        let n = &read.node;
        let hints = FieldHints {
            editable: n.states.editable,
            multiline: n.states.multiline,
            placeholder: n.placeholder.as_deref(),
            label: read.label.as_deref().or(n.name.as_deref()),
        };
        // A secure field is a text field even when the toolkit hides its editable state.
        if n.states.secure && n.role.is_text_input() {
            return Some(FieldRole::TextField);
        }
        field_role(n.role, hints)
    }

    /// Resolve the focused element to the field: the editor root when focus sits inside a
    /// contenteditable ([`editor_root`](crate::tree::editor_root)), else the element itself.
    /// The root's value is rebuilt from its children when it holds embedded-object characters,
    /// and the selection is mapped into it from the caret's element.
    fn resolve_field(&self, focus: P::Element) -> Result<Field<P::Element>> {
        let max = self.config.max_value_bytes;
        let focus_read = self.platform.read_field(&focus, max)?;
        if !focus_read.node.states.secure
            && let Some(root) = editable_ancestor(&focus)
        {
            let mut read = self.platform.read_field(&root, max)?;
            if !read.node.states.secure && Self::field_role_of(&read).is_some() {
                read.state = self.flat_state(&root, read.state, Some((&focus, &focus_read.state)));
                return Ok(Field {
                    root,
                    caret: Some(focus),
                    read,
                    write_state: focus_read.state,
                });
            }
        }
        let mut read = focus_read;
        let write_state = read.state.clone();
        read.state = self.flat_state(&focus, read.state, None);
        Ok(Field {
            root: focus,
            caret: None,
            read,
            write_state,
        })
    }

    /// `native` with embedded objects replaced by the text of `el`'s children. The selection
    /// comes from the caret's element (offset by where its text starts) or is mapped from
    /// `native`; it is `None` when neither can be placed.
    fn flat_state(
        &self,
        el: &P::Element,
        native: FieldState,
        caret: Option<(&P::Element, &FieldState)>,
    ) -> FieldState {
        if !native.value.contains(EMBEDDED_OBJECT) {
            return native;
        }
        let limits = FlattenLimits {
            max_bytes: self.config.max_value_bytes,
            ..Default::default()
        };
        let flat = flatten_text_with(el, Some(&native.value), caret.map(|c| c.0), &limits);
        let selection = match caret {
            Some((_, cs)) => caret_selection(&flat, cs),
            None => native
                .selection
                .and_then(|(s, e)| Some((flat.map_own_offset(s)?, flat.map_own_offset(e)?))),
        };
        FieldState {
            value: flat.text,
            selection,
            truncated: native.truncated || flat.truncated,
        }
    }

    fn read_back(&self, root: &P::Element) -> Result<FieldState> {
        let state = self
            .platform
            .read_field(root, self.config.max_value_bytes)?
            .state;
        Ok(self.flat_state(root, state, None))
    }

    /// Whether `Auto` should paste `text` before trying accessibility insertion.
    fn wants_rich_paste(&self, field: &Field<P::Element>, text: &str) -> bool {
        let rp = self.config.rich_paste;
        let trigger =
            (rp.on_newline && text.contains('\n')) || text.chars().count() >= rp.min_chars;
        trigger && is_rich_editor(&field.root, &field.read.node)
    }

    /// Insert through the accessibility API and poll the field until it verifies or the
    /// timeout passes. Only an error from the insertion itself is returned (nothing was
    /// inserted, another method may be tried); once it succeeded, a failed read-back counts as
    /// [`Attempt::Changed`].
    fn try_accessibility(
        &self,
        field: &Field<P::Element>,
        text: &str,
        before: &FieldState,
    ) -> Result<Attempt> {
        self.platform
            .insert_text(field.write_el(), text, &field.write_state)?;
        let deadline = Instant::now() + self.config.verify_timeout;
        loop {
            let after = match self.read_back(&field.root) {
                Ok(a) => a,
                Err(e) => {
                    tracing::debug!(error = %e, "read-back after insertion failed");
                    return Ok(Attempt::Changed);
                }
            };
            if verify::verify_insert(before, text, &after) {
                return Ok(Attempt::Verified);
            }
            if Instant::now() >= deadline {
                return Ok(if verify::changed(before, &after) {
                    Attempt::Changed
                } else {
                    Attempt::Unchanged
                });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Paste through the clipboard (saved and restored on every path) and verify. `Ok(false)`:
    /// the paste was sent but not verified (it may still land). `Err(NotSent)`: nothing reached
    /// the app.
    fn try_paste(
        &self,
        field: &Field<P::Element>,
        text: &str,
        before: &FieldState,
    ) -> std::result::Result<bool, PasteError> {
        let mut cb = self.platform.clipboard().map_err(PasteError::NotSent)?;
        let r = clipboard::paste_with_restore(
            cb.as_mut(),
            text,
            &mut || self.platform.paste(field.write_el()),
            &mut || {
                let after = self.read_back(&field.root)?;
                Ok(if verify::verify_insert(before, text, &after) {
                    PasteCheck::Verified
                } else if verify::changed(before, &after) {
                    PasteCheck::Changed
                } else {
                    PasteCheck::Unchanged
                })
            },
            self.config.paste,
        )?;
        Ok(r.verified)
    }

    fn insert_inner(&self, text: &str) -> Result<InsertOutcome> {
        let text = clean_insert_text(text);
        let Some(target) = self.platform.focus()? else {
            return Err(Error::NotFound("no focused text field".into()));
        };
        let app = self.with_url(target.app, &target.element);
        if self.config.deny.is_denied(&app) {
            return Err(Error::Invalid(format!(
                "{} is excluded from drafting",
                app.name
            )));
        }
        let field = self.resolve_field(target.element)?;
        if field.read.node.states.secure {
            return Err(Error::Invalid(
                "refusing to insert into a secure field".into(),
            ));
        }
        if Self::field_role_of(&field.read).is_none() {
            return Err(Error::NotFound(
                "focus is not in an editable text field".into(),
            ));
        }
        let before = field.read.state.clone();
        let outcome = |method, verified| InsertOutcome { method, verified };
        let strategy = self.config.insert;

        if strategy == InsertStrategy::ClipboardOnly {
            let verified = self.try_paste(&field, &text, &before)?;
            return Ok(outcome(InsertMethod::Clipboard, verified));
        }

        // Rich web editors: paste first. Once the paste was sent it may still land, so the
        // only fallback is for a paste that never reached the app.
        if strategy == InsertStrategy::Auto && self.wants_rich_paste(&field, &text) {
            match self.try_paste(&field, &text, &before) {
                Ok(verified) => return Ok(outcome(InsertMethod::Clipboard, verified)),
                Err(PasteError::Sent(e)) => {
                    tracing::debug!(error = %e, "rich editor paste failed after it was sent");
                    return Ok(outcome(InsertMethod::Clipboard, false));
                }
                Err(PasteError::NotSent(e)) => {
                    tracing::debug!(error = %e, "clipboard unavailable; inserting directly");
                }
            }
            let a = self.try_accessibility(&field, &text, &before)?;
            return Ok(outcome(InsertMethod::Accessibility, a == Attempt::Verified));
        }

        match self.try_accessibility(&field, &text, &before) {
            Ok(Attempt::Verified) => return Ok(outcome(InsertMethod::Accessibility, true)),
            // Something landed but not what we expected (or a read could not tell): never
            // insert a second copy.
            Ok(Attempt::Changed) => return Ok(outcome(InsertMethod::Accessibility, false)),
            Ok(Attempt::Unchanged) if strategy == InsertStrategy::AccessibilityOnly => {
                return Ok(outcome(InsertMethod::Accessibility, false));
            }
            Err(e) if strategy == InsertStrategy::AccessibilityOnly => return Err(e),
            Ok(Attempt::Unchanged) => {
                tracing::debug!("accessibility insertion left the field unchanged; pasting");
            }
            Err(e) => tracing::debug!(error = %e, "accessibility insertion failed; pasting"),
        }
        let verified = self.try_paste(&field, &text, &before)?;
        Ok(outcome(InsertMethod::Clipboard, verified))
    }
}

/// The selection in the flattened value, from the caret element's own selection and where its
/// text starts. `None` unless its text is found there unchanged.
fn caret_selection(flat: &FlatText, caret: &FieldState) -> Option<(u32, u32)> {
    let base = flat.mark_start?;
    let (s, e) = caret.selection?;
    let own = caret.value.trim_end_matches('\n');
    if own.contains(EMBEDDED_OBJECT) {
        return None;
    }
    let len = own.chars().count();
    if s.max(e) as usize > len {
        return None;
    }
    let found = flat.text.chars().skip(base as usize).take(len);
    if !found.eq(own.chars()) {
        return None;
    }
    Some((base + s, base + e))
}

/// DOM classes of rich-text editor roots (lower case).
const RICH_EDITOR_CLASSES: &[&str] = &[
    "prosemirror",
    "tiptap",
    "ql-editor",
    "public-drafteditor-content",
    "drafteditor-editorcontainer",
    "contenteditable__root",
    "ck-content",
    "ck-editor__editable",
    "cke_editable",
    "mce-content-body",
    "fr-element",
    "trix-content",
    "redactor-editor",
];

/// Whether `node` (the field root `el`) is a rich web editor: a contenteditable root inside web
/// content rather than a native `<input>` or `<textarea>`. Signals, strongest first: an
/// `html_tag`/`tag` attribute (`textarea`, `input`: plain), a `contenteditable` attribute, a
/// known editor class in `dom_classes`/`class`; then the role: editable containers (group,
/// section, paragraph, document, web area...) are contenteditable, search fields and combo
/// boxes never are, and a text field or area is rich when it is not single-line and has child
/// elements (paragraphs, text runs), which native text inputs do not expose.
pub fn is_rich_editor<E: Element>(el: &E, node: &Node) -> bool {
    if node.states.secure || !in_web_content(el, node) {
        return false;
    }
    if let Some(tag) = node.attr("html_tag").or(node.attr("tag")) {
        let tag = tag.trim().to_ascii_lowercase();
        if tag == "textarea" || tag == "input" {
            return false;
        }
    }
    if node
        .attr("contenteditable")
        .is_some_and(|v| !v.trim().eq_ignore_ascii_case("false"))
    {
        return true;
    }
    let classes = [node.attr("dom_classes"), node.attr("class")];
    if classes.into_iter().flatten().any(|c| {
        c.split_whitespace()
            .any(|c| RICH_EDITOR_CLASSES.contains(&c.to_ascii_lowercase().as_str()))
    }) {
        return true;
    }
    match node.role {
        Role::SearchField | Role::ComboBox => false,
        Role::TextField | Role::TextArea => {
            node.states.multiline != Some(false) && !el.children(1).is_empty()
        }
        _ => true,
    }
}

/// Whether `el` is (in) web content: a web area itself, or below one (or below a document that
/// has a URL) before any window.
fn in_web_content<E: Element>(el: &E, node: &Node) -> bool {
    if node.role == Role::WebArea {
        return true;
    }
    let mut cur = el.parent();
    for _ in 0..40 {
        let Some(e) = cur else { return false };
        let n = e.node();
        match n.role {
            Role::WebArea => return true,
            Role::Document if n.attrs.contains_key("url") => return true,
            Role::Window | Role::Application => return false,
            _ => {}
        }
        cur = e.parent();
    }
    false
}

/// Text we insert is ours, but keep it free of control characters: no `\r` (some fields treat
/// it as Return), no escape sequences.
fn clean_insert_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

fn clean_url(url: &str) -> Option<String> {
    let u = sanitize::clean_line(url, 2048);
    let lower = u.to_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://"))
    {
        return None;
    }
    // Drop credentials in the authority.
    let (scheme, rest) = u.split_once("://")?;
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(auth_end);
    let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    Some(format!("{scheme}://{auth}{tail}"))
}

/// The page URL from the nearest web-area ancestor that exposes one.
fn find_url<E: Element>(el: &E) -> Option<String> {
    let mut cur = el.parent();
    for _ in 0..40 {
        let e = cur?;
        let n = e.node();
        if let Some(u) = n.attrs.get("url") {
            return Some(u.clone());
        }
        if matches!(n.role, Role::Window | Role::Application) {
            return None;
        }
        cur = e.parent();
    }
    None
}

impl<P: Platform> Accessibility for Backend<P> {
    fn permission(&self) -> Permission {
        self.platform.permission()
    }

    fn request_permission(&self) -> Result<()> {
        self.platform.request_permission()
    }

    fn focused(&self) -> Result<Option<Focused>> {
        let Some(target) = self.platform.focus()? else {
            return Ok(None);
        };
        let app = self.with_url(target.app, &target.element);
        let field = self.resolve_field(target.element)?;
        let read = &field.read;
        let Some(role) = Self::field_role_of(read) else {
            return Ok(None);
        };
        let secure = read.node.states.secure;
        let denied = self.config.deny.is_denied(&app);
        let caret = if secure {
            None
        } else {
            self.platform
                .caret_bounds(field.write_el(), &field.write_state)
        };
        let label = read
            .label
            .clone()
            .or_else(|| read.node.name.clone())
            .and_then(|l| sanitize::clean_opt(Some(&l), sanitize::MAX_SHORT));
        let info = FieldInfo {
            role: role.as_str().to_string(),
            label,
            placeholder: sanitize::clean_opt(read.node.placeholder.as_deref(), sanitize::MAX_SHORT),
            value: if secure || denied {
                String::new()
            } else {
                read.state.value.clone()
            },
            selection: if secure || denied {
                None
            } else {
                read.state.selection
            },
            secure,
        };
        // The thread walk starts at the editor root, so the editor's own paragraphs are never
        // taken for messages.
        let ctx = if secure || denied {
            thread::ThreadContext::default()
        } else {
            let snap = snapshot_around(&field.root, &self.config.walk);
            thread::extract(&snap, &app, &read.state.value, &self.config.thread)
        };
        Ok(Some(Focused {
            app,
            field: info,
            caret,
            thread: ctx.messages,
            recipients: ctx.recipients,
            subject: ctx.subject,
        }))
    }

    fn insert(&self, text: &str) -> Result<InsertOutcome> {
        self.insert_inner(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{ClipboardFormat, ClipboardSnapshot, MemoryClipboard};
    use crate::tree::tests::MemEl;
    use std::sync::Mutex;

    /// A scripted platform over an in-memory tree, with a field whose value it tracks.
    struct Fake {
        tree: Node,
        path: Vec<usize>,
        app: AppInfo,
        value: Mutex<String>,
        selection: (u32, u32),
        /// What accessibility insertion does: insert, do nothing, or fail.
        a11y: &'static str,
        clipboard: std::sync::Arc<Mutex<MemoryClipboard>>,
        pasted: Mutex<u32>,
    }

    struct SharedCb(std::sync::Arc<Mutex<MemoryClipboard>>);
    impl Clipboard for SharedCb {
        fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
            self.0.lock().unwrap().snapshot()
        }
        fn set_text(&mut self, t: &str) -> Result<()> {
            self.0.lock().unwrap().set_text(t)
        }
        fn restore(&mut self, s: &ClipboardSnapshot) -> Result<()> {
            self.0.lock().unwrap().restore(s)
        }
    }

    // MemEl is Rc-based; the fake rebuilds it per call so it stays Send + Sync.
    impl Platform for Fake {
        type Element = MemEl;
        fn permission(&self) -> Permission {
            Permission::Granted
        }
        fn request_permission(&self) -> Result<()> {
            Ok(())
        }
        fn focus(&self) -> Result<Option<Target<MemEl>>> {
            Ok(Some(Target {
                app: self.app.clone(),
                element: MemEl::new(self.tree.clone(), self.path.clone()),
            }))
        }
        fn read_field(&self, el: &MemEl, _max: usize) -> Result<FieldRead> {
            let node = el.node();
            let value = if node.states.secure {
                String::new()
            } else {
                self.value.lock().unwrap().clone()
            };
            Ok(FieldRead {
                node,
                label: None,
                state: FieldState {
                    value,
                    selection: Some(self.selection),
                    truncated: false,
                },
            })
        }
        fn caret_bounds(&self, _el: &MemEl, _s: &FieldState) -> Option<Rect> {
            Some(Rect {
                x: 10.0,
                y: 20.0,
                width: 1.0,
                height: 14.0,
            })
        }
        fn insert_text(&self, _el: &MemEl, text: &str, before: &FieldState) -> Result<()> {
            match self.a11y {
                "ok" => {
                    *self.value.lock().unwrap() =
                        verify::expected_after_insert(before, text).unwrap();
                    Ok(())
                }
                "noop" => Ok(()),
                "garble" => {
                    self.value.lock().unwrap().push_str("??");
                    Ok(())
                }
                _ => Err(Error::Unsupported("no editable text".into())),
            }
        }
        fn clipboard(&self) -> Result<Box<dyn Clipboard>> {
            Ok(Box::new(SharedCb(self.clipboard.clone())))
        }
        fn paste(&self, _el: &MemEl) -> Result<()> {
            *self.pasted.lock().unwrap() += 1;
            let cb = self.clipboard.lock().unwrap();
            let text = String::from_utf8(cb.contents.items[0][0].data.clone()).unwrap();
            self.value.lock().unwrap().push_str(&text);
            Ok(())
        }
    }

    fn chat_tree() -> (Node, Vec<usize>) {
        let json = include_str!("../tests/fixtures/trees/generic_chat.json");
        let snap = Snapshot::from_json(json).unwrap();
        (snap.root, snap.focus_path)
    }

    fn fake(a11y: &'static str) -> Fake {
        let (tree, path) = chat_tree();
        Fake {
            tree,
            path,
            app: AppInfo {
                id: Some("org.example.chat".into()),
                name: "Example Chat".into(),
                window_title: Some("Team chat".into()),
                url: None,
            },
            value: Mutex::new("Hello ".into()),
            selection: (6, 6),
            a11y,
            clipboard: std::sync::Arc::new(Mutex::new(MemoryClipboard {
                contents: ClipboardSnapshot {
                    items: vec![vec![ClipboardFormat {
                        format: "image/png".into(),
                        data: vec![1, 2, 3],
                    }]],
                },
                ..Default::default()
            })),
            pasted: Mutex::new(0),
        }
    }

    fn fast_config() -> A11yConfig {
        A11yConfig {
            paste: PasteTiming {
                before_paste: Duration::ZERO,
                poll: Duration::from_millis(1),
                verify_timeout: Duration::from_millis(20),
                before_restore: Duration::ZERO,
                unverified_grace: Duration::from_millis(5),
            },
            verify_timeout: Duration::from_millis(10),
            ..Default::default()
        }
    }

    #[test]
    fn focused_reads_field_and_thread() {
        let b = Backend::new(fake("ok"), fast_config());
        let f = b.focused().unwrap().unwrap();
        assert_eq!(f.field.role, "text_area");
        assert_eq!(f.field.value, "Hello ");
        assert_eq!(f.field.selection, Some((6, 6)));
        assert!(!f.field.secure);
        assert!(f.caret.is_some());
        assert_eq!(f.thread.len(), 3);
        assert_eq!(f.thread[2].from_me, Some(true));
    }

    #[test]
    fn denied_app_gets_no_thread_or_value() {
        let mut p = fake("ok");
        p.app.name = "1Password".into();
        let b = Backend::new(p, fast_config());
        let f = b.focused().unwrap().unwrap();
        assert!(f.thread.is_empty());
        assert!(f.field.value.is_empty());
        assert!(b.insert("x").is_err());
    }

    #[test]
    fn secure_field_is_flagged_and_never_read() {
        let mut p = fake("ok");
        let mut tree = p.tree.clone();
        let mut n = &mut tree;
        for &i in &p.path {
            n = &mut n.children[i];
        }
        n.states.secure = true;
        n.role = Role::TextField;
        n.value = Some("hunter2".into());
        p.tree = tree;
        let b = Backend::new(p, fast_config());
        let f = b.focused().unwrap().unwrap();
        assert!(f.field.secure);
        assert_eq!(f.field.role, "text_field");
        assert!(f.field.value.is_empty());
        assert_eq!(f.field.selection, None);
        assert!(f.thread.is_empty());
        assert!(f.caret.is_none());
        assert!(b.insert("x").is_err());
    }

    #[test]
    fn accessibility_insert_is_verified() {
        let b = Backend::new(fake("ok"), fast_config());
        let out = b.insert("there").unwrap();
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(out.verified);
        assert_eq!(*b.platform().value.lock().unwrap(), "Hello there");
        assert_eq!(*b.platform().pasted.lock().unwrap(), 0);
    }

    #[test]
    fn falls_back_to_clipboard_and_restores() {
        for mode in ["noop", "fail"] {
            let b = Backend::new(fake(mode), fast_config());
            let out = b.insert("there\r\n").unwrap();
            assert_eq!(out.method, InsertMethod::Clipboard, "{mode}");
            assert!(out.verified);
            assert_eq!(*b.platform().value.lock().unwrap(), "Hello there\n");
            let cb = b.platform().clipboard.lock().unwrap();
            assert_eq!(cb.contents.items[0][0].format, "image/png");
        }
    }

    #[test]
    fn unverified_change_is_not_pasted_again() {
        let b = Backend::new(fake("garble"), fast_config());
        let out = b.insert("there").unwrap();
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(!out.verified);
        assert_eq!(*b.platform().pasted.lock().unwrap(), 0);
    }

    #[test]
    fn strategies() {
        let mut cfg = fast_config();
        cfg.insert = InsertStrategy::ClipboardOnly;
        let b = Backend::new(fake("ok"), cfg.clone());
        assert_eq!(b.insert("x").unwrap().method, InsertMethod::Clipboard);
        cfg.insert = InsertStrategy::AccessibilityOnly;
        let b = Backend::new(fake("fail"), cfg.clone());
        assert!(b.insert("x").is_err());
        let b = Backend::new(fake("noop"), cfg);
        let out = b.insert("x").unwrap();
        assert_eq!(
            out,
            InsertOutcome {
                method: InsertMethod::Accessibility,
                verified: false
            }
        );
    }

    #[test]
    fn urls_are_cleaned() {
        assert_eq!(
            clean_url("https://user:pw@mail.google.com/mail/u/0/").as_deref(),
            Some("https://mail.google.com/mail/u/0/")
        );
        assert_eq!(clean_url("javascript:alert(1)"), None);
        assert_eq!(clean_url("about:blank"), None);
    }

    // -----------------------------------------------------------------------------------------
    // Rich web editors (tests/fixtures/trees/web_*.json), with a live editor model

    use std::sync::Arc;

    /// An element over a shared tree that the fake platform mutates, so reads after an
    /// insertion see the new paragraphs.
    #[derive(Clone)]
    struct LiveEl {
        root: Arc<Mutex<Node>>,
        path: Vec<usize>,
    }

    impl LiveEl {
        fn with<R>(&self, f: impl FnOnce(Option<&Node>) -> R) -> R {
            let root = self.root.lock().unwrap();
            let mut n = Some(&*root);
            for &i in &self.path {
                n = n.and_then(|n| n.children.get(i));
            }
            f(n)
        }
    }

    impl Element for LiveEl {
        fn node(&self) -> Node {
            self.with(|n| {
                let mut n = n.cloned().unwrap_or_default();
                n.children.clear();
                n
            })
        }
        fn children(&self, max: usize) -> Vec<Self> {
            let count = self.with(|n| n.map_or(0, |n| n.children.len()));
            (0..count.min(max))
                .map(|i| {
                    let mut path = self.path.clone();
                    path.push(i);
                    LiveEl {
                        root: self.root.clone(),
                        path,
                    }
                })
                .collect()
        }
        fn parent(&self) -> Option<Self> {
            let mut path = self.path.clone();
            path.pop()?;
            Some(LiveEl {
                root: self.root.clone(),
                path,
            })
        }
        fn same(&self, other: &Self) -> bool {
            self.path == other.path
        }
    }

    /// The editor's model: paragraphs and the caret (paragraph, character offset).
    struct Doc {
        paras: Vec<String>,
        para: usize,
        off: usize,
    }

    impl Doc {
        /// Insert at the caret. `collapse`: runs of line breaks are one paragraph break (how
        /// ProseMirror parses pasted plain text).
        fn insert(&mut self, text: &str, collapse: bool) {
            let mut segs: Vec<&str> = text.split('\n').collect();
            if collapse && segs.len() > 2 {
                let last = segs.pop().unwrap();
                let first = segs.remove(0);
                segs.retain(|s| !s.is_empty());
                segs.insert(0, first);
                segs.push(last);
            }
            let cur = &self.paras[self.para];
            let at = cur
                .char_indices()
                .nth(self.off)
                .map_or(cur.len(), |(b, _)| b);
            let (head, tail) = (cur[..at].to_string(), cur[at..].to_string());
            let mut new: Vec<String> = segs.iter().map(|s| s.to_string()).collect();
            new[0] = format!("{head}{}", new[0]);
            let last = new.len() - 1;
            self.off = new[last].chars().count();
            new[last].push_str(&tail);
            let p = self.para;
            self.paras.splice(p..=p, new);
            self.para = p + last;
        }
    }

    struct WebFake {
        tree: Arc<Mutex<Node>>,
        app: AppInfo,
        /// Path of the editor root.
        editor: Vec<usize>,
        /// AT-SPI style: the root's text is one U+FFFC per paragraph and paragraphs hold their
        /// text. Otherwise AX style: paragraphs are groups of one static text and the root's
        /// value is the paragraphs joined with `\n`.
        hypertext: bool,
        /// Where the focus is: `para` (the caret's paragraph), `leaf` (its text run, AX),
        /// `root`, or a path.
        focus: Mutex<Option<Vec<usize>>>,
        focus_kind: &'static str,
        doc: Mutex<Doc>,
        /// Accessibility insertion: `ok`, `no-breaks` (line breaks lost), `noop`, `fail`.
        a11y: &'static str,
        /// Paste: `ok`, `first` (first paragraph only), `double`, `noop`, `fail`.
        paste: &'static str,
        clipboard: Arc<Mutex<MemoryClipboard>>,
        pasted: Mutex<u32>,
        inserted: Mutex<u32>,
        /// Reads fail from now on (the app went away after an insertion).
        blind: Mutex<bool>,
        /// Report the root's reads as truncated at the read cap.
        truncated: bool,
        /// AX only: the backend exposes `AXEditableAncestor` (as `states.editable` on every
        /// element inside the contenteditable) and `AXHighestEditableAncestor` (as the
        /// `editable_root` attr on the root). Without them (today's macOS backend) only the
        /// root, an `AXTextArea`, is editable.
        ax_editable_signals: bool,
    }

    fn png_clipboard() -> Arc<Mutex<MemoryClipboard>> {
        Arc::new(Mutex::new(MemoryClipboard {
            contents: ClipboardSnapshot {
                items: vec![vec![ClipboardFormat {
                    format: "image/png".into(),
                    data: vec![1, 2, 3],
                }]],
            },
            ..Default::default()
        }))
    }

    #[derive(serde::Deserialize)]
    struct FixtureApp {
        app: AppInfo,
    }

    impl WebFake {
        /// `fixture` is a web tree; `editor_up` is how many levels the editor root is above the
        /// fixture's focus.
        fn new(fixture: &str, editor_up: usize, hypertext: bool, focus_kind: &'static str) -> Self {
            let path = format!(
                "{}/tests/fixtures/trees/{fixture}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let json = std::fs::read_to_string(path).unwrap();
            let snap = Snapshot::from_json(&json).unwrap();
            let app = serde_json::from_str::<FixtureApp>(&json).unwrap().app;
            let editor = snap.focus_path[..snap.focus_path.len() - editor_up].to_vec();
            let paras: Vec<String> = snap
                .node_at(&editor)
                .unwrap()
                .children
                .iter()
                .map(|p| {
                    p.own_text()
                        .or_else(|| p.children.first().and_then(Node::own_text))
                        .unwrap_or("")
                        .to_string()
                })
                .collect();
            let last = paras.len() - 1;
            let off = paras[last].chars().count();
            let f = WebFake {
                tree: Arc::new(Mutex::new(snap.root)),
                app,
                editor,
                hypertext,
                focus: Mutex::new(None),
                focus_kind,
                doc: Mutex::new(Doc {
                    paras,
                    para: last,
                    off,
                }),
                a11y: "ok",
                paste: "ok",
                clipboard: png_clipboard(),
                pasted: Mutex::new(0),
                inserted: Mutex::new(0),
                blind: Mutex::new(false),
                truncated: false,
                ax_editable_signals: true,
            };
            f.sync();
            f
        }

        fn with(mut self, a11y: &'static str, paste: &'static str) -> Self {
            self.a11y = a11y;
            self.paste = paste;
            self
        }

        /// Rebuild the editor's children from the model.
        fn sync(&self) {
            let doc = self.doc.lock().unwrap();
            let mut tree = self.tree.lock().unwrap();
            let mut root = &mut *tree;
            for &i in &self.editor {
                root = &mut root.children[i];
            }
            // AT-SPI: the root is editable (State::Editable); AX: an AXTextArea is.
            root.states.editable = true;
            root.states.focused = self.focus_kind == "root";
            if !self.hypertext && self.ax_editable_signals {
                root.attrs.insert("editable_root".into(), "true".into());
            }
            root.value = (!self.hypertext).then(|| doc.paras.join("\n"));
            root.children = doc
                .paras
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let caret = i == doc.para;
                    let value = (!t.is_empty()).then(|| t.clone());
                    let mut p = if self.hypertext {
                        let mut p = Node::new(Role::Paragraph);
                        p.raw_role = "paragraph".into();
                        p.value = value;
                        p.states.multiline = Some(true);
                        p
                    } else {
                        let mut leaf = Node::new(Role::StaticText);
                        leaf.raw_role = "AXStaticText".into();
                        leaf.value = value;
                        leaf.states.editable = self.ax_editable_signals;
                        leaf.states.focused = caret && self.focus_kind == "leaf";
                        let mut g = Node::new(Role::Group);
                        g.raw_role = "AXGroup".into();
                        g.children = vec![leaf];
                        g
                    };
                    // Firefox reports State::Editable on every element inside a
                    // contenteditable; AX only through `AXEditableAncestor`.
                    p.states.editable = self.hypertext || self.ax_editable_signals;
                    p.states.focused = caret && self.focus_kind == "para";
                    p
                })
                .collect();
        }

        fn focus_path(&self) -> Vec<usize> {
            if let Some(p) = self.focus.lock().unwrap().clone() {
                return p;
            }
            let para = self.doc.lock().unwrap().para;
            let mut p = self.editor.clone();
            match self.focus_kind {
                "para" => p.push(para),
                "leaf" => p.extend([para, 0]),
                _ => {}
            }
            p
        }

        fn value(&self) -> String {
            self.doc.lock().unwrap().paras.join("\n")
        }

        fn clipboard_text(&self) -> String {
            let cb = self.clipboard.lock().unwrap();
            String::from_utf8(cb.contents.items[0][0].data.clone()).unwrap()
        }
    }

    impl Platform for WebFake {
        type Element = LiveEl;
        fn permission(&self) -> Permission {
            Permission::Granted
        }
        fn request_permission(&self) -> Result<()> {
            Ok(())
        }
        fn focus(&self) -> Result<Option<Target<LiveEl>>> {
            Ok(Some(Target {
                app: self.app.clone(),
                element: LiveEl {
                    root: self.tree.clone(),
                    path: self.focus_path(),
                },
            }))
        }
        fn read_field(&self, el: &LiveEl, _max: usize) -> Result<FieldRead> {
            if *self.blind.lock().unwrap() {
                return Err(Error::Other("element is gone".into()));
            }
            let node = el.node();
            let doc = self.doc.lock().unwrap();
            let state = if node.states.secure {
                FieldState::default()
            } else if el.path == self.editor {
                let abs = doc.paras[..doc.para]
                    .iter()
                    .map(|p| p.chars().count() as u32 + 1)
                    .sum::<u32>()
                    + doc.off as u32;
                if self.hypertext {
                    // The caret is inside a paragraph: the root reports its object's offset.
                    FieldState {
                        value: "\u{FFFC}".repeat(doc.paras.len()),
                        selection: Some((doc.para as u32, doc.para as u32)),
                        truncated: self.truncated,
                    }
                } else {
                    FieldState {
                        value: doc.paras.join("\n"),
                        selection: Some((abs, abs)),
                        truncated: self.truncated,
                    }
                }
            } else if el.path.len() > self.editor.len() && el.path.starts_with(&self.editor) {
                let i = el.path[self.editor.len()];
                FieldState {
                    value: doc.paras.get(i).cloned().unwrap_or_default(),
                    selection: (i == doc.para).then_some((doc.off as u32, doc.off as u32)),
                    truncated: false,
                }
            } else {
                FieldState {
                    value: node.value.clone().unwrap_or_default(),
                    selection: None,
                    truncated: false,
                }
            };
            Ok(FieldRead {
                node,
                label: None,
                state,
            })
        }
        fn caret_bounds(&self, el: &LiveEl, s: &FieldState) -> Option<Rect> {
            // The caret's own element and offsets.
            assert_eq!(el.path, self.focus_path());
            let (start, _) = s.selection?;
            Some(Rect {
                x: f64::from(start),
                y: 0.0,
                width: 1.0,
                height: 14.0,
            })
        }
        fn insert_text(&self, el: &LiveEl, text: &str, before: &FieldState) -> Result<()> {
            *self.inserted.lock().unwrap() += 1;
            // Writes address the caret's element in its own offsets.
            assert_eq!(el.path, self.focus_path(), "insertion target");
            assert_eq!(*before, self.read_field(el, 0)?.state, "native offsets");
            match self.a11y {
                "ok" => self.doc.lock().unwrap().insert(text, false),
                "ok-blind" => {
                    self.doc.lock().unwrap().insert(text, false);
                    *self.blind.lock().unwrap() = true;
                }
                "no-breaks" => self
                    .doc
                    .lock()
                    .unwrap()
                    .insert(&text.replace('\n', ""), false),
                "noop" => {}
                _ => return Err(Error::Unsupported("no editable text".into())),
            }
            self.sync();
            Ok(())
        }
        fn clipboard(&self) -> Result<Box<dyn Clipboard>> {
            match self.paste {
                "no-clipboard" => return Err(Error::Unsupported("no clipboard".into())),
                "set-fails" => self.clipboard.lock().unwrap().fail_set = true,
                _ => {}
            }
            Ok(Box::new(SharedCb(self.clipboard.clone())))
        }
        fn paste(&self, el: &LiveEl) -> Result<()> {
            *self.pasted.lock().unwrap() += 1;
            assert_eq!(el.path, self.focus_path(), "paste target");
            let text = self.clipboard_text();
            match self.paste {
                "ok" => self.doc.lock().unwrap().insert(&text, true),
                "first" => {
                    let first = text.split('\n').next().unwrap();
                    self.doc.lock().unwrap().insert(first, true);
                }
                "double" => {
                    let mut d = self.doc.lock().unwrap();
                    d.insert(&text, true);
                    d.insert(&text, true);
                }
                "noop" => {}
                _ => return Err(Error::Unsupported("no paste".into())),
            }
            self.sync();
            Ok(())
        }
    }

    fn prosemirror() -> WebFake {
        WebFake::new("web_prosemirror_atspi", 1, true, "para")
    }

    fn lexical(focus_kind: &'static str) -> WebFake {
        WebFake::new("web_lexical_chat_ax", 0, false, focus_kind)
    }

    fn png_restored(f: &WebFake) -> bool {
        f.clipboard.lock().unwrap().contents.items[0][0].format == "image/png"
    }

    const PARAS: &str = "Thanks!\n\nSee you Thursday.";

    #[test]
    fn focus_in_a_paragraph_reads_the_whole_editor_and_thread() {
        let b = Backend::new(prosemirror(), fast_config());
        let f = b.focused().unwrap().unwrap();
        assert_eq!(f.field.role, "text_area");
        assert_eq!(f.field.label.as_deref(), Some("Message"));
        assert_eq!(f.field.value, "Draft: hello\nsecond line");
        // The caret at the end of the second paragraph, mapped into the whole value.
        assert_eq!(f.field.selection, Some((24, 24)));
        assert!(f.caret.is_some());
        let authors: Vec<_> = f.thread.iter().map(|m| m.author.as_deref()).collect();
        assert_eq!(
            authors,
            [
                Some("Maya Chen"),
                Some("Maya Chen"),
                Some("Sam Ortiz"),
                Some("You")
            ]
        );
        assert_eq!(f.thread[3].from_me, Some(true));
    }

    #[test]
    fn caret_in_a_middle_paragraph_maps_into_the_whole_value() {
        let p = prosemirror();
        {
            let mut d = p.doc.lock().unwrap();
            d.paras = vec![
                "One.".into(),
                String::new(),
                "Three is here.".into(),
                "Four.".into(),
            ];
            d.para = 2;
            d.off = 6;
        }
        p.sync();
        let b = Backend::new(p, fast_config());
        let f = b.focused().unwrap().unwrap();
        // The empty paragraph is an empty line.
        assert_eq!(f.field.value, "One.\n\nThree is here.\nFour.");
        assert_eq!(f.field.selection, Some((12, 12)));
        // Inserting there verifies against the mapped position.
        let out = b.insert("still ").unwrap();
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(out.verified);
        assert_eq!(b.platform().value(), "One.\n\nThree still is here.\nFour.");
    }

    #[test]
    fn multiline_text_is_pasted_first_into_rich_editors() {
        for (name, fake) in [
            ("prosemirror/atspi", prosemirror()),
            ("lexical/ax root", lexical("root")),
            ("lexical/ax leaf", lexical("leaf")),
        ] {
            let b = Backend::new(fake, fast_config());
            let out = b.insert(PARAS).unwrap();
            assert_eq!(
                out,
                InsertOutcome {
                    method: InsertMethod::Clipboard,
                    verified: true
                },
                "{name}"
            );
            let p = b.platform();
            // One paste, one undo step; the blank line became a paragraph boundary.
            assert_eq!(*p.pasted.lock().unwrap(), 1, "{name}");
            assert_eq!(*p.inserted.lock().unwrap(), 0, "{name}");
            assert_eq!(
                p.value(),
                "Draft: hello\nsecond lineThanks!\nSee you Thursday.",
                "{name}"
            );
            assert!(png_restored(p), "{name}");
        }
    }

    #[test]
    fn short_single_line_text_uses_accessibility_in_rich_editors() {
        for (name, fake) in [
            ("prosemirror/atspi", prosemirror()),
            ("lexical/ax root", lexical("root")),
            ("lexical/ax leaf", lexical("leaf")),
        ] {
            let b = Backend::new(fake, fast_config());
            let out = b.insert(" ok").unwrap();
            assert_eq!(out.method, InsertMethod::Accessibility, "{name}");
            assert!(out.verified, "{name}");
            assert_eq!(
                b.platform().value(),
                "Draft: hello\nsecond line ok",
                "{name}"
            );
            assert_eq!(*b.platform().pasted.lock().unwrap(), 0, "{name}");
        }
    }

    #[test]
    fn long_text_is_pasted_first_and_verified() {
        let text = "All good here, the venue is booked and catering is sorted. ".repeat(90);
        assert!(text.chars().count() > 5000);
        let b = Backend::new(prosemirror(), fast_config());
        let out = b.insert(&text).unwrap();
        assert_eq!(out.method, InsertMethod::Clipboard);
        assert!(out.verified);
        assert_eq!(*b.platform().inserted.lock().unwrap(), 0);
        // Below the threshold and single-line: accessibility.
        let mut cfg = fast_config();
        cfg.rich_paste.min_chars = 10_000;
        let b = Backend::new(prosemirror(), cfg);
        assert_eq!(b.insert(&text).unwrap().method, InsertMethod::Accessibility);
    }

    #[test]
    fn rich_paste_falls_back_only_when_nothing_was_sent() {
        // The clipboard is unavailable, or the text could not be put on it: the field is
        // untouched, so accessibility insertion follows.
        for paste in ["no-clipboard", "set-fails"] {
            let b = Backend::new(prosemirror().with("ok", paste), fast_config());
            let out = b.insert(PARAS).unwrap();
            assert_eq!(
                out,
                InsertOutcome {
                    method: InsertMethod::Accessibility,
                    verified: true
                },
                "{paste}"
            );
            let p = b.platform();
            assert_eq!(*p.pasted.lock().unwrap(), 0, "{paste}");
            assert_eq!(*p.inserted.lock().unwrap(), 1, "{paste}");
            assert_eq!(
                p.value(),
                "Draft: hello\nsecond lineThanks!\n\nSee you Thursday."
            );
            assert!(png_restored(p), "{paste}");
        }
        // Neither landed: reported unverified, nothing inserted.
        let b = Backend::new(prosemirror().with("noop", "no-clipboard"), fast_config());
        let out = b.insert(PARAS).unwrap();
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(!out.verified);
        assert_eq!(b.platform().value(), "Draft: hello\nsecond line");
    }

    #[test]
    fn a_sent_paste_is_never_followed_by_a_second_copy() {
        // `noop`: the keystroke went out but the paste never showed up in time (it may land
        // late). `fail`: the keystroke reported an error after it may have been delivered.
        for paste in ["noop", "fail"] {
            let b = Backend::new(prosemirror().with("ok", paste), fast_config());
            let out = b.insert(PARAS).unwrap();
            assert_eq!(
                out,
                InsertOutcome {
                    method: InsertMethod::Clipboard,
                    verified: false
                },
                "{paste}"
            );
            let p = b.platform();
            assert_eq!(*p.pasted.lock().unwrap(), 1, "{paste}");
            assert_eq!(*p.inserted.lock().unwrap(), 0, "{paste}");
            assert!(png_restored(p), "{paste}");
        }
    }

    #[test]
    fn a_failed_read_after_a_successful_insertion_does_not_paste() {
        // Plain path (short text): accessibility first, then the field cannot be read.
        let b = Backend::new(prosemirror().with("ok-blind", "ok"), fast_config());
        let out = b.insert(" ok").unwrap();
        assert_eq!(
            out,
            InsertOutcome {
                method: InsertMethod::Accessibility,
                verified: false
            }
        );
        assert_eq!(*b.platform().pasted.lock().unwrap(), 0);
        assert_eq!(b.platform().value(), "Draft: hello\nsecond line ok");
    }

    #[test]
    fn a_truncated_read_is_never_followed_by_a_second_copy() {
        // The field is read cut at the cap: an unchanged-looking read proves nothing.
        let mut p = prosemirror().with("noop", "ok");
        p.truncated = true;
        let b = Backend::new(p, fast_config());
        let out = b.insert(" ok").unwrap();
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(!out.verified);
        assert_eq!(*b.platform().pasted.lock().unwrap(), 0);
    }

    #[test]
    fn focus_on_a_text_run_needs_editable_ancestor_signals() {
        // The climb from a text run relies on the AX backend reporting `AXEditableAncestor`
        // as `states.editable` (and `AXHighestEditableAncestor` as the `editable_root` attr).
        // Without them the run is not a field: nothing is reported rather than a wrong field.
        let mut p = lexical("leaf");
        p.ax_editable_signals = false;
        p.sync();
        let b = Backend::new(p, fast_config());
        assert!(b.focused().unwrap().is_none());
        assert!(b.insert("x").is_err());
        // With them, the run resolves to the editor root.
        let b = Backend::new(lexical("leaf"), fast_config());
        let f = b.focused().unwrap().unwrap();
        assert_eq!(f.field.value, "Draft: hello\nsecond line");
        assert_eq!(f.field.selection, Some((24, 24)));
        // Focus on the root itself (Chromium's usual case) works either way.
        let mut p = lexical("root");
        p.ax_editable_signals = false;
        p.sync();
        let b = Backend::new(p, fast_config());
        assert_eq!(
            b.focused().unwrap().unwrap().field.value,
            "Draft: hello\nsecond line"
        );
    }

    #[test]
    fn rich_paste_that_changed_the_field_is_not_inserted_again() {
        for paste in ["first", "double"] {
            let b = Backend::new(prosemirror().with("ok", paste), fast_config());
            let out = b.insert(PARAS).unwrap();
            assert_eq!(
                out,
                InsertOutcome {
                    method: InsertMethod::Clipboard,
                    verified: false
                },
                "{paste}"
            );
            assert_eq!(*b.platform().inserted.lock().unwrap(), 0, "{paste}");
            assert_eq!(*b.platform().pasted.lock().unwrap(), 1, "{paste}");
            assert!(png_restored(b.platform()));
        }
    }

    #[test]
    fn rich_paste_off_inserts_through_accessibility_first() {
        let mut cfg = fast_config();
        cfg.rich_paste = RichPaste::OFF;
        // The framework drops the line breaks of an `insertText`: changed but not as expected,
        // so no paste follows.
        let b = Backend::new(prosemirror().with("no-breaks", "ok"), cfg);
        let out = b.insert(PARAS).unwrap();
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(!out.verified);
        assert_eq!(*b.platform().pasted.lock().unwrap(), 0);
    }

    #[test]
    fn web_password_field_is_secure() {
        let p = prosemirror();
        *p.focus.lock().unwrap() = Some(vec![1, 0, 2, 0]);
        let b = Backend::new(p, fast_config());
        let f = b.focused().unwrap().unwrap();
        assert!(f.field.secure);
        assert!(f.field.value.is_empty());
        assert!(f.thread.is_empty());
        assert!(b.insert("x").is_err());
        assert_eq!(*b.platform().pasted.lock().unwrap(), 0);
    }

    #[test]
    fn rich_editor_detection() {
        let root = |editor: Node| {
            let mut web = Node::new(Role::WebArea);
            web.children = vec![editor];
            let mut win = Node::new(Role::Window);
            win.children = vec![web];
            win
        };
        let editable = |role: Role, multiline: Option<bool>| {
            let mut n = Node::new(role);
            n.states.editable = true;
            n.states.multiline = multiline;
            n
        };
        let check = |editor: Node| {
            let tree = root(editor);
            let el = crate::tree::tests::MemEl::new(tree, vec![0, 0]);
            is_rich_editor(&el, &el.node())
        };
        // contenteditable containers.
        assert!(check(editable(Role::Section, Some(true))));
        assert!(check(editable(Role::Group, None)));
        // A text area with paragraphs is a contenteditable; without children a <textarea>.
        let mut ta = editable(Role::TextArea, Some(true));
        assert!(!check(ta.clone()));
        ta.children = vec![Node::new(Role::Paragraph)];
        assert!(check(ta.clone()));
        // An explicit tag wins.
        let mut tagged = ta.clone();
        tagged.attrs.insert("html_tag".into(), "textarea".into());
        assert!(!check(tagged));
        // Known editor classes.
        let mut pm = editable(Role::TextArea, Some(true));
        pm.attrs
            .insert("dom_classes".into(), "ProseMirror tiptap".into());
        assert!(check(pm));
        // Single-line inputs and secure fields never.
        let mut tf = editable(Role::TextField, Some(false));
        tf.children = vec![Node::new(Role::StaticText)];
        assert!(!check(tf));
        assert!(!check(editable(Role::SearchField, None)));
        let mut pw = editable(Role::Section, None);
        pw.states.secure = true;
        assert!(!check(pw));
        // Outside web content (a native document editor).
        let mut native = Node::new(Role::Window);
        native.children = vec![editable(Role::Document, None)];
        let el = crate::tree::tests::MemEl::new(native, vec![0]);
        assert!(!is_rich_editor(&el, &el.node()));
    }
}
