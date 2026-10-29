//! AT-SPI on a native Wayland GTK 4 app: the focused field, its text and selection, the thread,
//! the tree walk, computer use (set value), and insertion with read-back.
//!
//! Ignored by default: it needs a running Wayland compositor with a session bus. Run it inside
//! the session `tests/fixtures/native/linux/wayland-session.sh` starts (headless sway, D-Bus, the
//! accessibility bus and the portals), as `.github/workflows/real-os.yml` does:
//!
//! ```sh
//! tests/fixtures/native/linux/wayland-session.sh \
//!     cargo test -p nugget-a11y --test linux_wayland -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The fixture is `tests/fixtures/native/linux/gtk4_fixture.py`, started with
//! `GDK_BACKEND=wayland` (never XWayland). GTK 4 has its own AT-SPI implementation, so this also
//! covers the GTK 4 accessibility tree, which differs from GTK 3's ATK bridge
//! (`linux_atspi.rs`). Every scenario runs in one test: they share the compositor's focus.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nugget_a11y::clipboard::{ClipboardFormat, ClipboardSnapshot};
use nugget_a11y::{
    A11yConfig, Accessibility, Backend, DenyList, Focused, InsertMethod, InsertStrategy,
    Permission, TreeLimits, UiAction, UiActionKind, UiTree, WindowSel,
};

const TITLE: &str = "Nugget Wayland - chat";

fn fixture() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("native")
        .join("linux")
        .join("gtk4_fixture.py");
    assert!(p.exists(), "{} is missing", p.display());
    p
}

