//! Wayland: chords through the XDG desktop portal's `GlobalShortcuts` interface (`ashpd`).
//!
//! The compositor owns the binding. Nugget asks for a preferred trigger; the desktop may show a
//! dialog and the user may pick a different key, so the portal, not this crate, decides what
//! finally fires. Activation is all the portal reports: no key events reach Nugget.
//!
//! Each listener binds one shortcut under a stable id ([`crate::DOUBLE_TAP_SHORTCUT_ID`] or
//! [`crate::KILL_SWITCH_SHORTCUT_ID`]). Desktops that ask for consent (GNOME, Plasma 6) remember
//! it per id, so a returning id is bound without a dialog, on the next launch and when this
//! module binds it again after the desktop closed the session or the portal restarted.
//!
//! Double taps of a modifier are impossible here without root (reading `/dev/input`), which this
//! crate will not do, so they are reported as unsupported before this module is reached.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ashpd::desktop::global_shortcuts::{
    Activated, BindShortcutsOptions, GlobalShortcuts, NewShortcut, ShortcutsChanged,
};
use ashpd::desktop::{CreateSessionOptions, ResponseError, Session};
use futures_util::{Stream, StreamExt};
use nugget_core::{Error, Result};
use tokio::sync::oneshot;

use crate::chord::Chord;
use crate::engine::Fire;
use crate::{APP_ID, Backend, SharedState, ShortcutState};

/// How long [`start_chord`] waits for the portal to answer before giving up, and how long a
/// later reconnection may take before it counts as failed.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long closing a session may take (the portal may already be gone).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// The first and the longest wait before binding again.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// The shortcut a listener binds: its stable id and the description the desktop shows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shortcut {
    pub(crate) id: &'static str,
    pub(crate) description: &'static str,
}

fn portal_err(e: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!(
        "the XDG desktop portal GlobalShortcuts interface is unavailable ({e}); it needs \
         xdg-desktop-portal 1.17+ with a backend that implements it (GNOME 48+, KDE Plasma 5.27+, \
         Hyprland)"
    ))
}

/// The portal refused a session after it could not put a name on Nugget.
fn unnamed_app_err(create: impl std::fmt::Display, register: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!(
        "the XDG desktop portal refused a GlobalShortcuts session ({create}) because it has no \
         app id for Nugget: registering the app id {APP_ID} failed ({register}). The portal \
         needs {APP_ID}.desktop in its XDG_DATA_DIRS (or ~/.local/share/applications), with an \
         Exec program that exists on its PATH: install the .deb or .rpm package, or start the \
         AppImage, which registers it under ~/.local/share/applications"
    ))
}

fn set_state(state: &SharedState, new: ShortcutState) {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    if *s != new {
        // The trigger description is not logged: only the state's kind.
        tracing::debug!(
            state = state_kind(&new),
            "nugget-hotkey: portal shortcut state"
        );
        *s = new;
    }
}

fn state_kind(s: &ShortcutState) -> &'static str {
    match s {
        ShortcutState::Active { .. } => "active",
        ShortcutState::AwaitingConsent => "awaiting consent",
        ShortcutState::NotAssigned => "not assigned",
        ShortcutState::Rebinding => "rebinding",
        ShortcutState::Failed(_) => "failed",
    }
}

/// The state after BindShortcuts answered with `bound` (id, trigger description) pairs: active
/// when our id came back with keys, otherwise left for the user to assign.
fn state_after_bind(id: &str, bound: &[(&str, &str)]) -> ShortcutState {
    match bound.iter().find(|(i, _)| *i == id) {
        Some((_, trigger)) if !trigger.trim().is_empty() => ShortcutState::Active {
            trigger: Some(trigger.trim().to_string()),
        },
        _ => ShortcutState::NotAssigned,
    }
}

/// The state after a ShortcutsChanged signal listing `shortcuts`. A session holds only our
/// shortcut, so the list is ours: our id gone or without keys means it is not assigned any more.
fn state_after_change(id: &str, shortcuts: &[(&str, &str)]) -> ShortcutState {
    state_after_bind(id, shortcuts)
}

/// Capped exponential backoff for binding again.
#[derive(Debug)]
struct Backoff {
    next: Duration,
    min: Duration,
    max: Duration,
}

impl Backoff {
    fn new(min: Duration, max: Duration) -> Self {
        Self {
            next: min,
            min,
            max,
        }
    }

