//! Freshness (change observed → committed) and queue-depth metrics.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Rolling window of freshness samples plus a global queue-depth gauge.
#[derive(Debug)]
pub(crate) struct Metrics {
    window: usize,
    samples: Mutex<VecDeque<u64>>,
    queue_depth: AtomicU64,
    committed: AtomicU64,
}

impl Metrics {
    pub fn new(window: usize) -> Self {
        Self {
            window: window.max(1),
            samples: Mutex::new(VecDeque::new()),
            queue_depth: AtomicU64::new(0),
            committed: AtomicU64::new(0),
        }
    }

    pub fn record_freshness(&self, latency: Duration) {
        let ms = latency.as_millis().min(u128::from(u64::MAX)) as u64;
        let mut s = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        if s.len() == self.window {
            s.pop_front();
        }
        s.push_back(ms);
        self.committed.fetch_add(1, Ordering::Relaxed);
    }

    /// `(p50, p95)` in milliseconds over the window (nearest-rank).
    pub fn freshness(&self) -> (Option<u64>, Option<u64>) {
        let mut v: Vec<u64> = self
            .samples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        if v.is_empty() {
            return (None, None);
        }
        v.sort_unstable();
        (Some(percentile(&v, 50)), Some(percentile(&v, 95)))
    }

    pub fn enqueued(&self) {
        self.queue_depth.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dequeued(&self) {
        // Saturating: never wrap below zero. (`fetch_update` is `try_update` from Rust 1.99,
        // newer than the MSRV.)
        #[allow(deprecated)]
        let _ = self
            .queue_depth
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |d| {
                Some(d.saturating_sub(1))
            });
    }

    pub fn queue_depth(&self) -> u64 {
        self.queue_depth.load(Ordering::Relaxed)
    }

    /// Changes committed since start.
    pub fn committed(&self) -> u64 {
        self.committed.load(Ordering::Relaxed)
    }
}

/// Nearest-rank percentile of sorted, non-empty `v`.
fn percentile(v: &[u64], p: usize) -> u64 {
    let rank = (p * v.len()).div_ceil(100).max(1);
    v[rank.min(v.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_over_a_rolling_window() {
        let m = Metrics::new(100);
        assert_eq!(m.freshness(), (None, None));
        for ms in 1..=100 {
            m.record_freshness(Duration::from_millis(ms));
        }
        assert_eq!(m.freshness(), (Some(50), Some(95)));
        // The window drops the oldest samples.
        for _ in 0..100 {
            m.record_freshness(Duration::from_millis(7));
        }
        assert_eq!(m.freshness(), (Some(7), Some(7)));
        assert_eq!(m.committed(), 200);
    }

    #[test]
    fn queue_depth_never_underflows() {
        let m = Metrics::new(1);
        m.enqueued();
        m.dequeued();
        m.dequeued();
        assert_eq!(m.queue_depth(), 0);
    }
}
