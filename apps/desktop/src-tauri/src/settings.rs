//! Desktop-only preferences (update channel, auto-install, start-at-login answer), kept in
//! `settings.json` under the app's config directory. The daemon never reads this file.

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub channel: Channel,
    /// Opt-in: download verified updates in the background and install them when Nugget quits.
    /// Off (the default) means notify only; nothing is downloaded until the user asks.
    pub auto_install: bool,
    /// The user's answer to "start Nugget at login?": `None` until asked.
    pub start_at_login: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            channel: Channel::Stable,
            auto_install: false,
            start_at_login: None,
        }
    }
}

pub fn file_in(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// Missing or unreadable settings fall back to the defaults (notify-only, stable).
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

/// Write via a temporary file and rename, so a crash never leaves half a file.
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
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
        assert!(!s.auto_install);
        assert_eq!(s.start_at_login, None);
        // Older or partial files fill in the rest from the defaults.
        let s: Settings = serde_json::from_str(r#"{"channel":"beta"}"#).unwrap();
        assert_eq!(s.channel, Channel::Beta);
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
            auto_install: true,
            start_at_login: Some(false),
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), Settings::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
