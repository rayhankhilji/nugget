//! The voice profile behind Double Tap: loaded (or built) once at startup on a blocking thread,
//! then refreshed incrementally after ingest commits and draft feedback.
//!
//! Until the first load finishes, drafts get neutral hints and `voice.status` reports zero
//! samples, so daemon startup never waits for the profile.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use nugget_core::Result;
use nugget_core::ipc::{IngestPhase, VoiceStatus};
use nugget_ingest::IngestEvent;
use nugget_store::Store;
use nugget_voice::{MeIdentity, VoiceConfig, VoiceHints, VoiceProfile, VoiceQuery, VoiceSource};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::daemon::blocking;

/// The profile shared by the drafter, `voice.status` and the refresher. `None` until the first
/// load or build finished (or when it failed).
#[derive(Default)]
pub(crate) struct SharedVoice {
    profile: RwLock<Option<VoiceProfile>>,
    /// Wakes the refresher without an ingest event (draft feedback stored a sample).
    kick: Notify,
}

impl SharedVoice {
    #[cfg(test)]
    pub(crate) fn is_loaded(&self) -> bool {
        self.profile
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn set(&self, profile: VoiceProfile) {
        *self.profile.write().unwrap_or_else(PoisonError::into_inner) = Some(profile);
    }

    /// Ask for a refresh soon (debounced like ingest events).
    pub(crate) fn kick(&self) {
        self.kick.notify_one();
    }

    /// Bring the loaded profile up to date and save its snapshot when anything changed.
    /// Blocking: call on a blocking thread. Holds the write lock meanwhile; drafts' voice
    /// lookups time out to neutral hints instead of waiting long.
    fn refresh_blocking(&self) -> Result<Option<nugget_voice::RefreshReport>> {
        let mut guard = self.profile.write().unwrap_or_else(PoisonError::into_inner);
        let Some(profile) = guard.as_mut() else {
            return Ok(None);
        };
        let report = profile.refresh()?;
        if report.fetched > 0 || report.removed > 0 {
            profile.save()?;
        }
        Ok(Some(report))
    }
}

impl VoiceSource for SharedVoice {
    fn hints(&self, query: &VoiceQuery) -> VoiceHints {
        match &*self.profile.read().unwrap_or_else(PoisonError::into_inner) {
            Some(p) => VoiceSource::hints(p, query),
            None => VoiceHints {
                channel: Some(query.channel),
                ..Default::default()
            },
        }
    }

    fn status(&self) -> VoiceStatus {
        match &*self.profile.read().unwrap_or_else(PoisonError::into_inner) {
            Some(p) => VoiceSource::status(p),
            None => VoiceStatus::default(),
        }
    }
}

/// Whether an ingest event may mean new or changed items in the store.
fn is_commit(ev: &IngestEvent) -> bool {
    match ev {
        IngestEvent::Progress(p) => p.phase == IngestPhase::Idle,
        IngestEvent::SourceChanged(_) | IngestEvent::SourceRemoved(_) => true,
    }
}

/// Wait for the first signal that the store changed (an ingest commit, a lagged receiver, or a
/// kick). Returns `false` when stopped or when ingest is gone.
async fn wait_for_change(
    events: &mut broadcast::Receiver<IngestEvent>,
    kick: &Notify,
    stop: &CancellationToken,
) -> bool {
    loop {
        tokio::select! {
            () = stop.cancelled() => return false,
            () = kick.notified() => return true,
            ev = events.recv() => match ev {
                Ok(ev) if is_commit(&ev) => return true,
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => return true,
                Err(broadcast::error::RecvError::Closed) => return false,
            },
        }
    }
}

/// Run `refresh` once per burst of changes: after the first change, wait `debounce` (changes
/// meanwhile are coalesced into the same refresh), then refresh. Stops on `stop`.
pub(crate) async fn refresh_loop<F, Fut>(
    mut events: broadcast::Receiver<IngestEvent>,
    kick: &Notify,
    debounce: Duration,
    stop: CancellationToken,
    mut refresh: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    while wait_for_change(&mut events, kick, &stop).await {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(debounce) => {}
        }
        // Everything up to now is covered by this refresh.
        events = events.resubscribe();
        tokio::select! {
            () = stop.cancelled() => return,
            () = refresh() => {}
        }
    }
}

