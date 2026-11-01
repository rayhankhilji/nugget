//! Resumable, parallel HTTP range download into a staging directory.
//!
//! Layout of a staging directory (`<root>/staging/<sha256>/`):
//! - `data.part`: the file being assembled; every part writes at its own offset.
//! - `state.json`: the part layout and how many bytes of each part are durably written.
//!
//! Invariant: `state.json` never claims bytes that have not been written and flushed to
//! `data.part`, so a crash or cancellation at any point leaves a resumable staging dir. The final
//! SHA-256 check is the backstop for anything the OS lost after a power cut.

use std::collections::BTreeMap;
use std::io::{Read, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nugget_core::egress::{EgressExchange, EgressSink};
use nugget_core::{Error, Result};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinSet;

use crate::manifest::ModelEntry;

pub(crate) const DATA_FILE: &str = "data.part";
const STATE_FILE: &str = "state.json";
const STATE_VERSION: u32 = 1;
/// Egress feature name recorded for model downloads.
pub const EGRESS_FEATURE: &str = "model_download";
/// Egress feature name recorded for inference-runtime downloads.
pub const RUNTIME_EGRESS_FEATURE: &str = "runtime_download";

/// Tuning knobs for [`crate::ModelStore::install`].
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Maximum number of parts downloaded in parallel.
    pub parts: usize,
    /// Parts are never smaller than this (small files use fewer parts).
    pub min_part_size: u64,
    /// Consecutive failed attempts (without any progress) tolerated per part before giving up.
    pub max_retries: u32,
    /// Longest time spent retrying without any progress before giving up, so a dead network or
    /// a failing server is reported within seconds rather than after a long backoff ladder.
    pub retry_window: Duration,
    /// A request that gets no response headers, or a body that delivers no bytes, for this long
    /// counts as stalled and fails the attempt (not retried: the user can retry and resume).
    pub stall_timeout: Duration,
    /// First retry delay; doubles per consecutive failure, plus up to 25% jitter.
    pub backoff_base: Duration,
    /// Upper bound for a single retry delay (also caps `Retry-After`).
    pub backoff_max: Duration,
    /// Bytes buffered per part before a write + flush + progress commit.
    pub commit_every: usize,
    /// Committed bytes between `state.json` checkpoints.
    pub persist_every: u64,
    /// Minimum interval between progress callbacks (phase changes are always reported).
    pub progress_interval: Duration,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            parts: 4,
            min_part_size: 32 * 1024 * 1024,
            max_retries: 4,
            retry_window: Duration::from_secs(8),
            stall_timeout: Duration::from_secs(10),
            backoff_base: Duration::from_millis(250),
            backoff_max: Duration::from_secs(2),
            commit_every: 1024 * 1024,
            persist_every: 16 * 1024 * 1024,
            progress_interval: Duration::from_millis(200),
        }
    }
}

/// Install phase reported through the progress callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Downloading,
    Verifying,
    Installed,
    Failed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Downloading => "downloading",
            Phase::Verifying => "verifying",
            Phase::Installed => "installed",
            Phase::Failed => "failed",
        }
    }
}

/// Why an install failed, in terms the user can act on. The technical detail goes to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// DNS or connection failure: no network, or the server cannot be reached.
    Offline,
    /// The server answered with an error status.
    HttpError(u16),
    /// The network served a sign-in page (HTML) instead of the file.
    CaptivePortal,
    /// No bytes arrived for [`DownloadOptions::stall_timeout`].
    Stalled,
    /// The connection kept closing before the data was complete.
    Interrupted,
    /// Not enough disk space (preflight, or the disk filled up while writing).
    DiskFull,
    /// The bytes did not match the manifest (hash or size); staging was discarded.
    Integrity,
    /// Cancelled by the user (or the install was stopped).
    Cancelled,
    Other,
}

impl FailReason {
    /// Best-effort reason for an error that did not come from the download itself.
    pub fn classify(err: &Error) -> Self {
        match err {
            Error::Cancelled => Self::Cancelled,
            Error::InsufficientSpace { .. } => Self::DiskFull,
            Error::Io(e) if e.kind() == std::io::ErrorKind::StorageFull => Self::DiskFull,
            Error::Integrity(_) => Self::Integrity,
            Error::RateLimited { .. } => Self::HttpError(429),
            Error::Network(_) => Self::Interrupted,
            _ => Self::Other,
        }
    }

    /// Short, plain-language message (English; clients localise by [`FailReason`]).
    pub fn message(self) -> String {
        match self {
            Self::Offline => "You're offline or the download server can't be reached. Check your connection and try again.".into(),
            Self::HttpError(code) => format!("The download server returned an error (HTTP {code}). Try again later."),
            Self::CaptivePortal => "This network needs you to sign in (a captive portal) before downloading. Sign in in your browser, then try again.".into(),
            Self::Stalled => "The download stopped making progress. Check your connection and try again.".into(),
            Self::Interrupted => "The download was interrupted. Try again to continue where it stopped.".into(),
            Self::DiskFull => "Not enough disk space. Free up some space and try again.".into(),
            Self::Integrity => "The downloaded file was not the expected one, so it was discarded. Try again.".into(),
            Self::Cancelled => "Download cancelled. Downloaded data is kept, so you can resume later.".into(),
            Self::Other => "The download failed. Try again.".into(),
        }
    }
}

