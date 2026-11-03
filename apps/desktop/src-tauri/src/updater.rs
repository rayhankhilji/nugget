//! User-controlled updates on top of `tauri-plugin-updater`.
//!
//! - The user picks a channel (stable, beta or off). Off never contacts the update server, and
//!   neither does a channel nobody chose: until the user answers (in setup or on the Updates
//!   card), no check runs (`Settings::channel_chosen`).
//! - Every request a check or download makes goes into the daemon's egress ledger
//!   (`update_check`, `update_download`) through `Daemon::record_egress`.
//! - The default is notify-only: a check finds a newer version and shows it (tray item plus the
//!   settings panel, with the release notes). Nothing is downloaded until the user clicks
//!   Install, and Install names the exact version whose notes were shown.
//! - Auto-install is opt-in: the update is downloaded and verified in the background and installed
//!   when Heed quits, and the panel shows its notes in the meantime.
//! - Every package is verified against the minisign public key compiled into this build
//!   (`NUGGET_UPDATER_PUBKEY`, set by the release workflow). Builds without a key (dev builds,
//!   self-built packages) have no updater at all.
//! - The channel feed is signed with the same key (`<feed>.sig`) and verified before anything in
//!   it is read; a feed that doesn't verify is refused (see `feed`).

mod feed;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nugget_core::ipc::EgressRecordRequest;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use time::OffsetDateTime;

use crate::daemon::{CmdError, Daemon};
use crate::settings::{self, Channel, Settings};

/// minisign public key (the base64 text `tauri signer generate` prints), baked in at build time
/// (`HEED_UPDATER_PUBKEY`, or `NUGGET_UPDATER_PUBKEY`).
const PUBKEY: Option<&str> = match option_env!("HEED_UPDATER_PUBKEY") {
    Some(v) => Some(v),
    None => option_env!("NUGGET_UPDATER_PUBKEY"),
};
/// Where the per-channel manifests live: the release workflow keeps `stable.json` and
/// `beta.json` on a rolling `updater-feed` release and moves them only when a maintainer
/// publishes a release. Forks override this at build time.
const DEFAULT_FEED: &str = "https://github.com/rayhankhilji/nugget/releases/download/updater-feed";
const FEED: Option<&str> = match option_env!("HEED_UPDATER_FEED") {
    Some(v) => Some(v),
    None => option_env!("NUGGET_UPDATER_FEED"),
};

/// First background check shortly after launch (not at launch: startup stays offline-fast),
/// then once a day.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Egress-ledger features (`nugget_core::ipc::CLIENT_EGRESS_FEATURES`).
const FEATURE_CHECK: &str = "update_check";
const FEATURE_DOWNLOAD: &str = "update_download";

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
    /// Downloaded and verified; installs when Heed quits (auto-install only).
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
    /// Wakes the background checks when the user turns updates on.
    wake: tokio::sync::Notify,
}

pub fn configured() -> bool {
    PUBKEY.is_some_and(|k| !k.trim().is_empty())
}

/// What a check would fetch, with the build's key: nothing unless this build has a key and the
/// user chose a channel other than Off.
fn check_target(
    settings: &Settings,
    pubkey: Option<&'static str>,
) -> Option<(&'static str, String)> {
    let pubkey = pubkey.filter(|k| !k.trim().is_empty())?;
    Some((pubkey, feed_url(settings.active_channel()?)?))
}

