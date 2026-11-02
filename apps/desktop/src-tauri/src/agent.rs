//! Agent kill switch in the desktop shell: Escape pressed three times anywhere, and the tray's
//! "Stop all agents" item.
//!
//! The agent runs inside `nuggetd`, which has its own `nugget_policy::KillSwitch`. The app's
//! process-wide [`KillSwitch::global`] is engaged too (for anything that runs in this process),
//! but what actually stops the agent is `policy.kill` sent to the daemon. The app keeps its global
//! switch in step with the daemon: `policy.kill.changed` notifications (and `policy.get` on each
//! (re)connection) mirror the daemon's state into it, which also drives the tray label.
//!
//! Forwarding rules, chosen so the two switches cannot ping-pong:
//! - A **halt** of the global switch from anywhere in this process is forwarded as `policy.kill`,
//!   except when it is the mirror of a daemon notification.
//! - A **resume** is never forwarded from the global switch: resuming is a deliberate action, made
//!   only from the tray (or the Safety page) through `policy.resume`.
//! - The hotkey and the tray always send `policy.kill`, even when the global switch already
//!   reads halted, so a stale local state can never make the kill switch a no-op.
//!
//! # The Escape ×3 listener
//!
//! It is `nugget_hotkey`'s kill-switch listener, which **observes** Escape and never grabs or
//! swallows it: other apps keep receiving every Escape press.
//!
//! | OS / session | Escape ×3 | Notes |
//! |---|---|---|
//! | macOS | NSEvent global key-down monitor | Needs Accessibility permission; without it the monitor sees nothing (the tray still works). |
//! | Windows | `WH_KEYBOARD_LL` hook | Passed through with `CallNextHookEx`. Elevated windows are not observed unless Nugget is elevated. |
//! | Linux X11 | XInput2 raw events | |
//! | Linux Wayland | Unsupported | Falls back to the [`FALLBACK_CHORD`] chord through the GlobalShortcuts portal (the desktop may ask the user to confirm the binding). |
//! | Headless / other | Unsupported | Logged; only the tray item works. |

use std::cell::Cell;
use std::sync::Mutex;

use nugget_core::ipc::{KillState, PolicyState, agent_method, agent_notification};
use nugget_hotkey::{Capabilities, KillSwitchListener, Trigger};
use nugget_ipc::Notification;
use nugget_policy::KillSwitch;
use nugget_policy::kill::HookGuard;
use serde::Deserialize;
use tauri::menu::MenuItem;
use tauri::{AppHandle, Listener, Manager, Runtime};

use crate::daemon::{Daemon, event};

/// Escape presses that engage the kill switch, each within [`ESCAPE_WINDOW_MS`] of the previous.
pub const ESCAPE_PRESSES: u8 = 3;
/// Three presses therefore fit in about one second.
pub const ESCAPE_WINDOW_MS: u32 = 500;
/// Used where Escape cannot be observed but chords can be registered (Wayland). A chord is
/// grabbed exclusively, so it uses a combination no app relies on rather than plain Escape.
pub const FALLBACK_CHORD: &str = "Ctrl+Alt+Shift+Escape";

const TRAY_ITEM_ID: &str = "agents";
const LABEL_STOP: &str = "Stop all agents";
const LABEL_RESUME: &str = "Resume agents";

/// The tray menu id of the "Stop all agents" / "Resume agents" item.
pub fn tray_item_id() -> &'static str {
    TRAY_ITEM_ID
}

/// Tray label for the current kill-switch state.
pub fn tray_label(halted: bool) -> &'static str {
    if halted { LABEL_RESUME } else { LABEL_STOP }
}

/// The kill trigger to listen for in a session with these capabilities, if any.
pub fn choose_kill_switch(caps: &Capabilities) -> Option<nugget_hotkey::KillSwitch> {
    if caps.escape_repeat {
        Some(nugget_hotkey::KillSwitch::EscapeRepeat {
            presses: ESCAPE_PRESSES,
            window_ms: ESCAPE_WINDOW_MS,
        })
    } else if caps.chord {
        Some(nugget_hotkey::KillSwitch::Trigger(Trigger::Chord(
            FALLBACK_CHORD.into(),
        )))
    } else {
        None
    }
}

