//! Read-ahead: while a reply field keeps focus, prefill the coming draft's prompt into the local
//! model's cache, so that when the trigger fires only the remainder has to be read.
//!
//! # What happens, and when
//!
//! A small loop watches what is in front, cheaply, and sleeps between looks for as long as
//! nothing can change ([`Planner::next_check`]):
//!
//! 1. It asks the accessibility API for the **frontmost app only** (its id, name, window title
//!    and page URL). No field, thread or message text is read at this step. It looks when the
//!    OS reports a focus change ([`super::focus`]: on macOS another app coming to the front, on
//!    Windows another window coming to the front or its title changing), when a dwell or re-read
//!    below falls due, and otherwise every [`SLOW_POLL`] (macOS does not report window or tab
//!    switches inside an app). Where there are no focus notifications (Linux) it looks every
//!    [`POLL`] while a dwell is pending or a read-ahead may be running, else every
//!    [`IDLE_POLL`].
//! 2. When the same app and window have stayed in front for [`DWELL`], and the app is allowed
//!    (not in the excluded list, not a terminal unless terminals are allowed), it does **one**
//!    full read of the focused field: the same read the trigger does (field, the messages
//!    around it, recipients, subject).
//! 3. If that is a normal text field (never a password or other secure field, whose value the
//!    accessibility layer never reads), it sends the daemon `draft.read_ahead` with exactly the
//!    request the trigger would send to `draft.begin`, including the per-app provider.
//! 4. While the user stays in that window it reads again at most every [`REREAD`] (slowing down
//!    to [`MAX_REREAD`] while nothing changes) and sends again only when the context changed.
//!    It sends at most [`RATE_MAX`] times per [`RATE_WINDOW`].
//! 5. When another app or window comes to the front (or the focus leaves the text field) while a
//!    read-ahead may still be running, it sends `draft.read_ahead_cancel`.
//!
//! It pauses while a draft is running and while a Nugget window is in front, and in low power
//! mode or, when `read_ahead_on_battery` is off, on battery ([`super::power`]; a running
//! read-ahead is cancelled, and the power state is looked at again every
//! [`power::RECHECK`]). It does not run at all while Double Tap or read-ahead is off (the loop
//! stops; it restarts when the settings change), or where accessibility is unsupported.
//!
//! # Privacy
//!
//! - **Read:** the focused field and its surroundings, only in allowed apps, only after the
//!   field has kept focus for a moment. Never in excluded apps, terminals (unless allowed) or
//!   secure fields.
//! - **Sent:** to the local Nugget daemon only, over its local socket. The daemon itself refuses
//!   read-ahead for cloud providers (nothing goes to a cloud API for read-ahead), for excluded
//!   apps and secure fields, and rate-limits; it feeds the prompt to the draft's provider (the
//!   on-device model server, or a local-network server the user configured, recorded in the
//!   egress ledger) to fill its cache and generates nothing.
//! - **Stored:** nothing. The shell keeps only a keyed hash of the last context it sent (to
//!   avoid sending the same one twice), in memory, with a key that is new every run. The local
//!   model server keeps the prefilled prompt in its in-memory cache, as it does for any draft.
//! - **Off switch:** the `read_ahead` setting (Double Tap settings page), and
//!   `read_ahead_on_battery` for battery power.
//!
//! The decisions live in [`Planner`], a pure state machine with the clock passed in; the loop
//! around it ([`restart`]) only reads, sends and feeds results back.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nugget_core::ipc::{AppInfo, DraftRequest, method};
use serde::Deserialize;
use tauri::{AppHandle, Manager};

use super::focus::FocusEvents;
use super::settings::DoubleTapSettings;
use super::{DoubleTap, flow, lock, power, probe};
use crate::daemon::Daemon;

/// How often the frontmost app is checked (a cheap read: no field content) while a dwell is
/// pending or a read-ahead may be running and the OS gives no focus notifications.
pub const POLL: Duration = Duration::from_millis(1_000);
/// How often it is checked otherwise without focus notifications (nothing is due soon).
pub const IDLE_POLL: Duration = Duration::from_millis(2_000);
/// The longest sleep with focus notifications: a fallback for changes they do not report
/// (window and tab switches inside an app on macOS).
pub const SLOW_POLL: Duration = Duration::from_secs(5);
/// After a focus notification, wait this long before looking (a switch often comes as a burst
/// of notifications, and the new window's title settles).
pub const EVENT_SETTLE: Duration = Duration::from_millis(250);
/// Looks prompted by focus notifications are at least this far apart (a window whose title
/// keeps changing must not turn into a busy loop).
pub const EVENT_MIN_GAP: Duration = Duration::from_millis(500);
/// The shortest sleep between two looks.
pub const MIN_WAIT: Duration = Duration::from_millis(100);
/// How long an app and window must stay in front before the field is read.
pub const DWELL: Duration = Duration::from_millis(1_500);
/// The shortest time between two full reads in the same window.
pub const REREAD: Duration = Duration::from_secs(10);
/// The longest time between two full reads in the same window, reached by doubling [`REREAD`]
/// while the context stays the same.
pub const MAX_REREAD: Duration = Duration::from_secs(60);
/// At most [`RATE_MAX`] read-aheads are sent per [`RATE_WINDOW`].
pub const RATE_MAX: usize = 4;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// After the daemon skips an app because its provider is a cloud one, or refuses it, that app
/// is left alone for this long; after it reports read-ahead disabled, everything is.
pub const BACKOFF: Duration = Duration::from_secs(10 * 60);
/// After the daemon cannot be reached or reports read-ahead unavailable, wait this long.
pub const BACKOFF_UNAVAILABLE: Duration = Duration::from_secs(2 * 60);

