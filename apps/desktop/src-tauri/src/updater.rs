//! User-controlled updates on top of `tauri-plugin-updater`.
//!
//! - The user picks a channel (stable, beta or off). Off never contacts the update server.
//! - The default is notify-only: a check finds a newer version and shows it (tray item plus the
//!   settings panel, with the release notes). Nothing is downloaded until the user clicks
//!   Install, and Install names the exact version whose notes were shown.
//! - Auto-install is opt-in: the update is downloaded and verified in the background and installed
//!   when Nugget quits, and the panel shows its notes in the meantime.
//! - Every package is verified against the minisign public key compiled into this build
//!   (`NUGGET_UPDATER_PUBKEY`, set by the release workflow). Builds without a key (dev builds,
//!   self-built packages) have no updater at all.
//! - The channel feed is signed with the same key (`<feed>.sig`) and verified before anything in
//!   it is read; a feed that doesn't verify is refused (see `feed`).

mod feed;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::daemon::CmdError;
use crate::settings::{self, Channel, Settings};

/// minisign public key (the base64 text `tauri signer generate` prints), baked in at build time.
const PUBKEY: Option<&str> = option_env!("NUGGET_UPDATER_PUBKEY");
/// Where the per-channel manifests live: the release workflow keeps `stable.json` and
/// `beta.json` on a rolling `updater-feed` release and moves them only when a maintainer
/// publishes a release. Forks override this at build time.
const DEFAULT_FEED: &str = "https://github.com/rayhankhilji/nugget/releases/download/updater-feed";
const FEED: Option<&str> = option_env!("NUGGET_UPDATER_FEED");

/// First background check shortly after launch (not at launch: startup stays offline-fast),
/// then once a day.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

pub mod event {
    /// `UpdateStatus`, whenever anything about the pending update changes.
    pub const STATUS: &str = "nugget:update-status";
    /// `{ downloaded, total }` while a package downloads.
    pub const PROGRESS: &str = "nugget:update-progress";
}