/// The daemon's kill state carried by a notification, if it is `policy.kill.changed`.
pub fn kill_state_from(note: &Notification) -> Option<bool> {
    if note.method != agent_notification::POLICY_KILL_CHANGED {
        return None;
    }
    note.params_as::<KillState>().ok().map(|s| s.halted)
}

/// What to do when the daemon (re)connects and reports its kill state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconcile {
    /// The app is halted but the daemon is not (it restarted, or a kill was sent while it was
    /// unreachable): halt it again. Erring towards stopped is the safe direction.
    PushKill,
    /// Take the daemon's state.
    Mirror(bool),
}

pub fn reconcile(local_halted: bool, daemon_halted: bool) -> Reconcile {
    if local_halted && !daemon_halted {
        Reconcile::PushKill
    } else {
        Reconcile::Mirror(daemon_halted)
    }
}

thread_local! {
    /// Set while this thread changes the global switch for a reason that must not be forwarded
    /// to the daemon (mirroring the daemon, or a path that sends `policy.kill` itself). Hooks run
    /// synchronously on the thread that changed the switch, so a thread-local is exact.
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

fn quietly<T>(f: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            QUIET.with(|q| q.set(self.0));
        }
    }
    let _reset = Reset(QUIET.with(|q| q.replace(true)));
    f()
}

/// Register the hook that reflects every change of `switch` through `on_state`, and forwards
/// halts (not resumes, and not quiet ones) through `forward_kill`.
fn watch(
    switch: &KillSwitch,
    on_state: impl Fn(bool) + Send + Sync + 'static,
    forward_kill: impl Fn() + Send + Sync + 'static,
) -> HookGuard {
    switch.on_change(move |halted| {
        on_state(halted);
        if halted && !QUIET.with(Cell::get) {
            forward_kill();
        }
    })
}

/// Kept in app state for as long as the app runs.
struct AgentShell {
    app: AppHandle,
    _listener: Mutex<Option<KillSwitchListener>>,
    _hook: HookGuard,
}

/// The tray item, managed by [`tray_item`] when the tray exists.
struct AgentTray<R: Runtime>(MenuItem<R>);

/// Create the tray's "Stop all agents" item (called from `tray::build`).
pub fn tray_item<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<MenuItem<R>> {
    let label = tray_label(KillSwitch::global().halted());
    let item = MenuItem::with_id(app, TRAY_ITEM_ID, label, true, None::<&str>)?;
    app.manage(AgentTray(item.clone()));
    Ok(item)
}

/// The tray item was clicked: stop everything, or resume when already stopped.
pub fn tray_clicked<R: Runtime>(app: &AppHandle<R>) {
    let Some(shell) = app.try_state::<AgentShell>() else {
        return;
    };
    if KillSwitch::global().halted() {
        resume(&shell.app);
    } else {
        engage(&shell.app, "tray");
    }
}

fn set_tray_label(app: &AppHandle, halted: bool) {
    if let Some(t) = app.try_state::<AgentTray<tauri::Wry>>() {
        let key = if halted {
            "shell.tray.resumeAgents"
        } else {
            "shell.tray.stopAgents"
        };
        let _ = t.0.set_text(crate::platform::label(app, key));
    }
}

/// The tray's labels changed language.
pub fn relabel_tray(app: &AppHandle) {
    set_tray_label(app, KillSwitch::global().halted());
}