// --- the state machine ----------------------------------------------------------------------------

/// What the loop sees in front, from the cheap read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Front {
    /// A draft is running: stay quiet (the trigger's own read supersedes any read-ahead).
    Busy,
    /// Nothing readable is in front, or a Nugget window is.
    Nothing,
    /// An app: `key` identifies it, `surface` its window and page (a hash of the title and
    /// URL), `allowed` whether drafting is allowed in it.
    App {
        key: String,
        surface: u64,
        allowed: bool,
    },
}

/// What to do after a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Wait,
    /// Read the focused field (once).
    Read,
    /// Cancel the read-ahead that may be running.
    Cancel,
}

/// What to do with a full read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Skip,
    /// Send `draft.read_ahead`; the number identifies it in [`Planner::outcome`].
    Send(u64),
    Cancel,
}

/// Why the daemon did not start a read-ahead (`skipped` in its answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Disabled,
    Refused,
    Cloud,
    RateLimited,
    Unchanged,
    Unavailable,
    Cancelled,
    /// A reason this shell does not know.
    Other,
}

impl Skip {
    pub fn parse(s: &str) -> Self {
        match s {
            "disabled" => Self::Disabled,
            "refused" => Self::Refused,
            "cloud" => Self::Cloud,
            "rate_limited" => Self::RateLimited,
            "unchanged" => Self::Unchanged,
            "unavailable" => Self::Unavailable,
            "cancelled" => Self::Cancelled,
            _ => Self::Other,
        }
    }
}

/// How a `draft.read_ahead` call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Started; `done` when the prefill already finished.
    Started {
        done: bool,
    },
    Skipped(Skip),
    /// The daemon could not be reached or answered with an error.
    Failed,
}

/// Decides when to read ahead. Pure: every input carries the time.
#[derive(Debug)]
pub struct Planner {
    /// The app and surface in front, and since when.
    current: Option<(String, u64)>,
    /// Whether drafting is allowed in the app in front (as of the last tick).
    allowed: bool,
    since: Instant,
    last_read: Option<Instant>,
    /// Consecutive full reads that found the same context (slows re-reads down).
    unchanged: u32,
    /// Fingerprint of the last context read in this surface (`0` for "no text field").
    last_seen: Option<u64>,
    /// Fingerprint of the last context sent (in any surface).
    last_sent: Option<u64>,
    /// The read-ahead that may be running.
    in_flight: Option<u64>,
    seq: u64,
    sends: VecDeque<Instant>,
    app_backoff: HashMap<String, Instant>,
    global_backoff: Option<Instant>,
}

impl Planner {
    pub fn new(now: Instant) -> Self {
        Self {
            current: None,
            allowed: false,
            since: now,
            last_read: None,
            unchanged: 0,
            last_seen: None,
            last_sent: None,
            in_flight: None,
            seq: 0,
            sends: VecDeque::new(),
            app_backoff: HashMap::new(),
            global_backoff: None,
        }
    }

    /// Whether a read-ahead may be running (the loop cancels it when it stops).
    pub fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Forget the running read-ahead, returning whether one may have been running.
    fn take_in_flight(&mut self) -> bool {
        self.in_flight.take().is_some()
    }

    fn reread_after(&self) -> Duration {
        let factor = 1u32 << self.unchanged.min(8);
        REREAD.saturating_mul(factor).min(MAX_REREAD)
    }

