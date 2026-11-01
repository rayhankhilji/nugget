//! Plumbing shared by every backend: the fire dispatcher thread and the detector driver.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use nugget_core::{Error, Result};

use crate::tap::{Detector, InputEvent, KeyClass};

/// Wakes the dispatcher. Cheap to clone, safe to call from a hook or the main thread: it never
/// runs the user's callback itself.
#[derive(Clone)]
pub(crate) struct Fire(mpsc::Sender<()>);

impl Fire {
    pub(crate) fn fire(&self) {
        let _ = self.0.send(());
    }
}

/// Runs `on_fire` on its own thread, so a slow callback never stalls an OS hook (Windows drops
/// low-level hooks that take too long) or the macOS main thread.
pub(crate) struct Dispatcher {
    tx: mpsc::Sender<()>,
    stopped: Arc<AtomicBool>,
}

impl Dispatcher {
    pub(crate) fn spawn(on_fire: Box<dyn Fn() + Send + Sync>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<()>();
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        std::thread::Builder::new()
            .name("nugget-hotkey-fire".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    if flag.load(Ordering::Acquire) {
                        break;
                    }
                    on_fire();
                }
            })
            .map_err(|e| Error::Other(format!("cannot start the hotkey dispatcher: {e}")))?;
        Ok(Self { tx, stopped })
    }

    pub(crate) fn fire_handle(&self) -> Fire {
        Fire(self.tx.clone())
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        // No join: the callback may be the one dropping the listener, and on macOS the last
        // `Fire` lives in a monitor block that the main thread releases later. The flag makes
        // sure nothing fires after the listener is gone; the thread exits once every `Fire`
        // clone is dropped or the next wake-up sees the flag.
        self.stopped.store(true, Ordering::Release);
        let _ = self.tx.send(());
    }
}

/// A detector plus its clock, fed classified key events by a backend.
pub(crate) struct Engine {
    detector: Detector,
    epoch: Instant,
    fire: Fire,
}

impl Engine {
    pub(crate) fn new(detector: Detector, fire: Fire) -> Self {
        Self {
            detector,
            epoch: Instant::now(),
            fire,
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(crate) fn key(&mut self, key: KeyClass, down: bool) {
        if let Some(event) = InputEvent::from_key(key, down, self.detector.observes_escape()) {
            self.event(event);
        }
    }

    pub(crate) fn mouse(&mut self) {
        self.event(InputEvent::MouseButton);
    }

    fn event(&mut self, event: InputEvent) {
        let now = self.now_ms();
        if self.detector.on_event(event, now) {
            self.fire.fire();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Modifier;
    use crate::tap::TapDetector;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn counting() -> (Arc<AtomicUsize>, Dispatcher, Engine) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let dispatcher = Dispatcher::spawn(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
        let detector = Detector::Tap(TapDetector::new(Modifier::LeftShift, 2_000));
        let engine = Engine::new(detector, dispatcher.fire_handle());
        (count, dispatcher, engine)
    }

    const SHIFT: KeyClass = KeyClass::Modifier(Modifier::LeftShift);

    fn tap(engine: &mut Engine) {
        engine.key(SHIFT, true);
        engine.key(SHIFT, false);
    }

    #[test]
    fn engine_fires_through_the_dispatcher_and_stops_after_drop() {
        let (count, dispatcher, mut engine) = counting();
        tap(&mut engine);
        tap(&mut engine);
        let deadline = Instant::now() + Duration::from_secs(5);
        while count.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);

        drop(dispatcher);
        tap(&mut engine);
        tap(&mut engine);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "fired after the listener was dropped"
        );
    }

    #[test]
    fn engine_ignores_other_key_releases_but_not_presses() {
        let (count, _dispatcher, mut engine) = counting();
        // The release of a key pressed before the first tap does not spoil it...
        tap(&mut engine);
        engine.key(KeyClass::Other, false);
        tap(&mut engine);
        // ...but a press does.
        tap(&mut engine);
        engine.key(KeyClass::Other, true);
        tap(&mut engine);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
