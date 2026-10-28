//! Permission onboarding per OS and the `DoubleTapStatus` the settings page shows.
//!
//! - **macOS:** reading and inserting need Accessibility (`nugget_a11y` asks with the system
//!   prompt). The trigger itself uses NSEvent modifier monitors or a Carbon hot key; the
//!   hotkey crate does not report an Input Monitoring state, so it is `unknown`.
//! - **Linux:** AT-SPI must be enabled for the session (`doubletap.hint.atspi`). Wayland cannot
//!   observe modifier double taps, so a double-tap trigger there gets
//!   `doubletap.hint.wayland_chord`.
//! - **Windows:** UI Automation needs no permission.

use nugget_a11y::Permission;
use nugget_hotkey::{Modifier, Platform, Session};
use serde::Serialize;

use super::settings::{ModifierId, TriggerSpec};

pub const HINT_ATSPI: &str = "doubletap.hint.atspi";
pub const HINT_WAYLAND_CHORD: &str = "doubletap.hint.wayland_chord";
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
) -> Option<&'static str> {
    match platform {
        Platform::Linux if session == Session::Wayland && trigger.is_double_tap() => {
            Some(HINT_WAYLAND_CHORD)
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
            hint_for(Platform::Linux, Session::Wayland, Granted, &tap),
            Some(HINT_WAYLAND_CHORD)
        );
        assert_eq!(
            hint_for(Platform::Linux, Session::Wayland, Denied, &chord),
            Some(HINT_ATSPI)
        );
        assert_eq!(hint_for(Platform::Linux, Session::X11, Granted, &tap), None);
        assert_eq!(
            hint_for(Platform::MacOs, Session::MacOs, Denied, &tap),
            Some(HINT_ACCESSIBILITY)
        );
        assert_eq!(
            hint_for(Platform::Windows, Session::Windows, NotRequired, &tap),
            None
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
