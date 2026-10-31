//! Shared retry helpers: `Retry-After` parsing (both forms, robust to clock skew) and
//! exponential backoff with jitter.

use std::time::Duration;

use reqwest::header::{DATE, HeaderMap, RETRY_AFTER};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;

/// Longest wait a `Retry-After` may ask for before it is capped (servers occasionally send
/// absurd values; ingest's own backoff takes over after this).
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// The wait a `Retry-After` header asks for.
///
/// Accepts delta-seconds (`120`) and an HTTP-date (`Wed, 21 Oct 2026 07:28:00 GMT`). A date is
/// measured against the response's own `Date` header when present, so a local clock that is
/// minutes off does not turn "retry in 30 s" into "retry now" or "retry in an hour". Dates in the
/// past mean "now" (zero). The result is capped at [`MAX_RETRY_AFTER`].
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    retry_after_at(headers, OffsetDateTime::now_utc())
}

/// [`retry_after`] against an explicit local clock (tests).
pub fn retry_after_at(headers: &HeaderMap, now: OffsetDateTime) -> Option<Duration> {
    let raw = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    let wait = if let Ok(secs) = raw.parse::<u64>() {
        Duration::from_secs(secs)
    } else {
        let at = parse_http_date(raw)?;
        let reference = headers
            .get(DATE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_http_date)
            .unwrap_or(now);
        let d = at - reference;
        if d.is_negative() {
            Duration::ZERO
        } else {
            d.unsigned_abs()
        }
    };
    Some(wait.min(MAX_RETRY_AFTER))
}

/// `Retry-After` in milliseconds, the unit of `Error::RateLimited`.
pub fn retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    retry_after(headers).map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
}

/// Parse an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the HTTP-date form servers send.
/// The obsolete RFC 850 and asctime forms are not accepted (`None`: callers fall back to their
/// own backoff).
pub fn parse_http_date(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s.trim(), &Rfc2822).ok()
}

/// Exponential backoff with jitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub base: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(500),
            max: Duration::from_secs(60),
        }
    }
}

impl Backoff {
    /// The wait before retry number `attempt` (0-based): `base * 2^attempt`, capped at `max`,
    /// plus up to 25% jitter so parallel sources do not retry in lockstep. A server-requested
    /// `retry_after` is a floor (never shortened by the cap or the jitter).
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exp = self
            .base
            .saturating_mul(1u32.checked_shl(attempt.min(20)).unwrap_or(u32::MAX))
            .min(self.max);
        let quarter = u64::try_from(exp.as_millis()).unwrap_or(u64::MAX) / 4;
        let jitter = if quarter == 0 {
            0
        } else {
            jitter_seed() % (quarter + 1)
        };
        let d = exp.saturating_add(Duration::from_millis(jitter));
        match retry_after {
            Some(ra) => d.max(ra),
            None => d,
        }
    }
}

/// Cheap, non-cryptographic jitter source (no RNG dependency).
fn jitter_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn h(pairs: &[(reqwest::header::HeaderName, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(k.clone(), HeaderValue::from_str(v).unwrap());
        }
        m
    }

    #[test]
    fn delta_seconds_and_cap() {
        assert_eq!(
            retry_after(&h(&[(RETRY_AFTER, "7")])),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after(&h(&[(RETRY_AFTER, " 0 ")])),
            Some(Duration::ZERO)
        );
        assert_eq!(
            retry_after(&h(&[(RETRY_AFTER, "999999999")])),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(retry_after(&h(&[(RETRY_AFTER, "soon")])), None);
        assert_eq!(retry_after(&HeaderMap::new()), None);
        assert_eq!(retry_after_ms(&h(&[(RETRY_AFTER, "2")])), Some(2000));
    }

    #[test]
    fn http_date_uses_the_servers_clock() {
        // Local clock is 10 minutes fast; the server says "30 s from my now".
        let local = parse_http_date("Wed, 21 Oct 2026 07:38:00 GMT").unwrap();
        let headers = h(&[
            (DATE, "Wed, 21 Oct 2026 07:28:00 GMT"),
            (RETRY_AFTER, "Wed, 21 Oct 2026 07:28:30 GMT"),
        ]);
        assert_eq!(
            retry_after_at(&headers, local),
            Some(Duration::from_secs(30))
        );
        // Without a Date header, fall back to the local clock; past dates mean now.
        let headers = h(&[(RETRY_AFTER, "Wed, 21 Oct 2026 07:28:30 GMT")]);
        assert_eq!(retry_after_at(&headers, local), Some(Duration::ZERO));
        let early = parse_http_date("Wed, 21 Oct 2026 07:28:00 GMT").unwrap();
        assert_eq!(
            retry_after_at(&headers, early),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn backoff_grows_caps_and_respects_retry_after() {
        let b = Backoff {
            base: Duration::from_millis(100),
            max: Duration::from_secs(1),
        };
        for attempt in 0..40 {
            let d = b.delay(attempt, None);
            let exp = Duration::from_millis(100 * (1u64 << attempt.min(20))).min(b.max);
            assert!(d >= exp && d <= exp + exp / 4, "attempt {attempt}: {d:?}");
        }
        assert_eq!(
            b.delay(0, Some(Duration::from_secs(5))),
            Duration::from_secs(5)
        );
        let huge = Backoff {
            base: Duration::MAX,
            max: Duration::MAX,
        };
        assert!(huge.delay(5, None) >= Duration::from_secs(u64::MAX / 2));
        // Obsolete date forms and a garbage Date header degrade gracefully.
        assert_eq!(
            retry_after(&h(&[(RETRY_AFTER, "Sunday, 06-Nov-94 08:49:37 GMT")])),
            None
        );
        let headers = h(&[
            (DATE, "yesterday"),
            (RETRY_AFTER, "Wed, 21 Oct 2026 07:28:30 GMT"),
        ]);
        let early = parse_http_date("Wed, 21 Oct 2026 07:28:00 GMT").unwrap();
        assert_eq!(
            retry_after_at(&headers, early),
            Some(Duration::from_secs(30))
        );
    }
}
