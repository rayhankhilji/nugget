//! The app's own folders, named after its bundle ID, move from the pre-rename ID
//! (`dev.nugget.desktop`) to `app.heed.desktop` before the webview starts: the app data folder
//! (window state, settings) and, on macOS, WebKit's folder, which holds the UI's local storage
//! (onboarding progress, preferences). Copied, checked, marked and never deleted
//! ([`nugget_core::migrate`]). The daemon's folders move separately (`Paths::discover`).

use std::path::{Path, PathBuf};

use nugget_core::identity::{BUNDLE_ID, LEGACY_BUNDLE_ID};
use nugget_core::migrate::{self, Move, Outcome, Rules};

/// Moves each folder; returns what happened, for the log (which starts after this).
pub fn migrate() -> Vec<(Move, Result<Outcome, String>)> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let os = if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Linux
    };
    let rules = Rules {
        // WebKit's and Tauri's lock files belong to a running process.
        skip: &|p: &Path| {
            p.extension()
                .is_some_and(|e| e == "lock" || e == "LOCK" || e == "lck")
        },
        link: &|_| false,
    };
    moves(os, &env)
        .into_iter()
        .map(|m| {
            let r = migrate::migrate_dir(&m, &rules).map_err(|e| e.to_string());
            (m, r)
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum Os {
    Mac,
    Linux,
    Windows,
}

/// The folders Tauri and the webview keep under the bundle ID, old and new, from the
/// environment (`HOME`, `XDG_*_HOME`, `APPDATA`, `LOCALAPPDATA`).
fn moves(os: Os, env: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<Move> {
    let home = env("HOME");
    let bases: Vec<PathBuf> = match os {
        Os::Mac => home
            .map(|h| {
                vec![
                    h.join("Library/Application Support"),
                    h.join("Library/WebKit"),
                ]
            })
            .unwrap_or_default(),
        Os::Linux => {
            let xdg = |k: &str, rel: &str| {
                env(k)
                    .filter(|p| p.is_absolute())
                    .or_else(|| home.as_ref().map(|h| h.join(rel)))
            };
            [
                xdg("XDG_DATA_HOME", ".local/share"),
                xdg("XDG_CONFIG_HOME", ".config"),
            ]
            .into_iter()
            .flatten()
            .collect()
        }
        Os::Windows => [env("APPDATA"), env("LOCALAPPDATA")]
            .into_iter()
            .flatten()
            .collect(),
    };
    bases
        .into_iter()
        .map(|b| Move {
            from: b.join(LEGACY_BUNDLE_ID),
            to: b.join(BUNDLE_ID),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            vars.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    #[test]
    fn tauri_builds_the_app_under_the_frozen_bundle_id() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], BUNDLE_ID);
        let linux = format!(
            "/usr/share/applications/{}.desktop",
            nugget_core::identity::LINUX_APP_ID
        );
        assert!(
            conf["bundle"]["linux"]["deb"]["files"]
                .get(&linux)
                .is_some(),
            "{linux}"
        );
    }

    #[test]
    fn mac_moves_app_support_and_webkit() {
        let m = moves(Os::Mac, &env(&[("HOME", "/Users/me")]));
        assert_eq!(
            m,
            vec![
                Move {
                    from: "/Users/me/Library/Application Support/dev.nugget.desktop".into(),
                    to: "/Users/me/Library/Application Support/app.heed.desktop".into(),
                },
                Move {
                    from: "/Users/me/Library/WebKit/dev.nugget.desktop".into(),
                    to: "/Users/me/Library/WebKit/app.heed.desktop".into(),
                },
            ]
        );
    }

    #[test]
    fn linux_follows_xdg_and_windows_both_appdata_folders() {
        let m = moves(
            Os::Linux,
            &env(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/cfg")]),
        );
        assert_eq!(
            m[0].to,
            PathBuf::from("/home/me/.local/share/app.heed.desktop")
        );
        assert_eq!(m[1].from, PathBuf::from("/cfg/dev.nugget.desktop"));
        let w = moves(
            Os::Windows,
            &env(&[("APPDATA", r"C:\R"), ("LOCALAPPDATA", r"C:\L")]),
        );
        assert_eq!(w.len(), 2);
        assert!(w.iter().all(|m| m.to.ends_with("app.heed.desktop")));
    }

    #[test]
    fn the_webviews_storage_survives_the_rename() {
        let root = std::env::temp_dir().join(format!("heed-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("Library/WebKit/dev.nugget.desktop/WebsiteData/LocalStorage");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join("file__0.localstorage"),
            b"nugget.onboarding.v1=done",
        )
        .unwrap();
        std::fs::write(old.join("file__0.localstorage-lck"), b"").unwrap();
        let home = root.to_string_lossy().into_owned();
        for m in moves(Os::Mac, &env(&[("HOME", &home)])) {
            migrate::migrate_dir(&m, &Rules::default()).unwrap();
        }
        let new = root.join("Library/WebKit/app.heed.desktop/WebsiteData/LocalStorage");
        assert_eq!(
            std::fs::read(new.join("file__0.localstorage")).unwrap(),
            b"nugget.onboarding.v1=done"
        );
        assert!(
            old.join("file__0.localstorage").exists(),
            "the old copy stays"
        );
    }
}