/// Start the Escape ×3 listener and keep the app's switch, the daemon and the tray in step.
/// Call from `tauri::Builder::setup` (the main thread, which macOS needs), after the tray.
pub fn setup(app: &tauri::App) {
    let handle = app.handle().clone();

    let label_app = handle.clone();
    let kill_app = handle.clone();
    let hook = watch(
        KillSwitch::global(),
        move |halted| set_tray_label(&label_app, halted),
        move || send_kill(&kill_app, "app"),
    );

    let listener = start_listener(&handle);
    app.manage(AgentShell {
        app: handle.clone(),
        _listener: Mutex::new(listener),
        _hook: hook,
    });

    // Daemon → app: mirror `policy.kill.changed`.
    let notes_app = handle.clone();
    handle.listen_any(event::NOTIFICATION, move |e| {
        let Ok(note) = serde_json::from_str::<Notification>(e.payload()) else {
            return;
        };
        if let Some(halted) = kill_state_from(&note) {
            mirror(&notes_app, halted);
        }
    });

    // On each (re)connection, reconcile with the daemon's current state.
    let conn_app = handle.clone();
    handle.listen_any(event::CONNECTION, move |e| {
        let connected =
            serde_json::from_str::<ConnectionInfoLite>(e.payload()).is_ok_and(|c| c.connected);
        if connected {
            let app = conn_app.clone();
            tauri::async_runtime::spawn(async move { sync_with_daemon(&app).await });
        }
    });
}

/// Just the field of `daemon::ConnectionInfo` this module reads.
#[derive(Deserialize)]
struct ConnectionInfoLite {
    connected: bool,
}

fn start_listener(app: &AppHandle) -> Option<KillSwitchListener> {
    let caps = nugget_hotkey::capabilities();
    let Some(kill_switch) = choose_kill_switch(&caps) else {
        tracing::warn!(
            session = ?caps.session,
            "no global kill-switch hotkey in this session; use the tray's \"Stop all agents\""
        );
        return None;
    };
    let fire_app = app.clone();
    let halt = KillSwitch::global().trigger_hook();
    let on_fire = Box::new(move || {
        // `send_kill` below covers the daemon, so the hook need not forward this halt.
        quietly(&halt);
        send_kill(&fire_app, "hotkey");
    });
    match nugget_hotkey::start_kill_switch(kill_switch.clone(), on_fire) {
        Ok(listener) => {
            if matches!(kill_switch, nugget_hotkey::KillSwitch::Trigger(_)) {
                tracing::warn!(
                    session = ?caps.session,
                    chord = FALLBACK_CHORD,
                    "Escape cannot be observed in this session; the agent kill switch is a chord"
                );
            } else {
                tracing::info!("agent kill switch: press Escape {ESCAPE_PRESSES} times");
            }
            Some(listener)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "agent kill-switch hotkey unavailable; use the tray's \"Stop all agents\""
            );
            None
        }
    }
}

/// Stop: engage the app's switch and tell the daemon.
fn engage(app: &AppHandle, from: &'static str) {
    quietly(|| KillSwitch::global().halt());
    send_kill(app, from);
}

/// Send `policy.kill`, whatever the local state says.
fn send_kill(app: &AppHandle, from: &'static str) {
    tracing::warn!(from, "agent kill switch engaged");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let daemon = app.state::<Daemon>();
        match daemon
            .call::<_, KillState>(&app, agent_method::POLICY_KILL, &serde_json::json!({}))
            .await
        {
            Ok(state) => mirror(&app, state.halted),
            // No daemon means no agent is running; the halt is pushed again on reconnect.
            Err(e) => tracing::warn!(error = %e, "couldn't send policy.kill to nuggetd"),
        }
    });
}

fn resume(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let daemon = app.state::<Daemon>();
        match daemon
            .call::<_, KillState>(&app, agent_method::POLICY_RESUME, &serde_json::json!({}))
            .await
        {
            Ok(state) => {
                tracing::info!(halted = state.halted, "agents resumed");
                mirror(&app, state.halted);
            }
            Err(e) => tracing::warn!(error = %e, "couldn't send policy.resume to nuggetd"),
        }
    });
}

/// Take the daemon's state without echoing it back.
fn mirror(app: &AppHandle, halted: bool) {
    let global = KillSwitch::global();
    let changed = quietly(|| {
        if halted {
            global.halt()
        } else {
            global.resume()
        }
    });
    if !changed {
        // The hook did not run; keep the label right anyway (e.g. the tray was built late).
        set_tray_label(app, halted);
    }
}

#[derive(Deserialize)]
struct PolicyHalted {
    halted: bool,
}