impl From<FailReason> for nugget_core::ipc::ModelFailReason {
    fn from(r: FailReason) -> Self {
        use nugget_core::ipc::ModelFailReason as R;
        match r {
            FailReason::Offline => R::Offline,
            FailReason::HttpError(_) => R::HttpError,
            FailReason::CaptivePortal => R::CaptivePortal,
            FailReason::Stalled => R::Stalled,
            FailReason::Interrupted => R::Interrupted,
            FailReason::DiskFull => R::DiskFull,
            FailReason::Integrity => R::Integrity,
            FailReason::Cancelled => R::Cancelled,
            FailReason::Other => R::Other,
        }
    }
}

/// Progress of one install. In `Verifying`, `done` counts hashed bytes. In `Failed`, `done` is
/// the number of bytes staged (kept for a resume), `error` a plain-language message and
/// `reason` why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub id: String,
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
    pub reason: Option<FailReason>,
}

impl From<&Progress> for nugget_core::ipc::ModelProgress {
    fn from(p: &Progress) -> Self {
        Self {
            id: p.id.clone(),
            downloaded: p.done,
            total: p.total,
            phase: match p.phase {
                Phase::Downloading => nugget_core::ipc::ModelPhase::Downloading,
                Phase::Verifying => nugget_core::ipc::ModelPhase::Verifying,
                Phase::Installed => nugget_core::ipc::ModelPhase::Installed,
                Phase::Failed => nugget_core::ipc::ModelPhase::Failed,
            },
            error: p.error.clone(),
            reason: p.reason.map(Into::into),
            http_status: match p.reason {
                Some(FailReason::HttpError(code)) => Some(code),
                _ => None,
            },
        }
    }
}

pub(crate) type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Throttled progress emitter.
#[derive(Clone)]
pub(crate) struct Reporter {
    id: String,
    total: u64,
    f: ProgressFn,
    interval: Duration,
    last: Arc<Mutex<Option<(Phase, Instant)>>>,
    /// Set by the download when it fails, so [`Reporter::fail`] knows why.
    reason: Arc<Mutex<Option<FailReason>>>,
}

