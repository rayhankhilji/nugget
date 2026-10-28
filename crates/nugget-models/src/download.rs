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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::{Error, Result};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE, RETRY_AFTER};
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

/// Tuning knobs for [`crate::ModelStore::install`].
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Maximum number of parts downloaded in parallel.
    pub parts: usize,
    /// Parts are never smaller than this (small files use fewer parts).
    pub min_part_size: u64,
    /// Consecutive failed attempts (without any progress) tolerated per part before giving up.
    pub max_retries: u32,
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
            max_retries: 8,
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
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

/// Progress of one install. In `Verifying`, `done` counts hashed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub id: String,
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
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
}

impl Reporter {
    pub(crate) fn new(id: &str, total: u64, f: ProgressFn, interval: Duration) -> Self {
        Self {
            id: id.to_string(),
            total,
            f,
            interval,
            last: Arc::new(Mutex::new(None)),
        }
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
        });
    }

    pub(crate) fn fail(&self, err: &Error) {
        (self.f)(Progress {
            id: self.id.clone(),
            phase: Phase::Failed,
            done: 0,
            total: self.total,
            error: Some(err.to_string()),
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
    let tmp = dir.join(format!("{STATE_FILE}.tmp"));
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

/// Per-host byte counts; recorded to the sink when dropped (success, failure or cancellation).
struct EgressTally {
    sink: Arc<dyn EgressSink>,
    hosts: Mutex<BTreeMap<String, (u64, u64)>>,
}

impl EgressTally {
    fn add(&self, host: &str, sent: u64, received: u64) {
        let mut h = lock(&self.hosts);
        let e = h.entry(host.to_string()).or_default();
        e.0 += sent;
        e.1 += received;
    }

    fn request(&self, url: &str) {
        // Request line + typical headers; an estimate, documented as such.
        self.add(&host_of(url), url.len() as u64 + 160, 0);
    }
}

impl Drop for EgressTally {
    fn drop(&mut self) {
        let hosts = std::mem::take(&mut *lock(&self.hosts));
        for (host, (sent, received)) in hosts {
            self.sink.record(EgressEvent {
                at: time::OffsetDateTime::now_utc(),
                host,
                feature: EGRESS_FEATURE.to_string(),
                bytes_sent: sent,
                bytes_received: received,
                items: Vec::new(),
            });
        }
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
    err: Error,
}

impl FetchErr {
    /// A transport failure (connect, timeout, reset, short body): [`Error::Network`], retried.
    fn transient(msg: impl Into<String>) -> Self {
        Self {
            retryable: true,
            retry_after: None,
            err: Error::Network(format!("download failed: {}", msg.into())),
        }
    }
    fn fatal(err: Error) -> Self {
        Self {
            retryable: false,
            retry_after: None,
            err,
        }
    }
    /// An unexpected HTTP status. 429 is [`Error::RateLimited`] (with the server's hint); other
    /// transient statuses (5xx, 408, 425) are [`Error::Network`], since the source is temporarily
    /// unreachable; anything else (403, 404, 416, ...) is a permanent [`Error::Other`].
    fn status(url: &str, status: StatusCode, retry_after: Option<Duration>) -> Self {
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
            err,
        }
    }
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
}

enum ProbeOutcome {
    /// Server honours byte ranges.
    Ranged,
    /// Server ignores ranges: this full-body response is the download.
    Full(reqwest::Response),
}

impl Download<'_> {
    /// Ensure `<dir>/data.part` holds all `entry.size` bytes (not yet hash-verified).
    pub(crate) async fn run(self) -> Result<PathBuf> {
        let Download {
            client,
            entry,
            dir,
            opts,
            reporter,
            egress,
        } = self;
        std::fs::create_dir_all(&dir)?;
        let data = dir.join(DATA_FILE);
        let resumed = load_state(&dir, entry);
        if resumed.as_ref().is_some_and(StagingState::complete) {
            return Ok(data);
        }
        if resumed.is_none() {
            // Unknown or inconsistent leftovers: start clean.
            let _ = std::fs::remove_file(&data);
        }

        let tally = Arc::new(EgressTally {
            sink: egress,
            hosts: Mutex::new(BTreeMap::new()),
        });

        let (url, probe) = probe_sources(client, entry, opts, &tally).await?;

        let state = match (resumed, &probe) {
            (Some(st), ProbeOutcome::Ranged) => st,
            (_, ProbeOutcome::Ranged) => StagingState::fresh(entry, layout(entry.size, opts)),
            // No range support: one sequential part from byte 0, no resume.
            (_, ProbeOutcome::Full(_)) => StagingState::fresh(
                entry,
                vec![PartState {
                    start: 0,
                    end: entry.size,
                    done: 0,
                }],
            ),
        };
        let done = state.done();
        if done > 0 {
            tracing::info!(model = %entry.id, done, total = entry.size, "resuming download");
        }
        persist_state(&dir, &state)?;
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
        });

        let result = match probe {
            ProbeOutcome::Ranged => run_parts(&ctx).await,
            ProbeOutcome::Full(resp) => run_sequential(&ctx, resp).await,
        };
        let st = lock(&ctx.state).0.clone();
        persist_state(&dir, &st)?;
        result?;
        if !st.complete() {
            return Err(Error::Other(
                "download ended before all parts completed".into(),
            ));
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
) -> Result<(String, ProbeOutcome)> {
    let mut last_err = None;
    for url in entry.sources() {
        let mut failures = 0;
        loop {
            match probe(client, url, entry.size, tally).await {
                Ok(outcome) => return Ok((url.to_string(), outcome)),
                Err(e) if e.retryable && failures < opts.max_retries => {
                    failures += 1;
                    tracing::warn!(url, error = %e.err, failures, "probe failed; retrying");
                    tokio::time::sleep(backoff(opts, failures, e.retry_after)).await;
                }
                Err(e) => {
                    tracing::warn!(url, error = %e.err, "source unusable");
                    last_err = Some(e.err);
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| Error::Invalid("model has no download source".into())))
}

async fn probe(
    client: &reqwest::Client,
    url: &str,
    size: u64,
    tally: &EgressTally,
) -> std::result::Result<ProbeOutcome, FetchErr> {
    tally.request(url);
    let resp = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| FetchErr::transient(e.to_string()))?;
    let host = resp.url().host_str().unwrap_or("unknown").to_string();
    match resp.status() {
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

async fn run_parts(ctx: &Arc<Ctx>) -> Result<()> {
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
    while let Some(joined) = set.join_next().await {
        let res = joined.map_err(|e| Error::Other(format!("download task failed: {e}")))?;
        if let Err(e) = res {
            // Fail fast; `state.json` keeps every committed byte for the next attempt.
            set.abort_all();
            return Err(e);
        }
    }
    Ok(())
}

async fn run_part(ctx: &Ctx, idx: usize) -> Result<()> {
    let mut failures = 0u32;
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
            failures = 0;
        }
        let err = match res {
            Ok(()) if progressed > 0 => continue,
            Ok(()) => FetchErr::transient("server returned an empty range"),
            Err(e) => e,
        };
        if !err.retryable || failures >= ctx.opts.max_retries {
            return Err(err.err);
        }
        failures += 1;
        tracing::debug!(part = idx, from, error = %err.err, failures, "part failed; retrying");
        tokio::time::sleep(backoff(&ctx.opts, failures, err.retry_after)).await;
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
    let resp = match ctx
        .client
        .get(&ctx.url)
        .header(RANGE, format!("bytes={from}-{}", end - 1))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (0, Err(FetchErr::transient(e.to_string()))),
    };
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
                Err(FetchErr::fatal(Error::Other(
                    "server stopped honouring Range requests".into(),
                ))),
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

    let outcome = loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                ctx.tally.add(&host, 0, chunk.len() as u64);
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
            }
            Ok(None) => break Ok(()),
            Err(e) => break Err(FetchErr::transient(e.to_string())),
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
async fn run_sequential(ctx: &Ctx, first: reqwest::Response) -> Result<()> {
    let mut next = Some(first);
    let mut failures = 0u32;
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
                match ctx.client.get(&ctx.url).send().await {
                    Ok(r) if r.status() == StatusCode::OK => Ok(r),
                    Ok(r) => Err(FetchErr::status(&ctx.url, r.status(), retry_after(&r))),
                    Err(e) => Err(FetchErr::transient(e.to_string())),
                }
            }
        };
        let err = match resp {
            Ok(resp) => {
                let (n, res) = write_body(ctx, 0, 0, ctx.size, resp).await;
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
        if !err.retryable || failures >= ctx.opts.max_retries {
            return Err(err.err);
        }
        failures += 1;
        tokio::time::sleep(backoff(&ctx.opts, failures, err.retry_after)).await;
    }
}

/// Streaming SHA-256 of a file (blocking; run on a blocking thread). `on_bytes` gets the running
/// count of hashed bytes.
pub(crate) fn sha256_file(path: &Path, mut on_bytes: impl FnMut(u64)) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
        on_bytes(total);
    }
    Ok(hex::encode(hasher.finalize()))
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
    fn retryable_statuses() {
        for (code, retry) in [
            (500, true),
            (503, true),
            (429, true),
            (408, true),
            (404, false),
            (403, false),
            (416, false),
        ] {
            let s = StatusCode::from_u16(code).unwrap();
            assert_eq!(
                FetchErr::status("http://h/x", s, None).retryable,
                retry,
                "{code}"
            );
        }
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
    fn sha256_of_known_input() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p, |_| {}).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
