//! Power-aware scheduling.
//!
//! Heavy work (backfills: first syncs and the bulk of a sync started without a cursor) pauses
//! while [`PowerPolicy::constrained`] is true. Live (watch-driven) changes and explicit
//! `sync_now` requests always run.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Decides whether heavy background work should wait.
pub trait PowerPolicy: Send + Sync {
    /// `true` while the machine is on battery below the threshold or in an OS low-power mode.
    /// Must be cheap or cached; it is called from blocking threads.
    fn constrained(&self) -> bool;
}

/// Never constrained (desktops, tests, unsupported platforms).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPowerPolicy;

impl PowerPolicy for NoPowerPolicy {
    fn constrained(&self) -> bool {
        false
    }
}

/// Best-effort system policy: any battery discharging below `threshold` (unless one is on AC
/// power), or an OS power-saver mode (Linux: the ACPI platform profile set to `low-power`;
/// Windows: battery saver). Results are cached for `ttl`. Platforms without battery
/// support report "not constrained".
#[derive(Debug)]
pub struct SystemPowerPolicy {
    threshold: f32,
    ttl: Duration,
    cache: Mutex<Option<(Instant, bool)>>,
}

impl SystemPowerPolicy {
    /// `threshold` is a fraction (0.2 = 20 %).
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold: threshold.clamp(0.0, 1.0),
            ttl: Duration::from_secs(30),
            cache: Mutex::new(None),
        }
    }

    fn probe(&self) -> bool {
        os_low_power_mode() || battery_low(self.threshold)
    }
}

impl PowerPolicy for SystemPowerPolicy {
    fn constrained(&self) -> bool {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = *cache
            && at.elapsed() < self.ttl
        {
            return v;
        }
        let v = self.probe();
        *cache = Some((Instant::now(), v));
        v
    }
}

/// One power source's state, as read from the OS.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows", test)),
    allow(dead_code)
)]
enum Reading {
    /// Charging, full or otherwise on external power.
    OnAc,
    /// Running on this battery, at this charge fraction (0.0..=1.0).
    Discharging(f32),
    /// State or charge not known; ignored.
    Unknown,
}

/// Any battery on AC power means the machine is plugged in: never constrained, whatever the
/// charge. Otherwise constrained when any discharging battery is below `threshold`.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows", test)),
    allow(dead_code)
)]
fn any_low(readings: impl IntoIterator<Item = Reading>, threshold: f32) -> bool {
    let mut any_low = false;
    for r in readings {
        match r {
            Reading::OnAc => return false,
            Reading::Discharging(f) if f < threshold => any_low = true,
            Reading::Discharging(_) | Reading::Unknown => {}
        }
    }
    any_low
}

#[cfg(target_os = "linux")]
fn battery_low(threshold: f32) -> bool {
    any_low(
        sysfs::readings(std::path::Path::new("/sys/class/power_supply")),
        threshold,
    )
}

#[cfg(target_os = "macos")]
fn battery_low(threshold: f32) -> bool {
    any_low(iokit::readings(), threshold)
}

#[cfg(target_os = "windows")]
fn battery_low(threshold: f32) -> bool {
    match win::status() {
        Some(s) => any_low([win::reading(&s)], threshold),
        None => false,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn battery_low(_threshold: f32) -> bool {
    false
}

/// OS "low power" / power-saver mode, where cheaply detectable.
fn os_low_power_mode() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(p) = std::fs::read_to_string("/sys/firmware/acpi/platform_profile") {
            return p.trim() == "low-power";
        }
    }
    #[cfg(target_os = "windows")]
    {
        // SystemStatusFlag 1: battery saver is on.
        if let Some(s) = win::status() {
            return s.SystemStatusFlag == 1;
        }
    }
    false
}

/// Linux: `/sys/class/power_supply/*` (system batteries only; peripherals such as mice report
/// `scope` = `Device` and are skipped).
#[cfg(any(target_os = "linux", test))]
mod sysfs {
    use super::Reading;
    use std::path::Path;

    pub(super) fn readings(root: &Path) -> Vec<Reading> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| reading(&e.path()))
            .collect()
    }

    fn read(dir: &Path, name: &str) -> Option<String> {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .map(|s| s.trim().to_owned())
    }

    fn num(dir: &Path, name: &str) -> Option<f64> {
        read(dir, name)?.parse().ok()
    }

    fn ratio(dir: &Path, now: &str, full: &str) -> Option<f32> {
        let (now, full) = (num(dir, now)?, num(dir, full)?);
        (full > 0.0).then(|| (now / full).clamp(0.0, 1.0) as f32)
    }

    fn fraction(dir: &Path) -> Option<f32> {
        if let Some(pct) = num(dir, "capacity") {
            return Some((pct / 100.0).clamp(0.0, 1.0) as f32);
        }
        ratio(dir, "energy_now", "energy_full").or_else(|| ratio(dir, "charge_now", "charge_full"))
    }

    /// `None` for anything that is not a system battery.
    fn reading(dir: &Path) -> Option<Reading> {
        if read(dir, "type")?.as_str() != "Battery" {
            return None;
        }
        if read(dir, "scope").is_some_and(|s| s.eq_ignore_ascii_case("device")) {
            return None;
        }
        Some(match read(dir, "status").as_deref() {
            // "Not charging": plugged in but held below full (charge limit).
            Some("Charging" | "Full" | "Not charging") => Reading::OnAc,
            Some("Discharging") => fraction(dir).map_or(Reading::Unknown, Reading::Discharging),
            _ => Reading::Unknown,
        })
    }
}

