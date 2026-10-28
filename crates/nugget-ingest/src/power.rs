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

/// Best-effort system policy: any battery discharging below `threshold`, or (Linux) the ACPI
/// platform profile set to `low-power`. Results are cached for `ttl`. Platforms without battery
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

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd"
))]
fn battery_low(threshold: f32) -> bool {
    use starship_battery::State;
    use starship_battery::units::ratio::ratio;
    let Ok(manager) = starship_battery::Manager::new() else {
        return false;
    };
    let Ok(batteries) = manager.batteries() else {
        return false;
    };
    let mut any_low = false;
    for b in batteries.flatten() {
        match b.state() {
            // On AC power: never constrained, whatever the charge.
            State::Charging | State::Full => return false,
            State::Discharging | State::Empty => {
                if b.state_of_charge().get::<ratio>() < threshold {
                    any_low = true;
                }
            }
            State::Unknown => {}
        }
    }
    any_low
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd"
)))]
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
    false
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
}