impl Reporter {
    pub(crate) fn new(id: &str, total: u64, f: ProgressFn, interval: Duration) -> Self {
        Self {
            id: id.to_string(),
            total,
            f,
            interval,
            last: Arc::new(Mutex::new(None)),
            reason: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn note_reason(&self, r: FailReason) {
        *lock(&self.reason) = Some(r);
    }

    pub(crate) fn emit(&self, phase: Phase, done: u64) {
        let now = Instant::now();
        {
            let mut last = lock(&self.last);
            let due = match *last {
                Some((p, at)) => p != phase || done >= self.total || now - at >= self.interval,
                None => true,
            };
            if !due {
                return;
            }
            *last = Some((phase, now));
        }
        (self.f)(Progress {
            id: self.id.clone(),
            phase,
            done,
            total: self.total,
            error: None,
            reason: None,
        });
    }

    /// Report the terminal failure. `staged` is the number of bytes kept for a resume.
    pub(crate) fn fail(&self, err: &Error, staged: u64) {
        // A cancellation always reads as one, whatever the download noted before it.
        let reason = match err {
            Error::Cancelled => FailReason::Cancelled,
            _ => lock(&self.reason)
                .take()
                .unwrap_or_else(|| FailReason::classify(err)),
        };
        tracing::warn!(id = %self.id, error = %err, ?reason, staged, "install failed");
        (self.f)(Progress {
            id: self.id.clone(),
            phase: Phase::Failed,
            done: staged,
            total: self.total,
            error: Some(reason.message()),
            reason: Some(reason),
        });
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------------
// Staging state
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagingState {
    version: u32,
    sha256: String,
    size: u64,
    parts: Vec<PartState>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct PartState {
    start: u64,
    /// Exclusive.
    end: u64,
    /// Bytes written and flushed from `start`.
    done: u64,
}

impl PartState {
    fn remaining(&self) -> u64 {
        (self.end - self.start).saturating_sub(self.done)
    }
}

impl StagingState {
    fn fresh(entry: &ModelEntry, parts: Vec<PartState>) -> Self {
        Self {
            version: STATE_VERSION,
            sha256: entry.sha256.clone(),
            size: entry.size,
            parts,
        }
    }

    fn done(&self) -> u64 {
        self.parts.iter().map(|p| p.done).sum()
    }

    fn complete(&self) -> bool {
        !self.parts.is_empty() && self.parts.iter().all(|p| p.remaining() == 0)
    }

    /// Structural sanity: parts are contiguous, cover `[0, size)`, and `done` fits.
    fn is_consistent(&self, entry: &ModelEntry) -> bool {
        if self.version != STATE_VERSION || self.sha256 != entry.sha256 || self.size != entry.size {
            return false;
        }
        let mut expect = 0;
        for p in &self.parts {
            if p.start != expect || p.end <= p.start || p.done > p.end - p.start {
                return false;
            }
            expect = p.end;
        }
        expect == self.size
    }
}

fn layout(size: u64, opts: &DownloadOptions) -> Vec<PartState> {
    let max_parts = opts.parts.max(1) as u64;
    let n = size.div_ceil(opts.min_part_size.max(1)).clamp(1, max_parts);
    let len = size.div_ceil(n);
    (0..n)
        .map(|i| i * len)
        .take_while(|&start| start < size)
        .map(|start| PartState {
            start,
            end: (start + len).min(size),
            done: 0,
        })
        .collect()
}

/// Load a usable staging state, or `None` (and the caller starts over).
fn load_state(dir: &Path, entry: &ModelEntry) -> Option<StagingState> {
    let bytes = std::fs::read(dir.join(STATE_FILE)).ok()?;
    let mut st: StagingState = serde_json::from_slice(&bytes).ok()?;
    if !st.is_consistent(entry) {
        return None;
    }
    // Never trust more than what is physically in the data file.
    let len = std::fs::metadata(dir.join(DATA_FILE)).map_or(0, |m| m.len());
    for p in &mut st.parts {
        p.done = p.done.min(len.saturating_sub(p.start));
    }
    Some(st)
}

fn persist_state(dir: &Path, st: &StagingState) -> std::io::Result<()> {
    // Unique per write: an aborted install's last checkpoint may race a new one's.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{STATE_FILE}.{}.{n}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(st)?)?;
    std::fs::rename(&tmp, dir.join(STATE_FILE))
}

/// Bytes of `entry` already staged in `dir` (counted by the disk preflight).
pub(crate) fn staged_bytes(dir: &Path, entry: &ModelEntry) -> u64 {
    load_state(dir, entry).map_or(0, |s| s.done())
}

// ---------------------------------------------------------------------------------------------
// Egress accounting
// ---------------------------------------------------------------------------------------------

/// Push running totals to the ledger after this many unflushed bytes...
const EGRESS_FLUSH_BYTES: u64 = 8 * 1024 * 1024;
/// ...or this long after the last flush, whichever comes first.
const EGRESS_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// One ledger exchange per host: opened before the first request to that host, flushed
/// periodically while bytes arrive, finished when dropped (success, failure or cancellation).
struct EgressTally {
    sink: Arc<dyn EgressSink>,
    feature: &'static str,
    inner: Mutex<TallyInner>,
}

struct TallyInner {
    hosts: BTreeMap<String, EgressExchange>,
    unflushed: u64,
    last_flush: Instant,
}

impl EgressTally {
    fn new(sink: Arc<dyn EgressSink>, feature: &'static str) -> Self {
        Self {
            sink,
            feature,
            inner: Mutex::new(TallyInner {
                hosts: BTreeMap::new(),
                unflushed: 0,
                last_flush: Instant::now(),
            }),
        }
    }

    fn add(&self, host: &str, sent: u64, received: u64) {
        let mut guard = lock(&self.inner);
        let t = &mut *guard;
        match t.hosts.get_mut(host) {
            Some(x) => {
                x.add_sent(sent);
                x.add_received(received);
            }
            None => {
                // First request to this host: the row is written before it is sent (`begin`
                // waits briefly for that, once per host).
                let mut x =
                    EgressExchange::begin(self.sink.clone(), host, self.feature, Vec::new(), sent);
                x.add_received(received);
                t.hosts.insert(host.to_string(), x);
            }
        }
        t.unflushed = t.unflushed.saturating_add(sent + received);
        if t.unflushed >= EGRESS_FLUSH_BYTES || t.last_flush.elapsed() >= EGRESS_FLUSH_INTERVAL {
            t.hosts.values_mut().for_each(EgressExchange::flush);
            t.unflushed = 0;
            t.last_flush = Instant::now();
        }
    }

    /// Call before sending a request to `url`.
    fn request(&self, url: &str) {
        // Request line + typical headers; an estimate, documented as such.
        self.add(&host_of(url), url.len() as u64 + 160, 0);
    }
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct FetchErr {
    retryable: bool,
    retry_after: Option<Duration>,
    reason: FailReason,
    err: Error,
}

impl FetchErr {
    /// A transport failure (reset, short body): [`Error::Network`], retried.
    fn transient(msg: impl Into<String>) -> Self {
        Self {
            retryable: true,
            retry_after: None,
            reason: FailReason::Interrupted,
            err: Error::Network(format!("download failed: {}", msg.into())),
        }
    }
    /// A request that failed in transport. Connect/DNS failures read as offline; all are retried
    /// (within the retry window).
    fn request(e: &reqwest::Error) -> Self {
        let reason = if e.is_connect() {
            FailReason::Offline
        } else if e.is_timeout() {
            FailReason::Stalled
        } else {
            FailReason::Interrupted
        };
        // reqwest names the URL, which after a redirect may be a signed CDN link: log the host
        // only.
        let mut detail = error_chain(e);
        if let Some(url) = e.url() {
            detail = detail.replace(url.as_str(), &host_of(url.as_str()));
        }
        Self {
            reason,
            ..Self::transient(detail)
        }
    }
    /// No headers or body bytes for `after`. Retried within the retry window, which restarts
    /// on progress: a brief gap mid-download (Wi-Fi roaming, a congested link) resumes, while a
    /// source that never sends anything fails once the window is used up.
    fn stalled(after: Duration) -> Self {
        Self {
            retryable: true,
            retry_after: None,
            reason: FailReason::Stalled,
            err: Error::Network(format!("download stalled: no data for {after:?}")),
        }
    }
    /// The network answered with a web page instead of the file (a captive portal).
    fn portal(detail: impl Into<String>) -> Self {
        Self {
            retryable: false,
            retry_after: None,
            reason: FailReason::CaptivePortal,
            err: Error::Network(format!(
                "download failed: got a web page instead of the file ({})",
                detail.into()
            )),
        }
    }
    fn cancelled() -> Self {
        Self::fatal(Error::Cancelled)
    }
    fn fatal(err: Error) -> Self {
        Self {
            retryable: false,
            retry_after: None,
            reason: FailReason::classify(&err),
            err,
        }
    }
    /// An unexpected HTTP status. 429 is [`Error::RateLimited`] (with the server's hint); other
    /// transient statuses (5xx, 408, 425) are [`Error::Network`], since the source is temporarily
    /// unreachable; anything else (403, 404, 416, ...) is a permanent [`Error::Other`].
    fn status(url: &str, status: StatusCode, retry_after: Option<Duration>) -> Self {
        // 511 Network Authentication Required: the standard captive-portal answer.
        if status == StatusCode::NETWORK_AUTHENTICATION_REQUIRED {
            return Self::portal(format!("HTTP 511 from {}", host_of(url)));
        }
        let retryable = status.is_server_error() || matches!(status.as_u16(), 408 | 425 | 429);
        let msg = format!("download failed: HTTP {status} from {}", host_of(url));
        let err = if status == StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(%msg, ?retry_after, "rate limited");
            Error::RateLimited {
                retry_after_ms: retry_after
                    .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            }
        } else if retryable {
            Error::Network(msg)
        } else {
            Error::Other(msg)
        };
        Self {
            retryable,
            retry_after,
            reason: FailReason::HttpError(status.as_u16()),
            err,
        }
    }
}

/// `e` with its sources (reqwest's top-level text alone rarely says what went wrong). Log only.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn backoff(opts: &DownloadOptions, failures: u32, hint: Option<Duration>) -> Duration {
    let exp = opts
        .backoff_base
        .saturating_mul(1u32 << failures.saturating_sub(1).min(16));
    let base = exp.max(hint.unwrap_or_default()).min(opts.backoff_max);
    base + base.mul_f64(jitter_fraction() * 0.25)
}

/// Retry bookkeeping for one request loop: consecutive failures and time since the last
/// progress, both bounded ([`DownloadOptions::max_retries`], [`DownloadOptions::retry_window`]).
struct Retry {
    failures: u32,
    since: Instant,
    /// Sum of the delays handed out since the last progress.
    planned: Duration,
}

impl Retry {
    fn new() -> Self {
        Self {
            failures: 0,
            since: Instant::now(),
            planned: Duration::ZERO,
        }
    }

    fn progressed(&mut self) {
        *self = Self::new();
    }

    /// How long to wait before trying again after `err`, or `None` to give up.
    fn next(&mut self, opts: &DownloadOptions, err: &FetchErr) -> Option<Duration> {
        if !err.retryable || self.failures >= opts.max_retries {
            return None;
        }
        self.failures += 1;
        let delay = backoff(opts, self.failures, err.retry_after);
        let spent = self.since.elapsed().max(self.planned);
        if spent + delay > opts.retry_window {
            return None;
        }
        self.planned += delay;
        Some(delay)
    }
}

/// A cheap uniform-ish value in `[0, 1)` from std's randomly keyed hasher.
fn jitter_fraction() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(0);
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// Parse `bytes START-END/TOTAL` (TOTAL may be `*`).
fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (s, e) = range.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((s.trim().parse().ok()?, e.trim().parse().ok()?, total))
}

/// The response is a web page (a captive portal's sign-in page, or an error page with 200).
fn is_html(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            let v = v.trim_start().to_ascii_lowercase();
            v.starts_with("text/html") || v.starts_with("application/xhtml")
        })
}

/// `bytes` (the start of a file) look like an HTML document.
fn sniff_html(bytes: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let head: Vec<u8> = bytes[start..bytes.len().min(start + 16)]
        .iter()
        .map(u8::to_ascii_lowercase)
        .collect();
    [
        &b"<!doctype html"[..],
        b"<html",
        b"<head",
        b"<body",
        b"<meta",
    ]
    .iter()
    .any(|p| head.starts_with(p))
}

/// Send `req`, giving up after [`DownloadOptions::stall_timeout`] without response headers.
async fn send(
    opts: &DownloadOptions,
    req: reqwest::RequestBuilder,
) -> std::result::Result<reqwest::Response, FetchErr> {
    match tokio::time::timeout(opts.stall_timeout, req.send()).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(FetchErr::request(&e)),
        Err(_) => Err(FetchErr::stalled(opts.stall_timeout)),
    }
}

