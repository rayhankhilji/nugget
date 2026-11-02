//! Permission onboarding per OS and the `DoubleTapStatus` the settings page shows.
//!
//! - **macOS:** reading and inserting need Accessibility (`nugget_a11y` asks with the system
//!   prompt). The trigger itself uses NSEvent modifier monitors or a Carbon hot key; Input
//!   Monitoring is read with `CGPreflightListenEventAccess` (`nugget_hotkey::input_monitoring`).
//!   Full Disk Access is reported too: the Messages source needs it (nuggetd, started by the app,
//!   reads `~/Library/Messages`, and macOS attributes that to Heed.app), and the page shows it
//!   only when such a source is set up.
//! - **Linux:** AT-SPI must be enabled for the session (`doubletap.hint.atspi`). Wayland cannot
//!   observe modifier double taps, so a double-tap trigger there gets
//!   `doubletap.hint.wayland_chord`. A chord goes through the desktop's GlobalShortcuts portal:
//!   without one (sway, older portals) it cannot start (`doubletap.hint.wayland_no_portal`), and
//!   with one [`ShortcutStatus`] says where the desktop's consent stands.
//! - **Windows:** UI Automation needs no permission.

use nugget_a11y::Permission;
use nugget_hotkey::{Modifier, Platform, Session, ShortcutState};
use serde::Serialize;

use super::settings::{ModifierId, TriggerSpec};

pub const HINT_ATSPI: &str = "doubletap.hint.atspi";
pub const HINT_WAYLAND_CHORD: &str = "doubletap.hint.wayland_chord";
pub const HINT_WAYLAND_NO_PORTAL: &str = "doubletap.hint.wayland_no_portal";
pub const HINT_ACCESSIBILITY: &str = "doubletap.hint.accessibility";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Granted,
    Denied,
    NotRequired,
    Unknown,
}

impl From<Permission> for PermissionState {
    fn from(p: Permission) -> Self {
        match p {
            Permission::Granted => Self::Granted,
            Permission::Denied => Self::Denied,
            Permission::NotRequired => Self::NotRequired,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModifierOption {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DoubleTapStatus {
    pub platform: &'static str,
    pub session: &'static str,
    pub listening: bool,
    pub trigger_error: Option<String>,
    pub accessibility: PermissionState,
    pub input_monitoring: PermissionState,
    /// macOS only (`not_required` elsewhere): whether Heed can read the folders macOS keeps
    /// behind Full Disk Access (Messages, Mail, Safari).
    pub full_disk_access: PermissionState,
    pub hint: Option<&'static str>,
    pub modifiers: Vec<ModifierOption>,
    pub recommended_trigger: TriggerSpec,
    /// Where the running listener's shortcut stands; `None` when nothing is listening.
    pub shortcut: Option<ShortcutStatus>,
    /// Ghost text: whether the suggestion keys work here (else Double Tap uses the panel).
    pub ghost: super::ghost_flow::GhostStatus,
}

/// A running listener's [`ShortcutState`] for the settings page. Only the Wayland portal moves
/// through the states; every other backend is `active` with no `trigger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortcutStatus {
    /// `active`, `awaiting_consent`, `not_assigned`, `rebinding` or `failed`.
    pub state: &'static str,
    /// `active` only: the key the desktop bound, when it reports one that differs from the
    /// configured chord (desktops may keep a key the user chose in their own settings).
    pub trigger: Option<String>,
    /// `failed` only: why.
    pub error: Option<String>,
}

pub fn shortcut_status(state: ShortcutState, configured: &TriggerSpec) -> ShortcutStatus {
    let (state, trigger, error) = match state {
        ShortcutState::Active { trigger } => {
            let differs = |t: &String| match configured {
                TriggerSpec::Chord { chord } => !same_chord(t, chord),
                TriggerSpec::DoubleTap { .. } => true,
            };
            ("active", trigger.filter(differs), None)
        }
        ShortcutState::AwaitingConsent => ("awaiting_consent", None, None),
        ShortcutState::NotAssigned => ("not_assigned", None, None),
        ShortcutState::Rebinding => ("rebinding", None, None),
        ShortcutState::Failed(e) => ("failed", None, Some(e)),
        other => ("failed", None, Some(format!("{other:?}"))),
    };
    ShortcutStatus {
        state,
        trigger,
        error,
    }
}

/// Whether two key descriptions name the same keys, ignoring case, order, separators and common
/// spellings (`Ctrl+Alt+Space` and the portal's `<Control><Alt>space`).
fn same_chord(a: &str, b: &str) -> bool {
    fn keys(s: &str) -> Vec<String> {
        let mut k: Vec<String> = s
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| {
                let t = t.to_lowercase();
                match t.as_str() {
                    "control" | "primary" | "cmdorctrl" => "ctrl".into(),
                    "option" => "alt".into(),
                    "meta" | "logo" | "win" | "cmd" | "command" => "super".into(),
                    "return" => "enter".into(),
                    _ => t,
                }
            })
            .collect();
        k.sort();
        k.dedup();
        k
    }
    keys(a) == keys(b)
}

/// Whether a recorded trigger error is `nugget_core::Error::Unsupported`: this session cannot
/// observe the trigger at all.
fn is_unsupported(error: &str) -> bool {
    let prefix = nugget_core::Error::Unsupported(String::new()).to_string();
    error.starts_with(prefix.trim_end())
}

pub fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::MacOs => "macos",
        Platform::Windows => "windows",
        Platform::Linux => "linux",
    }
}

