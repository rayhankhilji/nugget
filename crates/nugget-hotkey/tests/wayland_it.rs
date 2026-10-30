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
//! - **GlobalShortcuts available** (kwin: xdg-desktop-portal-kde, Plasma 6; gnome:
//!   xdg-desktop-portal-gnome, GNOME 48+): nugget-hotkey registers the app id `dev.nugget.Nugget`
//!   (the session provides its desktop file, or the installed package does), the portal session
//!   is created and the shortcut bound. A new shortcut makes the desktop ask in a dialog, which
//!   the test accepts over AT-SPI (`accept-shortcut-dialog.py`), as a user would. The bound
//!   shortcut is then fired by a real key press, Ctrl+Alt+Space on the compositor's virtual
//!   keyboard (`virtual-keyboard.py`: KWin fake input, Mutter RemoteDesktop), which goes through
//!   the desktop's own shortcut handling to the portal's Activated signal. Without a virtual
//!   keyboard, kglobalaccel's `invokeShortcut` is used and labelled (it is not a key press).
//!   `NUGGET_WAYLAND_EXPECT_SHORTCUT_FIRES=1` makes every stop short of a key press firing the
//!   shortcut a failure. Without it the test stops at the documented points: the portal refuses
//!   an app it cannot name ("An app id is required"); Plasma 5.27 leaves the keys to the user in
//!   System Settings (the manual step).
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
    APP_ID, DOUBLE_TAP_SHORTCUT_ID, KillSwitch, Listener, Modifier, Session, ShortcutState,
    Trigger, capabilities, check, check_kill_switch, detect_linux_session, recommended_trigger,
    session, start, start_kill_switch,
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

