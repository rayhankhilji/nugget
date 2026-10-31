//! Interactive priority for the local model slot.
//!
//! Background triage shares the on-device chat model with interactive features (Ask, Double
//! Tap, the agent, drafting). A local server usually has one slot, so a triage request in
//! flight makes an interactive request wait for it. [`Foreground`] lets interactive callers
//! announce themselves: the daemon wraps its on-device chat providers so that every chat that
//! is not triage holds a [`ForegroundGuard`] for as long as its stream lives. The triage job
//! then
//! - waits until no interactive request is in flight and none ended in the last
//!   [`Foreground::grace`] (follow-up calls of one interaction, like query rewrite then answer,
//!   come in quick succession), and
//! - abandons its own request as soon as an interactive one starts (dropping the stream closes
//!   the connection, which stops generation on the server); the batch is retried later.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::watch;

/// Tracks interactive model requests in flight. Cheap to share (`Arc`).
#[derive(Debug)]
pub struct Foreground {
    active: watch::Sender<usize>,
    last_end: Mutex<Option<Instant>>,
    grace: Duration,
}

impl Default for Foreground {
    fn default() -> Self {
        Self::new(Duration::from_millis(3_000))
    }
}

impl Foreground {
    /// `grace`: how long after the last interactive request ends background work stays off.
    pub fn new(grace: Duration) -> Self {
        Self {
            active: watch::Sender::new(0),
            last_end: Mutex::new(None),
            grace,
        }
    }

    pub fn grace(&self) -> Duration {
        self.grace
    }

    /// Mark an interactive request in flight until the guard drops.
    pub fn begin(self: &Arc<Self>) -> ForegroundGuard {
        self.active.send_modify(|n| *n += 1);
        ForegroundGuard {
            fg: Arc::clone(self),
        }
    }

    /// Interactive requests in flight right now.
    pub fn in_flight(&self) -> usize {
        *self.active.borrow()
    }

    /// Whether background work should hold off now: a request is in flight or one ended less
    /// than [`grace`](Self::grace) ago.
    pub fn busy(&self) -> bool {
        self.in_flight() > 0 || self.remaining_grace().is_some()
    }

    fn remaining_grace(&self) -> Option<Duration> {
        let last = (*self.last_end.lock().unwrap_or_else(|e| e.into_inner()))?;
        self.grace
            .checked_sub(last.elapsed())
            .filter(|d| !d.is_zero())
    }

    /// Resolves once no interactive request is in flight and the grace period has passed.
    pub async fn idle(&self) {
        let mut rx = self.active.subscribe();
        loop {
            // Never fails: `self` owns the sender.
            let _ = rx.wait_for(|n| *n == 0).await;
            match self.remaining_grace() {
                None => {
                    if *rx.borrow_and_update() == 0 {
                        return;
                    }
                }
                Some(d) => {
                    tokio::select! {
                        _ = tokio::time::sleep(d) => {}
                        _ = rx.changed() => {}
                    }
                }
            }
        }
    }

    /// Resolves as soon as an interactive request is in flight (immediately if one is).
    pub async fn started(&self) {
        let mut rx = self.active.subscribe();
        let _ = rx.wait_for(|n| *n > 0).await;
    }
}

/// Held by an interactive request; see [`Foreground::begin`].
#[derive(Debug)]
pub struct ForegroundGuard {
    fg: Arc<Foreground>,
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        *self.fg.last_end.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        self.fg.active.send_modify(|n| *n = n.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn idle_waits_for_requests_and_grace() {
        let fg = Arc::new(Foreground::new(Duration::from_millis(500)));
        assert!(!fg.busy());
        fg.idle().await;
        let g = fg.begin();
        assert!(fg.busy());
        assert_eq!(fg.in_flight(), 1);
        fg.started().await;
        let start = tokio::time::Instant::now();
        let fg2 = fg.clone();
        let waiter = tokio::spawn(async move { fg2.idle().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiter.is_finished());
        drop(g);
        assert!(fg.busy(), "still in the grace period");
        waiter.await.unwrap();
        assert!(start.elapsed() >= Duration::from_millis(700));
        assert!(!fg.busy());
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_request_during_grace_extends_the_wait() {
        let fg = Arc::new(Foreground::new(Duration::from_millis(500)));
        drop(fg.begin());
        let fg2 = fg.clone();
        let waiter = tokio::spawn(async move { fg2.idle().await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        let g = fg.begin();
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert!(!waiter.is_finished());
        drop(g);
        waiter.await.unwrap();
    }
}