    /// The wait before the next attempt; each call doubles the following one, up to the cap.
    fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = self.next.saturating_mul(2).min(self.max);
        d
    }

    /// A binding that held for at least the cap starts the next series from the minimum, so a
    /// desktop that drops the session right after every bind is not retried at full speed.
    fn served(&mut self, lasted: Duration) {
        if lasted >= self.max {
            self.next = self.min;
        }
    }
}

struct PortalListener {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for PortalListener {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn start_chord(chord: Chord, fire: Fire, shortcut: Shortcut) -> Result<Backend> {
    let trigger = chord.portal_trigger();
    let state: SharedState = Arc::new(Mutex::new(ShortcutState::AwaitingConsent));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();

    let thread_state = state.clone();
    let thread = std::thread::Builder::new()
        .name("nugget-hotkey-portal".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(Error::Other(format!("tokio runtime: {e}"))));
                    return;
                }
            };
            let spec = Spec { shortcut, trigger };
            rt.block_on(run(spec, fire, thread_state, ready_tx, stop_rx));
        })
        .map_err(|e| Error::Other(format!("cannot start the portal thread: {e}")))?;

    let listener = PortalListener {
        stop: Some(stop_tx),
        thread: Some(thread),
    };
    match ready_rx.recv_timeout(PORTAL_TIMEOUT) {
        Ok(Ok(())) => Ok(Backend {
            handle: Box::new(listener),
            state: Some(state),
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(portal_err("no answer from the portal")),
    }
}

struct Spec {
    shortcut: Shortcut,
    trigger: String,
}

/// A session on a connection of its own, with its signal streams subscribed.
struct Bound {
    portal: GlobalShortcuts,
    session: Session<GlobalShortcuts>,
    activated: Pin<Box<dyn Stream<Item = Activated>>>,
    changed: Pin<Box<dyn Stream<Item = ShortcutsChanged>>>,
    /// Ticks when the portal's bus name changes owner (it exited or restarted).
    owner: Pin<Box<dyn Stream<Item = ()>>>,
}

impl Bound {
    async fn close(self) {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, self.session.close()).await;
    }
}

/// How a bound session ended.
enum Ended {
    Stopped,
    Lost { reason: String, failed: bool },
}

/// Runs `f` unless the listener is stopped first (`None`).
async fn or_stop<F: Future>(stop: &mut oneshot::Receiver<()>, f: F) -> Option<F::Output> {
    tokio::select! {
        r = f => Some(r),
        _ = stop => None,
    }
}

async fn setup() -> Result<Bound> {
    // A connection of this session's own. `GlobalShortcuts::new()` would reuse ashpd's
    // process-wide one, whose I/O task lives on the runtime that first created it: once that
    // listener is dropped (its runtime with it), every later listener in the process would
    // wait on a dead connection and time out. A fresh connection is also what lets a rebind
    // register the app id again with a restarted portal.
    let conn = ashpd::zbus::Connection::session()
        .await
        .map_err(portal_err)?;
    // xdg-desktop-portal 1.19+ refuses GlobalShortcuts to an unsandboxed app it cannot name
    // ("An app id is required"). A host app names itself through the host Registry, first
    // thing on its connection. Older portals lack it, and a sandbox (Flatpak) or a systemd
    // scope already gives the app id: failure here is not an error by itself.
    let registered = match APP_ID.parse::<ashpd::AppID>() {
        Ok(app_id) => ashpd::register_host_app_with_connection(conn.clone(), app_id)
            .await
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = &registered {
        tracing::debug!(error = %e, "nugget-hotkey: host portal registration failed");
    }
    let portal = GlobalShortcuts::with_connection(conn)
        .await
        .map_err(portal_err)?;
    let owner = portal.receive_owner_changed().await.map_err(portal_err)?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| match &registered {
            Err(r) => unnamed_app_err(e, r),
            Ok(()) => portal_err(e),
        })?;
    // Subscribed before binding, so no signal about the bind is missed.
    let activated = portal.receive_activated().await.map_err(portal_err)?;
    let changed = portal
        .receive_shortcuts_changed()
        .await
        .map_err(portal_err)?;
    Ok(Bound {
        portal,
        session,
        activated: Box::pin(activated),
        changed: Box::pin(changed),
        owner: Box::pin(owner.map(|_| ())),
    })
}

