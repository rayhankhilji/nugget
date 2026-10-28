//! One supervisor task per enabled source: sync from the stored cursor, then watch (or sleep for
//! the poll interval), retrying failures with capped exponential backoff and jitter.
//! `Error::Unauthorized` parks the source in `NeedsAuth` until the user acts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nugget_connector::{CancellationToken, ChangeSink, Connector};
use nugget_core::Error;
use nugget_core::ids::SourceId;
use nugget_core::ipc::{IngestPhase, IngestProgress, SourceState};
use tokio::sync::{Notify, mpsc};

use crate::ingest::{IngestEvent, Inner};
use crate::known::join_err;
use crate::pipeline::{Mode, PipelineSink, Worker};
use crate::util::cap_error;

/// Signals from the API to a running source.
#[derive(Debug, Default)]
pub(crate) struct Control {
    /// `sync_now`, credentials changed, or auth completed.
    pub wake: Notify,
    /// Set by `sync_now`: run heavy work even under power constraints until the pass ends.
    pub power_override: AtomicBool,
    /// Wakes a worker paused for power.
    pub power_changed: Notify,
}

impl Control {
    pub fn sync_now(&self) {
        self.power_override.store(true, Ordering::Relaxed);
        self.wake.notify_one();
        self.power_changed.notify_waiters();
    }
}

/// How long a watch must run before its failure no longer counts as "flapping".
const WATCH_STABLE: Duration = Duration::from_secs(60);

enum Next {
    /// Run another pass now.
    Again,
    Stop,
}

pub(crate) async fn run_source(
    inner: Arc<Inner>,
    id: SourceId,
    connector: Arc<dyn Connector>,
    ctl: Arc<Control>,
    cancel: CancellationToken,
) {
    let (tx, rx) = mpsc::channel(inner.cfg.queue_capacity.max(1));
    let worker_cancel = cancel.child_token();
    let worker = Worker::new(inner.clone(), id.clone(), ctl.clone(), rx);
    let worker_task = tokio::spawn(worker.run(worker_cancel.clone()));

    let mut attempt: u32 = 0;
    let mut watch_failures: u32 = 0;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let next = pass(
            &inner,
            &id,
            &connector,
            &ctl,
            &cancel,
            &tx,
            &mut attempt,
            &mut watch_failures,
        )
        .await;
        if matches!(next, Next::Stop) {
            break;
        }
    }
    drop(tx);
    worker_cancel.cancel();
    let _ = worker_task.await;
}

#[allow(clippy::too_many_arguments)]
async fn pass(
    inner: &Arc<Inner>,
    id: &SourceId,
    connector: &Arc<dyn Connector>,
    ctl: &Arc<Control>,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<crate::pipeline::Op>,
    attempt: &mut u32,
    watch_failures: &mut u32,
) -> Next {
    // Resume point.
    let cursor = {
        let store = inner.store.clone();
        let sid = id.clone();
        match tokio::task::spawn_blocking(move || store.get_cursor(&sid))
            .await
            .map_err(join_err)
            .and_then(|r| r)
        {
            Ok(c) => c.filter(|v| !v.is_null()),
            Err(e) => return fail(inner, id, ctl, cancel, attempt, e).await,
        }
    };
    let mode = if cursor.is_none() {
        Mode::Backfill
    } else {
        Mode::Incremental
    };

    // Heavy work waits for power (unless the user asked for it).
    if mode == Mode::Backfill
        && !ctl.power_override.load(Ordering::Relaxed)
        && inner.power.constrained()
    {
        inner.set_paused(id, true);
        let poll = Duration::from_millis(inner.cfg.power_poll_ms.max(10));
        tokio::select! {
            _ = cancel.cancelled() => return Next::Stop,
            _ = tokio::time::sleep(poll) => {}
            _ = ctl.wake.notified() => {}
        }
        return Next::Again;
    }
    inner.set_paused(id, false);

    inner.set_state(id, SourceState::Syncing).await;
    inner.emit(IngestEvent::Progress(IngestProgress {
        source: id.clone(),
        phase: IngestPhase::Scanning,
        done: 0,
        total: None,
        error: None,
    }));

    let sink = PipelineSink::new(tx.clone(), mode, inner.clone());
    if sink.begin_pass().await.is_err() {
        return Next::Stop;
    }
    let pass_cancel = cancel.child_token();
    let result = tokio::select! {
        r = connector.sync(cursor, &sink, &pass_cancel) => r,
        _ = cancel.cancelled() => Err(Error::Cancelled),
    };
    // The returned cursor is committed only after every change of the pass is applied.
    let result = match result {
        Ok(next_cursor) => sink.checkpoint(next_cursor).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        return fail(inner, id, ctl, cancel, attempt, e).await;
    }

    *attempt = 0;
    ctl.power_override.store(false, Ordering::Relaxed);
    inner.sync_succeeded(id).await;
    inner.emit(IngestEvent::Progress(IngestProgress {
        source: id.clone(),
        phase: IngestPhase::Idle,
        done: 0,
        total: None,
        error: None,
    }));

    if connector.supports_watch() {
        inner.set_state(id, SourceState::Watching).await;
        let live = Arc::new(PipelineSink::new(tx.clone(), Mode::Live, inner.clone()));
        let watch_cancel = cancel.child_token();
        let resync = inner.cfg.watch_resync_ms;
        let started = Instant::now();
        enum W {
            Ended(nugget_core::Result<()>),
            Resync,
            Stop,
        }
        let outcome = tokio::select! {
            r = connector.watch(live, watch_cancel.clone()) => W::Ended(r),
            _ = ctl.wake.notified() => W::Resync,
            _ = sleep_or_forever(resync) => W::Resync,
            _ = cancel.cancelled() => W::Stop,
        };
        watch_cancel.cancel();
        match outcome {
            W::Stop => Next::Stop,
            W::Resync => Next::Again,
            W::Ended(Ok(())) => {
                *watch_failures = 0;
                idle(inner, id, connector, ctl, cancel).await
            }
            W::Ended(Err(Error::Cancelled)) if cancel.is_cancelled() => Next::Stop,
            W::Ended(Err(e @ Error::Unauthorized(_))) => {
                needs_auth(inner, id, ctl, cancel, e).await
            }
            W::Ended(Err(e)) => {
                if started.elapsed() >= WATCH_STABLE {
                    *watch_failures = 1;
                } else {
                    *watch_failures += 1;
                }
                tracing::warn!(source = %id, error = %cap_error(e.to_string()), "watch failed");
                let delay = inner.cfg.backoff(watch_failures.saturating_sub(1));
                record_error(inner, id, &e).await;
                wait(ctl, cancel, delay).await
            }
        }
    } else {
        idle(inner, id, connector, ctl, cancel).await
    }
}