    fn rate_limited(&mut self, now: Instant) -> bool {
        while self
            .sends
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= RATE_WINDOW)
        {
            self.sends.pop_front();
        }
        self.sends.len() >= RATE_MAX
    }

    fn backed_off(&mut self, now: Instant, key: &str) -> bool {
        if self.global_backoff.is_some_and(|until| now >= until) {
            self.global_backoff = None;
        }
        self.app_backoff.retain(|_, until| now < *until);
        self.global_backoff.is_some() || self.app_backoff.contains_key(key)
    }

    /// When sending is possible again, if [`RATE_MAX`] sends already happened in the window.
    fn rate_free_at(&self, now: Instant) -> Option<Instant> {
        let mut recent = self
            .sends
            .iter()
            .filter(|t| now.saturating_duration_since(**t) < RATE_WINDOW);
        let oldest = *recent.next()?;
        (1 + recent.count() >= RATE_MAX).then_some(oldest + RATE_WINDOW)
    }

    /// How long the loop may sleep after a tick before the next one could do anything, given
    /// whether the OS reports focus changes (`events`; the loop then also wakes on those).
    ///
    /// - Nothing allowed in front, or backed off: [`SLOW_POLL`] with events, else
    ///   [`IDLE_POLL`] (or [`POLL`] while a read-ahead may be running, to cancel it promptly).
    /// - A dwell pending: until it ends (no longer than [`POLL`] without events).
    /// - After a read: until the next re-read (or until a send is allowed again), no longer
    ///   than the same fallback as above.
    ///
    /// Never shorter than [`MIN_WAIT`]. Only the sleep changes: ticking earlier never changes
    /// a decision, it only costs a look.
    pub fn next_check(&self, now: Instant, events: bool) -> Duration {
        let fallback = if events {
            SLOW_POLL
        } else if self.in_flight.is_some() {
            POLL
        } else {
            IDLE_POLL
        };
        let Some((key, _)) = &self.current else {
            return fallback;
        };
        let backed_off = self.global_backoff.is_some_and(|until| now < until)
            || self.app_backoff.get(key).is_some_and(|until| now < *until);
        if !self.allowed || backed_off {
            return fallback;
        }
        let (due, cap) = match self.last_read {
            None if events => (self.since + DWELL, fallback),
            None => (self.since + DWELL, POLL),
            Some(t) => (t + self.reread_after(), fallback),
        };
        let due = self.rate_free_at(now).map_or(due, |free| due.max(free));
        due.saturating_duration_since(now).min(cap).max(MIN_WAIT)
    }

    /// One poll: what is in front now.
    pub fn tick(&mut self, now: Instant, front: Front) -> Action {
        match front {
            Front::Busy => {
                // The trigger's draft takes over the model; a running read-ahead feeds it.
                self.current = None;
                self.in_flight = None;
                Action::Wait
            }
            Front::Nothing => {
                self.current = None;
                if self.take_in_flight() {
                    Action::Cancel
                } else {
                    Action::Wait
                }
            }
            Front::App {
                key,
                surface,
                allowed,
            } => {
                let same = self
                    .current
                    .as_ref()
                    .is_some_and(|(k, s)| *k == key && *s == surface);
                self.allowed = allowed;
                if !same {
                    self.current = Some((key, surface));
                    self.since = now;
                    self.last_read = None;
                    self.unchanged = 0;
                    self.last_seen = None;
                    return if self.take_in_flight() {
                        Action::Cancel
                    } else {
                        Action::Wait
                    };
                }
                if !allowed {
                    return if self.take_in_flight() {
                        Action::Cancel
                    } else {
                        Action::Wait
                    };
                }
                if self.backed_off(now, &key_of(&self.current)) {
                    return Action::Wait;
                }
                if now.saturating_duration_since(self.since) < DWELL {
                    return Action::Wait;
                }
                if self
                    .last_read
                    .is_some_and(|t| now.saturating_duration_since(t) < self.reread_after())
                {
                    return Action::Wait;
                }
                if self.rate_limited(now) {
                    return Action::Wait;
                }
                self.last_read = Some(now);
                Action::Read
            }
        }
    }

    /// The full read for `key` finished: `fingerprint` of its context, or `None` when there is
    /// no draftable text field (or it is secure, or refused).
    pub fn read(&mut self, now: Instant, key: &str, fingerprint: Option<u64>) -> Decision {
        if self.current.as_ref().is_none_or(|(k, _)| k != key) {
            // Focus moved on while reading; the next tick handles it.
            return Decision::Skip;
        }
        let seen = fingerprint.unwrap_or(0);
        if self.last_seen == Some(seen) {
            self.unchanged = self.unchanged.saturating_add(1);
        } else {
            self.unchanged = 0;
        }
        self.last_seen = Some(seen);
        let Some(fp) = fingerprint else {
            return if self.take_in_flight() {
                Decision::Cancel
            } else {
                Decision::Skip
            };
        };
        if self.last_sent == Some(fp) || self.rate_limited(now) {
            return Decision::Skip;
        }
        self.last_sent = Some(fp);
        self.sends.push_back(now);
        self.seq += 1;
        self.in_flight = Some(self.seq);
        Decision::Send(self.seq)
    }

    /// The daemon answered read-ahead `seq` for `key`.
    pub fn outcome(&mut self, now: Instant, seq: u64, key: &str, outcome: Outcome) {
        let running = matches!(outcome, Outcome::Started { done: false });
        if self.in_flight == Some(seq) && !running {
            self.in_flight = None;
        }
        match outcome {
            Outcome::Started { .. } => {}
            Outcome::Skipped(Skip::Cloud | Skip::Refused) => {
                self.app_backoff.insert(key.to_owned(), now + BACKOFF);
            }
            Outcome::Skipped(Skip::Disabled) => self.global_backoff = Some(now + BACKOFF),
            Outcome::Skipped(Skip::Unavailable) | Outcome::Failed => {
                self.global_backoff = Some(now + BACKOFF_UNAVAILABLE);
                // Let the same context be sent again once the daemon is back.
                self.last_sent = None;
            }
            Outcome::Skipped(Skip::RateLimited | Skip::Cancelled) => self.last_sent = None,
            Outcome::Skipped(Skip::Unchanged | Skip::Other) => {}
        }
    }
}

fn key_of(current: &Option<(String, u64)>) -> String {
    current.as_ref().map(|(k, _)| k.clone()).unwrap_or_default()
}

/// The key identifying an app: its id, else its name, lower-cased.
pub fn app_key(app: &AppInfo) -> String {
    app.id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or(&app.name)
        .trim()
        .to_lowercase()
}

/// A keyed hash of the window and page in front (title and URL), never stored.
pub fn surface(hasher: &impl BuildHasher, app: &AppInfo) -> u64 {
    let mut h = hasher.build_hasher();
    app.window_title.hash(&mut h);
    app.url.hash(&mut h);
    h.finish()
}

/// A keyed hash of everything the prompt is built from (app, field, thread, recipients,
/// subject, provider), never stored. Never `0` (that marks "no text field").
pub fn fingerprint(hasher: &impl BuildHasher, r: &DraftRequest) -> u64 {
    let mut h = hasher.build_hasher();
    let c = &r.context;
    (&c.app.id, &c.app.name, &c.app.url).hash(&mut h);
    (
        &c.field.role,
        &c.field.label,
        &c.field.placeholder,
        &c.field.value,
    )
        .hash(&mut h);
    for m in &c.thread {
        (&m.author, &m.text, m.from_me, &m.timestamp).hash(&mut h);
    }
    (&c.recipients, &c.subject).hash(&mut h);
    r.provider.as_ref().map(|p| p.0.as_str()).hash(&mut h);
    h.finish().max(1)
}

