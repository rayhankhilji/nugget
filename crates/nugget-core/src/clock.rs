//! Elapsed time that counts system sleep.
//!
//! `std::time::Instant` (and `tokio::time::Instant`) stop while the machine sleeps on Linux and
//! macOS, so a "every 10 minutes" timer started before a night of sleep fires 10 minutes of
//! *awake* time later. [`Stopwatch`] also reads the wall clock and takes the larger of the two
//! elapsed times, ignoring the wall clock when it jumps backwards (a clock change), so periodic
//! work is due again right after a long sleep. Pair it with a bounded nap (at most
//! [`RECHECK_EVERY`]) so a sleeping task notices the wake soon.

use std::time::{Duration, Instant, SystemTime};

/// The longest a periodic loop should sleep between checks of a [`Stopwatch`]: after a system
/// sleep, overdue work starts within this much awake time.
pub const RECHECK_EVERY: Duration = Duration::from_secs(60);

/// A start time on both the monotonic and the wall clock.
#[derive(Debug, Clone, Copy)]
pub struct Stopwatch {
    mono: Instant,
    wall: SystemTime,
}

impl Stopwatch {
    /// Start now.
    pub fn start() -> Self {
        Self::start_at(Instant::now(), SystemTime::now())
    }

    /// Start at the given clock readings (tests inject the wall clock here).
    pub fn start_at(mono: Instant, wall: SystemTime) -> Self {
        Self { mono, wall }
    }

    /// Time since the start, including time the system spent asleep.
    pub fn elapsed(&self) -> Duration {
        self.elapsed_at(Instant::now(), SystemTime::now())
    }

    /// [`Stopwatch::elapsed`] at the given clock readings: the larger of the monotonic and the
    /// wall-clock elapsed time; a wall clock earlier than the start counts as no wall time.
    pub fn elapsed_at(&self, mono_now: Instant, wall_now: SystemTime) -> Duration {
        let mono = mono_now.saturating_duration_since(self.mono);
        let wall = wall_now.duration_since(self.wall).unwrap_or(Duration::ZERO);
        mono.max(wall)
    }

    /// Whether `period` has elapsed since the start.
    pub fn is_due(&self, period: Duration) -> bool {
        self.elapsed() >= period
    }

    /// How long until `period` has elapsed (zero when it has).
    pub fn remaining(&self, period: Duration) -> Duration {
        period.saturating_sub(self.elapsed())
    }

    /// How long a loop waiting for `period` should sleep before checking again:
    /// [`Stopwatch::remaining`], capped at [`RECHECK_EVERY`].
    pub fn next_check(&self, period: Duration) -> Duration {
        self.remaining(period).min(RECHECK_EVERY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn counts_sleep_but_not_backward_clock_jumps() {
        let (m0, w0) = (Instant::now(), SystemTime::UNIX_EPOCH + 1000 * MIN);
        let sw = Stopwatch::start_at(m0, w0);
        // 8 hours asleep: the monotonic clock saw 3 minutes, the wall clock 8 hours.
        assert_eq!(sw.elapsed_at(m0 + 3 * MIN, w0 + 480 * MIN), 480 * MIN);
        assert_eq!(sw.elapsed_at(m0 + 5 * MIN, w0 - 60 * MIN), 5 * MIN);
        let slept = Stopwatch::start_at(Instant::now(), SystemTime::now() - 60 * MIN);
        assert!(slept.is_due(10 * MIN));
        assert_eq!(slept.next_check(10 * MIN), Duration::ZERO);
        assert_eq!(Stopwatch::start().next_check(10 * MIN), RECHECK_EVERY);
    }
}
