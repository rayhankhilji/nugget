//! Wayland integration tests: session detection under a real compositor, the chord path through
//! the XDG desktop portal's GlobalShortcuts interface (or its documented "portal unavailable"
//! error), and Double Tap's trigger-to-insert path into a native Wayland GTK 4 text field.
//!
//! Ignored by default. Run them inside the session `tests/fixtures/native/linux/wayland-session.sh`
//! starts, one at a time, as `.github/workflows/real-os.yml` does:
//!
//! ```sh
//! NUGGET_WAYLAND_COMPOSITOR=kwin tests/fixtures/native/linux/wayland-session.sh \
//!     cargo test -p nugget-hotkey --test wayland_it -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! What runs depends on the portal the session offers:
//! - **GlobalShortcuts available** (KDE: kwin + xdg-desktop-portal-kde): a real portal session
//!   and bind request. When the desktop binds the shortcut, it is activated through
//!   kglobalaccel's `invokeShortcut` (synthetic key presses cannot reach a Wayland compositor's
//!   shortcut handling from a test). Where it cannot get further, the test stops at the
//!   documented point: xdg-desktop-portal 1.19+ refuses an unscoped host process ("An app id is
//!   required"); Plasma 5.27 answers the bind by leaving the keys to the user in System
//!   Settings (the manual step); GNOME 48+ and Plasma 6 ask in a dialog.
//! - **No GlobalShortcuts** (sway: xdg-desktop-portal-wlr; GNOME before 48): `start` must fail fast
//!   with the documented `Unsupported` error, and Double Tap is driven by a direct call (the
//!   shell's fallback when no trigger can be registered is the tray / app button).

#![cfg(target_os = "linux")]

mod support;

use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nugget_core::Error;
use nugget_hotkey::{
    KillSwitch, Modifier, Session, Trigger, capabilities, check, check_kill_switch,
    detect_linux_session, recommended_trigger, session, start, start_kill_switch,
};
use support::{Mode, Pipeline};

const CHORD: &str = "Ctrl+Alt+Space";

fn require_wayland() {
    assert!(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        "WAYLAND_DISPLAY is not set: run inside tests/fixtures/native/linux/wayland-session.sh"
    );
}

