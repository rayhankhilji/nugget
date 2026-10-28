//! Tunables for the ingest runtime.

use std::time::Duration;

use nugget_extract::{ChunkOptions, ExtractOptions};
use serde::{Deserialize, Serialize};

/// Ingest settings. Every field has a sensible default, so `IngestConfig::default()` is a
/// working configuration and a partial `[ingest]` TOML table deserializes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestConfig {
    /// Per-item extraction timeout. On timeout the item is indexed by title only and the error
    /// is recorded in its metadata. Default 30 s.
    pub extract_timeout_ms: u64,
    /// Refuse inputs larger than this many bytes (extraction fails; title-only item).
    pub max_input_bytes: u64,
    /// Truncate extracted text beyond this many chars.
    pub max_text_chars: usize,
    /// Run OCR on images when OCR models are present.
    pub ocr: bool,
    /// Chunker target size, in estimated tokens.
    pub chunk_target_tokens: usize,
    /// Chunker hard maximum, in estimated tokens.
    pub chunk_max_tokens: usize,
    /// Chunk overlap, in estimated tokens.
    pub chunk_overlap_tokens: usize,
    /// Changes buffered per source before `ChangeSink::send` waits (backpressure).
    pub queue_capacity: usize,
    /// Keep the full extracted text on the item row (needed for previews and re-chunking).
    pub keep_text: bool,
    /// Run triage (keep / junk / sensitive / summary) when a local `Tier::Triage` provider
    /// with JSON-schema output exists. Cloud triage providers are never used.
    pub triage: bool,
    /// Timeout for one triage call.
    pub triage_timeout_ms: u64,
    /// Characters of text sent to triage.
    pub triage_max_chars: usize,
    /// Compute embeddings with `Tier::Embedding` providers.
    pub embeddings: bool,
    /// Allow `Cloud` embedding providers for non-sensitive items. Default `false`: on-device and
    /// LAN providers only.
    pub allow_cloud_embeddings: bool,
    /// Chunks per embedding request.
    pub embed_batch: usize,
    /// Timeout for one embedding request.
    pub embed_timeout_ms: u64,
    /// First retry delay after a failed sync pass; doubles per failure.
    pub backoff_base_ms: u64,
    /// Retry delay cap.
    pub backoff_max_ms: u64,
    /// While watching, re-run a reconciling sync this often (0 = only on `sync_now` or when
    /// the watch ends). Default 1 h.
    pub watch_resync_ms: u64,
    /// Battery fraction (0..1) under which backfills pause while discharging.
    pub low_battery_threshold: f32,
    /// How often the power policy is re-checked while paused.
    pub power_poll_ms: u64,
    /// Item-level errors kept per source.
    pub error_ring: usize,
    /// Extractions allowed to run at once across all sources (also bounds threads left behind
    /// by timed-out extractions). 0 = number of CPUs.
    pub max_concurrent_extractions: usize,
    /// Freshness samples kept for the rolling p50/p95.
    pub freshness_window: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        let extract = ExtractOptions::default();
        let chunk = ChunkOptions::default();
        Self {
            extract_timeout_ms: 30_000,
            max_input_bytes: extract.max_input_bytes,
            max_text_chars: extract.max_text_chars,
            ocr: extract.ocr,
            chunk_target_tokens: chunk.target_tokens,
            chunk_max_tokens: chunk.max_tokens,
            chunk_overlap_tokens: chunk.overlap_tokens,
            queue_capacity: 64,
            keep_text: true,
            triage: true,
            triage_timeout_ms: 30_000,
            triage_max_chars: 6_000,
            embeddings: true,
            allow_cloud_embeddings: false,
            embed_batch: 32,
            embed_timeout_ms: 120_000,
            backoff_base_ms: 2_000,
            backoff_max_ms: 15 * 60 * 1000,
            watch_resync_ms: 60 * 60 * 1000,
            low_battery_threshold: 0.20,
            power_poll_ms: 60_000,
            error_ring: 32,
            max_concurrent_extractions: 0,
            freshness_window: 1024,
        }
    }
}

impl IngestConfig {
    pub(crate) fn extract_options(&self) -> ExtractOptions {
        ExtractOptions {
            max_input_bytes: self.max_input_bytes,
            max_text_chars: self.max_text_chars,
            ocr: self.ocr,
        }
    }

    pub(crate) fn chunk_options(&self) -> ChunkOptions {
        ChunkOptions {
            target_tokens: self.chunk_target_tokens.max(1),
            max_tokens: self.chunk_max_tokens.max(1),
            overlap_tokens: self.chunk_overlap_tokens,
        }
    }

    pub(crate) fn extract_timeout(&self) -> Duration {
        Duration::from_millis(self.extract_timeout_ms.max(1))
    }

    pub(crate) fn triage_timeout(&self) -> Duration {
        Duration::from_millis(self.triage_timeout_ms.max(1))
    }

    pub(crate) fn embed_timeout(&self) -> Duration {
        Duration::from_millis(self.embed_timeout_ms.max(1))
    }

    pub(crate) fn extraction_slots(&self) -> usize {
        if self.max_concurrent_extractions > 0 {
            self.max_concurrent_extractions
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
                .max(2)
        }
    }

    /// Delay before retry number `attempt` (0-based), with ±25 % jitter, capped.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let base = self.backoff_base_ms.max(1);
        let cap = self.backoff_max_ms.max(base);
        let exp = base.saturating_mul(1u64 << attempt.min(32));
        let delay = exp.min(cap);
        // Jitter in [-25 %, +25 %], from the process-random std hasher (no RNG dependency).
        let r = jitter_unit();
        let spread = delay / 4;
        let jittered = (delay - spread).saturating_add((2.0 * spread as f64 * r) as u64);
        Duration::from_millis(jittered.min(cap).max(1))
    }
}

/// A pseudo-random number in `[0, 1)`.
fn jitter_unit() -> f64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        let cfg = IngestConfig {
            backoff_base_ms: 100,
            backoff_max_ms: 1_000,
            ..Default::default()
        };
        for attempt in 0..40 {
            let d = cfg.backoff(attempt).as_millis() as u64;
            let nominal = (100u64 << attempt.min(32)).min(1_000);
            assert!(d >= nominal - nominal / 4, "attempt {attempt}: {d}");
            assert!(d <= 1_000, "attempt {attempt}: {d}");
        }
        assert!(cfg.backoff(0) < Duration::from_millis(200));
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let cfg: IngestConfig = toml::from_str("queue_capacity = 8").unwrap();
        assert_eq!(cfg.queue_capacity, 8);
        assert_eq!(cfg.extract_timeout_ms, 30_000);
        assert!(!cfg.allow_cloud_embeddings);
    }
}