async fn run(
    spec: Spec,
    fire: Fire,
    state: SharedState,
    ready: mpsc::Sender<Result<()>>,
    mut stop: oneshot::Receiver<()>,
) {
    // An AppImage has no desktop file the portal can find; install one first (best effort).
    match crate::desktop_entry::ensure_appimage_entry() {
        Ok(Some(path)) => {
            tracing::debug!(path = %path.display(), "nugget-hotkey: desktop entry installed");
        }
        Ok(None) => {}
        Err(e) => tracing::debug!(error = %e, "nugget-hotkey: cannot install a desktop entry"),
    }

    let mut bound = match or_stop(&mut stop, setup()).await {
        None => return,
        Some(Err(e)) => {
            let _ = ready.send(Err(e));
            return;
        }
        Some(Ok(b)) => b,
    };
    let _ = ready.send(Ok(()));

    let mut backoff = Backoff::new(BACKOFF_MIN, BACKOFF_MAX);
    let mut first = true;
    loop {
        let started = Instant::now();
        let ended = serve(&spec, &mut bound, &fire, &state, &mut stop, first).await;
        bound.close().await;
        let (reason, failed) = match ended {
            Ended::Stopped => return,
            Ended::Lost { reason, failed } => (reason, failed),
        };
        tracing::warn!(%reason, "nugget-hotkey: the portal shortcut was lost; binding it again");
        set_state(
            &state,
            if failed {
                ShortcutState::Failed(reason)
            } else {
                ShortcutState::Rebinding
            },
        );
        backoff.served(started.elapsed());
        first = false;

        bound = loop {
            if or_stop(&mut stop, tokio::time::sleep(backoff.next_delay()))
                .await
                .is_none()
            {
                return;
            }
            match or_stop(&mut stop, tokio::time::timeout(PORTAL_TIMEOUT, setup())).await {
                None => return,
                Some(Ok(Ok(b))) => break b,
                Some(Ok(Err(e))) => set_state(&state, ShortcutState::Failed(e.to_string())),
                Some(Err(_)) => set_state(
                    &state,
                    ShortcutState::Failed("no answer from the portal".into()),
                ),
            }
        };
    }
}