pub fn session_name(s: Session) -> &'static str {
    match s {
        Session::MacOs => "macos",
        Session::Windows => "windows",
        Session::X11 => "x11",
        Session::Wayland => "wayland",
        Session::Headless | Session::Other => "unknown",
    }
}

pub fn modifiers(platform: Platform) -> Vec<ModifierOption> {
    ModifierId::ALL
        .iter()
        .map(|&m| ModifierOption {
            id: m.as_str(),
            label: Modifier::from(m).label_for(platform),
        })
        .collect()
}

/// Input Monitoring is a macOS notion (`CGPreflightListenEventAccess`).
pub fn input_monitoring(platform: Platform) -> PermissionState {
    match (platform, nugget_hotkey::input_monitoring()) {
        (Platform::MacOs, Some(true)) => PermissionState::Granted,
        (Platform::MacOs, Some(false)) => PermissionState::Denied,
        (Platform::MacOs, None) => PermissionState::Unknown,
        _ => PermissionState::NotRequired,
    }
}

/// Places under the home folder that macOS keeps behind Full Disk Access. Reading any one of
/// them answers for all: macOS has no API to ask, so this is how apps find out.
const FULL_DISK_ACCESS_PROBES: [&str; 4] = [
    "Library/Messages",
    "Library/Mail",
    "Library/Safari",
    "Library/Application Support/com.apple.TCC",
];

/// Full Disk Access is a macOS notion. Reads (never writes) a few protected folders; reading
/// them never prompts.
pub fn full_disk_access(platform: Platform) -> PermissionState {
    if platform != Platform::MacOs {
        return PermissionState::NotRequired;
    }
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        return PermissionState::Unknown;
    };
    let home = std::path::PathBuf::from(home);
    full_disk_access_from(
        FULL_DISK_ACCESS_PROBES
            .iter()
            .map(|p| std::fs::read_dir(home.join(p)).map(drop)),
    )
}

/// One refused read means no access (macOS answers `EPERM`); otherwise one that worked means
/// access. Folders that are missing (no Mail account ever set up) say nothing.
fn full_disk_access_from(reads: impl Iterator<Item = std::io::Result<()>>) -> PermissionState {
    let mut granted = false;
    for read in reads {
        match read {
            Ok(()) => granted = true,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return PermissionState::Denied;
            }
            Err(_) => {}
        }
    }
    if granted {
        PermissionState::Granted
    } else {
        PermissionState::Unknown
    }
}