/// A Python that can import GTK 4 through PyGObject (`NUGGET_GTK4_PYTHON` first).
fn python() -> String {
    let mut cands: Vec<String> = std::env::var("NUGGET_GTK4_PYTHON").into_iter().collect();
    cands.extend(
        ["/usr/bin/python3", "python3", "python3.12", "python3.13"]
            .into_iter()
            .map(String::from),
    );
    for c in cands {
        let ok = Command::new(&c)
            .args([
                "-c",
                "import gi; gi.require_version('Gtk', '4.0'); from gi.repository import Gtk",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return c;
        }
    }
    panic!("no python with PyGObject + GTK 4 (install python3-gi gir1.2-gtk-4.0)");
}

/// The GTK 4 fixture, killed on drop, with a reader thread collecting its stdout.
struct App {
    child: Child,
    lines: mpsc::Receiver<String>,
    backend: String,
    last_composer: Option<String>,
    last_entry: Option<String>,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Let the compositor move focus away before the next scenario looks for it.
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl App {
    fn start(focus: &str) -> App {
        let mut child = Command::new(python())
            .arg(fixture())
            .arg(focus)
            .env("GDK_BACKEND", "wayland")
            .env("GTK_A11Y", "atspi")
            // No GPU on the runner: skip the EGL/Vulkan probes.
            .env("GSK_RENDERER", "cairo")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start the GTK 4 fixture");
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut app = App {
            child,
            lines: rx,
            backend: String::new(),
            last_composer: None,
            last_entry: None,
        };
        app.wait_line(|l| l == "READY", Duration::from_secs(20));
        app
    }

    fn note(&mut self, l: &str) {
        let parse = |s: &str| serde_json::from_str::<String>(s).ok();
        if let Some(v) = l.strip_prefix("COMPOSER ") {
            self.last_composer = parse(v);
        } else if let Some(v) = l.strip_prefix("ENTRY ") {
            self.last_entry = parse(v);
        } else if let Some(v) = l.strip_prefix("BACKEND ") {
            self.backend = v.to_string();
        }
    }

    fn wait_line(&mut self, pred: impl Fn(&str) -> bool, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) => {
                    self.note(&l);
                    if pred(&l) {
                        return;
                    }
                }
                Err(_) => panic!("timed out waiting for the GTK 4 fixture"),
            }
        }
    }

    /// The app's own report of a widget, waiting (bounded) until it equals `want`.
    fn reported(&mut self, which: &str, want: &str) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while let Ok(l) = self.lines.try_recv() {
                self.note(&l);
            }
            let got = match which {
                "COMPOSER" => self.last_composer.clone(),
                _ => self.last_entry.clone(),
            };
            if got.as_deref() == Some(want) || Instant::now() > deadline {
                return got;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn config() -> A11yConfig {
    A11yConfig {
        deny: DenyList::defaults(),
        ..Default::default()
    }
}

/// Poll `focused()` until the focus is in the fixture's field labelled `label`.
fn wait_focus(a: &dyn Accessibility, label: &str) -> Focused {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let last = match a.focused() {
            Ok(Some(f))
                if f.app.window_title.as_deref() == Some(TITLE)
                    && f.field.label.as_deref() == Some(label) =>
            {
                return f;
            }
            Ok(Some(f)) => format!(
                "{:?} / {:?} field {:?}",
                f.app.name, f.app.window_title, f.field.label
            ),
            Ok(None) => "no focused text field".into(),
            Err(e) => format!("error: {e}"),
        };
        assert!(
            Instant::now() < deadline,
            "focus never reached {label:?} in the fixture: {last}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "needs a Wayland compositor and session bus: run inside wayland-session.sh"]
fn wayland_gtk4_atspi() {
    assert!(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        "WAYLAND_DISPLAY is not set: run inside tests/fixtures/native/linux/wayland-session.sh"
    );
    let a = nugget_a11y::system();
    a.request_permission().expect("enable AT-SPI");
    assert_eq!(a.permission(), Permission::Granted);

    read_composer_and_thread();
    insert_accessibility_verified();
    insert_clipboard_restores_formats();
    secure_field();
    entry_placeholder_and_insert();
    focus_follows_the_compositor();
    tree_walk_and_set_value();
    eprintln!("Wayland GTK 4 AT-SPI: all scenarios passed");
}

fn read_composer_and_thread() {
    let app = App::start("composer");
    assert_eq!(
        app.backend, "GdkWaylandDisplay",
        "the fixture must run natively on Wayland, not XWayland"
    );
    let a = nugget_a11y::system_with(config());
    wait_focus(a.as_ref(), "Message composer");
    let t = Instant::now();
    let f = a.focused().unwrap().expect("focused");
    eprintln!("focused() with thread took {:?}: {f:?}", t.elapsed());
    assert!(
        f.app.name.contains("gtk4_fixture") || f.app.name.contains("WaylandFixture"),
        "app name {:?}",
        f.app.name
    );
    assert!(f.app.id.is_some());
    assert_eq!(f.field.label.as_deref(), Some("Message composer"));
    assert_eq!(f.field.value, "Hello ");
    assert_eq!(f.field.selection, Some((6, 6)));
    assert!(!f.field.secure);
    // Wayland has no global coordinates: GTK 4 reports extents relative to the window, or
    // none. The overlay then falls back to its default position, so only record it.
    eprintln!("caret on Wayland: {:?}", f.caret);
    let got: Vec<(Option<&str>, &str, Option<bool>)> = f
        .thread
        .iter()
        .map(|m| (m.author.as_deref(), m.text.as_str(), m.from_me))
        .collect();
    eprintln!("thread: {got:?}");
    let texts: Vec<&str> = got.iter().map(|m| m.1).collect();
    assert_eq!(
        texts,
        vec![
            "Are we still on for lunch tomorrow?",
            "I can bring the slides.",
            "Yes, noon works for me.",
        ],
        "thread"
    );
    drop(app);
}

fn insert_accessibility_verified() {
    let mut app = App::start("composer");
    let a = nugget_a11y::system_with(config());
    wait_focus(a.as_ref(), "Message composer");
    let text = "there — ünïcödé ✓\nsecond line";
    let out = a.insert(text).expect("insert");
    eprintln!("insert into the GTK 4 text view: {out:?}");
    assert_eq!(out.method, InsertMethod::Accessibility);
    assert!(out.verified);
    let expected = format!("Hello {text}");
    assert_eq!(
        app.reported("COMPOSER", &expected).as_deref(),
        Some(expected.as_str())
    );
    let f = a.focused().unwrap().unwrap();
    assert_eq!(f.field.value, expected);
    let n = expected.chars().count() as u32;
    assert_eq!(f.field.selection, Some((n, n)), "caret after the text");
}

fn sorted(s: &ClipboardSnapshot) -> Vec<ClipboardFormat> {
    let mut v: Vec<ClipboardFormat> = s.items.iter().flatten().cloned().collect();
    v.sort_by(|a, b| a.format.cmp(&b.format));
    v
}

/// The clipboard path on Wayland: data control (wlroots) for the clipboard, AT-SPI
/// `EditableText.PasteText` for the paste (no XTest on Wayland).
fn insert_clipboard_restores_formats() {
    let mut app = App::start("composer");
    let seed = ClipboardSnapshot {
        items: vec![vec![
            ClipboardFormat {
                format: "text/html".into(),
                data: b"<b>bold</b> previous".to_vec(),
            },
            ClipboardFormat {
                format: "text/plain;charset=utf-8".into(),
                data: b"previous clipboard text".to_vec(),
            },
        ]],
    };
    let mut cb = nugget_a11y::system_clipboard().expect("a clipboard on Wayland");
    cb.restore(&seed).expect("seed the clipboard");
    let before = cb.snapshot().expect("read the clipboard");
    eprintln!(
        "seeded clipboard: {:?}",
        sorted(&before)
            .iter()
            .map(|f| &f.format)
            .collect::<Vec<_>>()
    );
    for f in sorted(&seed) {
        assert!(
            sorted(&before).contains(&f),
            "seeded format {} does not read back",
            f.format
        );
    }

    let a = nugget_a11y::system_with(A11yConfig {
        insert: InsertStrategy::ClipboardOnly,
        ..config()
    });
    wait_focus(a.as_ref(), "Message composer");
    let out = a.insert("pasted text").expect("clipboard insert");
    eprintln!("clipboard insert on Wayland: {out:?}");
    assert_eq!(out.method, InsertMethod::Clipboard);
    assert!(out.verified, "paste verified by read-back");
    assert_eq!(
        app.reported("COMPOSER", "Hello pasted text").as_deref(),
        Some("Hello pasted text")
    );
    let after = nugget_a11y::system_clipboard().unwrap().snapshot().unwrap();
    for f in sorted(&seed) {
        assert!(
            sorted(&after).contains(&f),
            "format {} not restored: {:?}",
            f.format,
            sorted(&after).iter().map(|f| &f.format).collect::<Vec<_>>()
        );
    }
}

fn secure_field() {
    let _app = App::start("password");
    let a = nugget_a11y::system_with(config());
    // GTK 4 names every text widget's role "text box" (GetRoleName); only the numeric role
    // (PasswordText) marks the password entry.
    let f = wait_focus(a.as_ref(), "Password");
    assert!(
        f.field.secure,
        "the GTK 4 password entry is not secure: {f:?}"
    );
    assert!(f.field.value.is_empty(), "secure value must never be read");
    assert_eq!(f.field.selection, None);
    assert!(f.thread.is_empty());
    assert!(f.caret.is_none());
    assert!(a.insert("x").is_err(), "insert into a secure field");
    let dump = format!("{f:?}");
    assert!(!dump.contains("hunter2"), "secret leaked: {dump}");
}

fn entry_placeholder_and_insert() {
    let mut app = App::start("entry");
    let a = nugget_a11y::system_with(config());
    let f = wait_focus(a.as_ref(), "Search");
    eprintln!("GTK 4 entry: {:?}", f.field);
    assert_eq!(f.field.value, "");
    assert_eq!(f.field.placeholder.as_deref(), Some("Search messages"));
    let out = a.insert("lunch").unwrap();
    assert_eq!(out.method, InsertMethod::Accessibility);
    assert!(out.verified);
    assert_eq!(app.reported("ENTRY", "lunch").as_deref(), Some("lunch"));
}

/// Which app is "focused" on Wayland: AT-SPI's `active` window state, which GTK sets from the
/// compositor's xdg_toplevel `activated` state. A second window that takes focus wins; when it
/// closes, focus goes back to the first.
fn focus_follows_the_compositor() {
    let first = App::start("composer");
    let a = nugget_a11y::system_with(config());
    wait_focus(a.as_ref(), "Message composer");
    let pid1 = first.child.id();
    let second = App::start("entry");
    wait_focus(a.as_ref(), "Search");
    let cu = nugget_a11y::system_computer_with(config());
    let ws = cu.windows().unwrap();
    let active: Vec<_> = ws.iter().filter(|w| w.focused).collect();
    eprintln!("windows: {ws:?}");
    assert_eq!(active.len(), 1, "exactly one active window: {ws:?}");
    assert_eq!(active[0].app.pid, Some(second.child.id()));
    drop(second);
    wait_focus(a.as_ref(), "Message composer");
    let ws = cu.windows().unwrap();
    assert_eq!(ws.iter().find(|w| w.focused).unwrap().app.pid, Some(pid1));
    drop(first);
}

fn tree_walk_and_set_value() {
    let mut app = App::start("entry");
    let a = nugget_a11y::system_with(config());
    let b = Backend::new(nugget_a11y::AtspiPlatform::new(), config());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !matches!(a.focused(), Ok(Some(ref f)) if f.app.window_title.as_deref() == Some(TITLE)) {
        assert!(Instant::now() < deadline, "the fixture never took focus");
        std::thread::sleep(Duration::from_millis(100));
    }

    // The draft's tree walk around the focused field.
    let (_, snap) = b.snapshot().unwrap().expect("snapshot");
    eprintln!("snapshot around the focus: {} nodes", snap.root.count());
    let focused = snap.focused().expect("focused node");
    assert!(focused.states.focused && focused.states.editable);
    let json = snap.to_json();
    assert!(!json.contains("hunter2"), "password value in the snapshot");
    assert!(json.contains("I can bring the slides."), "thread text");

    // Computer use: the window's tree, then set the entry's value.
    let cu = nugget_a11y::system_computer_with(config());
    let win = cu
        .windows()
        .unwrap()
        .into_iter()
        .find(|w| w.title.as_deref() == Some(TITLE) && w.focused)
        .expect("the fixture's window, focused");
    eprintln!("window: {win:?}");
    let t: UiTree = cu
        .snapshot(&WindowSel::Id(win.id.clone()), &TreeLimits::default())
        .expect("window snapshot");
    eprintln!("window tree: {} nodes", t.count());
    let entry = t
        .root
        .iter()
        .find(|n| n.states.focused && n.actions.contains(&UiActionKind::SetValue))
        .unwrap_or_else(|| {
            panic!(
                "no focused settable node:\n{}",
                serde_json::to_string_pretty(&t.root).unwrap()
            )
        })
        .clone();
    assert!(
        !serde_json::to_string(&t).unwrap().contains("hunter2"),
        "secret leaked into the window tree"
    );
    let pw = t.root.iter().find(|n| n.states.secure).expect("password");
    assert!(pw.value.is_none());
    assert!(!pw.actions.contains(&UiActionKind::SetValue));
    let out = cu
        .act(
            &entry.id,
            &UiAction::SetValue {
                value: "Ada ✓".into(),
            },
        )
        .expect("set value");
    assert!(out.verified, "set value not verified: {}", out.detail);
    assert_eq!(app.reported("ENTRY", "Ada ✓").as_deref(), Some("Ada ✓"));
}