// ---------------------------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------------------------

/// A cancellation flag that can also be awaited. Shared by an install and whoever may cancel it.
#[derive(Debug, Default)]
pub(crate) struct CancelToken {
    flag: Arc<AtomicBool>,
    notify: tokio::sync::Notify,
}

impl CancelToken {
    pub(crate) fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Resolves once [`Self::cancel`] has been called (immediately if it already was).
    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// Run `fut` unless `cancel` fires first.
async fn or_cancel<T>(
    cancel: &CancelToken,
    fut: impl std::future::Future<Output = std::result::Result<T, FetchErr>>,
) -> std::result::Result<T, FetchErr> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(FetchErr::cancelled()),
        r = fut => r,
    }
}

// ---------------------------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------------------------

pub(crate) struct Download<'a> {
    pub client: &'a reqwest::Client,
    pub entry: &'a ModelEntry,
    pub dir: PathBuf,
    pub opts: &'a DownloadOptions,
    pub reporter: Reporter,
    pub egress: Arc<dyn EgressSink>,
    /// Egress feature name ([`EGRESS_FEATURE`] or [`RUNTIME_EGRESS_FEATURE`]).
    pub feature: &'static str,
    /// Stops the download (keeping every committed byte) when cancelled.
    pub cancel: Arc<CancelToken>,
}