/// Whether a field is worth reading ahead for: a text field that is not secure and not a
/// search box.
pub fn draftable(field: &nugget_core::ipc::FieldInfo) -> bool {
    !field.secure && !field.role.trim().is_empty() && field.role != "search_field"
}

// --- the loop -----------------------------------------------------------------------------------------

/// The running loop's generation; bumping it stops the loop.
#[derive(Debug, Default)]
pub struct Runner {
    generation: AtomicU64,
    /// The latest generation, to wake a sleeping loop so it stops at once.
    wake: tokio::sync::watch::Sender<u64>,
    /// Focus change notifications, started with the first loop (`None`: none on this OS).
    focus: OnceLock<Option<FocusEvents>>,
}

/// Stop the read-ahead loop and start one if Double Tap and read-ahead are on (call after the
/// settings change, like `flow::restart_listener`).
pub fn restart(app: &AppHandle) {
    let Some(state) = app.try_state::<DoubleTap>() else {
        return;
    };
    let generation = state.read_ahead.generation.fetch_add(1, Ordering::SeqCst) + 1;
    state.read_ahead.wake.send_replace(generation);
    let s = lock(&state.settings).clone();
    let supported = cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ));
    let on = supported && wanted(&s);
    probe::record("read_ahead_loop", || serde_json::json!({ "running": on }));
    if on {
        tauri::async_runtime::spawn(run(app.clone(), generation));
    }
}

fn wanted(s: &DoubleTapSettings) -> bool {
    s.enabled && s.read_ahead
}

/// The daemon's answer to `draft.read_ahead`.
#[derive(Debug, Deserialize)]
struct Reply {
    #[serde(default)]
    started: bool,
    #[serde(default)]
    skipped: Option<String>,
    #[serde(default)]
    done: Option<bool>,
}

impl Reply {
    fn outcome(&self) -> Outcome {
        if self.started {
            Outcome::Started {
                done: self.done == Some(true),
            }
        } else {
            Outcome::Skipped(self.skipped.as_deref().map_or(Skip::Other, Skip::parse))
        }
    }
}

/// What ended the loop's sleep.
enum Woke {
    Due,
    Focus,
    Outcome((u64, String, Outcome)),
    Settings,
}

async fn run(app: AppHandle, generation: u64) {
    let hasher = std::collections::hash_map::RandomState::new();
    let mut planner = Planner::new(Instant::now());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, String, Outcome)>();
    let (focus, mut settings_changed) = {
        let Some(state) = app.try_state::<DoubleTap>() else {
            return;
        };
        let runner = &state.read_ahead;
        let focus = runner
            .focus
            .get_or_init(|| FocusEvents::start(&app))
            .clone();
        (focus, runner.wake.subscribe())
    };
    let events = focus.is_some();
    let mut power = power::Cached::default();
    let mut paused = false;
    // The first look is at once.
    let mut deadline = tokio::time::Instant::now();
    let mut last_look: Option<tokio::time::Instant> = None;
    loop {
        let focus_changed = async {
            match &focus {
                Some(f) => f.changed().await,
                None => std::future::pending().await,
            }
        };
        let woke = tokio::select! {
            () = tokio::time::sleep_until(deadline) => Woke::Due,
            () = focus_changed, if !paused => Woke::Focus,
            Some(o) = rx.recv() => Woke::Outcome(o),
            _ = settings_changed.changed() => Woke::Settings,
        };
        match woke {
            Woke::Due | Woke::Settings => {}
            Woke::Outcome((seq, key, outcome)) => {
                // It can end a read-ahead or start a backoff: sleep again for as long as that
                // allows.
                planner.outcome(Instant::now(), seq, &key, outcome);
                if !paused {
                    deadline =
                        tokio::time::Instant::now() + planner.next_check(Instant::now(), events);
                }
                continue;
            }
            Woke::Focus => {
                let now = tokio::time::Instant::now();
                let earliest = last_look.map_or(now, |t| t + EVENT_MIN_GAP);
                tokio::time::sleep_until((now + EVENT_SETTLE).max(earliest)).await;
                // Notifications during the settle are covered by this look.
                if let Some(f) = &focus {
                    let _ = futures::FutureExt::now_or_never(f.changed());
                }
            }
        }
        let Some(state) = app.try_state::<DoubleTap>() else {
            return;
        };
        let s = lock(&state.settings).clone();
        if state.read_ahead.generation.load(Ordering::SeqCst) != generation || !wanted(&s) {
            if planner.in_flight() {
                cancel(&app);
            }
            return;
        }
        while let Ok((seq, key, outcome)) = rx.try_recv() {
            planner.outcome(Instant::now(), seq, &key, outcome);
        }
        let now_power = power.current().await;
        let pause = power::paused(now_power, &s);
        if pause != paused {
            paused = pause;
            probe::record("read_ahead_paused", || {
                serde_json::json!({
                    "paused": pause,
                    "on_battery": now_power.on_battery,
                    "low_power": now_power.low_power,
                })
            });
        }
        if paused {
            // As if nothing were in front: a running read-ahead is cancelled, and the dwell
            // starts over once it resumes.
            if planner.tick(Instant::now(), Front::Nothing) == Action::Cancel {
                cancel(&app);
            }
            deadline = tokio::time::Instant::now() + power.until_due(Instant::now()).max(MIN_WAIT);
            continue;
        }
        last_look = Some(tokio::time::Instant::now());
        let front = observe(&app, &s, &hasher).await;
        let key = match &front {
            Front::App { key, .. } => key.clone(),
            _ => String::new(),
        };
        match planner.tick(Instant::now(), front) {
            Action::Wait => {}
            Action::Cancel => cancel(&app),
            Action::Read => {
                let a11y = state.a11y.clone();
                let s2 = s.clone();
                let h2 = hasher.clone();
                let read = tauri::async_runtime::spawn_blocking(move || {
                    let focused = a11y.api().focused().ok().flatten()?;
                    if !draftable(&focused.field)
                        || flow::local_refusal(&focused.app, &s2).is_some()
                    {
                        return None;
                    }
                    let request = flow::draft_request(focused, &s2);
                    let fp = fingerprint(&h2, &request);
                    Some((request, fp))
                })
                .await
                .ok()
                .flatten();
                let fp = read.as_ref().map(|(_, fp)| *fp);
                match planner.read(Instant::now(), &key, fp) {
                    Decision::Skip => {}
                    Decision::Cancel => cancel(&app),
                    Decision::Send(seq) => {
                        if let Some((request, _)) = read {
                            let h = app.clone();
                            let tx = tx.clone();
                            tauri::async_runtime::spawn(async move {
                                let outcome = send(&h, &request).await;
                                let _ = tx.send((seq, key, outcome));
                            });
                        }
                    }
                }
            }
        }
        deadline = tokio::time::Instant::now() + planner.next_check(Instant::now(), events);
    }
}