/// Load the saved profile (refreshing it incrementally) or build it, then keep it current.
/// `events` must be subscribed before the daemon can commit anything this task should see.
pub(crate) async fn run(
    voice: Arc<SharedVoice>,
    store: Store,
    me: MeIdentity,
    config: VoiceConfig,
    events: broadcast::Receiver<IngestEvent>,
    debounce: Duration,
    stop: CancellationToken,
) {
    let started = std::time::Instant::now();
    let load = blocking(move || VoiceProfile::load_or_build(&store, me, config));
    let profile = tokio::select! {
        () = stop.cancelled() => return,
        p = load => p,
    };
    match profile {
        Ok(p) => {
            let status = VoiceSource::status(&p);
            tracing::info!(
                samples = status.samples,
                channels = ?status.channels,
                took_ms = started.elapsed().as_millis() as u64,
                "voice profile ready"
            );
            voice.set(p);
        }
        Err(e) => {
            // Drafts still work with neutral hints; the next change retries nothing, so say so.
            tracing::warn!(error = %e, "voice profile unavailable; drafting without it");
            return;
        }
    }
    let shared = voice.clone();
    refresh_loop(events, &voice.kick, debounce, stop, move || {
        let v = shared.clone();
        async move {
            match blocking(move || v.refresh_blocking()).await {
                Ok(Some(r)) => tracing::debug!(
                    fetched = r.fetched,
                    removed = r.removed,
                    samples = r.samples,
                    "voice profile refreshed"
                ),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "voice profile refresh failed"),
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nugget_core::ids::SourceId;
    use nugget_core::ipc::IngestProgress;

    use super::*;

    fn progress(phase: IngestPhase) -> IngestEvent {
        IngestEvent::Progress(IngestProgress {
            source: SourceId::new("files-1"),
            phase,
            done: 1,
            total: None,
            error: None,
        })
    }

    #[test]
    fn only_commits_count_as_changes() {
        assert!(is_commit(&progress(IngestPhase::Idle)));
        assert!(!is_commit(&progress(IngestPhase::Indexing)));
        assert!(!is_commit(&progress(IngestPhase::Scanning)));
        assert!(is_commit(&IngestEvent::SourceRemoved(SourceId::new("x"))));
    }

    #[tokio::test]
    async fn bursts_are_coalesced_into_one_refresh_and_stop_ends_the_loop() {
        let (tx, rx) = broadcast::channel(64);
        let kick = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let count = Arc::new(AtomicUsize::new(0));
        let task = {
            let (kick, stop, count) = (kick.clone(), stop.clone(), count.clone());
            tokio::spawn(async move {
                refresh_loop(rx, &kick, Duration::from_millis(300), stop, move || {
                    let c = count.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            })
        };
        tokio::task::yield_now().await;

        // Progress while indexing does not refresh.
        tx.send(progress(IngestPhase::Indexing)).unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);

        // A burst of commits within the debounce window: one refresh.
        for _ in 0..5 {
            tx.send(progress(IngestPhase::Idle)).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // A later commit refreshes again; so does a kick (draft feedback).
        tx.send(IngestEvent::SourceRemoved(SourceId::new("files-1")))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
        kick.notify_one();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(count.load(Ordering::SeqCst), 3);

        stop.cancel();
        task.await.unwrap();
    }

    #[test]
    fn neutral_until_loaded() {
        let v = SharedVoice::default();
        assert!(!v.is_loaded());
        assert_eq!(v.status(), VoiceStatus::default());
        let h = v.hints(&VoiceQuery::new(nugget_voice::Channel::Chat));
        assert_eq!(h.channel, Some(nugget_voice::Channel::Chat));
        assert!(h.exemplars.is_empty());
    }
}