struct Ctx {
    client: reqwest::Client,
    url: String,
    size: u64,
    dir: PathBuf,
    opts: DownloadOptions,
    state: Mutex<(StagingState, u64)>,
    downloaded: AtomicU64,
    reporter: Reporter,
    tally: Arc<EgressTally>,
    /// Set once `run` has written the final `state.json`.
    persisted: AtomicBool,
}

impl Drop for Ctx {
    /// Backstop for a download whose future was dropped (not cancelled through its token):
    /// checkpoint what was committed, or a resume would start from the last periodic checkpoint.
    fn drop(&mut self) {
        if !self.persisted.load(Ordering::SeqCst) {
            let st = lock(&self.state).0.clone();
            if let Err(e) = persist_state(&self.dir, &st) {
                tracing::warn!(dir = %self.dir.display(), error = %e, "cannot checkpoint download");
            }
        }
    }
}

enum ProbeOutcome {
    /// Server honours byte ranges.
    Ranged,
    /// Server ignores ranges: this full-body response is the download.
    Full(reqwest::Response),
}

impl Download<'_> {
    /// Ensure `<dir>/data.part` holds all `entry.size` bytes (not yet hash-verified). On failure
    /// the reason is noted on the reporter.
    pub(crate) async fn run(self) -> Result<PathBuf> {
        let reporter = self.reporter.clone();
        self.run_inner().await.map_err(|e| {
            reporter.note_reason(e.reason);
            e.err
        })
    }

    async fn run_inner(self) -> std::result::Result<PathBuf, FetchErr> {
        let Download {
            client,
            entry,
            dir,
            opts,
            reporter,
            egress,
            feature,
            cancel,
        } = self;
        let io = |e: std::io::Error| FetchErr::fatal(e.into());
        std::fs::create_dir_all(&dir).map_err(io)?;
        let data = dir.join(DATA_FILE);
        let resumed = load_state(&dir, entry);
        if resumed.as_ref().is_some_and(StagingState::complete) {
            return Ok(data);
        }
        if resumed.is_none() {
            // Unknown or inconsistent leftovers: start clean.
            let _ = std::fs::remove_file(&data);
        }

        let tally = Arc::new(EgressTally::new(egress, feature));

        let (url, probe) = or_cancel(&cancel, probe_sources(client, entry, opts, &tally)).await?;

        let state = match (resumed, &probe) {
            (Some(st), ProbeOutcome::Ranged) => st,
            (_, ProbeOutcome::Ranged) => StagingState::fresh(entry, layout(entry.size, opts)),
            // No range support: one sequential part from byte 0, no resume.
            (resumed, ProbeOutcome::Full(_)) => {
                if resumed.is_some_and(|s| s.done() > 0) {
                    tracing::info!(model = %entry.id, "server ignores Range; restarting from byte 0");
                }
                StagingState::fresh(
                    entry,
                    vec![PartState {
                        start: 0,
                        end: entry.size,
                        done: 0,
                    }],
                )
            }
        };
        let done = state.done();
        if done > 0 {
            tracing::info!(model = %entry.id, done, total = entry.size, "resuming download");
        }
        persist_state(&dir, &state).map_err(io)?;
        reporter.emit(Phase::Downloading, done);

        let ctx = Arc::new(Ctx {
            client: client.clone(),
            url,
            size: entry.size,
            dir: dir.clone(),
            opts: opts.clone(),
            state: Mutex::new((state, 0)),
            downloaded: AtomicU64::new(done),
            reporter,
            tally,
            persisted: AtomicBool::new(false),
        });

        let result = match probe {
            ProbeOutcome::Ranged => run_parts(&ctx, &cancel).await,
            ProbeOutcome::Full(resp) => or_cancel(&cancel, run_sequential(&ctx, resp)).await,
        };
        let st = lock(&ctx.state).0.clone();
        persist_state(&dir, &st).map_err(io)?;
        ctx.persisted.store(true, Ordering::SeqCst);
        result?;
        if !st.complete() {
            return Err(FetchErr::fatal(Error::Other(
                "download ended before all parts completed".into(),
            )));
        }
        Ok(data)
    }
}

