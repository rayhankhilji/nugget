//! Real AT-SPI integration tests on Linux: Xvfb, a private session bus with the accessibility
//! bus, and a small GTK3 app (`tests/fixtures/gtk_test_app.py`).
//!
//! Gated: they run only with `NUGGET_A11Y_IT=1` (see the crate README for the packages). The
//! outer test starts Xvfb and `dbus-daemon`, then re-runs this test binary with `DISPLAY` and
//! `DBUS_SESSION_BUS_ADDRESS` pointing at them (so no process-global environment is mutated),
//! and the inner run drives the app through `nugget_a11y::system_with`.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const INNER: &str = "NUGGET_A11Y_IT_INNER";

fn enabled() -> bool {
    std::env::var("NUGGET_A11Y_IT").is_ok_and(|v| v == "1")
}

/// Kills its child on drop.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn which(cands: &[&str]) -> Option<String> {
    cands.iter().find_map(|c| {
        if c.contains('/') {
            Path::new(c).exists().then(|| c.to_string())
        } else {
            std::env::var_os("PATH").and_then(|p| {
                std::env::split_paths(&p)
                    .map(|d| d.join(c))
                    .find(|f| f.exists())
                    .map(|f| f.to_string_lossy().into_owned())
            })
        }
    })
}

/// A Python that can import GTK 3 through PyGObject.
fn python() -> String {
    let mut cands: Vec<String> = std::env::var("NUGGET_A11Y_IT_PYTHON").into_iter().collect();
    cands.extend(
        [
            "python3",
            "/usr/bin/python3",
            "python3.13",
            "python3.12",
            "python3.11",
            "python3.10",
        ]
        .map(String::from),
    );
    for c in cands {
        let ok = Command::new(&c)
            .args([
                "-c",
                "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return c;
        }
    }
    panic!("no python with PyGObject + GTK 3 found (install python3-gi gir1.2-gtk-3.0)");
}

#[test]
fn atspi_integration() {
    if !enabled() {
        eprintln!("skipped: set NUGGET_A11Y_IT=1 to run the AT-SPI integration tests");
        return;
    }
    if std::env::var_os(INNER).is_some() {
        inner::run_all();
        return;
    }

    // Xvfb on a free display.
    let display = (90..140)
        .find(|n| {
            !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
        })
        .expect("a free X display number");
    let xvfb = which(&["Xvfb"]).expect("Xvfb (package xvfb)");
    let _x = Proc(
        Command::new(xvfb)
            .args([
                format!(":{display}").as_str(),
                "-screen",
                "0",
                "1280x1024x24",
                "-nolisten",
                "tcp",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start Xvfb"),
    );
    let sock = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sock.exists() {
        assert!(Instant::now() < deadline, "Xvfb did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

    // A private session bus; org.a11y.Bus (at-spi-bus-launcher) is D-Bus activated on it.
    let dbus = which(&["dbus-daemon"]).expect("dbus-daemon (package dbus)");
    let mut bus = Proc(
        Command::new(dbus)
            .args(["--session", "--nofork", "--print-address=1"])
            .env("DISPLAY", format!(":{display}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start dbus-daemon"),
    );
    let mut line = String::new();
    BufReader::new(bus.0.stdout.take().unwrap())
        .read_line(&mut line)
        .expect("bus address");
    let address = line.trim().to_string();
    assert!(!address.is_empty(), "dbus-daemon printed no address");

    let exe = std::env::current_exe().unwrap();
    let status = Command::new(exe)
        .args([
            "--exact",
            "atspi_integration",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(INNER, "1")
        .env("DISPLAY", format!(":{display}"))
        .env("DBUS_SESSION_BUS_ADDRESS", &address)
        .env("NUGGET_A11Y_IT_PY", python())
        .env("GTK_MODULES", "gail:atk-bridge")
        .env("NO_AT_BRIDGE", "0")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("AT_SPI_BUS_ADDRESS")
        .status()
        .expect("run the inner integration test");
    // Stop the accessibility bus launched by activation along with the session bus.
    drop(bus);
    assert!(status.success(), "inner AT-SPI integration test failed");
}

mod inner {
    use super::*;
    use nugget_a11y::clipboard::{ClipboardFormat, ClipboardSnapshot};
    use nugget_a11y::{
        A11yConfig, Accessibility, Backend, DenyList, Focused, InsertMethod, InsertStrategy,
        Permission,
    };

    /// The GTK test app, with a reader thread collecting its stdout lines.
    struct App {
        _proc: Proc,
        lines: mpsc::Receiver<String>,
        last_composer: Option<String>,
        last_entry: Option<String>,
    }

    impl App {
        fn start(focus: &str) -> App {
            let script = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/gtk_test_app.py"
            );
            let py = std::env::var("NUGGET_A11Y_IT_PY").unwrap();
            let mut child = Command::new(py)
                .args([script, focus])
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("start the GTK test app");
            let out: ChildStdout = child.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let mut app = App {
                _proc: Proc(child),
                lines: rx,
                last_composer: None,
                last_entry: None,
            };
            app.wait_line(|l| l == "READY", Duration::from_secs(20));
            app
        }

        fn drain(&mut self) {
            while let Ok(l) = self.lines.try_recv() {
                self.note(&l);
            }
        }

        fn note(&mut self, l: &str) {
            let parse = |s: &str| serde_json::from_str::<String>(s).ok();
            if let Some(v) = l.strip_prefix("COMPOSER ") {
                self.last_composer = parse(v);
            } else if let Some(v) = l.strip_prefix("ENTRY ") {
                self.last_entry = parse(v);
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
                    Err(_) => panic!("timed out waiting for the GTK app"),
                }
            }
        }

        /// The composer text as the app itself reports it (not through AT-SPI).
        fn composer(&mut self) -> Option<String> {
            std::thread::sleep(Duration::from_millis(100));
            self.drain();
            self.last_composer.clone()
        }

        fn entry(&mut self) -> Option<String> {
            std::thread::sleep(Duration::from_millis(100));
            self.drain();
            self.last_entry.clone()
        }
    }

    fn config() -> A11yConfig {
        A11yConfig {
            deny: DenyList::defaults(),
            ..Default::default()
        }
    }

    /// Poll `focused()` until the focus is in the test app's window with the expected role.
    fn wait_focus(a: &dyn Accessibility, role: &str) -> Focused {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(Some(f)) = a.focused()
                && f.app.window_title.as_deref() == Some("Nugget IT - chat")
                && f.field.role == role
            {
                return f;
            }
            assert!(
                Instant::now() < deadline,
                "focus never reached a {role} in the test app"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn run_all() {
        permission();
        read_composer_and_thread();
        insert_accessibility_verified();
        insert_clipboard_restores_every_format();
        secure_field();
        entry_placeholder_and_insert();
        deny_list();
        snapshot_records_real_tree();
        eprintln!("AT-SPI integration: all scenarios passed");
    }

    fn permission() {
        let a = nugget_a11y::system();
        a.request_permission().expect("enable AT-SPI");
        assert_eq!(a.permission(), Permission::Granted);
    }

    fn read_composer_and_thread() {
        let _app = App::start("composer");
        let a = nugget_a11y::system_with(config());
        wait_focus(a.as_ref(), "text_area");
        let t = Instant::now();
        let f = a.focused().unwrap().expect("focused");
        eprintln!("focused() with thread took {:?}", t.elapsed());
        assert!(
            f.app.name.contains("gtk_test_app"),
            "app name {:?}",
            f.app.name
        );
        assert!(f.app.id.is_some());
        assert_eq!(f.field.label.as_deref(), Some("Message composer"));
        assert_eq!(f.field.value, "Hello ");
        assert_eq!(f.field.selection, Some((6, 6)));
        assert!(!f.field.secure);
        let caret = f.caret.expect("caret bounds");
        assert!(
            caret.height > 0.0 && caret.x >= 0.0 && caret.y >= 0.0,
            "{caret:?}"
        );
        let got: Vec<(Option<&str>, &str, Option<bool>)> = f
            .thread
            .iter()
            .map(|m| (m.author.as_deref(), m.text.as_str(), m.from_me))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    Some("Alice"),
                    "Are we still on for lunch tomorrow?",
                    Some(false)
                ),
                (Some("Bob"), "I can bring the slides.", Some(false)),
                (Some("You"), "Yes, noon works for me.", Some(true)),
            ],
            "thread"
        );
        assert!(f.recipients.is_empty());
    }

    fn insert_accessibility_verified() {
        let mut app = App::start("composer");
        let a = nugget_a11y::system_with(config());
        wait_focus(a.as_ref(), "text_area");
        let text = "there — ünïcödé ✓\nsecond line";
        let out = a.insert(text).expect("insert");
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(out.verified);
        let expected = format!("Hello {text}");
        assert_eq!(app.composer().as_deref(), Some(expected.as_str()));
        let f = a.focused().unwrap().unwrap();
        assert_eq!(f.field.value, expected);
        // The caret moved past the inserted text.
        let n = expected.chars().count() as u32;
        assert_eq!(f.field.selection, Some((n, n)));
    }

    fn rich_clipboard() -> ClipboardSnapshot {
        ClipboardSnapshot {
            items: vec![vec![
                ClipboardFormat {
                    format: "text/html".into(),
                    data: b"<b>bold</b> previous".to_vec(),
                },
                ClipboardFormat {
                    format: "image/png".into(),
                    data: [0x89, b'P', b'N', b'G', 13, 10, 26, 10]
                        .into_iter()
                        .chain((0..300_000u32).map(|i| (i % 251) as u8))
                        .collect(),
                },
                ClipboardFormat {
                    format: "UTF8_STRING".into(),
                    data: b"previous clipboard text".to_vec(),
                },
                ClipboardFormat {
                    format: "application/x-nugget-test".into(),
                    data: vec![0, 1, 2, 3, 255],
                },
            ]],
        }
    }

    fn sorted(s: &ClipboardSnapshot) -> Vec<ClipboardFormat> {
        let mut v: Vec<ClipboardFormat> = s.items.iter().flatten().cloned().collect();
        v.sort_by(|a, b| a.format.cmp(&b.format));
        v
    }

    fn insert_clipboard_restores_every_format() {
        let mut app = App::start("composer");
        // Seed the clipboard with several formats, one large enough to need INCR.
        let seed = rich_clipboard();
        nugget_a11y::system_clipboard()
            .unwrap()
            .restore(&seed)
            .unwrap();
        let before = nugget_a11y::system_clipboard().unwrap().snapshot().unwrap();
        assert_eq!(
            sorted(&before),
            sorted(&seed),
            "seeded clipboard reads back"
        );

        let a = nugget_a11y::system_with(A11yConfig {
            insert: InsertStrategy::ClipboardOnly,
            ..config()
        });
        wait_focus(a.as_ref(), "text_area");
        let out = a.insert("pasted text").expect("clipboard insert");
        assert_eq!(out.method, InsertMethod::Clipboard);
        assert!(out.verified, "paste verified by read-back");
        assert_eq!(app.composer().as_deref(), Some("Hello pasted text"));

        let after = nugget_a11y::system_clipboard().unwrap().snapshot().unwrap();
        assert_eq!(
            sorted(&after),
            sorted(&seed),
            "every clipboard format restored"
        );
    }

    fn secure_field() {
        let _app = App::start("password");
        let a = nugget_a11y::system_with(config());
        let f = wait_focus(a.as_ref(), "text_field");
        assert!(f.field.secure);
        assert!(f.field.value.is_empty(), "secure value must never be read");
        assert_eq!(f.field.selection, None);
        assert!(f.thread.is_empty());
        assert!(f.caret.is_none());
        assert_eq!(f.field.label.as_deref(), Some("Password"));
        assert!(
            a.insert("x").is_err(),
            "insert into a secure field is refused"
        );
        let dump = format!("{f:?}");
        assert!(!dump.contains("hunter2"), "secret leaked: {dump}");
    }

    fn entry_placeholder_and_insert() {
        let mut app = App::start("entry");
        let a = nugget_a11y::system_with(config());
        let f = wait_focus(a.as_ref(), "search_field");
        assert_eq!(f.field.placeholder.as_deref(), Some("Search messages"));
        assert_eq!(f.field.value, "");
        let out = a.insert("lunch").unwrap();
        assert_eq!(out.method, InsertMethod::Accessibility);
        assert!(out.verified);
        assert_eq!(app.entry().as_deref(), Some("lunch"));
    }

    fn deny_list() {
        let _app = App::start("composer");
        let a = nugget_a11y::system_with(A11yConfig {
            deny: DenyList::with_user_rules(["name~gtk_test_app"]),
            ..config()
        });
        let f = wait_focus(a.as_ref(), "text_area");
        assert!(f.thread.is_empty());
        assert!(f.field.value.is_empty());
        assert!(a.insert("x").is_err());
    }

    fn snapshot_records_real_tree() {
        let _app = App::start("composer");
        let a = nugget_a11y::system_with(config());
        wait_focus(a.as_ref(), "text_area");
        let b = Backend::new(nugget_a11y::AtspiPlatform::new(), config());
        let (_, snap) = b.snapshot().unwrap().expect("snapshot");
        let focused = snap.focused().expect("focused node");
        assert!(focused.states.focused && focused.states.editable);
        assert!(
            !snap.to_json().contains("hunter2"),
            "password value in snapshot"
        );
        if let Ok(dir) = std::env::var("NUGGET_A11Y_RECORD") {
            std::fs::write(Path::new(&dir).join("gtk_recorded.json"), snap.to_json()).unwrap();
        }
    }
}
