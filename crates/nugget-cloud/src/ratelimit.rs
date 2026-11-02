//! In-process rate limits: a token bucket per key (client IP, user, or user for one route).
//!
//! Per machine, not global: with N machines a client gets up to N times the limit. That is fine
//! for abuse control (the AI quota in the gateway is the hard, global cost limit). Memory is
//! bounded: past `MAX_KEYS` keys, idle (full) buckets are dropped, and if that is not enough the
//! map is cleared, which only ever errs towards allowing requests.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_KEYS: usize = 100_000;

/// Limits, from the environment (see `Config`). Zero disables a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Requests per minute per client IP, every route except health checks.
    pub ip_per_min: u32,
    /// Authenticated requests per minute per user.
    pub user_per_min: u32,
    /// `POST /v1/ask` per minute per user (each one costs model tokens).
    pub ask_per_min: u32,
    /// Minimum seconds between `POST /v1/sync` calls per user (each one costs Composio calls).
    pub sync_every_secs: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            ip_per_min: 300,
            user_per_min: 120,
            ask_per_min: 10,
            sync_every_secs: 60,
        }
    }
}

impl Limits {
    /// No limits (load tests against a local instance).
    pub fn unlimited() -> Self {
        Self {
            ip_per_min: 0,
            user_per_min: 0,
            ask_per_min: 0,
            sync_every_secs: 0,
        }
    }
}

/// A keyed token bucket: `capacity` tokens, refilled at `capacity` per `period`.
pub struct RateLimiter<K> {
    capacity: f64,
    per_sec: f64,
    buckets: Mutex<HashMap<K, (f64, Instant)>>,
}

impl<K: Hash + Eq + Clone> RateLimiter<K> {
    /// `capacity` 0 means unlimited.
    pub fn new(capacity: u32, period: Duration) -> Self {
        let secs = period.as_secs_f64().max(0.001);
        Self {
            capacity: f64::from(capacity),
            per_sec: f64::from(capacity) / secs,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn per_minute(n: u32) -> Self {
        Self::new(n, Duration::from_secs(60))
    }

    /// One request every `secs` seconds (no burst).
    pub fn every(secs: u32) -> Self {
        if secs == 0 {
            return Self::new(0, Duration::from_secs(1));
        }
        Self::new(1, Duration::from_secs(u64::from(secs)))
    }

    /// Takes a token for `key`, or says how long until one is available.
    pub fn check(&self, key: &K) -> Result<(), Duration> {
        self.check_at(key, Instant::now())
    }

    pub fn check_at(&self, key: &K, now: Instant) -> Result<(), Duration> {
        if self.capacity <= 0.0 {
            return Ok(());
        }
        let mut map = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= MAX_KEYS && !map.contains_key(key) {
            let (cap, rate) = (self.capacity, self.per_sec);
            map.retain(|_, (tokens, at)| {
                *tokens + now.saturating_duration_since(*at).as_secs_f64() * rate < cap
            });
            if map.len() >= MAX_KEYS {
                map.clear();
            }
        }
        let (tokens, at) = map.entry(key.clone()).or_insert((self.capacity, now));
        let refilled = (*tokens + now.saturating_duration_since(*at).as_secs_f64() * self.per_sec)
            .min(self.capacity);
        *at = now;
        if refilled >= 1.0 {
            *tokens = refilled - 1.0;
            Ok(())
        } else {
            *tokens = refilled;
            Err(Duration::from_secs_f64((1.0 - refilled) / self.per_sec))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_allows_burst_then_refills() {
        let l = RateLimiter::per_minute(3);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(l.check_at(&"a", t0).is_ok());
        }
        let wait = l.check_at(&"a", t0).unwrap_err();
        assert!(wait > Duration::from_secs(19) && wait <= Duration::from_secs(20));
        assert!(l.check_at(&"b", t0).is_ok(), "keys are independent");
        assert!(l.check_at(&"a", t0 + Duration::from_secs(20)).is_ok());
        assert!(l.check_at(&"a", t0 + Duration::from_secs(21)).is_err());
    }

    #[test]
    fn every_and_unlimited() {
        let l = RateLimiter::every(60);
        let t0 = Instant::now();
        assert!(l.check_at(&1, t0).is_ok());
        assert!(l.check_at(&1, t0 + Duration::from_secs(59)).is_err());
        assert!(l.check_at(&1, t0 + Duration::from_secs(120)).is_ok());
        let off = RateLimiter::every(0);
        for _ in 0..1000 {
            assert!(off.check_at(&1, t0).is_ok());
        }
    }

    #[test]
    fn memory_is_bounded() {
        let l = RateLimiter::per_minute(1);
        let t0 = Instant::now();
        for k in 0..(MAX_KEYS + 10) {
            let _ = l.check_at(&k, t0);
        }
        assert!(l.buckets.lock().unwrap().len() <= MAX_KEYS);
    }
}