/// Find the first source that serves a file of the expected size.
async fn probe_sources(
    client: &reqwest::Client,
    entry: &ModelEntry,
    opts: &DownloadOptions,
    tally: &EgressTally,
) -> std::result::Result<(String, ProbeOutcome), FetchErr> {
    let mut last_err = None;
    for url in entry.sources() {
        let mut retry = Retry::new();
        loop {
            match probe(client, url, entry.size, opts, tally).await {
                Ok(outcome) => return Ok((url.to_string(), outcome)),
                Err(e) => match retry.next(opts, &e) {
                    Some(delay) => {
                        tracing::warn!(url, error = %e.err, failures = retry.failures, "probe failed; retrying");
                        tokio::time::sleep(delay).await;
                    }
                    None => {
                        tracing::warn!(url, error = %e.err, reason = ?e.reason, "source unusable");
                        last_err = Some(e);
                        break;
                    }
                },
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| FetchErr::fatal(Error::Invalid("model has no download source".into()))))
}

async fn probe(
    client: &reqwest::Client,
    url: &str,
    size: u64,
    opts: &DownloadOptions,
    tally: &EgressTally,
) -> std::result::Result<ProbeOutcome, FetchErr> {
    tally.request(url);
    let resp = send(opts, client.get(url).header(RANGE, "bytes=0-0")).await?;
    let host = resp.url().host_str().unwrap_or("unknown").to_string();
    let status = resp.status();
    if (status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT) && is_html(&resp) {
        return Err(FetchErr::portal(format!("{host} answered with text/html")));
    }
    match status {
        StatusCode::PARTIAL_CONTENT => {
            tally.add(&host, 0, 1);
            let total = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .and_then(|(_, _, t)| t);
            match total {
                Some(t) if t == size => Ok(ProbeOutcome::Ranged),
                Some(t) => Err(FetchErr::fatal(Error::Integrity(format!(
                    "{host} serves {t} bytes but the manifest says {size}"
                )))),
                None => Err(FetchErr::fatal(Error::Other(format!(
                    "{host} sent a partial response without a usable Content-Range"
                )))),
            }
        }
        StatusCode::OK => match resp.content_length() {
            Some(len) if len != size => Err(FetchErr::fatal(Error::Integrity(format!(
                "{host} serves {len} bytes but the manifest says {size}"
            )))),
            _ => Ok(ProbeOutcome::Full(resp)),
        },
        s => Err(FetchErr::status(url, s, retry_after(&resp))),
    }
}

async fn run_parts(ctx: &Arc<Ctx>, cancel: &CancelToken) -> std::result::Result<(), FetchErr> {
    let pending: Vec<usize> = {
        let st = lock(&ctx.state);
        (0..st.0.parts.len())
            .filter(|&i| st.0.parts[i].remaining() > 0)
            .collect()
    };
    let mut set = JoinSet::new();
    for idx in pending {
        let ctx = Arc::clone(ctx);
        set.spawn(async move { run_part(&ctx, idx).await });
    }
    loop {
        let res = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(FetchErr::cancelled()),
            joined = set.join_next() => match joined {
                None => return Ok(()),
                Some(Ok(r)) => r,
                Some(Err(e)) => Err(FetchErr::fatal(Error::Other(format!("download task failed: {e}")))),
            },
        };
        if let Err(e) = res {
            // Fail fast; `state.json` keeps every committed byte for the next attempt. Wait for
            // the aborted parts to stop: until then they can still commit progress and record
            // egress, which would race the caller's final `persist_state` and its accounting.
            set.abort_all();
            while set.join_next().await.is_some() {}
            return Err(e);
        }
    }
}

async fn run_part(ctx: &Ctx, idx: usize) -> std::result::Result<(), FetchErr> {
    let mut retry = Retry::new();
    loop {
        let (from, end) = {
            let st = lock(&ctx.state);
            let p = st.0.parts[idx];
            (p.start + p.done, p.end)
        };
        if from >= end {
            return Ok(());
        }
        let (progressed, res) = fetch_range(ctx, idx, from, end).await;
        if progressed > 0 {
            retry.progressed();
        }
        let err = match res {
            Ok(()) if progressed > 0 => continue,
            Ok(()) => FetchErr::transient("server returned an empty range"),
            Err(e) => e,
        };
        let Some(delay) = retry.next(&ctx.opts, &err) else {
            return Err(err);
        };
        tracing::debug!(part = idx, from, error = %err.err, failures = retry.failures, "part failed; retrying");
        tokio::time::sleep(delay).await;
    }
}

/// One ranged request for `[from, end)` of part `idx`. Returns bytes committed and the outcome.
async fn fetch_range(
    ctx: &Ctx,
    idx: usize,
    from: u64,
    end: u64,
) -> (u64, std::result::Result<(), FetchErr>) {
    ctx.tally.request(&ctx.url);
    let req = ctx
        .client
        .get(&ctx.url)
        .header(RANGE, format!("bytes={from}-{}", end - 1));
    let resp = match send(&ctx.opts, req).await {
        Ok(r) => r,
        Err(e) => return (0, Err(e)),
    };
    if resp.status().is_success() && is_html(&resp) {
        return (
            0,
            Err(FetchErr::portal("text/html response to a range request")),
        );
    }
    match resp.status() {
        StatusCode::PARTIAL_CONTENT => {
            let cr = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range);
            match cr {
                Some((s, e, t)) if s == from && e < end && t.is_none_or(|t| t == ctx.size) => {}
                _ => {
                    return (
                        0,
                        Err(FetchErr::fatal(Error::Other(format!(
                            "server answered range {from}-{} with Content-Range {:?}",
                            end - 1,
                            resp.headers().get(CONTENT_RANGE)
                        )))),
                    );
                }
            }
        }
        // Whole-file request that the server answered in full: equally good.
        StatusCode::OK if from == 0 && end == ctx.size => {}
        StatusCode::OK => {
            return (
                0,
                Err(FetchErr {
                    reason: FailReason::Interrupted,
                    ..FetchErr::fatal(Error::Other(
                        "server stopped honouring Range requests".into(),
                    ))
                }),
            );
        }
        s => return (0, Err(FetchErr::status(&ctx.url, s, retry_after(&resp)))),
    }
    write_body(ctx, idx, from, end, resp).await
}