/// Put requests the updater made into the daemon's egress ledger.
async fn ledger(app: &AppHandle, feature: &str, at: OffsetDateTime, requests: Vec<feed::Exchange>) {
    let daemon = app.state::<Daemon>();
    for r in requests {
        let row = EgressRecordRequest {
            at: Some(at),
            host: r.host,
            feature: feature.to_owned(),
            bytes_sent: r.bytes_sent,
            bytes_received: r.bytes_received,
        };
        daemon.record_egress(app, row).await;
    }
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
            wake: tokio::sync::Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    /// The user's answer to "start Heed at login?" (the per-user `nuggetd` service).
    pub fn start_at_login(&self) -> Option<bool> {
        self.lock().settings.start_at_login
    }

    /// An update is downloaded and will install as Heed quits (`install_staged`).
    pub fn has_staged(&self) -> bool {
        let s = self.lock();
        s.settings.auto_install && s.pending.is_some() && s.staged.is_some()
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
        let turned_on = {
            let mut s = self.lock();
            let turned_on = new.active_channel().is_some()
                && new.active_channel() != s.settings.active_channel();
            if s.settings.channel != new.channel {
                s.pending = None;
                s.staged = None;
                s.last_error = None;
            }
            if !new.auto_install {
                s.staged = None;
            }
            s.settings = new;
            turned_on
        };
        if turned_on {
            self.wake.notify_one();
        }
        Ok(self.publish(app))
    }

    /// Ask the channel's feed whether a newer version exists. Never downloads in notify-only mode.
    /// Contacts nothing while updates are off or no channel was chosen.
    pub async fn check(&self, app: &AppHandle) -> Result<UpdateStatus, CmdError> {
        let _op = self.op.lock().await;
        let target = check_target(&self.lock().settings, PUBKEY);
        let Some((pubkey, url)) = target else {
            return Ok(self.publish(app));
        };
        self.set_busy(app, true);
        let result = async {
            // Verify the feed before the plugin reads it; then accept the plugin's answer only
            // if it came from that same feed (`feed::consistent`).
            let at = OffsetDateTime::now_utc();
            let mut requests = Vec::new();
            let fetched = feed::fetch_verified(&url, pubkey, HTTP_TIMEOUT, &mut requests).await;
            ledger(app, FEATURE_CHECK, at, requests).await;
            let verified = fetched.map_err(err)?;
            let seen = Arc::new(Mutex::new(None::<String>));
            let record = Arc::clone(&seen);
            let want = verified.version.clone();
            let updater = app
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
                .map_err(err)?;
            // The plugin fetches the feed again, through the same hosts; it read the same bytes
            // when the check below finds it consistent, and the byte counts are ours otherwise.
            let at = OffsetDateTime::now_utc();
            let checked = updater.check().await;
            let mut repeat = verified.fetched_by.clone();
            if checked.is_err() {
                repeat.iter_mut().for_each(|r| r.bytes_received = 0);
            }
            ledger(app, FEATURE_CHECK, at, repeat).await;
            let update = checked.map_err(err)?;
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
            // The user switched channel (or turned updates off) while this check ran:
            // `set_settings` already cleared the old answer; don't bring it back.
            if check_target(&s.settings, PUBKEY).map(|(_, u)| u).as_deref() != Some(url.as_str()) {
                drop(s);
                return Ok(self.publish(app));
            }
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
        let update = {
            let s = self.lock();
            if s.settings.active_channel().is_none() || !s.settings.auto_install {
                return;
            }
            let Some(update) = s.pending.clone() else {
                return;
            };
            update
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
            Err(e) => {
                tracing::warn!(version = %update.version, error = %e, "update download failed");
                s.last_error = Some(e.to_string());
            }
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
        // on macOS and Linux nothing else stops the daemon the app started, which would
        // keep serving the new app from the old binary. (On Windows the installer stops it.) A
        // login service is restarted on the new version after the restart instead.
        let stopped = bytes.is_ok()
            && cfg!(not(windows))
            && !matches!(
                crate::lifecycle::stop_owned(app, "update").await,
                crate::lifecycle::StopOutcome::Left(_)
            );
        let result = bytes.and_then(|b| update.install(b).map_err(err));
        if let Err(e) = result {
            if stopped {
                // Nothing was replaced: bring the daemon back.
                let _ = app.state::<Daemon>().start(app).await;
            }
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
    let at = OffsetDateTime::now_utc();
    let url = update.download_url.as_str().to_owned();
    let result = update
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
        .await;
    // The host the feed names; the plugin may be redirected to a content host it doesn't report.
    let request = feed::Exchange {
        host: reqwest::Url::parse(&url)
            .map_or_else(|_| "unknown".to_owned(), |u| feed::host_of(&u)),
        bytes_sent: url.len() as u64 + 160,
        bytes_received: downloaded,
    };
    ledger(app, FEATURE_DOWNLOAD, at, vec![request]).await;
    result.map_err(|e| err(format!("download or signature check failed: {e}")))
}

/// Background checks: once shortly after launch, then daily, only while the user has chosen a
/// channel other than Off. Choosing one checks right away.
pub fn spawn_background_checks(app: &AppHandle) {
    if !configured() {
        tracing::info!("this build has no update key; updates are disabled");
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut wait = FIRST_CHECK_DELAY;
        loop {
            let updates = app.state::<Updates>();
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = updates.wake.notified() => {}
            }
            if updates.settings().active_channel().is_some() {
                let _ = updates.check(&app).await;
            }
            wait = CHECK_INTERVAL;
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
    fn nothing_is_checked_until_a_channel_is_chosen() {
        const KEY: Option<&str> = Some("a-key");
        let unchosen = Settings::default();
        assert_eq!(check_target(&unchosen, KEY), None);
        // An old settings file with the former default is unchosen too.
        let old: Settings = serde_json::from_str(r#"{"channel":"stable"}"#).unwrap();
        assert_eq!(check_target(&old, KEY), None);
        let chose = |channel| Settings {
            channel,
            channel_chosen: true,
            ..Settings::default()
        };
        let (key, url) = check_target(&chose(Channel::Stable), KEY).unwrap();
        assert_eq!((key, url.ends_with("/stable.json")), ("a-key", true));
        assert!(check_target(&chose(Channel::Beta), KEY).is_some());
        assert_eq!(check_target(&chose(Channel::Off), KEY), None);
        // A build without a key never checks.
        assert_eq!(check_target(&chose(Channel::Stable), None), None);
        assert_eq!(check_target(&chose(Channel::Stable), Some(" ")), None);
    }

    #[test]
    fn ledger_features_are_ones_the_daemon_accepts() {
        for f in [FEATURE_CHECK, FEATURE_DOWNLOAD] {
            assert!(nugget_core::ipc::CLIENT_EGRESS_FEATURES.contains(&f), "{f}");
        }
    }
}

#[tauri::command]
pub fn updates_status(app: AppHandle, updates: tauri::State<'_, Updates>) -> UpdateStatus {
    updates.status(&app)
}

/// Channel and auto-install; this marks the channel as chosen. The start-at-login answer is
/// changed only through `login_set`.
#[tauri::command]
pub fn updates_set_settings(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
    channel: Channel,
    auto_install: bool,
) -> Result<UpdateStatus, CmdError> {
    // Setting the channel, here or in setup, is the user's choice.
    let new = Settings {
        channel,
        channel_chosen: true,
        auto_install,
        ..updates.settings()
    };
    updates.set_settings(&app, new)
}

/// "Check now". Needs a chosen channel: the UI asks first.
#[tauri::command]
pub async fn updates_check(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
) -> Result<UpdateStatus, CmdError> {
    if !updates.settings().channel_chosen {
        return Err(err("choose an update channel first"));
    }
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

/// Answer "start Heed at login?". Yes registers the per-user service; no after a yes removes
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