/// The kglobalaccel component and action the KDE portal bound for Double Tap: component
/// `dev.nugget.Nugget` (the app id, object path `/component/dev_nugget_Nugget`), action
/// [`DOUBLE_TAP_SHORTCUT_ID`].
fn find_kglobalaccel_shortcut() -> Option<(String, String)> {
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
    let want = format!("/component/{}", APP_ID.replace(['.', '-'], "_"));
    for comp in tokens(&comps).into_iter().filter(|c| *c == want) {
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
        if tokens(&names).iter().any(|n| n == DOUBLE_TAP_SHORTCUT_ID) {
            return Some((comp, DOUBLE_TAP_SHORTCUT_ID.to_string()));
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

/// Press keys on the compositor's virtual keyboard (`virtual-keyboard.py` from the session:
/// KWin fake input, Mutter RemoteDesktop), when this session has one.
fn virtual_keyboard() -> Option<String> {
    let script = std::env::var("NUGGET_WAYLAND_KEYS").ok()?;
    matches!(compositor().as_str(), "kwin" | "gnome").then_some(script)
}

fn press(script: &str, chord: &str) {
    let out = Command::new("python3")
        .arg(script)
        .arg(chord)
        .output()
        .expect("run virtual-keyboard.py");
    print!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        out.status.success(),
        "virtual keyboard: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The keys the portal binds, as virtual-keyboard.py spells them.
const KEYS: &str = "ctrl+alt+space";

/// Whether this run must get as far as a real key press firing the shortcut
/// (`NUGGET_WAYLAND_EXPECT_SHORTCUT_FIRES=1`: the kwin and gnome jobs).
fn expect_fires() -> bool {
    std::env::var_os("NUGGET_WAYLAND_EXPECT_SHORTCUT_FIRES").is_some_and(|v| v == "1")
}

/// How the listener's shortcut got bound (or did not).
struct Bind {
    state: ShortcutState,
    consent: String,
    bound_ms: f64,
}

/// Wait for the portal bind to settle, accepting the desktop's consent dialog the way a user
/// does (`accept-shortcut-dialog.py`, AT-SPI) when the listener reports it waiting for one.
fn settle(listener: &dyn Listener, t: Instant) -> Bind {
    let mut consent = "none asked (returning shortcut)".to_string();
    let mut asked = false;
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let state = listener.state();
        match &state {
            ShortcutState::AwaitingConsent if !asked => {
                asked = true;
                consent = accept_dialog();
            }
            ShortcutState::Active { .. }
            | ShortcutState::NotAssigned
            | ShortcutState::Failed(_) => {
                return Bind {
                    state,
                    consent,
                    bound_ms: support::ms(t.elapsed()),
                };
            }
            _ => {}
        }
        if Instant::now() > deadline {
            return Bind {
                state,
                consent: format!("{consent}; still unsettled after 40 s"),
                bound_ms: support::ms(t.elapsed()),
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn accept_dialog() -> String {
    let Ok(script) = std::env::var("NUGGET_WAYLAND_ACCEPT_DIALOG") else {
        return "dialog left open (no accept-shortcut-dialog.py)".into();
    };
    let out = Command::new("python3")
        .arg(script)
        .args(["--timeout", "25"])
        .output()
        .expect("run accept-shortcut-dialog.py");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    println!("{text}");
    match out.status.code() {
        Some(0) => "dialog accepted over AT-SPI".into(),
        Some(3) => "no dialog appeared".into(),
        c => format!("dialog not accepted (exit {c:?}): {}", text.trim()),
    }
}

/// Press the chord on the virtual keyboard until `rx` sees a fire (3 attempts); the ms from the
/// last press to the fire.
fn press_until_fired(script: &str, rx: &mpsc::Receiver<Instant>) -> Option<f64> {
    for attempt in 1..=3 {
        let pressed = Instant::now();
        press(script, KEYS);
        if let Ok(at) = rx.recv_timeout(Duration::from_secs(3)) {
            println!("fired on virtual key press attempt {attempt}");
            return Some(support::ms(at.saturating_duration_since(pressed)));
        }
        println!("virtual key press attempt {attempt}: no fire");
    }
    None
}

/// Activate the bound shortcut through kglobalaccel (not a key press: the fallback when the
/// session has no virtual keyboard).
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

fn keyboard_kind() -> &'static str {
    match compositor().as_str() {
        "kwin" => "KWin org_kde_kwin_fake_input",
        "gnome" => "Mutter RemoteDesktop",
        _ => "none",
    }
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
            "| {} | - | portal without GlobalShortcuts: Unsupported in {start_ms:.1} ms | - | - | - | no (tray / app button fallback) |",
            compositor()
        ));
        return;
    }

    // xdg-desktop-portal 1.19+ gives GlobalShortcuts to an unsandboxed app only when it can
    // name it: nugget-hotkey registers APP_ID, which the portal accepts when a desktop file of
    // that name with an existing Exec program is installed (the .deb's, or the session's test
    // one). Without it the session is refused, with the reason.
    let listener = match r {
        Ok(l) => l,
        Err(Error::Unsupported(msg)) if msg.contains("app id") => {
            println!("STOP: portal refused the session after {start_ms:.1} ms: {msg}");
            assert!(
                !expect_fires(),
                "the portal refused the session although {APP_ID}.desktop is installed: {msg}"
            );
            assert!(msg.contains(APP_ID), "reason missing: {msg}");
            summary(&format!(
                "| {} | registration refused | refused in {start_ms:.1} ms (no {APP_ID}.desktop) | - | - | - | no |",
                compositor()
            ));
            return;
        }
        Err(e) => panic!("portal session: {e}"),
    };
    println!("portal session created for {APP_ID} in {start_ms:.1} ms");
    assert!(
        start_ms < 6_000.0,
        "creating the session took {start_ms:.0} ms"
    );

    // The bind runs after start() returns; Plasma 6 and GNOME ask the user about a new
    // shortcut in a dialog (accepted here as a user would), and bind a returning one silently.
    let bind = settle(listener.as_ref(), t);
    println!(
        "bind settled after {:.1} ms: {:?}; consent: {}",
        bind.bound_ms, bind.state, bind.consent
    );
    let ShortcutState::Active { trigger } = &bind.state else {
        // Plasma 5.27 answers without binding (System Settings opens for the user): the manual
        // step. Anything else is a failure where a key press is expected.
        assert!(
            !expect_fires(),
            "the shortcut was not bound: {:?} (consent: {}); kglobalaccel:\n{}",
            bind.state,
            bind.consent,
            dump_kglobalaccel()
        );
        println!(
            "MANUAL STEP: the desktop did not bind the shortcut ({:?})",
            bind.state
        );
        assert_eq!(fired(&rx), 0, "nothing fires before the keys are bound");
        drop(listener);
        summary(&format!(
            "| {} | {APP_ID} | created in {start_ms:.1} ms | {:?} | {} | - | no (manual step) |",
            compositor(),
            bind.state,
            bind.consent
        ));
        return;
    };
    let bound = format!(
        "bound {} in {:.0} ms",
        trigger.as_deref().unwrap_or("(trigger not reported)"),
        bind.bound_ms
    );

    // Activation: a real key press on the compositor's virtual keyboard where the session has
    // one; kglobalaccel's invokeShortcut otherwise (not a key press, labelled as such).
    let (activation, fire_ms) = if let Some(keys) = virtual_keyboard() {
        let ms = press_until_fired(&keys, &rx).unwrap_or_else(|| {
            panic!(
                "Ctrl+Alt+Space on the virtual keyboard did not fire the portal shortcut ({bound})"
            )
        });
        assert_eq!(fired(&rx), 0, "one press fires once");
        press(&keys, KEYS);
        assert_eq!(fired(&rx), 1, "a second press fires again");
        (format!("virtual keyboard ({})", keyboard_kind()), ms)
    } else if let Some((comp, name)) = find_kglobalaccel_shortcut() {
        assert!(
            !expect_fires(),
            "a key press is expected, but this session has no virtual keyboard"
        );
        let pressed = Instant::now();
        invoke(&comp, &name);
        let at = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("the portal's Activated signal did not fire within 3 s");
        (
            "kglobalaccel invokeShortcut (not a key press)".to_string(),
            support::ms(at.saturating_duration_since(pressed)),
        )
    } else {
        panic!("bound, but no way to activate the shortcut in this session");
    };
    println!(
        "DOUBLETAP-HOTKEY os=linux-wayland-{} trigger=portal({CHORD}) activation={activation} \
         press_to_fire_ms={fire_ms:.2}",
        compositor()
    );
    summary(&format!(
        "| {} | {APP_ID} | created in {start_ms:.1} ms | {bound} | {} | {activation} | yes, {fire_ms:.1} ms |",
        compositor(),
        bind.consent
    ));

    // Dropping closes the portal session: the keys no longer reach us.
    drop(listener);
    std::thread::sleep(Duration::from_millis(500));
    if let Some(keys) = virtual_keyboard() {
        press(&keys, KEYS);
    }
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
        // The real trigger when the desktop binds the portal chord (consent accepted in its
        // dialog, keys pressed on the virtual keyboard); otherwise the listener is still started,
        // as the shell does, and the same path is driven by a direct call, as the tray/app
        // button does.
        let (cb, direct) = split_callback(cb);
        let t = Instant::now();
        let listener = has_portal
            .then(|| start(Trigger::Chord(CHORD.into()), cb))
            .and_then(|r| {
                r.map_err(|e| println!("portal chord unavailable: {e}"))
                    .ok()
            });
        let bound = listener.as_ref().is_some_and(|l| {
            let bind = settle(l.as_ref(), t);
            println!("portal bind: {:?}; consent: {}", bind.state, bind.consent);
            matches!(bind.state, ShortcutState::Active { .. })
        });
        let keys = virtual_keyboard().filter(|_| bound);
        if expect_fires() {
            assert!(keys.is_some(), "the portal shortcut was not bound");
        }
        let trigger = if keys.is_some() {
            "portal, virtual key press"
        } else {
            "direct"
        };
        let report = support::trigger_and_draft(&backend, &pipeline, mode, rx, |_| match &keys {
            Some(k) => press(k, KEYS),
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