/// Stream `resp` into `data.part` at `from`, committing every `commit_every` bytes.
async fn write_body(
    ctx: &Ctx,
    idx: usize,
    from: u64,
    end: u64,
    mut resp: reqwest::Response,
) -> (u64, std::result::Result<(), FetchErr>) {
    let host = resp.url().host_str().unwrap_or("unknown").to_string();
    let mut file = match open_at(&ctx.dir.join(DATA_FILE), from).await {
        Ok(f) => f,
        Err(e) => return (0, Err(FetchErr::fatal(e.into()))),
    };
    let cap = ctx.opts.commit_every.max(1);
    let mut buf: Vec<u8> = Vec::with_capacity(cap);
    let mut pos = from;
    let mut committed = 0u64;
    let mut first = from == 0;

    let outcome = loop {
        let chunk = match tokio::time::timeout(ctx.opts.stall_timeout, resp.chunk()).await {
            Err(_) => break Err(FetchErr::stalled(ctx.opts.stall_timeout)),
            Ok(Err(e)) => break Err(FetchErr::request(&e)),
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some(chunk))) => chunk,
        };
        ctx.tally.add(&host, 0, chunk.len() as u64);
        if std::mem::take(&mut first) && sniff_html(&chunk) {
            // A sign-in page served as if it were the file: never stage it.
            break Err(FetchErr::portal("the body is an HTML document"));
        }
        let room = (end - pos - buf.len() as u64) as usize;
        let take = chunk.len().min(room);
        buf.extend_from_slice(&chunk[..take]);
        if buf.len() >= cap || take < chunk.len() {
            match flush(ctx, idx, &mut file, &mut buf).await {
                Ok(n) => {
                    pos += n;
                    committed += n;
                }
                Err(e) => break Err(FetchErr::fatal(e.into())),
            }
        }
        if take < chunk.len() || pos >= end {
            // Server sent more than asked for; we have what we need.
            break Ok(());
        }
    };
    // Keep whatever arrived before an error: it is valid data at a known offset.
    match flush(ctx, idx, &mut file, &mut buf).await {
        Ok(n) => committed += n,
        Err(e) => return (committed, Err(FetchErr::fatal(e.into()))),
    }
    (committed, outcome)
}

async fn open_at(path: &Path, offset: u64) -> std::io::Result<tokio::fs::File> {
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await?;
    f.seek(SeekFrom::Start(offset)).await?;
    Ok(f)
}

/// Write + flush the buffer, then (and only then) record it as done.
async fn flush(
    ctx: &Ctx,
    idx: usize,
    file: &mut tokio::fs::File,
    buf: &mut Vec<u8>,
) -> std::io::Result<u64> {
    if buf.is_empty() {
        return Ok(0);
    }
    file.write_all(buf).await?;
    file.flush().await?;
    let n = buf.len() as u64;
    buf.clear();
    commit(ctx, idx, n)?;
    Ok(n)
}

fn commit(ctx: &Ctx, idx: usize, n: u64) -> std::io::Result<()> {
    {
        let mut guard = lock(&ctx.state);
        let (st, unpersisted) = &mut *guard;
        st.parts[idx].done += n;
        *unpersisted += n;
        if *unpersisted >= ctx.opts.persist_every {
            persist_state(&ctx.dir, st)?;
            *unpersisted = 0;
        }
    }
    let total = ctx.downloaded.fetch_add(n, Ordering::Relaxed) + n;
    ctx.reporter.emit(Phase::Downloading, total);
    Ok(())
}

/// Server without range support: stream the whole body, restarting from zero on failure.
async fn run_sequential(ctx: &Ctx, first: reqwest::Response) -> std::result::Result<(), FetchErr> {
    let mut next = Some(first);
    let mut retry = Retry::new();
    loop {
        {
            let mut st = lock(&ctx.state);
            st.0.parts[0].done = 0;
        }
        ctx.downloaded.store(0, Ordering::Relaxed);
        let resp = match next.take() {
            Some(r) => Ok(r),
            None => {
                ctx.tally.request(&ctx.url);
                match send(&ctx.opts, ctx.client.get(&ctx.url)).await {
                    Ok(r) if r.status() == StatusCode::OK && is_html(&r) => {
                        Err(FetchErr::portal("text/html response"))
                    }
                    Ok(r) if r.status() == StatusCode::OK => Ok(r),
                    Ok(r) => Err(FetchErr::status(&ctx.url, r.status(), retry_after(&r))),
                    Err(e) => Err(e),
                }
            }
        };
        let err = match resp {
            Ok(resp) => {
                let (n, res) = write_body(ctx, 0, 0, ctx.size, resp).await;
                if n > 0 {
                    retry.progressed();
                }
                match res {
                    Ok(()) if n == ctx.size => return Ok(()),
                    Ok(()) => FetchErr::transient(format!(
                        "connection closed after {n} of {} bytes",
                        ctx.size
                    )),
                    Err(e) => e,
                }
            }
            Err(e) => e,
        };
        let Some(delay) = retry.next(&ctx.opts, &err) else {
            return Err(err);
        };
        tokio::time::sleep(delay).await;
    }
}

