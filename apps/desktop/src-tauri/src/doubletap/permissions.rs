//! Permission onboarding per OS and the `DoubleTapStatus` the settings page shows.
//!
//! - **macOS:** reading and inserting need Accessibility (`nugget_a11y` asks with the system
//!   prompt). The trigger itself uses NSEvent modifier monitors or a Carbon hot key; the
//!   hotkey crate does not report an Input Monitoring state, so it is `unknown`.
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
    pub hint: Option<&'static str>,
    pub modifiers: Vec<ModifierOption>,
    pub recommended_trigger: TriggerSpec,
    /// Where the running listener's shortcut stands; `None` when nothing is listening.
    pub shortcut: Option<ShortcutStatus>,
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

/// Input Monitoring is a macOS notion; the hotkey crate does not report it.
pub fn input_monitoring(platform: Platform) -> PermissionState {
    match platform {
        Platform::MacOs => PermissionState::Unknown,
        _ => PermissionState::NotRequired,
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
    fn hints_per_os() {
        let tap = TriggerSpec::DoubleTap {
            modifier: ModifierId::RightControl,
            window_ms: 350,
        };
        let chord = TriggerSpec::Chord {
            chord: "Ctrl+Alt+Space".into(),
        };
        use PermissionState::*;
        assert_eq!(
            hint_for(Platform::Linux, Session::Wayland, Granted, &tap, None),
            Some(HINT_WAYLAND_CHORD)
        );
        assert_eq!(
            hint_for(Platform::Linux, Session::Wayland, Denied, &chord, None),
            Some(HINT_ATSPI)
        );
        assert_eq!(
            hint_for(Platform::Linux, Session::X11, Granted, &tap, None),
            None
        );
        assert_eq!(
            hint_for(Platform::MacOs, Session::MacOs, Denied, &tap, None),
            Some(HINT_ACCESSIBILITY)
        );
        assert_eq!(
            hint_for(Platform::Windows, Session::Windows, NotRequired, &tap, None),
            None
        );
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

    #[test]
    fn names_and_modifiers() {
        assert_eq!(session_name(Session::Headless), "unknown");
        assert_eq!(session_name(Session::Wayland), "wayland");
        assert_eq!(platform_name(Platform::MacOs), "macos");
        let mods = modifiers(Platform::Windows);
        assert_eq!(mods.len(), 8);
        assert!(mods.contains(&ModifierOption {
            id: "right_command",
            label: "Right Win"
        }));
        assert_eq!(input_monitoring(Platform::MacOs), PermissionState::Unknown);
        assert_eq!(
            input_monitoring(Platform::Linux),
            PermissionState::NotRequired
        );
        assert_eq!(
            serde_json::to_value(PermissionState::NotRequired).unwrap(),
            "not_required"
        );
    }
}
