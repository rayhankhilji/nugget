//! Computer use on real AT-SPI (M4): Xvfb, a private session bus with the accessibility bus,
//! and a GTK3 app with common controls (`tests/fixtures/gtk_computer_app.py`), driven through
//! `nugget_a11y::system_computer_with`.
//!
//! Gated like `linux_atspi.rs`: runs only with `NUGGET_A11Y_IT=1`. The outer test starts Xvfb
//! and `dbus-daemon` and re-runs this binary against them.

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
fn atspi_computer_use() {
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
            "atspi_computer_use",
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
    use nugget_a11y::computer::KeyChord;
    use nugget_a11y::{
        A11yConfig, ComputerUse, DenyList, ElementId, KeyInput, ScrollDirection, TreeLimits,
        UiAction, UiActionKind, UiNode, UiTree, WindowSel,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    const TITLE: &str = "Nugget IT - controls";

    /// The GTK app, with the last value it reported for each widget.
    struct App {
        _proc: Proc,
        lines: mpsc::Receiver<String>,
        last: HashMap<String, serde_json::Value>,
    }

    impl App {
        fn start() -> App {
            let script = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/gtk_computer_app.py"
            );
            let py = std::env::var("NUGGET_A11Y_IT_PY").unwrap();
            let mut child = Command::new(py)
                .arg(script)
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
            let app = App {
                _proc: Proc(child),
                lines: rx,
                last: HashMap::new(),
            };
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match app.lines.recv_timeout(left) {
                    Ok(l) if l == "READY" => return app,
                    Ok(_) => {}
                    Err(_) => panic!("timed out waiting for the GTK app"),
                }
            }
        }

        /// The app's own report for `kind`, waiting briefly for it to arrive.
        fn reported(&mut self, kind: &str) -> Option<serde_json::Value> {
            let deadline = Instant::now() + Duration::from_millis(1500);
            loop {
                while let Ok(l) = self.lines.try_recv() {
                    if let Some((k, v)) = l.split_once(' ')
                        && let Ok(v) = serde_json::from_str(v)
                    {
                        self.last.insert(k.to_string(), v);
                    }
                }
                if self.last.contains_key(kind) || Instant::now() > deadline {
                    return self.last.get(kind).cloned();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn snap(cu: &dyn ComputerUse) -> UiTree {
        cu.snapshot(
            &WindowSel::App("gtk_computer_app".into()),
            &TreeLimits::default(),
        )
        .expect("snapshot of the test window")
    }

    fn find<'a>(t: &'a UiTree, raw_role: &str, name: &str) -> &'a UiNode {
        t.root
            .iter()
            .find(|n| n.raw_role == raw_role && n.name.as_deref() == Some(name))
            .unwrap_or_else(|| {
                panic!(
                    "no {raw_role} named {name:?} in\n{}",
                    serde_json::to_string_pretty(&t.root).unwrap()
                )
            })
    }

    fn id_of(t: &UiTree, raw_role: &str, name: &str) -> ElementId {
        find(t, raw_role, name).id.clone()
    }

    fn has_text(t: &UiTree, text: &str) -> bool {
        t.root
            .iter()
            .any(|n| n.name.as_deref() == Some(text) || n.value.as_deref() == Some(text))
    }

    fn keys(s: &str) -> KeyInput {
        KeyInput::Keys(KeyChord::parse_list(s).unwrap())
    }

    pub fn run_all() {
        let a = nugget_a11y::system();
        a.request_permission().expect("enable AT-SPI");
        let mut app = App::start();
        let cu: Arc<dyn ComputerUse> = nugget_a11y::system_computer_with(A11yConfig {
            deny: DenyList::defaults(),
            ..Default::default()
        });

        // Enumeration: the app and its window, focused.
        let deadline = Instant::now() + Duration::from_secs(15);
        let win = loop {
            let ws = cu.windows().expect("windows");
            if let Some(w) = ws
                .iter()
                .find(|w| w.title.as_deref() == Some(TITLE) && w.focused)
            {
                break w.clone();
            }
            assert!(
                Instant::now() < deadline,
                "test window never focused: {ws:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(win.app.name.contains("gtk_computer_app"), "{:?}", win.app);
        assert!(win.app.pid.is_some() && win.app.frontmost);
        assert!(win.bounds.is_some());
        let apps = cu.apps().unwrap();
        assert!(apps[0].name.contains("gtk_computer_app"), "{apps:?}");

        // Snapshot: roles, names, ids, actions, states; stable ids; no secret.
        let t = snap(cu.as_ref());
        eprintln!("snapshot: {} nodes, truncated={}", t.count(), t.truncated);
        let button = find(&t, "push button", "Apply");
        assert!(button.actions.contains(&UiActionKind::Press));
        let check = find(&t, "check box", "Notifications");
        assert_eq!(check.states.checked, Some(false));
        let name = find(&t, "text", "Name");
        assert!(name.actions.contains(&UiActionKind::SetValue));
        assert!(name.states.focused);
        assert_eq!(t.focused.as_ref(), Some(&name.id));
        let pw = find(&t, "password text", "Password");
        assert!(pw.value.is_none() && pw.states.secure);
        assert!(!pw.actions.contains(&UiActionKind::SetValue));
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.contains("hunter2"), "secret leaked into the snapshot");
        assert!(has_text(&t, "Status: idle"));
        let t2 = snap(cu.as_ref());
        assert_eq!(
            id_of(&t2, "push button", "Apply"),
            button.id,
            "ids are stable"
        );

        // Press the button: verified, and the label changed.
        let out = cu.act(&button.id, &UiAction::Press).expect("press");
        eprintln!("press: {out:?}");
        assert!(out.verified, "press not verified: {}", out.detail);
        assert_eq!(app.reported("STATUS"), Some("Status: applied".into()));
        assert!(has_text(&snap(cu.as_ref()), "Status: applied"));

        // Toggle the check box.
        let out = cu.act(&check.id, &UiAction::Press).expect("toggle");
        assert!(out.verified, "toggle not verified: {}", out.detail);
        assert_eq!(out.after.as_ref().unwrap().states.checked, Some(true));
        assert_eq!(app.reported("CHECK"), Some(true.into()));

        // Set the entry's value.
        let out = cu
            .act(
                &name.id,
                &UiAction::SetValue {
                    value: "Ada".into(),
                },
            )
            .expect("set value");
        assert!(out.verified, "set value not verified: {}", out.detail);
        assert_eq!(app.reported("NAME"), Some("Ada".into()));

        // Type into it, then select all and delete with keys.
        let out = cu
            .key_input(Some(&name.id), &KeyInput::Text(" Lovelace ✓".into()))
            .expect("type");
        assert!(out.verified, "typing not verified: {}", out.detail);
        assert_eq!(app.reported("NAME"), Some("Ada Lovelace ✓".into()));
        let out = cu
            .key_input(Some(&name.id), &keys("ctrl+a backspace"))
            .expect("keys");
        assert!(out.verified, "keys not verified: {}", out.detail);
        assert_eq!(app.reported("NAME"), Some("".into()));
        // Synthesised keys for characters (XTest).
        let out = cu.key_input(Some(&name.id), &keys("h i")).expect("keys");
        assert!(out.verified, "{}", out.detail);
        assert_eq!(app.reported("NAME"), Some("hi".into()));

        // The password field is never set or typed into.
        assert!(
            cu.act(&pw.id, &UiAction::SetValue { value: "x".into() })
                .is_err()
        );
        assert!(
            cu.key_input(Some(&pw.id), &KeyInput::Text("x".into()))
                .is_err()
        );

        // Select a list row.
        let t = snap(cu.as_ref());
        let cherry = t
            .root
            .iter()
            .find(|n| {
                n.raw_role == "list item" && n.iter().any(|c| c.name.as_deref() == Some("Cherry"))
            })
            .expect("Cherry row")
            .id
            .clone();
        let out = cu.act(&cherry, &UiAction::Select).expect("select");
        assert!(out.verified, "select not verified: {}", out.detail);
        assert_eq!(app.reported("FRUIT"), Some("Cherry".into()));

        // Combo box: select an item of its menu.
        let blue = t
            .root
            .iter()
            .find(|n| n.raw_role == "menu item" && n.name.as_deref() == Some("Blue"));
        match blue {
            Some(b) => {
                let out = cu.act(&b.id, &UiAction::Select).expect("combo select");
                eprintln!("combo select: {out:?}");
                assert_eq!(app.reported("COMBO"), Some("Blue".into()));
            }
            None => eprintln!("combo items not exposed while the popup is closed; skipped"),
        }

        // Expand the expander.
        let exp = t
            .root
            .iter()
            .find(|n| n.name.as_deref() == Some("Advanced") && n.states.expanded.is_some())
            .expect("expander");
        assert_eq!(exp.states.expanded, Some(false));
        let out = cu
            .act(&exp.id, &UiAction::Expand { expand: true })
            .expect("expand");
        assert!(out.verified, "expand not verified: {}", out.detail);
        assert_eq!(app.reported("EXPANDED"), Some(true.into()));

        // Focus and scroll (best effort: the window is not scrollable).
        let out = cu.act(&button.id, &UiAction::Focus).expect("focus");
        assert!(out.verified, "focus not verified: {}", out.detail);
        let scroll = cu.act(
            &button.id,
            &UiAction::Scroll {
                direction: ScrollDirection::Down,
                amount: 3,
            },
        );
        eprintln!("scroll without a scroll bar: {scroll:?}");

        // Open (activate) the running app; a missing one is not found.
        let out = cu.open_app("gtk_computer_app").expect("open running app");
        assert!(out.verified, "open not verified: {}", out.detail);
        assert!(
            cu.open_app("no-such-app-nugget").is_err()
                || !cu.open_app("no-such-app-nugget").unwrap().verified
        );

        // Screenshot fallback.
        let png = cu.screenshot(&WindowSel::Focused).expect("screenshot");
        let png = png.expect("a PNG on X11");
        assert_eq!(&png[1..4], b"PNG");

        eprintln!("AT-SPI computer use: all scenarios passed");
    }
}
