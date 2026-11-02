//! Desktop-only preferences (update channel, auto-install, start-at-login answer, autostart),
//! kept in `settings.json` under the app's config directory. The daemon never reads this file.
//!
//! The update channel counts only once the user has chosen it (`channel_chosen`): until then
//! the app contacts no update server. Files written before that flag existed can't tell a
//! picked Stable from the old default, so a missing flag means "chosen" only for Beta and Off,
//! which were never defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which release feed the updater follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Published, non-prerelease versions.
    #[default]
    Stable,
    /// Prereleases as well as stable versions.
    Beta,
    /// Never contact the update server.
    Off,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredSettings")]
pub struct Settings {
    /// What the update settings show; it takes effect only once `channel_chosen`.
    pub channel: Channel,
    /// The user picked the channel (in setup, the Updates card, or by any change to it). Until
    /// then nothing checks for updates, whatever `channel` says.
    pub channel_chosen: bool,
    /// Opt-in: download verified updates in the background and install them when Heed quits.
    /// Off (the default) means notify only; nothing is downloaded until the user asks.
    pub auto_install: bool,
    /// The user's answer to "start Heed at login?": `None` until asked.
    pub start_at_login: Option<bool>,
    /// Open the desktop app (hidden in the tray) at login. Distinct from `start_at_login`, which
    /// is about the `nuggetd` service. Mirrors the OS entry `autostart.rs` writes.
    pub autostart: bool,
}

impl Settings {
    /// The channel to check, or `None` when updates are off or not chosen yet.
    pub fn active_channel(&self) -> Option<Channel> {
        Some(self.channel).filter(|c| self.channel_chosen && *c != Channel::Off)
    }
}

/// `settings.json` as read, with every field optional (older or partial files).
#[derive(Default, Deserialize)]
#[serde(default)]
struct StoredSettings {
    channel: Channel,
    channel_chosen: Option<bool>,
    auto_install: bool,
    start_at_login: Option<bool>,
    autostart: bool,
}

impl From<StoredSettings> for Settings {
    fn from(s: StoredSettings) -> Self {
        Self {
            channel: s.channel,
            // No flag: written before it existed. Stable was the silent default, so it
            // doesn't count as a choice; Beta and Off were never defaults, so they do.
            channel_chosen: s.channel_chosen.unwrap_or(s.channel != Channel::Stable),
            auto_install: s.auto_install,
            start_at_login: s.start_at_login,
            autostart: s.autostart,
        }
    }
}

pub fn file_in(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// Missing or unreadable settings fall back to the defaults (notify-only, no channel chosen).
pub fn load(path: &Path) -> Settings {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable settings");
            Settings::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read settings");
            Settings::default()
        }
    }
}

pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    write_json(path, settings)
}

/// Write `value` as JSON via a temporary file and rename, so a crash never leaves half a file.
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_notify_only_on_stable() {
        let s = Settings::default();
        assert_eq!(s.channel, Channel::Stable);
        assert_eq!(s.active_channel(), None);
        assert!(!s.auto_install);
    }

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir =
            std::env::temp_dir().join(format!("nugget-desktop-settings-{}", std::process::id()));
        let path = file_in(&dir);
        assert_eq!(load(&path), Settings::default());
        let s = Settings {
            channel: Channel::Off,
            channel_chosen: true,
            auto_install: true,
            start_at_login: Some(false),
            autostart: true,
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), Settings::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_without_the_flag_count_stable_as_unchosen() {
        let parse = |json: &str| serde_json::from_str::<Settings>(json).unwrap();
        // The old default: no choice was ever made, so nothing is checked.
        let s = parse(r#"{"channel":"stable","auto_install":false,"start_at_login":true}"#);
        assert_eq!((s.channel, s.channel_chosen), (Channel::Stable, false));
        assert_eq!(s.active_channel(), None);
        assert_eq!(s.start_at_login, Some(true));
        // Beta and Off were never defaults: the user picked them.
        let s = parse(r#"{"channel":"beta"}"#);
        assert_eq!(
            (s.channel_chosen, s.active_channel()),
            (true, Some(Channel::Beta))
        );
        let s = parse(r#"{"channel":"off"}"#);
        assert_eq!((s.channel_chosen, s.active_channel()), (true, None));
        // No channel at all is the default, unchosen.
        assert_eq!(parse("{}"), Settings::default());
    }

    #[test]
    fn the_flag_round_trips() {
        for (channel, chosen, active) in [
            (Channel::Stable, true, Some(Channel::Stable)),
            (Channel::Beta, false, None),
            (Channel::Off, true, None),
        ] {
            let s = Settings {
                channel,
                channel_chosen: chosen,
                ..Settings::default()
            };
            let json = serde_json::to_string(&s).unwrap();
            assert!(json.contains("\"channel_chosen\""), "{json}");
            let back: Settings = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
            assert_eq!(back.active_channel(), active);
        }
    }
}