/// The cheap read: what is in front, without reading any field.
async fn observe(app: &AppHandle, s: &DoubleTapSettings, hasher: &impl BuildHasher) -> Front {
    let Some(state) = app.try_state::<DoubleTap>() else {
        return Front::Nothing;
    };
    if lock(&state.machine).busy() {
        return Front::Busy;
    }
    if app
        .webview_windows()
        .values()
        .any(|w| w.is_focused().unwrap_or(false))
    {
        return Front::Nothing;
    }
    let a11y = state.a11y.clone();
    let front = tauri::async_runtime::spawn_blocking(move || a11y.frontmost())
        .await
        .ok()
        .flatten();
    let Some(front) = front else {
        return Front::Nothing;
    };
    if is_own(app, &front) {
        return Front::Nothing;
    }
    Front::App {
        key: app_key(&front),
        surface: surface(hasher, &front),
        allowed: flow::local_refusal(&front, s).is_none(),
    }
}

/// Whether `front` is Nugget itself (its bundle id or product name).
fn is_own(app: &AppHandle, front: &AppInfo) -> bool {
    let config = app.config();
    let ours = AppInfo {
        id: Some(config.identifier.clone()),
        name: config.product_name.clone().unwrap_or_default(),
        window_title: None,
        url: None,
    };
    flow::same_app(front, &ours)
}

/// `draft.read_ahead`, once (never retried: a busy daemon has better things to do).
async fn send(app: &AppHandle, request: &DraftRequest) -> Outcome {
    let daemon = app.state::<Daemon>();
    let Ok(client) = daemon.client(app).await else {
        return Outcome::Failed;
    };
    let outcome = match client
        .call::<_, Reply>(method::DRAFT_READ_AHEAD, request)
        .await
    {
        Ok(reply) => reply.outcome(),
        Err(e) => {
            tracing::debug!(error = %e, "draft.read_ahead failed");
            Outcome::Failed
        }
    };
    probe::record("read_ahead", || serde_json::json!(format!("{outcome:?}")));
    outcome
}

