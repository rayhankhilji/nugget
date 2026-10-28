//! The platform-neutral half of every backend: [`Backend`] implements [`Accessibility`] on top
//! of a small [`Platform`] trait (find the focused element, read a field, insert, paste,
//! clipboard), and owns the policy: role normalisation, secure fields, the deny list, the
//! thread walk, read-back verification and the clipboard fallback.

use std::time::{Duration, Instant};

use nugget_core::ipc::{AppInfo, FieldInfo};
use nugget_core::{Error, Result};

use crate::clipboard::{self, Clipboard, PasteTiming};
use crate::deny::DenyList;
use crate::role::{FieldHints, FieldRole, Role, field_role};
use crate::sanitize;
use crate::thread::{self, ThreadLimits};
use crate::tree::{Element, Node, Snapshot, WalkLimits, snapshot_around};
use crate::verify::{self, FieldState};
use crate::{Accessibility, Focused, InsertMethod, InsertOutcome, Permission, Rect};

/// How `insert` may put text into the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsertStrategy {
    /// Accessibility insertion, falling back to the clipboard when the field did not change.
    #[default]
    Auto,
    AccessibilityOnly,
    ClipboardOnly,
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

    fn read_back(&self, el: &P::Element) -> Result<FieldState> {
        Ok(self
            .platform
            .read_field(el, self.config.max_value_bytes)?
            .state)
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
        let el = target.element;
        let read = self.platform.read_field(&el, self.config.max_value_bytes)?;
        if read.node.states.secure {
            return Err(Error::Invalid(
                "refusing to insert into a secure field".into(),
            ));
        }
        if Self::field_role_of(&read).is_none() {
            return Err(Error::NotFound(
                "focus is not in an editable text field".into(),
            ));
        }
        let before = read.state;

        if self.config.insert != InsertStrategy::ClipboardOnly {
            match self.platform.insert_text(&el, &text, &before) {
                Ok(()) => {
                    let deadline = Instant::now() + self.config.verify_timeout;
                    let mut after = self.read_back(&el)?;
                    loop {
                        if verify::verify_insert(&before, &text, &after) {
                            return Ok(InsertOutcome {
                                method: InsertMethod::Accessibility,
                                verified: true,
                            });
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                        after = self.read_back(&el)?;
                    }
                    // Something landed but not what we expected: never insert a second copy.
                    if verify::changed(&before, &after)
                        || self.config.insert == InsertStrategy::AccessibilityOnly
                    {
                        return Ok(InsertOutcome {
                            method: InsertMethod::Accessibility,
                            verified: false,
                        });
                    }
                    tracing::debug!("accessibility insertion left the field unchanged; pasting");
                }
                Err(e) if self.config.insert == InsertStrategy::AccessibilityOnly => return Err(e),
                Err(e) => tracing::debug!(error = %e, "accessibility insertion failed; pasting"),
            }
        }

        let mut cb = self.platform.clipboard()?;
        let r = clipboard::paste_with_restore(
            cb.as_mut(),
            &text,
            &mut || self.platform.paste(&el),
            &mut || Ok(verify::verify_insert(&before, &text, &self.read_back(&el)?)),
            self.config.paste,
        )?;
        Ok(InsertOutcome {
            method: InsertMethod::Clipboard,
            verified: r.verified,
        })
    }
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
        let el = target.element;
        let read = self.platform.read_field(&el, self.config.max_value_bytes)?;
        let Some(role) = Self::field_role_of(&read) else {
            return Ok(None);
        };
        let secure = read.node.states.secure;
        let denied = self.config.deny.is_denied(&app);
        let caret = if secure {
            None
        } else {
            self.platform.caret_bounds(&el, &read.state)
        };
        let label = read
            .label
            .clone()
            .or_else(|| read.node.name.clone())
            .and_then(|l| sanitize::clean_opt(Some(&l), sanitize::MAX_SHORT));
        let field = FieldInfo {
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
        let ctx = if secure || denied {
            thread::ThreadContext::default()
        } else {
            let snap = snapshot_around(&el, &self.config.walk);
            thread::extract(&snap, &app, &read.state.value, &self.config.thread)
        };
        Ok(Some(Focused {
            app,
            field,
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
}
