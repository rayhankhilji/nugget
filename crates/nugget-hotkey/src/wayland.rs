//! Wayland: chords through the XDG desktop portal's `GlobalShortcuts` interface (`ashpd`).
//!
//! The compositor owns the binding. Nugget asks for a preferred trigger; the desktop may show a
//! dialog and the user may pick a different key, so the portal, not this crate, decides what
//! finally fires. Activation is all the portal reports: no key events reach Nugget.
//!
//! Double taps of a modifier are impossible here without root (reading `/dev/input`), which this
//! crate will not do, so they are reported as unsupported before this module is reached.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use ashpd::desktop::CreateSessionOptions;
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use nugget_core::{Error, Result};
use tokio::sync::oneshot;

use crate::chord::Chord;
use crate::engine::Fire;

/// The desktop app's id (the Tauri `identifier`, which names its `.desktop` file): the name the
/// portal shows the user and keys its permissions by.
const APP_ID: &str = "dev.nugget.desktop";

/// How long [`start_chord`] waits for the portal to answer before giving up.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(5);

fn portal_err(e: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!(
        "the XDG desktop portal GlobalShortcuts interface is unavailable ({e}); it needs \
         xdg-desktop-portal 1.17+ with a backend that implements it (GNOME 48+, KDE Plasma 5.27+, \
         Hyprland)"
    ))
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

pub(crate) fn start_chord(chord: Chord, fire: Fire) -> Result<Box<dyn Send>> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let id = format!(
        "nugget-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let trigger = chord.portal_trigger();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();

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
            rt.block_on(run(id, trigger, fire, ready_tx, stop_rx));
        })
        .map_err(|e| Error::Other(format!("cannot start the portal thread: {e}")))?;

    let listener = PortalListener {
        stop: Some(stop_tx),
        thread: Some(thread),
    };
    match ready_rx.recv_timeout(PORTAL_TIMEOUT) {
        Ok(Ok(())) => Ok(Box::new(listener)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(portal_err("no answer from the portal")),
    }
}

async fn run(
    id: String,
    trigger: String,
    fire: Fire,
    ready: mpsc::Sender<Result<()>>,
    mut stop: oneshot::Receiver<()>,
) {
    let setup = async {
        // A connection of this listener's own. `GlobalShortcuts::new()` would reuse ashpd's
        // process-wide one, whose I/O task lives on the runtime that first created it: once
        // that listener is dropped (its runtime with it), every later listener in the process
        // would wait on a dead connection and time out.
        let conn = ashpd::zbus::Connection::session()
            .await
            .map_err(portal_err)?;
        // xdg-desktop-portal 1.19+ refuses GlobalShortcuts to an unsandboxed app it cannot
        // name ("An app id is required"). A host app names itself through the host Registry,
        // first thing on its connection. Older portals lack it, and a sandbox (Flatpak) or a
        // systemd scope already gives the app id: failure here is not an error.
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
        let session = portal
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(|e| match &registered {
                Err(r) => portal_err(format!("{e}; registering the app id failed: {r}")),
                Ok(()) => portal_err(e),
            })?;
        let activated = portal.receive_activated().await.map_err(portal_err)?;
        Ok::<_, Error>((portal, session, activated))
    };
    let (portal, session, mut activated) = tokio::select! {
        r = setup => match r {
            Ok(v) => v,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        },
        _ = &mut stop => return,
    };
    let _ = ready.send(Ok(()));

    // Binding may wait on a confirmation dialog, so it runs after start() has returned.
    let shortcut =
        NewShortcut::new(id.clone(), "Nugget: draft a reply").preferred_trigger(trigger.as_str());
    let bind = async {
        portal
            .bind_shortcuts(&session, &[shortcut], None, BindShortcutsOptions::default())
            .await?
            .response()
    };
    tokio::select! {
        r = bind => match r {
            // A desktop may answer without binding anything and leave the keys to the user
            // (Plasma 5.27 opens System Settings' shortcuts page instead). The session stays
            // open, so the shortcut fires once the user assigns it there.
            Ok(bound) if !bound.shortcuts().iter().any(|s| s.id() == id) => {
                tracing::warn!(
                    "nugget-hotkey: the desktop did not bind the shortcut yet; assign it in the \
                     desktop's keyboard shortcut settings"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "nugget-hotkey: the portal did not bind the shortcut");
                let _ = session.close().await;
                return;
            }
        },
        _ = &mut stop => {
            let _ = session.close().await;
            return;
        }
    }

    loop {
        tokio::select! {
            next = activated.next() => match next {
                Some(a) if a.shortcut_id() == id => fire.fire(),
                Some(_) => {}
                None => break,
            },
            _ = &mut stop => break,
        }
    }
    let _ = session.close().await;
}