// `PolicyState` is decoded partially so unrelated contract changes don't break the tray.
const _: fn(PolicyState) -> bool = |s| s.halted;

async fn sync_with_daemon(app: &AppHandle) {
    let daemon = app.state::<Daemon>();
    let daemon_halted = match daemon
        .call::<_, PolicyHalted>(app, agent_method::POLICY_GET, &serde_json::json!({}))
        .await
    {
        Ok(s) => s.halted,
        // E.g. a daemon without the policy feature.
        Err(e) => {
            tracing::debug!(error = %e, "policy.get failed");
            return;
        }
    };
    match reconcile(KillSwitch::global().halted(), daemon_halted) {
        Reconcile::PushKill => send_kill(app, "reconnect"),
        Reconcile::Mirror(halted) => mirror(app, halted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_hotkey::Session;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn caps(escape_repeat: bool, chord: bool) -> Capabilities {
        Capabilities {
            session: Session::X11,
            double_tap: escape_repeat,
            chord,
            escape_repeat,
            suggest_tap: escape_repeat,
        }
    }

    #[test]
    fn kill_switch_choice_degrades_to_a_chord_then_nothing() {
        assert_eq!(
            choose_kill_switch(&caps(true, true)),
            Some(nugget_hotkey::KillSwitch::EscapeRepeat {
                presses: 3,
                window_ms: 500
            })
        );
        assert_eq!(
            choose_kill_switch(&caps(false, true)),
            Some(nugget_hotkey::KillSwitch::Trigger(Trigger::Chord(
                FALLBACK_CHORD.into()
            )))
        );
        assert_eq!(choose_kill_switch(&caps(false, false)), None);
        // Both are valid configurations for the hotkey crate.
        nugget_hotkey::parse_chord(FALLBACK_CHORD).unwrap();
        assert!((2..=10).contains(&ESCAPE_PRESSES));
        assert!(nugget_hotkey::WINDOW_MS_RANGE.contains(&ESCAPE_WINDOW_MS));
    }

    #[test]
    fn kill_changed_notifications_are_recognised() {
        let note = |method: &str, params| Notification {
            method: method.into(),
            params,
        };
        let halted = serde_json::json!({ "halted": true });
        assert_eq!(
            kill_state_from(&note("policy.kill.changed", halted.clone())),
            Some(true)
        );
        assert_eq!(kill_state_from(&note("agent.event", halted)), None);
        assert_eq!(
            kill_state_from(&note("policy.kill.changed", serde_json::json!(null))),
            None
        );
    }

    #[test]
    fn reconnect_errs_towards_stopped() {
        assert_eq!(reconcile(true, false), Reconcile::PushKill);
        assert_eq!(reconcile(false, false), Reconcile::Mirror(false));
    }

    #[test]
    fn halts_are_forwarded_once_and_resumes_and_mirrors_never() {
        let k = KillSwitch::new();
        let states = Arc::new(Mutex::new(Vec::new()));
        let forwarded = Arc::new(AtomicUsize::new(0));
        let (s, f) = (Arc::clone(&states), Arc::clone(&forwarded));
        let _guard = watch(
            &k,
            move |h| s.lock().unwrap().push(h),
            move || {
                f.fetch_add(1, Ordering::SeqCst);
            },
        );

        // A plain halt (e.g. the trigger hook) is forwarded; halting again changes nothing.
        (k.trigger_hook())();
        k.halt();
        assert_eq!(forwarded.load(Ordering::SeqCst), 1);
        // Resumes are reflected but never forwarded.
        k.resume();
        assert_eq!(forwarded.load(Ordering::SeqCst), 1);
        // A quiet halt (mirroring the daemon) is reflected but not forwarded.
        quietly(|| k.halt());
        assert_eq!(forwarded.load(Ordering::SeqCst), 1);
        assert_eq!(*states.lock().unwrap(), vec![true, false, true]);
        // Quiet is scoped: the next halt is forwarded again.
        assert!(!QUIET.with(Cell::get));
        k.resume();
        k.halt();
        assert_eq!(forwarded.load(Ordering::SeqCst), 2);
    }
}