async fn sleep_or_forever(ms: u64) {
    if ms == 0 {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

async fn idle(
    inner: &Arc<Inner>,
    id: &SourceId,
    connector: &Arc<dyn Connector>,
    ctl: &Arc<Control>,
    cancel: &CancellationToken,
) -> Next {
    inner.set_state(id, SourceState::Idle).await;
    wait(ctl, cancel, connector.poll_interval()).await
}

async fn wait(ctl: &Arc<Control>, cancel: &CancellationToken, delay: Duration) -> Next {
    tokio::select! {
        _ = cancel.cancelled() => Next::Stop,
        _ = tokio::time::sleep(delay) => Next::Again,
        _ = ctl.wake.notified() => Next::Again,
    }
}

async fn record_error(inner: &Arc<Inner>, id: &SourceId, e: &Error) {
    let msg = cap_error(e.to_string());
    inner.record_item_error(id, None, msg.clone());
    inner.sync_failed(id, SourceState::Error, msg.clone()).await;
    inner.emit(IngestEvent::Progress(IngestProgress {
        source: id.clone(),
        phase: IngestPhase::Error,
        done: 0,
        total: None,
        error: Some(msg),
    }));
}

async fn fail(
    inner: &Arc<Inner>,
    id: &SourceId,
    ctl: &Arc<Control>,
    cancel: &CancellationToken,
    attempt: &mut u32,
    e: Error,
) -> Next {
    if cancel.is_cancelled() {
        return Next::Stop;
    }
    if matches!(e, Error::Unauthorized(_)) {
        *attempt = 0;
        return needs_auth(inner, id, ctl, cancel, e).await;
    }
    let mut delay = inner.cfg.backoff(*attempt);
    if let Error::RateLimited {
        retry_after_ms: Some(ms),
    } = &e
    {
        delay = delay.max(Duration::from_millis(*ms));
    }
    *attempt = attempt.saturating_add(1);
    tracing::warn!(source = %id, error = %cap_error(e.to_string()), retry_in_ms = delay.as_millis() as u64, "sync failed");
    record_error(inner, id, &e).await;
    wait(ctl, cancel, delay).await
}

/// Park until the user acts (`sync_now`, new credentials, completed auth).
async fn needs_auth(
    inner: &Arc<Inner>,
    id: &SourceId,
    ctl: &Arc<Control>,
    cancel: &CancellationToken,
    e: Error,
) -> Next {
    let msg = cap_error(e.to_string());
    tracing::info!(source = %id, error = %msg, "source needs authorization");
    inner
        .sync_failed(id, SourceState::NeedsAuth, msg.clone())
        .await;
    inner.emit(IngestEvent::Progress(IngestProgress {
        source: id.clone(),
        phase: IngestPhase::Error,
        done: 0,
        total: None,
        error: Some(msg),
    }));
    tokio::select! {
        _ = cancel.cancelled() => Next::Stop,
        _ = ctl.wake.notified() => Next::Again,
    }
}
