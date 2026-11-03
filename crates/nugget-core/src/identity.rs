//! The app's frozen identity (bundle ID, keychain service, directory names, user agent) and the
//! names it had before the rename to Heed, which the first launch migrates from (see
//! [`crate::migrate`]). These cannot change after the first public release: macOS keys
//! permissions, keychain access and app data to the bundle ID.

/// macOS bundle ID and Tauri identifier of the desktop app.
pub const BUNDLE_ID: &str = "app.heed.desktop";
/// The bundle ID before the rename.
pub const LEGACY_BUNDLE_ID: &str = "dev.nugget.desktop";

/// Linux application ID (the `.desktop` file name, the Wayland and portal app ID).
pub const LINUX_APP_ID: &str = "app.heed.Heed";
/// The Linux application ID before the rename.
pub const LEGACY_LINUX_APP_ID: &str = "dev.nugget.Nugget";

/// `directories::ProjectDirs` parts for the daemon's data, config and cache directories.
pub const DIRS: (&str, &str, &str) = ("app", "heed", "Heed");
/// The directory parts before the rename.
pub const LEGACY_DIRS: (&str, &str, &str) = ("dev", "Nugget", "Nugget");

/// `User-Agent` for every HTTP request Heed makes: the product and version, nothing else.
pub const USER_AGENT: &str = concat!("Heed/", env!("CARGO_PKG_VERSION"));

/// Environment variables: `HEED_*` is the primary prefix; `NUGGET_*` still works.
pub const ENV_PREFIX: &str = "HEED_";
/// The prefix before the rename, read when the `HEED_*` name is not set.
pub const LEGACY_ENV_PREFIX: &str = "NUGGET_";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_the_product_and_version_only() {
        let v = USER_AGENT.strip_prefix("Heed/").expect("Heed/<version>");
        assert!(
            !v.is_empty()
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c)),
            "{USER_AGENT}"
        );
    }

    #[test]
    fn the_frozen_names() {
        // Changing these after the first release orphans permissions, keychain items and data.
        assert_eq!(BUNDLE_ID, "app.heed.desktop");
        assert_eq!(LINUX_APP_ID, "app.heed.Heed");
        assert_eq!(crate::secrets::KEYCHAIN_SERVICE, "app.heed.Heed");
        assert_eq!(DIRS, ("app", "heed", "Heed"));
    }
}