/// Fire and forget: `draft.read_ahead_cancel`.
fn cancel(app: &AppHandle) {
    probe::record("read_ahead_cancel", || serde_json::Value::Null);
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        let daemon = h.state::<Daemon>();
        let Ok(client) = daemon.client(&h).await else {
            return;
        };
        if let Err(e) = client
            .call::<_, serde_json::Value>(method::DRAFT_READ_AHEAD_CANCEL, &serde_json::json!({}))
            .await
        {
            tracing::debug!(error = %e, "draft.read_ahead_cancel failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ipc::{DraftContext, FieldInfo, ThreadMessage};

    fn app(key: &str) -> Front {
        Front::App {
            key: key.into(),
            surface: 1,
            allowed: true,
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Ticks every second from `from` until the planner asks to read; returns that time.
    fn until_read(p: &mut Planner, from: Instant, front: &Front, limit: u64) -> Option<Instant> {
        (0..=limit)
            .map(|i| from + Duration::from_secs(i))
            .find(|t| p.tick(*t, front.clone()) == Action::Read)
    }

    #[test]
    fn reads_once_after_the_dwell_then_sends() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        assert_eq!(p.tick(t0, app("slack")), Action::Wait);
        assert_eq!(p.tick(t0 + ms(1_000), app("slack")), Action::Wait);
        assert_eq!(p.tick(t0 + ms(1_500), app("slack")), Action::Read);
        // One read, not one per tick.
        assert_eq!(p.tick(t0 + ms(2_500), app("slack")), Action::Wait);
        assert_eq!(p.read(t0 + ms(2_600), "slack", Some(7)), Decision::Send(1));
        assert!(p.in_flight());
    }

    #[test]
    fn rereads_slow_down_and_send_only_on_change() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("mail"));
        let r1 = until_read(&mut p, t0, &app("mail"), 5).unwrap();
        assert_eq!(p.read(r1, "mail", Some(7)), Decision::Send(1));
        p.outcome(r1, 1, "mail", Outcome::Started { done: true });
        let r2 = until_read(&mut p, r1 + ms(1), &app("mail"), 30).unwrap();
        assert!(r2.duration_since(r1) >= REREAD);
        // Unchanged: nothing sent, and the next read waits longer.
        assert_eq!(p.read(r2, "mail", Some(7)), Decision::Skip);
        let r3 = until_read(&mut p, r2 + ms(1), &app("mail"), 120).unwrap();
        assert!(r3.duration_since(r2) >= REREAD * 2);
        assert_eq!(p.read(r3, "mail", Some(7)), Decision::Skip);
        let mut prev = r3;
        for _ in 0..10 {
            let next = until_read(&mut p, prev + ms(1), &app("mail"), 200).unwrap();
            assert!(next.duration_since(prev) <= MAX_REREAD + Duration::from_secs(1));
            assert_eq!(p.read(next, "mail", Some(7)), Decision::Skip);
            prev = next;
        }
        // A changed context is sent at the next read.
        let r = until_read(&mut p, prev + MAX_REREAD, &app("mail"), 5).unwrap();
        assert_eq!(p.read(r, "mail", Some(8)), Decision::Send(2));
    }

    #[test]
    fn switching_app_or_window_cancels_and_restarts_the_dwell() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("slack"));
        assert_eq!(p.tick(t0 + ms(1_500), app("slack")), Action::Read);
        assert_eq!(p.read(t0 + ms(1_600), "slack", Some(3)), Decision::Send(1));
        // Another app: cancel, then a fresh dwell.
        assert_eq!(p.tick(t0 + ms(2_000), app("mail")), Action::Cancel);
        assert!(!p.in_flight());
        assert_eq!(p.tick(t0 + ms(3_000), app("mail")), Action::Wait);
        assert_eq!(p.tick(t0 + ms(3_500), app("mail")), Action::Read);
        assert_eq!(p.read(t0 + ms(3_600), "mail", Some(4)), Decision::Send(2));
        // Another window of the same app also cancels.
        let other_window = Front::App {
            key: "mail".into(),
            surface: 2,
            allowed: true,
        };
        assert_eq!(p.tick(t0 + ms(4_000), other_window), Action::Cancel);
        // Nothing in front (or Nugget itself): cancel only if something may be running.
        assert_eq!(p.tick(t0 + ms(5_000), Front::Nothing), Action::Wait);
    }

    #[test]
    fn no_text_field_cancels_a_running_read_ahead() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("slack"));
        p.tick(t0 + ms(1_500), app("slack"));
        assert_eq!(p.read(t0 + ms(1_600), "slack", Some(3)), Decision::Send(1));
        let r = until_read(&mut p, t0 + ms(2_000), &app("slack"), 30).unwrap();
        assert_eq!(p.read(r, "slack", None), Decision::Cancel);
        assert!(!p.in_flight());
        // A read for an app no longer in front is dropped.
        p.tick(r + ms(100), app("mail"));
        assert_eq!(p.read(r + ms(200), "slack", Some(9)), Decision::Skip);
    }

    #[test]
    fn disallowed_apps_are_never_read() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        let vault = Front::App {
            key: "bitwarden".into(),
            surface: 1,
            allowed: false,
        };
        for i in 0..120 {
            assert_eq!(
                p.tick(t0 + Duration::from_secs(i), vault.clone()),
                Action::Wait
            );
        }
    }

    #[test]
    fn pauses_while_a_draft_runs() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("slack"));
        p.tick(t0 + ms(1_500), app("slack"));
        p.read(t0 + ms(1_600), "slack", Some(3));
        // The draft takes over: no cancel, nothing read while busy.
        for i in 2..20 {
            assert_eq!(
                p.tick(t0 + Duration::from_secs(i), Front::Busy),
                Action::Wait
            );
        }
        assert!(!p.in_flight());
        // Afterwards the dwell starts over.
        let t1 = t0 + Duration::from_secs(20);
        assert_eq!(p.tick(t1, app("slack")), Action::Wait);
        assert_eq!(p.tick(t1 + ms(1_500), app("slack")), Action::Read);
    }

    #[test]
    fn sends_are_rate_limited() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        let mut now = t0;
        let mut sent = 0;
        // Change app every 2 s for a minute: each dwell ends in a read with a new context.
        for i in 0..30u64 {
            let front = app(&format!("app{i}"));
            p.tick(now, front.clone());
            now += ms(1_500);
            if p.tick(now, front) == Action::Read
                && matches!(
                    p.read(now, &format!("app{i}"), Some(i + 1)),
                    Decision::Send(_)
                )
            {
                sent += 1;
            }
            now += ms(500);
        }
        assert!(sent <= RATE_MAX, "{sent} sends in a minute");
        assert_eq!(sent, RATE_MAX);
        // A minute after the first send there is room again.
        let later = t0 + RATE_WINDOW + Duration::from_secs(70);
        p.tick(later, app("fresh"));
        assert_eq!(p.tick(later + ms(1_500), app("fresh")), Action::Read);
    }

    #[test]
    fn cloud_and_disabled_back_off() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("mail"));
        p.tick(t0 + ms(1_500), app("mail"));
        assert_eq!(p.read(t0 + ms(1_600), "mail", Some(3)), Decision::Send(1));
        p.outcome(t0 + ms(1_700), 1, "mail", Outcome::Skipped(Skip::Cloud));
        assert!(!p.in_flight());
        // The app is left alone for the backoff, other apps are not.
        assert_eq!(until_read(&mut p, t0 + ms(2_000), &app("mail"), 500), None);
        let t1 = t0 + BACKOFF + Duration::from_secs(5);
        assert!(until_read(&mut p, t1, &app("mail"), 70).is_some());
        p.tick(t1 + Duration::from_secs(100), app("notes"));
        let r = until_read(&mut p, t1 + Duration::from_secs(100), &app("notes"), 5).unwrap();
        assert!(matches!(p.read(r, "notes", Some(5)), Decision::Send(_)));
        // Disabled: everything waits.
        let seq = p.seq;
        p.outcome(r, seq, "notes", Outcome::Skipped(Skip::Disabled));
        p.tick(r + ms(1), app("chat"));
        assert_eq!(until_read(&mut p, r + ms(2), &app("chat"), 500), None);
    }

    #[test]
    fn failures_back_off_briefly_and_allow_a_resend() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("mail"));
        p.tick(t0 + ms(1_500), app("mail"));
        assert_eq!(p.read(t0 + ms(1_600), "mail", Some(3)), Decision::Send(1));
        p.outcome(t0 + ms(1_700), 1, "mail", Outcome::Failed);
        let quiet = until_read(&mut p, t0 + ms(2_000), &app("mail"), 100);
        assert_eq!(quiet, None);
        let r = until_read(
            &mut p,
            t0 + BACKOFF_UNAVAILABLE + ms(2_000),
            &app("mail"),
            70,
        )
        .unwrap();
        assert_eq!(p.read(r, "mail", Some(3)), Decision::Send(2));
    }

    #[test]
    fn a_started_read_ahead_stays_in_flight_until_done() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("mail"));
        p.tick(t0 + ms(1_500), app("mail"));
        p.read(t0 + ms(1_600), "mail", Some(3));
        p.outcome(t0 + ms(1_700), 1, "mail", Outcome::Started { done: false });
        assert!(p.in_flight());
        // A stale answer for an older read-ahead does not clear the newer one.
        p.outcome(t0 + ms(1_800), 0, "mail", Outcome::Started { done: true });
        assert!(p.in_flight());
        p.outcome(t0 + ms(1_900), 1, "mail", Outcome::Started { done: true });
        assert!(!p.in_flight());
    }

    #[test]
    fn next_check_waits_for_the_dwell_and_the_reread() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        // Nothing in front yet.
        assert_eq!(p.next_check(t0, true), SLOW_POLL);
        assert_eq!(p.next_check(t0, false), IDLE_POLL);
        // A dwell pending: until it ends with events, at most POLL without.
        p.tick(t0, app("mail"));
        assert_eq!(p.next_check(t0, true), DWELL);
        assert_eq!(p.next_check(t0, false), POLL);
        assert_eq!(p.next_check(t0 + ms(1_000), false), ms(500));
        // Read and sent: with events, the re-read is further off than the fallback poll.
        assert_eq!(p.tick(t0 + DWELL, app("mail")), Action::Read);
        let r1 = t0 + DWELL;
        assert_eq!(p.read(r1, "mail", Some(7)), Decision::Send(1));
        assert_eq!(p.next_check(r1, true), SLOW_POLL);
        // Without events a running read-ahead is watched every POLL, otherwise IDLE_POLL.
        assert_eq!(p.next_check(r1, false), POLL);
        p.outcome(r1, 1, "mail", Outcome::Started { done: true });
        assert_eq!(p.next_check(r1, false), IDLE_POLL);
        // Close to the re-read: until it.
        assert_eq!(p.next_check(r1 + REREAD - ms(1_000), true), ms(1_000));
        // Never a busy loop.
        assert_eq!(p.next_check(r1 + REREAD * 3, true), MIN_WAIT);
    }

    #[test]
    fn next_check_waits_long_when_nothing_can_happen() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        let vault = Front::App {
            key: "bitwarden".into(),
            surface: 1,
            allowed: false,
        };
        p.tick(t0, vault.clone());
        p.tick(t0 + ms(100), vault);
        assert_eq!(p.next_check(t0 + ms(100), true), SLOW_POLL);
        assert_eq!(p.next_check(t0 + ms(100), false), IDLE_POLL);
        // Backed off: likewise.
        p.tick(t0 + ms(200), app("mail"));
        p.tick(t0 + ms(1_700), app("mail"));
        p.read(t0 + ms(1_800), "mail", Some(3));
        p.outcome(t0 + ms(1_900), 1, "mail", Outcome::Skipped(Skip::Cloud));
        assert_eq!(p.next_check(t0 + ms(2_000), true), SLOW_POLL);
        assert_eq!(p.next_check(t0 + ms(2_000), false), IDLE_POLL);
        // Busy (a draft runs): nothing to time.
        p.tick(t0 + ms(3_000), Front::Busy);
        assert_eq!(p.next_check(t0 + ms(3_000), true), SLOW_POLL);
    }

    #[test]
    fn next_check_waits_out_the_rate_limit() {
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        let mut now = t0;
        for i in 0..RATE_MAX as u64 {
            let front = app(&format!("app{i}"));
            p.tick(now, front.clone());
            now += DWELL;
            assert_eq!(p.tick(now, front), Action::Read);
            assert!(matches!(
                p.read(now, &format!("app{i}"), Some(i + 1)),
                Decision::Send(_)
            ));
        }
        p.tick(now, app("next"));
        // The dwell ends long before a send is allowed again.
        let free = t0 + DWELL + RATE_WINDOW;
        assert_eq!(p.next_check(now, true), SLOW_POLL);
        assert_eq!(p.next_check(free - ms(2_000), true), ms(2_000));
        assert_eq!(p.next_check(free - ms(2_000), false), POLL);
    }

    /// Ticking only when `next_check` says so reads at the same moments as polling every 100 ms.
    #[test]
    fn scheduled_ticks_read_when_polling_would() {
        let schedule = |events: bool, step: Option<Duration>| {
            let t0 = Instant::now();
            let mut p = Planner::new(t0);
            let mut reads = Vec::new();
            let mut now = t0;
            while now < t0 + Duration::from_secs(150) {
                if p.tick(now, app("mail")) == Action::Read {
                    reads.push(now.duration_since(t0).as_millis() / 100);
                    p.read(now, "mail", Some(7));
                }
                now += step.unwrap_or_else(|| p.next_check(now, events));
            }
            reads
        };
        let polled = schedule(false, Some(ms(100)));
        assert!(polled.len() >= 4, "{polled:?}");
        assert_eq!(schedule(true, None), polled);
        assert_eq!(schedule(false, None), polled);
    }

    #[test]
    fn a_pause_cancels_and_restarts_the_dwell() {
        // The loop ticks `Front::Nothing` while power pauses read-ahead.
        let t0 = Instant::now();
        let mut p = Planner::new(t0);
        p.tick(t0, app("mail"));
        p.tick(t0 + DWELL, app("mail"));
        p.read(t0 + DWELL, "mail", Some(3));
        assert_eq!(p.tick(t0 + ms(2_000), Front::Nothing), Action::Cancel);
        assert_eq!(p.tick(t0 + ms(3_000), Front::Nothing), Action::Wait);
        let t1 = t0 + Duration::from_secs(60);
        assert_eq!(p.tick(t1, app("mail")), Action::Wait);
        assert_eq!(p.tick(t1 + DWELL, app("mail")), Action::Read);
    }

    #[test]
    fn replies_parse() {
        let r: Reply = serde_json::from_str(r#"{"started":true}"#).unwrap();
        assert_eq!(r.outcome(), Outcome::Started { done: false });
        let r: Reply = serde_json::from_str(r#"{"started":true,"done":true}"#).unwrap();
        assert_eq!(r.outcome(), Outcome::Started { done: true });
        for (s, k) in [
            ("disabled", Skip::Disabled),
            ("refused", Skip::Refused),
            ("cloud", Skip::Cloud),
            ("rate_limited", Skip::RateLimited),
            ("unchanged", Skip::Unchanged),
            ("unavailable", Skip::Unavailable),
            ("cancelled", Skip::Cancelled),
            ("new_reason", Skip::Other),
        ] {
            let r: Reply =
                serde_json::from_value(serde_json::json!({"started": false, "skipped": s}))
                    .unwrap();
            assert_eq!(r.outcome(), Outcome::Skipped(k));
        }
    }

    fn request(value: &str, thread: &[&str]) -> DraftRequest {
        DraftRequest {
            context: DraftContext {
                app: AppInfo {
                    id: Some("com.tinyspeck.slackmacgap".into()),
                    name: "Slack".into(),
                    window_title: None,
                    url: None,
                },
                field: FieldInfo {
                    role: "text_area".into(),
                    value: value.into(),
                    ..FieldInfo::default()
                },
                thread: thread
                    .iter()
                    .map(|t| ThreadMessage {
                        author: None,
                        text: (*t).into(),
                        from_me: None,
                        timestamp: None,
                    })
                    .collect(),
                ..DraftContext::default()
            },
            instruction: None,
            provider: None,
            max_tokens: None,
        }
    }

    #[test]
    fn fingerprints_follow_the_context() {
        let h = std::collections::hash_map::RandomState::new();
        let a = fingerprint(&h, &request("", &["hi"]));
        assert_eq!(a, fingerprint(&h, &request("", &["hi"])));
        assert_ne!(a, fingerprint(&h, &request("x", &["hi"])));
        assert_ne!(a, fingerprint(&h, &request("", &["hi", "there"])));
        assert_ne!(a, 0);
    }

    #[test]
    fn what_is_read_ahead_for() {
        let field = |role: &str, secure: bool| FieldInfo {
            role: role.into(),
            secure,
            ..FieldInfo::default()
        };
        assert!(draftable(&field("text_area", false)));
        assert!(!draftable(&field("text_field", true)));
        assert!(!draftable(&field("search_field", false)));
        assert!(!draftable(&field("", false)));
        let s = DoubleTapSettings::default();
        assert!(wanted(&s));
        assert!(!wanted(&DoubleTapSettings {
            read_ahead: false,
            ..s.clone()
        }));
        assert!(!wanted(&DoubleTapSettings {
            enabled: false,
            ..s
        }));
        assert_eq!(
            app_key(&AppInfo {
                id: Some(" Com.Apple.Mail ".into()),
                name: "Mail".into(),
                window_title: None,
                url: None,
            }),
            "com.apple.mail"
        );
        assert_eq!(
            app_key(&AppInfo {
                id: None,
                name: "Thunderbird".into(),
                window_title: None,
                url: None,
            }),
            "thunderbird"
        );
    }
}