fn gdbus(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gdbus")
        .args(args)
        .output()
        .map_err(|e| format!("gdbus: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// Whether the portal front end exposes GlobalShortcuts (a backend implements it).
fn portal_has_global_shortcuts() -> bool {
    gdbus(&[
        "introspect",
        "--session",
        "--dest",
        "org.freedesktop.portal.Desktop",
        "--object-path",
        "/org/freedesktop/portal/desktop",
    ])
    .is_ok_and(|s| s.contains("org.freedesktop.portal.GlobalShortcuts"))
}

/// Quoted strings and object paths in a gdbus reply.
fn tokens(reply: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in reply.split(['\'', '"']).skip(1).step_by(2) {
        out.push(part.to_string());
    }
    out
}

/// The kglobalaccel component and action the portal bound for this process (ids
/// `nugget-<pid>-<n>`, from `wayland.rs`).
fn find_kglobalaccel_shortcut() -> Option<(String, String)> {
    let prefix = format!("nugget-{}-", std::process::id());
    let comps = gdbus(&[
        "call",
        "--session",
        "--dest",
        "org.kde.kglobalaccel",
        "--object-path",
        "/kglobalaccel",
        "--method",
        "org.kde.KGlobalAccel.allComponents",
    ])
    .ok()?;
    for comp in tokens(&comps).into_iter().filter(|c| c.starts_with('/')) {
        let Ok(names) = gdbus(&[
            "call",
            "--session",
            "--dest",
            "org.kde.kglobalaccel",
            "--object-path",
            &comp,
            "--method",
            "org.kde.kglobalaccel.Component.shortcutNames",
        ]) else {
            continue;
        };
        if let Some(n) = tokens(&names).into_iter().find(|n| n.starts_with(&prefix)) {
            return Some((comp, n));
        }
    }
    None
}

fn dump_kglobalaccel() -> String {
    let comps = gdbus(&[
        "call",
        "--session",
        "--dest",
        "org.kde.kglobalaccel",
        "--object-path",
        "/kglobalaccel",
        "--method",
        "org.kde.KGlobalAccel.allComponents",
    ])
    .unwrap_or_else(|e| e);
    let mut s = format!("components: {comps}");
    for comp in tokens(&comps).into_iter().filter(|c| c.starts_with('/')) {
        let names = gdbus(&[
            "call",
            "--session",
            "--dest",
            "org.kde.kglobalaccel",
            "--object-path",
            &comp,
            "--method",
            "org.kde.kglobalaccel.Component.shortcutNames",
        ])
        .unwrap_or_else(|e| e);
        s.push_str(&format!("\n  {comp}: {}", names.trim()));
    }
    s
}

/// Activate the bound shortcut the way the compositor does when its keys are pressed.
fn invoke(comp: &str, name: &str) {
    gdbus(&[
        "call",
        "--session",
        "--dest",
        "org.kde.kglobalaccel",
        "--object-path",
        comp,
        "--method",
        "org.kde.kglobalaccel.Component.invokeShortcut",
        name,
    ])
    .unwrap_or_else(|e| panic!("invokeShortcut {name}: {e}"));
}

fn channel_callback() -> (Box<dyn Fn() + Send + Sync>, mpsc::Receiver<Instant>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    (
        Box::new(move || {
            let _ = tx.lock().unwrap().send(Instant::now());
        }),
        rx,
    )
}

/// The same callback twice: one for a listener, one to call directly.
type Callback = Box<dyn Fn() + Send + Sync>;
fn split_callback(cb: Callback) -> (Callback, impl Fn()) {
    let cb = std::sync::Arc::new(cb);
    let direct = cb.clone();
    (Box::new(move || cb()), move || direct())
}

fn fired(rx: &mpsc::Receiver<Instant>) -> usize {
    let mut n = 0;
    while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
        n += 1;
    }
    n
}

fn summary(line: &str) {
    println!("{line}");
    if let Ok(path) = std::env::var("NUGGET_HOTKEY_SUMMARY") {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn compositor() -> String {
    std::env::var("NUGGET_WAYLAND_COMPOSITOR").unwrap_or_else(|_| "wayland".into())
}

#[test]
#[ignore = "needs a Wayland session: run inside wayland-session.sh"]
fn wayland_session_and_capabilities() {
    require_wayland();
    assert_eq!(session(), Session::Wayland);
    let caps = capabilities();
    assert_eq!(caps.session, Session::Wayland);
    assert!(
        caps.chord && !caps.double_tap && !caps.escape_repeat,
        "{caps:?}"
    );
    assert_eq!(recommended_trigger(), Trigger::Chord(CHORD.into()));

    // Double taps and the Escape kill switch are refused up front, with the documented hint.
    let tap = Trigger::DoubleTap {
        modifier: Modifier::RightControl,
        window_ms: 350,
    };
    for err in [
        check(&tap).unwrap_err(),
        start(tap.clone(), Box::new(|| {})).err().unwrap(),
    ] {
        let Error::Unsupported(msg) = &err else {
            panic!("expected Unsupported, got {err:?}")
        };
        assert!(
            msg.contains("Wayland") && msg.contains("Ctrl+Alt+Space"),
            "hint: {msg}"
        );
    }
    assert!(matches!(
        check_kill_switch(&KillSwitch::default()),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        start_kill_switch(KillSwitch::default(), Box::new(|| {})),
        Err(Error::Unsupported(_))
    ));
    assert!(check(&Trigger::Chord(CHORD.into())).is_ok());

    // Detection with this session's real environment, and the NUGGET_HOTKEY_SESSION override.
    let var = |k: &str| std::env::var(k).ok();
    let (xdg, wl, disp) = (
        var("XDG_SESSION_TYPE"),
        var("WAYLAND_DISPLAY"),
        var("DISPLAY"),
    );
    let d = |forced: Option<&str>| {
        detect_linux_session(forced, xdg.as_deref(), wl.as_deref(), disp.as_deref())
    };
    assert_eq!(d(None), Session::Wayland);
    assert_eq!(d(Some("x11")), Session::X11);
    assert_eq!(d(Some("wayland")), Session::Wayland);
    assert_eq!(d(Some("bogus")), Session::Wayland);

    // The override in a real process: this session has no X server, so X11 listeners fail.
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "forced_x11_inner",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("NUGGET_HOTKEY_SESSION", "x11")
        .env("NUGGET_WAYLAND_IT_INNER", "1")
        .output()
        .expect("re-run this test binary");
    let text = String::from_utf8_lossy(&out.stdout);
    println!("{text}");
    assert!(out.status.success(), "forced x11 run failed: {text}");
    assert!(
        text.contains("1 passed"),
        "the inner test did not run: {text}"
    );
}

/// Run by `wayland_session_and_capabilities` with `NUGGET_HOTKEY_SESSION=x11`.
#[test]
#[ignore = "run by wayland_session_and_capabilities"]
fn forced_x11_inner() {
    if std::env::var_os("NUGGET_WAYLAND_IT_INNER").is_none() {
        return;
    }
    assert_eq!(session(), Session::X11);
    assert!(capabilities().double_tap);
    assert!(capabilities().escape_repeat);
    // The X11 backend is chosen, and with no X server here its double-tap listener (its own X
    // connection) fails to start instead of silently listening to nothing.
    let r = start(
        Trigger::DoubleTap {
            modifier: Modifier::RightControl,
            window_ms: 350,
        },
        Box::new(|| {}),
    );
    println!(
        "forced x11 double tap without an X server: {:?}",
        r.as_ref().err()
    );
    assert!(
        r.is_err(),
        "an X11 listener cannot start without an X server"
    );
}

#[test]
#[ignore = "needs a Wayland session with the XDG portals: run inside wayland-session.sh"]
fn wayland_portal_chord() {
    require_wayland();
    let has = portal_has_global_shortcuts();
    if std::env::var_os("NUGGET_WAYLAND_EXPECT_GLOBAL_SHORTCUTS").is_some_and(|v| v == "1") {
        assert!(has, "this session's portal should offer GlobalShortcuts");
    }
    println!("compositor={} portal GlobalShortcuts={has}", compositor());
    let (cb, rx) = channel_callback();
    let t = Instant::now();
    let r = start(Trigger::Chord(CHORD.into()), cb);
    let start_ms = support::ms(t.elapsed());

    if !has {
        // The documented fallback: no backend implements GlobalShortcuts.
        let err = r
            .err()
            .expect("start must fail without a GlobalShortcuts backend");
        println!("portal unavailable after {start_ms:.1} ms: {err}");
        let Error::Unsupported(msg) = &err else {
            panic!("expected Unsupported, got {err:?}")
        };
        assert!(
            msg.contains("GlobalShortcuts") && msg.contains("GNOME 48+"),
            "message: {msg}"
        );
        assert!(start_ms < 6_000.0, "the portal check took {start_ms:.0} ms");
        summary(&format!(
            "| {} | chord {CHORD} | portal without GlobalShortcuts: Unsupported in {start_ms:.1} ms | - |",
            compositor()
        ));
        return;
    }

    // xdg-desktop-portal 1.19+ (Ubuntu 26.04) gives GlobalShortcuts only to an app it can name:
    // a sandboxed app, an app in a systemd `app-<id>-*.scope`, or one whose host registration
    // matches an installed desktop file it resolves. A headless test process launched from a
    // shell has none of these, so the portal refuses the session; that refusal, with its reason,
    // is where the automated path stops (an installed, desktop-launched Nugget gets its scope).
    let listener = match r {
        Ok(l) => l,
        Err(Error::Unsupported(msg)) if msg.contains("app id") => {
            println!("STOP: portal refused the session after {start_ms:.1} ms: {msg}");
            assert!(start_ms < 6_000.0, "the portal took {start_ms:.0} ms");
            assert!(
                msg.contains("registering the app id"),
                "reason missing: {msg}"
            );
            summary(&format!(
                "| {} | chord {CHORD} via GlobalShortcuts | refused in {start_ms:.1} ms: the \
                 portal needs an app id (unscoped host process) | - |",
                compositor()
            ));
            return;
        }
        Err(e) => panic!("portal session: {e}"),
    };
    println!("portal session created in {start_ms:.1} ms");
    assert!(
        start_ms < 6_000.0,
        "creating the session took {start_ms:.0} ms"
    );
    // The bind runs after start() returns. Plasma 5.27 answers it at once without binding
    // anything: it opens System Settings' shortcuts page (systemsettings://kcm_keys/<session>)
    // for the user to assign the keys. That is the manual step; nothing can be activated
    // before it, so the test stops there (the listener stays subscribed meanwhile).
    let Some((comp, name)) = support::wait_for(Duration::from_secs(5), find_kglobalaccel_shortcut)
    else {
        println!(
            "MANUAL STEP: the desktop did not bind the shortcut; the user assigns it in the \
             desktop's shortcut settings (Plasma 5.27: System Settings > Shortcuts). \
             kglobalaccel:\n{}",
            dump_kglobalaccel()
        );
        assert_eq!(
            fired(&rx),
            0,
            "nothing fires before the user binds the keys"
        );
        let t = Instant::now();
        drop(listener);
        let close_ms = support::ms(t.elapsed());
        assert!(
            close_ms < 3_000.0,
            "closing the session took {close_ms:.0} ms"
        );
        summary(&format!(
            "| {} | chord {CHORD} via GlobalShortcuts | session created in {start_ms:.1} ms; bind \
             answered, keys left to the user (manual step) | - |",
            compositor()
        ));
        return;
    };
    let bound_ms = support::ms(t.elapsed());
    println!("bound as {name} in {comp} after {bound_ms:.1} ms");

    let pressed = Instant::now();
    invoke(&comp, &name);
    let at = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("the portal's Activated signal did not fire within 3 s");
    let fire_ms = support::ms(at.saturating_duration_since(pressed));
    println!(
        "DOUBLETAP-HOTKEY os=linux-wayland trigger=portal({CHORD}) activate_to_fire_ms={fire_ms:.2}"
    );
    assert_eq!(fired(&rx), 0, "one activation fires once");
    invoke(&comp, &name);
    assert_eq!(fired(&rx), 1, "a second activation fires again");
    summary(&format!(
        "| {} | chord {CHORD} via GlobalShortcuts | session {start_ms:.1} ms, bound {bound_ms:.1} ms | activate to fire {fire_ms:.2} ms |",
        compositor()
    ));

    // Dropping closes the portal session: activations no longer reach us.
    drop(listener);
    std::thread::sleep(Duration::from_millis(300));
    let _ = gdbus(&[
        "call",
        "--session",
        "--dest",
        "org.kde.kglobalaccel",
        "--object-path",
        &comp,
        "--method",
        "org.kde.kglobalaccel.Component.invokeShortcut",
        &name,
    ]);
    assert_eq!(fired(&rx), 0, "fired after drop");
}

// --- Double Tap into a native Wayland GTK 4 field -------------------------------------------

/// The GTK 4 fixture from `tests/fixtures/native/linux/`, killed on drop.
struct Fixture {
    child: std::process::Child,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Fixture {
    fn start(focus: &str) -> Fixture {
        use std::io::{BufRead, BufReader};
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("native")
            .join("linux")
            .join("gtk4_fixture.py");
        let python =
            std::env::var("NUGGET_GTK4_PYTHON").unwrap_or_else(|_| "/usr/bin/python3".into());
        let mut child = Command::new(python)
            .arg(&script)
            .arg(focus)
            .env("GDK_BACKEND", "wayland")
            .env("GTK_A11Y", "atspi")
            .env("GSK_RENDERER", "cairo")
            .env_remove("DISPLAY")
            .stdout(std::process::Stdio::piped())
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
        let fixture = Fixture { child };
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut backend = String::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = rx
                .recv_timeout(left)
                .expect("the GTK 4 fixture did not start");
            if let Some(b) = line.strip_prefix("BACKEND ") {
                backend = b.to_string();
            }
            if line == "READY" {
                break;
            }
        }
        assert_eq!(
            backend, "GdkWaylandDisplay",
            "the fixture must be native Wayland"
        );
        // Keep draining so the app never blocks on a full pipe.
        std::thread::spawn(move || while rx.recv().is_ok() {});
        fixture
    }
}

fn is_fixture(app: &nugget_core::ipc::AppInfo) -> bool {
    app.window_title.as_deref() == Some("Nugget Wayland - chat")
}

#[test]
#[ignore = "opens a GTK 4 window in a Wayland session: run inside wayland-session.sh"]
fn wayland_doubletap_into_gtk4() {
    require_wayland();
    let a11y = nugget_a11y::system();
    a11y.request_permission().expect("enable AT-SPI");
    let has_portal = portal_has_global_shortcuts();
    let backend = nugget_a11y::Backend::new(
        nugget_a11y::AtspiPlatform::new(),
        nugget_a11y::A11yConfig::default(),
    );
    let os = format!("linux-wayland-{}", compositor());

    for (mode, chunks) in [
        (Mode::Shell, support::SHELL_CHUNKS),
        (Mode::Streamed, support::STREAMED_CHUNKS),
    ] {
        let _app = Fixture::start("composer");
        let focused = support::wait_for_field(a11y.as_ref(), Duration::from_secs(15), is_fixture)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(focused.field.label.as_deref(), Some("Message composer"));
        if mode == Mode::Shell {
            support::read_breakdown(&backend, &os);
        }
        let pipeline = Pipeline::new(chunks);
        let (cb, rx) = channel_callback();
        // The real trigger when the desktop binds the portal chord (activated through
        // kglobalaccel); otherwise the listener is still started, as the shell does, and the
        // same path is driven by a direct call, as the tray/app button does.
        let (cb, direct) = split_callback(cb);
        // A portal that refuses the session (no app id, see wayland_portal_chord) leaves the
        // direct trigger too.
        let listener = has_portal
            .then(|| start(Trigger::Chord(CHORD.into()), cb))
            .and_then(|r| {
                r.map_err(|e| println!("portal chord unavailable: {e}"))
                    .ok()
            });
        let bound = listener
            .as_ref()
            .and_then(|_| support::wait_for(Duration::from_secs(5), find_kglobalaccel_shortcut));
        let trigger = if bound.is_some() { "portal" } else { "direct" };
        let report = support::trigger_and_draft(&backend, &pipeline, mode, rx, |_| match &bound {
            Some((comp, name)) => invoke(comp, name),
            None => direct(),
        })
        .unwrap_or_else(|e| panic!("{} mode: {e}", mode.as_str()));
        drop(listener);
        report.publish(&os);
        assert!(is_fixture(&report.app), "{:?}", report.app);
        println!(
            "DOUBLETAP-WAYLAND compositor={} trigger={} mode={} insert_methods={:?} \
             first_token_ms={:.1}",
            compositor(),
            trigger,
            mode.as_str(),
            report.methods,
            support::ms(report.first_token)
        );
    }
}
