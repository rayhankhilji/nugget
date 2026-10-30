//! Power: whether the computer runs on battery or in low power mode, so read-ahead can pause.
//!
//! Read-ahead costs CPU (the local model reads the prompt), so it pauses in low power mode
//! (battery saver) always, and on battery when the user turned `read_ahead_on_battery` off
//! ([`paused`]). The state is read lazily from the read-ahead loop and cached for [`RECHECK`]
//! ([`Cached`]); reading it never starts a process:
//!
//! - macOS: `NSProcessInfo.isLowPowerModeEnabled` (macOS 12 and later) and IOKit's providing
//!   power source (`IOPSGetProvidingPowerSourceType`).
//! - Windows: `GetSystemPowerStatus` (AC line status, battery saver flag).
//! - Linux: `/sys/class/power_supply` (mains online, a system battery discharging) and the ACPI
//!   platform profile (`low-power`).
//!
//! Anywhere the state cannot be read, it reads as "on mains, not low power" (never pauses).

use std::time::{Duration, Instant};

use super::settings::DoubleTapSettings;

/// How long a power reading is trusted before it is read again (also how often a paused loop
/// looks whether it may resume).
pub const RECHECK: Duration = Duration::from_secs(60);

/// What the power source and power mode are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PowerState {
    pub on_battery: bool,
    /// Low power mode / battery saver.
    pub low_power: bool,
}

/// Whether read-ahead pauses: always in low power mode, and on battery unless the user keeps it
/// on there.
pub fn paused(power: PowerState, s: &DoubleTapSettings) -> bool {
    power.low_power || (power.on_battery && !s.read_ahead_on_battery)
}

/// The last reading and when it was taken.
#[derive(Debug, Default)]
pub struct Cached {
    at: Option<Instant>,
    state: PowerState,
}

impl Cached {
    /// Whether the reading is missing or older than [`RECHECK`].
    pub fn due(&self, now: Instant) -> bool {
        self.at
            .is_none_or(|t| now.saturating_duration_since(t) >= RECHECK)
    }

    pub fn set(&mut self, now: Instant, state: PowerState) {
        self.at = Some(now);
        self.state = state;
    }

    /// When the reading goes stale.
    pub fn until_due(&self, now: Instant) -> Duration {
        self.at.map_or(Duration::ZERO, |t| {
            (t + RECHECK).saturating_duration_since(now)
        })
    }

    /// The cached reading, read again (off the async runtime: sysfs battery reads can block on
    /// ACPI) when it is stale.
    pub async fn current(&mut self) -> PowerState {
        let now = Instant::now();
        if self.due(now) {
            let state = tauri::async_runtime::spawn_blocking(read)
                .await
                .unwrap_or_default();
            self.set(now, state);
        }
        self.state
    }
}

/// Read the power state now.
pub fn read() -> PowerState {
    #[cfg(target_os = "macos")]
    {
        macos::read()
    }
    #[cfg(windows)]
    {
        windows::read()
    }
    #[cfg(target_os = "linux")]
    {
        linux::read(std::path::Path::new("/sys"))
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        PowerState::default()
    }
}