/// Streaming SHA-256 of a file (blocking; run on a blocking thread). `on_bytes` gets the running
/// count of hashed bytes. `cancel` is checked every 1 MiB: once set, hashing stops with
/// [`Error::Cancelled`], so a dropped install does not keep reading gigabytes (and does not hold
/// up runtime shutdown).
pub(crate) fn sha256_file(
    path: &Path,
    cancel: &AtomicBool,
    mut on_bytes: impl FnMut(u64),
) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = read_full(&mut f, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
        on_bytes(total);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Bytes hashed between cancellation checks.
const HASH_CHUNK: usize = 1024 * 1024;

/// Fill `buf` unless EOF comes first; returns the bytes read (0 only at EOF).
fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Cancels its token when dropped. Held by an install/verify future, so dropping the future
/// stops its blocking hash (and its download, if that is still running).
pub(crate) struct CancelOnDrop(pub(crate) Arc<CancelToken>);

impl CancelOnDrop {
    pub(crate) fn new() -> Self {
        Self(Arc::new(CancelToken::default()))
    }

    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        self.0.flag.clone()
    }

    pub(crate) fn token(&self) -> Arc<CancelToken> {
        self.0.clone()
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(parts: usize, min: u64) -> DownloadOptions {
        DownloadOptions {
            parts,
            min_part_size: min,
            ..DownloadOptions::default()
        }
    }

    #[test]
    fn layout_covers_range_without_gaps() {
        for (size, parts, min) in [(1, 4, 1), (10, 4, 1), (10, 3, 1), (100, 4, 30), (7, 8, 1)] {
            let l = layout(size, &opts(parts, min));
            assert!(!l.is_empty() && l.len() <= parts);
            assert_eq!(l[0].start, 0);
            assert_eq!(l.last().unwrap().end, size);
            for w in l.windows(2) {
                assert_eq!(w[0].end, w[1].start);
            }
        }
        // Small files are not split below min_part_size.
        assert_eq!(layout(100, &opts(4, 60)).len(), 2);
        assert_eq!(layout(100, &opts(4, 1000)).len(), 1);
    }

    #[test]
    fn html_sniffing() {
        assert!(sniff_html(b"<!DOCTYPE html><html>"));
        assert!(sniff_html(b"\r\n  <HTML><body>sign in</body>"));
        assert!(!sniff_html(b"GGUF\x03\x00\x00\x00"));
        assert!(!sniff_html(b""));
    }

    #[test]
    fn retry_gives_up_at_the_window() {
        let o = DownloadOptions {
            max_retries: 100,
            backoff_base: Duration::from_millis(400),
            backoff_max: Duration::from_millis(400),
            retry_window: Duration::from_millis(1000),
            ..DownloadOptions::default()
        };
        let mut r = Retry::new();
        let e = FetchErr::transient("reset");
        // 400-500 ms each: at most two fit in a one-second window.
        let n = std::iter::from_fn(|| r.next(&o, &e)).take(10).count();
        assert!(n <= 2, "{n}");
        assert!(r.next(&o, &FetchErr::stalled(Duration::ZERO)).is_none());
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(parse_content_range("bytes 0-0/10"), Some((0, 0, Some(10))));
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("items 0-1/2"), None);
        assert_eq!(parse_content_range("bytes */10"), None);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let o = DownloadOptions {
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(1),
            ..DownloadOptions::default()
        };
        let d1 = backoff(&o, 1, None);
        let d3 = backoff(&o, 3, None);
        assert!(d1 >= Duration::from_millis(100) && d1 <= Duration::from_millis(125));
        assert!(d3 >= Duration::from_millis(400) && d3 <= Duration::from_millis(500));
        assert!(backoff(&o, 30, None) <= Duration::from_millis(1250));
        // Retry-After raises the delay but stays capped.
        assert!(backoff(&o, 1, Some(Duration::from_secs(60))) >= Duration::from_secs(1));
        assert!(backoff(&o, 1, Some(Duration::from_secs(60))) <= Duration::from_millis(1250));
    }

    #[test]
    fn status_errors_map_to_core_variants() {
        let err = |code: u16, hint: Option<Duration>| {
            FetchErr::status("http://h/x", StatusCode::from_u16(code).unwrap(), hint).err
        };
        assert!(matches!(
            err(429, Some(Duration::from_secs(2))),
            Error::RateLimited {
                retry_after_ms: Some(2000)
            }
        ));
        assert!(matches!(
            err(429, None),
            Error::RateLimited {
                retry_after_ms: None
            }
        ));
        for code in [500, 503, 408] {
            assert!(
                matches!(err(code, None), Error::Network(m) if m.contains(&code.to_string())),
                "{code}"
            );
        }
        for code in [403, 404, 416] {
            assert!(
                matches!(err(code, None), Error::Other(m) if m.contains(&code.to_string())),
                "{code}"
            );
        }
        assert!(matches!(
            FetchErr::transient("connection reset").err,
            Error::Network(_)
        ));
    }

    #[test]
    fn hashing_stops_within_a_chunk_of_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big");
        // Sparse: no disk used.
        std::fs::File::create(&p)
            .unwrap()
            .set_len(64 * HASH_CHUNK as u64)
            .unwrap();
        let cancel = CancelOnDrop::new();
        let flag = cancel.flag();
        let mut cancel = Some(cancel);
        let mut hashed = 0;
        let res = sha256_file(&p, &flag, |n| {
            hashed = n;
            if n >= 3 * HASH_CHUNK as u64 {
                drop(cancel.take()); // the install future going away
            }
        });
        assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");
        assert_eq!(hashed, 3 * HASH_CHUNK as u64);
    }
}