/// The onboarding hint to show, most blocking first.
pub fn hint_for(
    platform: Platform,
    session: Session,
    accessibility: PermissionState,
    trigger: &TriggerSpec,
    trigger_error: Option<&str>,
) -> Option<&'static str> {
    match platform {
        Platform::Linux if session == Session::Wayland && trigger.is_double_tap() => {
            Some(HINT_WAYLAND_CHORD)
        }
        // A chord the session cannot observe: no GlobalShortcuts portal.
        Platform::Linux
            if session == Session::Wayland && trigger_error.is_some_and(is_unsupported) =>
        {
            Some(HINT_WAYLAND_NO_PORTAL)
        }
        Platform::Linux if accessibility == PermissionState::Denied => Some(HINT_ATSPI),
        Platform::MacOs if accessibility == PermissionState::Denied => Some(HINT_ACCESSIBILITY),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_disk_access_from_reads() {
        use std::io::{Error, ErrorKind};
        let ok = || Ok(());
        let missing = || Err(Error::from(ErrorKind::NotFound));
        let refused = || Err(Error::from(ErrorKind::PermissionDenied));
        let state = |reads: Vec<std::io::Result<()>>| full_disk_access_from(reads.into_iter());
        assert_eq!(state(vec![ok(), missing()]), PermissionState::Granted);
        assert_eq!(state(vec![missing(), refused()]), PermissionState::Denied);
        assert_eq!(state(vec![ok(), refused()]), PermissionState::Denied);
        assert_eq!(state(vec![missing(), missing()]), PermissionState::Unknown);
        assert_eq!(state(vec![]), PermissionState::Unknown);
    }

    #[test]
    fn macos_only_permissions_elsewhere() {
        for p in [Platform::Linux, Platform::Windows] {
            assert_eq!(input_monitoring(p), PermissionState::NotRequired);
            assert_eq!(full_disk_access(p), PermissionState::NotRequired);
        }
    }

    #[test]
    fn wayland_without_a_portal() {
        let chord = TriggerSpec::Chord {
            chord: "Ctrl+Alt+Space".into(),
        };
        let unsupported = nugget_core::Error::Unsupported("no GlobalShortcuts".into()).to_string();
        let taken = nugget_core::Error::Other("already registered".into()).to_string();
        let hint = |session, a11y, error: &str| {
            hint_for(Platform::Linux, session, a11y, &chord, Some(error))
        };
        use PermissionState::*;
        assert_eq!(
            hint(Session::Wayland, Granted, &unsupported),
            Some(HINT_WAYLAND_NO_PORTAL)
        );
        // Before AT-SPI: without a trigger nothing else matters yet.
        assert_eq!(
            hint(Session::Wayland, Denied, &unsupported),
            Some(HINT_WAYLAND_NO_PORTAL)
        );
        assert_eq!(hint(Session::Wayland, Granted, &taken), None);
        assert_eq!(hint(Session::X11, Granted, &unsupported), None);
    }

    #[test]
    fn shortcut_states() {
        let chord = TriggerSpec::Chord {
            chord: "Ctrl+Alt+Space".into(),
        };
        let st = |s| shortcut_status(s, &chord);
        let active = |t: Option<&str>| ShortcutState::Active {
            trigger: t.map(Into::into),
        };
        assert_eq!(st(active(None)).state, "active");
        assert_eq!(st(active(Some("<Control><Alt>space"))).trigger, None);
        assert_eq!(st(active(Some("CTRL+ALT+SPACE"))).trigger, None);
        assert_eq!(
            st(active(Some("Super+D"))).trigger.as_deref(),
            Some("Super+D")
        );
        assert_eq!(st(ShortcutState::AwaitingConsent).state, "awaiting_consent");
        assert_eq!(st(ShortcutState::NotAssigned).state, "not_assigned");
        assert_eq!(st(ShortcutState::Rebinding).state, "rebinding");
        let failed = st(ShortcutState::Failed("portal gone".into()));
        assert_eq!(
            (failed.state, failed.error.as_deref()),
            ("failed", Some("portal gone"))
        );
        assert_eq!(
            serde_json::to_value(st(active(Some("Super+D")))).unwrap(),
            serde_json::json!({ "state": "active", "trigger": "Super+D", "error": null })
        );
    }
}