/// `GetSystemPowerStatus`: `ACLineStatus` 0 is offline (on battery; 1 is online, 255 unknown),
/// `SystemStatusFlag` 1 is battery saver on.
#[cfg(any(windows, test))]
fn from_system_power_status(ac_line_status: u8, system_status_flag: u8) -> PowerState {
    PowerState {
        on_battery: ac_line_status == 0,
        low_power: system_status_flag == 1,
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_foundation::NSProcessInfo;
    use objc2_io_kit::{
        IOPSCopyPowerSourcesInfo, IOPSGetProvidingPowerSourceType, kIOPMBatteryPowerKey,
    };

    use super::PowerState;

    pub(super) fn read() -> PowerState {
        // `isLowPowerModeEnabled` exists from macOS 12 (the app supports 11).
        let low_power =
            objc2::available!(macos = 12.0) && NSProcessInfo::processInfo().isLowPowerModeEnabled();
        PowerState {
            on_battery: on_battery(),
            low_power,
        }
    }

    #[allow(unsafe_code)]
    fn on_battery() -> bool {
        let Some(info) = IOPSCopyPowerSourcesInfo() else {
            return false;
        };
        // SAFETY: `info` is the snapshot IOPSCopyPowerSourcesInfo returned, which is exactly
        // the type this function takes; it stays alive (retained) for the call.
        let Some(source) = (unsafe { IOPSGetProvidingPowerSourceType(Some(&info)) }) else {
            return false;
        };
        kIOPMBatteryPowerKey
            .to_str()
            .is_ok_and(|battery| source.to_string() == battery)
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    use super::PowerState;

    #[allow(unsafe_code)]
    pub(super) fn read() -> PowerState {
        let mut status = SYSTEM_POWER_STATUS {
            ACLineStatus: 255,
            BatteryFlag: 255,
            BatteryLifePercent: 255,
            SystemStatusFlag: 0,
            BatteryLifeTime: u32::MAX,
            BatteryFullLifeTime: u32::MAX,
        };
        // SAFETY: `status` is a valid, writable SYSTEM_POWER_STATUS for the call.
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
            return PowerState::default();
        }
        super::from_system_power_status(status.ACLineStatus, status.SystemStatusFlag)
    }
}

/// Linux: `/sys/class/power_supply/*` and `/sys/firmware/acpi/platform_profile`, under `sys`
/// (a directory of fixtures in tests).
#[cfg(any(target_os = "linux", test))]
mod linux {
    use std::path::Path;

    use super::PowerState;

    /// One entry of `/sys/class/power_supply`.
    #[derive(Debug, Default)]
    pub(super) struct Supply {
        /// `Mains`, `USB`, `Battery`, `UPS`, …
        pub kind: String,
        pub online: Option<bool>,
        /// `Charging`, `Discharging`, `Full`, `Not charging`, `Unknown`.
        pub status: String,
        /// `Device` for a peripheral's battery (a mouse, a headset): not what powers the computer.
        pub scope: String,
    }

    pub(super) fn read(sys: &Path) -> PowerState {
        let supplies = std::fs::read_dir(sys.join("class/power_supply"))
            .map(|dir| {
                dir.flatten()
                    .map(|entry| supply(&entry.path()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let profile = std::fs::read_to_string(sys.join("firmware/acpi/platform_profile")).ok();
        state(&supplies, profile.as_deref())
    }

    fn supply(dir: &Path) -> Supply {
        let field = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default()
        };
        Supply {
            kind: field("type"),
            online: match field("online").as_str() {
                "0" => Some(false),
                "" => None,
                _ => Some(true),
            },
            status: field("status"),
            scope: field("scope"),
        }
    }

    /// On battery: a system battery is discharging and no mains (or USB power) supply is
    /// online. Low power: the platform profile is `low-power`.
    pub(super) fn state(supplies: &[Supply], platform_profile: Option<&str>) -> PowerState {
        let external = supplies
            .iter()
            .any(|s| matches!(s.kind.as_str(), "Mains" | "USB") && s.online == Some(true));
        let discharging = supplies
            .iter()
            .any(|s| s.kind == "Battery" && s.scope != "Device" && s.status == "Discharging");
        PowerState {
            on_battery: discharging && !external,
            low_power: platform_profile.is_some_and(|p| p.trim() == "low-power"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_power_always_pauses_battery_only_when_asked() {
        let s = DoubleTapSettings::default();
        let off = DoubleTapSettings {
            read_ahead_on_battery: false,
            ..s.clone()
        };
        let mains = PowerState::default();
        let battery = PowerState {
            on_battery: true,
            low_power: false,
        };
        let saver = PowerState {
            on_battery: true,
            low_power: true,
        };
        let plugged_saver = PowerState {
            on_battery: false,
            low_power: true,
        };
        assert!(!paused(mains, &s));
        assert!(!paused(mains, &off));
        // On battery: keeps reading ahead by default, pauses when the user turned it off.
        assert!(!paused(battery, &s));
        assert!(paused(battery, &off));
        // Low power mode pauses whatever the setting says.
        for p in [saver, plugged_saver] {
            assert!(paused(p, &s));
            assert!(paused(p, &off));
        }
    }

    #[test]
    fn readings_are_cached() {
        let t0 = Instant::now();
        let mut c = Cached::default();
        assert!(c.due(t0));
        assert_eq!(c.until_due(t0), Duration::ZERO);
        let battery = PowerState {
            on_battery: true,
            low_power: false,
        };
        c.set(t0, battery);
        assert_eq!(c.state, battery);
        assert!(!c.due(t0 + RECHECK - Duration::from_secs(1)));
        assert_eq!(
            c.until_due(t0 + Duration::from_secs(20)),
            RECHECK - Duration::from_secs(20)
        );
        assert!(c.due(t0 + RECHECK));
    }

    #[test]
    fn windows_power_status() {
        assert_eq!(from_system_power_status(1, 0), PowerState::default());
        assert_eq!(
            from_system_power_status(0, 0),
            PowerState {
                on_battery: true,
                low_power: false
            }
        );
        assert_eq!(
            from_system_power_status(0, 1),
            PowerState {
                on_battery: true,
                low_power: true
            }
        );
        // Unknown AC status: not treated as battery.
        assert!(!from_system_power_status(255, 0).on_battery);
    }

    /// Writes a fake `/sys` tree.
    fn fixture(
        name: &str,
        supplies: &[(&str, &[(&str, &str)])],
        profile: Option<&str>,
    ) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nugget-power-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (supply, files) in supplies {
            let dir = root.join("class/power_supply").join(supply);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, value) in *files {
                std::fs::write(dir.join(file), format!("{value}\n")).unwrap();
            }
        }
        if let Some(p) = profile {
            let dir = root.join("firmware/acpi");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("platform_profile"), format!("{p}\n")).unwrap();
        }
        root
    }

    #[test]
    fn linux_sysfs() {
        let unplugged = fixture(
            "unplugged",
            &[
                ("AC", &[("type", "Mains"), ("online", "0")]),
                ("BAT0", &[("type", "Battery"), ("status", "Discharging")]),
                // A wireless mouse's battery does not power the computer.
                (
                    "hidpp_battery_0",
                    &[
                        ("type", "Battery"),
                        ("scope", "Device"),
                        ("status", "Charging"),
                    ],
                ),
            ],
            Some("balanced"),
        );
        assert_eq!(
            linux::read(&unplugged),
            PowerState {
                on_battery: true,
                low_power: false
            }
        );
        let plugged = fixture(
            "plugged",
            &[
                ("AC", &[("type", "Mains"), ("online", "1")]),
                ("BAT0", &[("type", "Battery"), ("status", "Charging")]),
            ],
            Some("low-power"),
        );
        assert_eq!(
            linux::read(&plugged),
            PowerState {
                on_battery: false,
                low_power: true
            }
        );
        // A desktop: no battery, no profile. Only a peripheral discharging.
        let desktop = fixture(
            "desktop",
            &[(
                "hid-mouse",
                &[
                    ("type", "Battery"),
                    ("scope", "Device"),
                    ("status", "Discharging"),
                ],
            )],
            None,
        );
        assert_eq!(linux::read(&desktop), PowerState::default());
        // No sysfs at all.
        assert_eq!(linux::read(&desktop.join("missing")), PowerState::default());
        for dir in [unplugged, plugged, desktop] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn linux_rules() {
        use linux::{Supply, state};
        let battery = |status: &str| Supply {
            kind: "Battery".into(),
            status: status.into(),
            ..Supply::default()
        };
        let supply = |kind: &str, online: bool| Supply {
            kind: kind.into(),
            online: Some(online),
            ..Supply::default()
        };
        // No mains entry at all (some laptops): a discharging battery means battery.
        assert!(state(&[battery("Discharging")], None).on_battery);
        // USB-C power counts as external power.
        assert!(!state(&[battery("Discharging"), supply("USB", true)], None).on_battery);
        assert!(state(&[battery("Discharging"), supply("Mains", false)], None).on_battery);
        assert!(!state(&[battery("Full"), supply("Mains", false)], None).on_battery);
        assert!(!state(&[], Some("performance")).low_power);
        assert!(state(&[], Some("low-power\n")).low_power);
    }
}