/// macOS: IOKit power sources (`IOPowerSources.h`), read through CoreFoundation.
#[cfg(target_os = "macos")]
mod iokit {
    use super::Reading;
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    #[allow(unsafe_code)]
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        // Returns a +1 (owned) blob, or NULL.
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        // Returns a +1 (owned) array of power source handles, or NULL.
        fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
        // Returns a borrowed (+0) dictionary owned by `blob`, or NULL.
        fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: CFTypeRef) -> CFDictionaryRef;
    }

    type Desc = CFDictionary<CFString, CFType>;

    fn string(d: &Desc, key: &'static str) -> Option<String> {
        d.find(CFString::from_static_string(key))?
            .downcast::<CFString>()
            .map(|s| s.to_string())
    }

    fn number(d: &Desc, key: &'static str) -> Option<i64> {
        d.find(CFString::from_static_string(key))?
            .downcast::<CFNumber>()?
            .to_i64()
    }

    fn boolean(d: &Desc, key: &'static str) -> Option<bool> {
        d.find(CFString::from_static_string(key))?
            .downcast::<CFBoolean>()
            .map(bool::from)
    }

    fn reading(d: &Desc) -> Option<Reading> {
        // Only the internal battery (skip UPSes and accessories).
        if string(d, "Type").is_some_and(|t| t != "InternalBattery") {
            return None;
        }
        if boolean(d, "Is Present") == Some(false) {
            return None;
        }
        if boolean(d, "Is Charging") == Some(true) {
            return Some(Reading::OnAc);
        }
        Some(match string(d, "Power Source State").as_deref() {
            Some("AC Power") => Reading::OnAc,
            Some("Battery Power") => {
                match (number(d, "Current Capacity"), number(d, "Max Capacity")) {
                    (Some(cur), Some(max)) if max > 0 => {
                        Reading::Discharging((cur as f64 / max as f64).clamp(0.0, 1.0) as f32)
                    }
                    _ => Reading::Unknown,
                }
            }
            _ => Reading::Unknown,
        })
    }

    pub(super) fn readings() -> Vec<Reading> {
        // SAFETY: no arguments; the result is NULL or a CF object we own (Copy rule).
        #[allow(unsafe_code)]
        let blob = unsafe { IOPSCopyPowerSourcesInfo() };
        if blob.is_null() {
            return Vec::new();
        }
        // SAFETY: `blob` is a non-NULL owned CF object; wrapping it releases it on drop.
        #[allow(unsafe_code)]
        let blob = unsafe { CFType::wrap_under_create_rule(blob) };
        // SAFETY: `blob` is a valid power sources blob; the result is NULL or owned (Copy rule).
        #[allow(unsafe_code)]
        let list = unsafe { IOPSCopyPowerSourcesList(blob.as_CFTypeRef()) };
        if list.is_null() {
            return Vec::new();
        }
        // SAFETY: `list` is a non-NULL owned CFArray of CF objects; wrapping releases it on drop.
        #[allow(unsafe_code)]
        let list: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(list) };
        let mut out = Vec::new();
        for ps in list.iter() {
            // SAFETY: `blob` and `ps` are valid and alive for this call; the result is NULL or
            // a dictionary borrowed from `blob` (Get rule).
            #[allow(unsafe_code)]
            let desc =
                unsafe { IOPSGetPowerSourceDescription(blob.as_CFTypeRef(), ps.as_CFTypeRef()) };
            if desc.is_null() {
                continue;
            }
            // SAFETY: non-NULL borrowed CFDictionary; the Get rule retains it for our wrapper.
            // Keys are CFStrings and values arbitrary CF objects, which `find` + `downcast`
            // check before use.
            #[allow(unsafe_code)]
            let desc: Desc = unsafe { CFDictionary::wrap_under_get_rule(desc) };
            if let Some(r) = reading(&desc) {
                out.push(r);
            }
        }
        out
    }
}

/// Windows: `GetSystemPowerStatus`.
#[cfg(target_os = "windows")]
mod win {
    use super::Reading;
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    pub(super) fn status() -> Option<SYSTEM_POWER_STATUS> {
        let mut s = SYSTEM_POWER_STATUS {
            ACLineStatus: 0,
            BatteryFlag: 0,
            BatteryLifePercent: 0,
            SystemStatusFlag: 0,
            BatteryLifeTime: 0,
            BatteryFullLifeTime: 0,
        };
        // SAFETY: `s` is a valid, writable SYSTEM_POWER_STATUS for the duration of the call.
        #[allow(unsafe_code)]
        let ok = unsafe { GetSystemPowerStatus(&mut s) };
        (ok != 0).then_some(s)
    }