/// A newer version announced on the user's channel.
#[derive(Debug, Clone, Serialize)]
pub struct AvailableUpdate {
    pub version: String,
    /// Release notes (Markdown from the release), shown before anything is installed.
    pub notes: Option<String>,
    /// Publication time, Unix seconds.
    pub date: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    /// This build carries an update key; `false` means updates are unavailable, whatever the
    /// settings say.
    pub configured: bool,
    pub settings: Settings,
    pub available: Option<AvailableUpdate>,
    /// Downloaded and verified; installs when Nugget quits (auto-install only).
    pub staged: bool,
    /// A check, download or install is running.
    pub busy: bool,
    /// Unix seconds of the last completed check.
    pub last_checked: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

#[derive(Default)]
struct State {
    settings: Settings,
    pending: Option<Update>,
    staged: Option<Vec<u8>>,
    busy: bool,
    last_checked: Option<u64>,
    last_error: Option<String>,
}

pub struct Updates {
    settings_path: Option<PathBuf>,
    state: Mutex<State>,
    /// Serialises check/download/install.
    op: tokio::sync::Mutex<()>,
}

pub fn configured() -> bool {
    PUBKEY.is_some_and(|k| !k.trim().is_empty())
}

/// The manifest URL for a channel, or `None` for Off.
pub fn feed_url(channel: Channel) -> Option<String> {
    let base = FEED.unwrap_or(DEFAULT_FEED).trim_end_matches('/');
    match channel {
        Channel::Stable => Some(format!("{base}/stable.json")),
        Channel::Beta => Some(format!("{base}/beta.json")),
        Channel::Off => None,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn err(e: impl std::fmt::Display) -> CmdError {
    CmdError::Other(e.to_string())
}

impl Updates {
    pub fn new(app: &AppHandle) -> Self {
        let settings_path = app
            .path()
            .app_config_dir()
            .map(|d| settings::file_in(&d))
            .map_err(|e| tracing::warn!(error = %e, "no config directory; settings won't persist"))
            .ok();
        let settings = settings_path
            .as_deref()
            .map(settings::load)
            .unwrap_or_default();
        Self {
            settings_path,
            state: Mutex::new(State {
                settings,
                ..State::default()
            }),
            op: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    pub fn status(&self, app: &AppHandle) -> UpdateStatus {
        let s = self.lock();
        UpdateStatus {
            current_version: app.package_info().version.to_string(),
            configured: configured(),
            settings: s.settings.clone(),
            available: s.pending.as_ref().map(|u| AvailableUpdate {
                version: u.version.clone(),
                notes: u.body.clone(),
                date: u.date.map(|d| d.unix_timestamp()),
            }),
            staged: s.staged.is_some(),
            busy: s.busy,
            last_checked: s.last_checked,
            last_error: s.last_error.clone(),
        }
    }

    fn publish(&self, app: &AppHandle) -> UpdateStatus {
        let status = self.status(app);
        crate::tray::set_update(app, status.available.as_ref().map(|a| a.version.as_str()));
        let _ = app.emit(event::STATUS, &status);
        status
    }

    fn set_busy(&self, app: &AppHandle, busy: bool) {
        self.lock().busy = busy;
        self.publish(app);
    }

    /// Save new settings. Changing channel forgets whatever the old channel announced.
    pub fn set_settings(&self, app: &AppHandle, new: Settings) -> Result<UpdateStatus, CmdError> {
        if let Some(path) = &self.settings_path {
            settings::save(path, &new).map_err(|e| err(format!("couldn't save settings: {e}")))?;
        }
        {
            let mut s = self.lock();
            if s.settings.channel != new.channel {
                s.pending = None;
                s.staged = None;
                s.last_error = None;
            }
            if !new.auto_install {
                s.staged = None;
            }
            s.settings = new;
        }
        Ok(self.publish(app))
    }

    /// Ask the channel's feed whether a newer version exists. Never downloads in notify-only mode.
    pub async fn check(&self, app: &AppHandle) -> Result<UpdateStatus, CmdError> {
        let _op = self.op.lock().await;
        let channel = self.lock().settings.channel;
        let (Some(pubkey), Some(url)) = (PUBKEY.filter(|_| configured()), feed_url(channel)) else {
            return Ok(self.publish(app));
        };
        self.set_busy(app, true);
        let result = async {
            // Verify the feed before the plugin reads it; then accept the plugin's answer only
            // if it came from that same feed (`feed::consistent`).
            let verified = feed::fetch_verified(&url, pubkey, HTTP_TIMEOUT)
                .await
                .map_err(err)?;
            let seen = Arc::new(Mutex::new(None::<String>));
            let record = Arc::clone(&seen);
            let want = verified.version.clone();
            let update = app
                .updater_builder()
                .pubkey(pubkey)
                .endpoints(vec![url.parse().map_err(err)?])
                .map_err(err)?
                .timeout(HTTP_TIMEOUT)
                .version_comparator(move |current, release| {
                    let version = release.version.to_string();
                    let same = version == want;
                    *record.lock().unwrap_or_else(|p| p.into_inner()) = Some(version);
                    same && release.version > current
                })
                .build()
                .map_err(err)?
                .check()
                .await
                .map_err(err)?;
            let seen = seen.lock().unwrap_or_else(|p| p.into_inner()).take();
            feed::consistent(
                &verified,
                seen.as_deref(),
                update.as_ref().map(|u| &u.raw_json),
            )
            .map_err(err)?;
            Ok::<_, CmdError>(update)
        }
        .await;
        let auto = {
            let mut s = self.lock();
            s.busy = false;
            s.last_checked = Some(now_secs());
            match result {
                Ok(update) => {
                    let same = matches!((&s.pending, &update), (Some(a), Some(b)) if a.version == b.version);
                    if !same {
                        s.staged = None;
                    }
                    s.pending = update;
                    s.last_error = None;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "update check failed");
                    s.last_error = Some(e.to_string());
                }
            }
            s.settings.auto_install && s.pending.is_some() && s.staged.is_none()
        };
        if let Some(a) = &self.status(app).available {
            tracing::info!(version = %a.version, "update available");
        }
        if auto {
            self.download_staged(app).await;
        }
        Ok(self.publish(app))
    }

    /// Auto-install: fetch and verify now, install at quit (see `install_staged`).
    async fn download_staged(&self, app: &AppHandle) {
        let Some(update) = self.lock().pending.clone() else {
            return;
        };
        self.set_busy(app, true);
        let result = download(app, &update).await;
        let mut s = self.lock();
        s.busy = false;
        match result {
            Ok(bytes) => {
                tracing::info!(version = %update.version, "update downloaded; installs at quit");
                s.staged = Some(bytes);
            }
            Err(e) => s.last_error = Some(e.to_string()),
        }
    }

    /// Install the pending update now and restart. `version` must be the one the user was
    /// shown, so a feed that changed in between can't slip a different release in.
    pub async fn install(&self, app: &AppHandle, version: &str) -> Result<(), CmdError> {
        let _op = self.op.lock().await;
        let (update, staged) = {
            let mut s = self.lock();
            let update = s
                .pending
                .clone()
                .ok_or_else(|| err("there is no update to install"))?;
            if update.version != version {
                return Err(err(format!(
                    "the available update is now {}; review its notes first",
                    update.version
                )));
            }
            (update, s.staged.take())
        };
        self.set_busy(app, true);
        let bytes = match staged {
            Some(b) => Ok(b),
            None => download(app, &update).await,
        };
        let result = bytes.and_then(|b| update.install(b).map_err(err));
        if let Err(e) = result {
            {
                let mut s = self.lock();
                s.busy = false;
                s.last_error = Some(e.to_string());
            }
            self.publish(app);
            return Err(e);
        }
        tracing::info!(version = %update.version, "update installed; restarting");
        // Windows never gets here: the installer takes over and the process exits.
        app.restart()
    }

    /// Called as the app exits: install an update the user opted to auto-install.
    pub fn install_staged(&self) {
        let (update, bytes) = {
            let mut s = self.lock();
            if !s.settings.auto_install {
                return;
            }
            match (s.pending.clone(), s.staged.take()) {
                (Some(u), Some(b)) => (u, b),
                _ => return,
            }
        };
        tracing::info!(version = %update.version, "installing downloaded update at quit");
        if let Err(e) = update.install(bytes) {
            tracing::warn!(error = %e, "update install at quit failed");
        }
    }
}

async fn download(app: &AppHandle, update: &Update) -> Result<Vec<u8>, CmdError> {
    let mut downloaded = 0u64;
    let mut last_emit = 0u64;
    update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                // About every MiB, so the UI isn't flooded.
                if downloaded - last_emit >= 1 << 20 || Some(downloaded) == total {
                    last_emit = downloaded;
                    let _ = app.emit(event::PROGRESS, Progress { downloaded, total });
                }
            },
            || {},
        )
        .await
        .map_err(|e| err(format!("download or signature check failed: {e}")))
}

/// Background checks: once shortly after launch, then daily, unless the channel is off.
pub fn spawn_background_checks(app: &AppHandle) {
    if !configured() {
        tracing::info!("this build has no update key; updates are disabled");
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let updates = app.state::<Updates>();
            if updates.settings().channel != Channel::Off {
                let _ = updates.check(&app).await;
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_map_to_feeds() {
        let stable = feed_url(Channel::Stable).unwrap();
        let beta = feed_url(Channel::Beta).unwrap();
        assert!(stable.starts_with("https://") && stable.ends_with("/stable.json"));
        assert!(beta.starts_with("https://") && beta.ends_with("/beta.json"));
        assert_eq!(feed_url(Channel::Off), None);
    }

    #[test]
    fn event_names_are_valid() {
        for name in [event::STATUS, event::PROGRESS] {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-/:_".contains(c))
            );
        }
    }
}

#[tauri::command]
pub fn updates_status(app: AppHandle, updates: tauri::State<'_, Updates>) -> UpdateStatus {
    updates.status(&app)
}

/// Channel and auto-install. The start-at-login answer is changed only through `login_set`.
#[tauri::command]
pub fn updates_set_settings(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
    channel: Channel,
    auto_install: bool,
) -> Result<UpdateStatus, CmdError> {
    let new = Settings {
        channel,
        auto_install,
        ..updates.settings()
    };
    updates.set_settings(&app, new)
}

#[tauri::command]
pub async fn updates_check(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
) -> Result<UpdateStatus, CmdError> {
    updates.check(&app).await
}

/// Install `version` (the one whose notes the user just saw) and restart.
#[tauri::command]
pub async fn updates_install(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
    version: String,
) -> Result<(), CmdError> {
    updates.install(&app, &version).await
}

/// Answer "start Nugget at login?". Yes registers the per-user service; no after a yes removes
/// it; no otherwise just records the answer so the app stops asking.
#[tauri::command]
pub async fn login_set(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
    enabled: bool,
) -> Result<UpdateStatus, CmdError> {
    let current = updates.settings();
    let message = if enabled || current.start_at_login == Some(true) {
        tauri::async_runtime::spawn_blocking(move || crate::login::run(enabled))
            .await
            .map_err(err)??
    } else {
        String::new()
    };
    if !message.is_empty() {
        tracing::info!(%message, "start at login");
    }
    updates.set_settings(
        &app,
        Settings {
            start_at_login: Some(enabled),
            ..current
        },
    )
}
