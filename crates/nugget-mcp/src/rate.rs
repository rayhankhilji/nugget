//! Per-client token-bucket rate limiting.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A token bucket: `burst` requests at once, refilled at `per_minute`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RateLimit {
    pub burst: u32,
    pub per_minute: u32,
}

impl Default for RateLimit {
    /// 60 requests at once, 120 per minute sustained: generous for an agent, a hard stop for a
    /// runaway loop.
    fn default() -> Self {
        Self {
            burst: 60,
            per_minute: 120,
        }
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Buckets keyed by client id.
#[derive(Debug)]
pub struct RateLimiter {
    limit: RateLimit,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn limit(&self) -> RateLimit {
        self.limit
    }

    /// Take one token for `client`. `Err(retry_after)` when the bucket is empty.
    pub fn check(&self, client: &str) -> Result<(), Duration> {
        let burst = f64::from(self.limit.burst.max(1));
        let rate = f64::from(self.limit.per_minute) / 60.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if buckets.len() > 10_000 {
            // Bounded memory: forget full buckets.
            buckets.retain(|_, b| b.tokens + now.duration_since(b.at).as_secs_f64() * rate < burst);
        }
        let b = buckets.entry(client.to_owned()).or_insert(Bucket {
            tokens: burst,
            at: now,
        });
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rate).min(burst);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else if rate > 0.0 {
            Err(Duration::from_secs_f64((1.0 - b.tokens) / rate))
        } else {
            Err(Duration::from_secs(60))
        }
    }
}