    pub(super) fn reading(s: &SYSTEM_POWER_STATUS) -> Reading {
        // BatteryFlag 128: no system battery (255, "unknown", has that bit set too).
        const NO_BATTERY: u8 = 128;
        match s.ACLineStatus {
            1 => Reading::OnAc,
            0 if s.BatteryFlag & NO_BATTERY != 0 => Reading::Unknown,
            // BatteryLifePercent 255: unknown.
            0 if s.BatteryLifePercent <= 100 => {
                Reading::Discharging(f32::from(s.BatteryLifePercent) / 100.0)
            }
            _ => Reading::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_policy_does_not_panic_and_caches() {
        let p = SystemPowerPolicy::new(0.2);
        let a = p.constrained();
        assert_eq!(a, p.constrained());
        assert!(!NoPowerPolicy.constrained());
    }

    #[test]
    fn any_low_semantics() {
        use Reading::*;
        assert!(any_low([Discharging(0.1)], 0.2));
        assert!(!any_low([Discharging(0.5)], 0.2));
        assert!(!any_low([Discharging(0.1), OnAc], 0.2));
        assert!(any_low([Unknown, Discharging(0.1), Discharging(0.9)], 0.2));
        assert!(!any_low([Unknown], 0.2));
        assert!(!any_low([], 0.2));
    }

    mod sysfs_fixtures {
        use super::super::{Reading, any_low, sysfs};
        use std::path::Path;

        fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            for (f, v) in files {
                std::fs::write(dir.join(f), format!("{v}\n")).unwrap();
            }
        }

        fn low(root: &Path) -> bool {
            any_low(sysfs::readings(root), 0.2)
        }

        #[test]
        fn discharging_low_is_constrained() {
            let t = tempfile::tempdir().unwrap();
            supply(t.path(), "AC", &[("type", "Mains"), ("online", "0")]);
            supply(
                t.path(),
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("capacity", "12"),
                ],
            );
            assert!(low(t.path()));
        }

        #[test]
        fn charging_low_is_not_constrained() {
            let t = tempfile::tempdir().unwrap();
            supply(
                t.path(),
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("status", "Charging"),
                    ("capacity", "5"),
                ],
            );
            assert!(!low(t.path()));
            // One battery charging means AC power, even if another reads discharging.
            supply(
                t.path(),
                "BAT1",
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("capacity", "5"),
                ],
            );
            assert!(!low(t.path()));
        }

        #[test]
        fn discharging_high_is_not_constrained() {
            let t = tempfile::tempdir().unwrap();
            supply(
                t.path(),
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("capacity", "80"),
                ],
            );
            assert!(!low(t.path()));
        }

        #[test]
        fn device_scope_battery_is_ignored() {
            let t = tempfile::tempdir().unwrap();
            supply(
                t.path(),
                "hidpp_battery_0",
                &[
                    ("type", "Battery"),
                    ("scope", "Device"),
                    ("status", "Discharging"),
                    ("capacity", "3"),
                ],
            );
            assert!(sysfs::readings(t.path()).is_empty());
            assert!(!low(t.path()));
        }

        #[test]
        fn energy_and_charge_fallbacks() {
            let t = tempfile::tempdir().unwrap();
            supply(
                t.path(),
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("energy_now", "1000000"),
                    ("energy_full", "50000000"),
                ],
            );
            assert_eq!(sysfs::readings(t.path()), vec![Reading::Discharging(0.02)]);
            assert!(low(t.path()));

            let t = tempfile::tempdir().unwrap();
            supply(
                t.path(),
                "BAT0",
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("charge_now", "3000000"),
                    ("charge_full", "4000000"),
                ],
            );
            assert_eq!(sysfs::readings(t.path()), vec![Reading::Discharging(0.75)]);
            assert!(!low(t.path()));
        }

        #[test]
        fn missing_files_are_not_constrained() {
            let t = tempfile::tempdir().unwrap();
            // No root at all.
            assert!(!low(&t.path().join("absent")));
            // No `type`, no `status`, no charge, zero full capacity.
            supply(
                t.path(),
                "BAT0",
                &[("status", "Discharging"), ("capacity", "1")],
            );
            supply(t.path(), "BAT1", &[("type", "Battery"), ("capacity", "1")]);
            supply(
                t.path(),
                "BAT2",
                &[("type", "Battery"), ("status", "Discharging")],
            );
            supply(
                t.path(),
                "BAT3",
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("energy_now", "1"),
                    ("energy_full", "0"),
                ],
            );
            assert_eq!(
                sysfs::readings(t.path()),
                vec![Reading::Unknown, Reading::Unknown, Reading::Unknown]
            );
            assert!(!low(t.path()));
        }
    }
}