/// Binds the shortcut on `b` and listens until the listener stops or the session is lost.
async fn serve(
    spec: &Spec,
    b: &mut Bound,
    fire: &Fire,
    state: &SharedState,
    stop: &mut oneshot::Receiver<()>,
    first: bool,
) -> Ended {
    let id = spec.shortcut.id;
    let Bound {
        portal,
        session,
        activated,
        changed,
        owner,
    } = b;
    let closed = match or_stop(stop, session.receive_closed()).await {
        None => return Ended::Stopped,
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            return Ended::Lost {
                reason: format!("cannot watch the portal session: {e}"),
                failed: true,
            };
        }
    };
    let mut closed = pin!(closed);

    // A new id waits on the desktop's consent dialog; a returning one is bound right away.
    set_state(
        state,
        if first {
            ShortcutState::AwaitingConsent
        } else {
            ShortcutState::Rebinding
        },
    );
    let shortcut =
        NewShortcut::new(id, spec.shortcut.description).preferred_trigger(spec.trigger.as_str());
    let portal: &GlobalShortcuts = portal;
    let session: &Session<GlobalShortcuts> = session;
    let bind = async move {
        portal
            .bind_shortcuts(session, &[shortcut], None, BindShortcutsOptions::default())
            .await?
            .response()
    };
    let mut bind = pin!(bind);
    let mut binding = true;

    loop {
        tokio::select! {
            r = &mut bind, if binding => {
                binding = false;
                match r {
                    Ok(bound) => {
                        let pairs: Vec<_> = bound
                            .shortcuts()
                            .iter()
                            .map(|s| (s.id(), s.trigger_description()))
                            .collect();
                        let new = state_after_bind(id, &pairs);
                        if new == ShortcutState::NotAssigned {
                            // Plasma 5.27 leaves the keys to the user; so does a declined dialog.
                            tracing::warn!(
                                "nugget-hotkey: the desktop did not assign keys to the shortcut; \
                                 assign them in the desktop's keyboard shortcut settings"
                            );
                        }
                        set_state(state, new);
                    }
                    // The user declined. The session stays open: keys assigned later in the
                    // desktop's settings arrive through ShortcutsChanged.
                    Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                        tracing::warn!("nugget-hotkey: the shortcut was declined in the desktop");
                        set_state(state, ShortcutState::NotAssigned);
                    }
                    Err(e) => {
                        return Ended::Lost {
                            reason: format!("the portal did not bind the shortcut: {e}"),
                            failed: true,
                        };
                    }
                }
            }
            next = activated.next() => match next {
                Some(a) if a.shortcut_id() == id => fire.fire(),
                Some(_) => {}
                None => {
                    return Ended::Lost {
                        reason: "the portal's activation signal ended".into(),
                        failed: false,
                    };
                }
            },
            next = changed.next() => match next {
                Some(c) => {
                    let pairs: Vec<_> = c
                        .shortcuts()
                        .iter()
                        .map(|s| (s.id(), s.trigger_description()))
                        .collect();
                    set_state(state, state_after_change(id, &pairs));
                }
                None => {
                    return Ended::Lost {
                        reason: "the portal's shortcut-change signal ended".into(),
                        failed: false,
                    };
                }
            },
            _ = closed.next() => {
                return Ended::Lost {
                    reason: "the desktop closed the shortcut session".into(),
                    failed: false,
                };
            }
            _ = owner.next() => {
                return Ended::Lost {
                    reason: "the desktop portal exited or restarted".into(),
                    failed: false,
                };
            }
            _ = &mut *stop => return Ended::Stopped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = crate::DOUBLE_TAP_SHORTCUT_ID;

    #[test]
    fn bind_with_keys_is_active_with_the_desktops_trigger() {
        assert_eq!(
            state_after_bind(ID, &[("other", "Ctrl+X"), (ID, " Ctrl+Alt+Space ")]),
            ShortcutState::Active {
                trigger: Some("Ctrl+Alt+Space".into())
            }
        );
    }

    #[test]
    fn bind_without_our_id_or_keys_is_not_assigned() {
        assert_eq!(state_after_bind(ID, &[]), ShortcutState::NotAssigned);
        assert_eq!(
            state_after_bind(ID, &[("stop-agents", "Ctrl+Alt+Esc")]),
            ShortcutState::NotAssigned
        );
        assert_eq!(
            state_after_bind(ID, &[(ID, "")]),
            ShortcutState::NotAssigned
        );
        assert_eq!(
            state_after_bind(ID, &[(ID, "  ")]),
            ShortcutState::NotAssigned
        );
    }

    #[test]
    fn changes_follow_the_desktop_both_ways() {
        // Assigned later in the desktop's settings.
        assert_eq!(
            state_after_change(ID, &[(ID, "Meta+N")]),
            ShortcutState::Active {
                trigger: Some("Meta+N".into())
            }
        );
        // Keys removed, or the shortcut dropped.
        assert_eq!(
            state_after_change(ID, &[(ID, "")]),
            ShortcutState::NotAssigned
        );
        assert_eq!(state_after_change(ID, &[]), ShortcutState::NotAssigned);
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let mut b = Backoff::new(BACKOFF_MIN, BACKOFF_MAX);
        let secs: Vec<u64> = (0..9).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn backoff_resets_only_after_a_binding_that_held() {
        let mut b = Backoff::new(BACKOFF_MIN, BACKOFF_MAX);
        for _ in 0..4 {
            b.next_delay();
        }
        b.served(Duration::from_secs(3));
        assert_eq!(b.next_delay(), Duration::from_secs(16));
        b.served(BACKOFF_MAX);
        assert_eq!(b.next_delay(), BACKOFF_MIN);
    }

    #[test]
    fn refused_session_names_the_desktop_file() {
        let msg = unnamed_app_err(
            "An app id is required",
            "App info not found for 'dev.nugget.Nugget'",
        )
        .to_string();
        assert!(msg.contains("app id"), "{msg}");
        assert!(msg.contains("dev.nugget.Nugget.desktop"), "{msg}");
        assert!(msg.contains("Exec"), "{msg}");
        assert!(msg.contains("AppImage"), "{msg}");
        assert!(msg.contains("App info not found"), "{msg}");
        assert!(msg.contains("An app id is required"), "{msg}");
    }

    #[test]
    fn shortcut_ids_are_stable() {
        assert_eq!(crate::DOUBLE_TAP_SHORTCUT_ID, "double-tap");
        assert_eq!(crate::KILL_SWITCH_SHORTCUT_ID, "stop-agents");
    }
}
