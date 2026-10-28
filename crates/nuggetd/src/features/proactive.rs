//! Proactive cards and the morning brief (W4): `nugget-proactive` wired into the daemon.
//!
//! - **Config**: `[proactive]`, see [`settings`] (enabled, brief time, judge locality, refresh
//!   period, debounce, UTC offset, and the engine's own knobs under `[proactive.engine]`).
//! - **Refresh** ([`Core::refresh`]; never two at once): shortly after start, after ingest passes
//!   that indexed something, a removed source and knowledge runs (debounced by `debounce_secs`),
//!   and every `refresh_minutes`. A refresh that changed the card set broadcasts `cards.changed`.
//! - **Clock**: once a day at `brief_time` (local) a refresh then `brief.ready`; the date it last
//!   fired is kept in the store so a restart does not fire it twice. Every minute, due reminders
//!   are broadcast as `reminder.due`.
//! - **Methods**: `cards.list`, `cards.feedback`, `cards.preview`, `cards.execute`, `brief.get`,
//!   `proactive.metrics`, `proactive.refresh`, `reminders.list` (see the
//!   `---- Proactive (M5) ----` block of `nugget_core::ipc`). Executing an action needs the digest
//!   of a preview made in this daemon less than an hour ago, for the same card and action, and
//!   goes through `ActionRunner` (approval bound to the digest, then the policy gate).
//!
//! When `enabled = false` the methods still answer (`cards.list` is empty with `enabled: false`)
//! but nothing runs in the background.

mod convert;
mod exec;
mod settings;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::SourceId;
use nugget_core::ipc::{
    self, ActionArtifact, BriefView, CardExecuteRequest, CardExecuteResult, CardFeedbackRequest,
    CardPreview, CardPreviewRequest, CardView, CardsChanged, CardsList, CardsListRequest,
    ProactiveMetrics, ReminderView, method, notification,
};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::{Provider, Router};
use nugget_ingest::IngestEvent;
use nugget_ipc::{Notifier, RpcError, parse_params, to_result};
use nugget_policy::{
    ActionDescriptor, Decision, JsonFilePolicyStore, KillSwitch, PolicyEngine, PolicyGate,
};
use nugget_proactive::{
    APPROVAL_KEY_LEN, Action, ActionRunner, CardState, Enrichment, ExecOutcome, Judge, JudgeInput,
    LlmJudge, PreparedAction, Proactive, RefreshReport, new_approval_key,
};
use nugget_store::Store;
use serde_json::{Value, json};
use time::{Date, OffsetDateTime};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use self::settings::{Debounce, JudgeMode, Settings};
use super::{Feature, FeatureContext};
use crate::daemon::{State, blocking};
use crate::hub::Hub;

/// First refresh after start.
const STARTUP_DELAY: Duration = Duration::from_secs(15);
/// A preview's digest is accepted by `cards.execute` this long.
const PREVIEW_TTL: time::Duration = time::Duration::hours(1);
/// Previews kept at once (the oldest is dropped first).
const MAX_PREVIEWS: usize = 64;
/// Store cursor key holding the local date the brief last fired.
const BRIEF_MARK_KEY: &str = "proactive:brief";
/// Store cursor key holding the install's approval key (bytes).
const APPROVAL_KEY_KEY: &str = "proactive:approval_key";
/// The clock wakes at least this often (reminders are checked on each wake).
const CLOCK_TICK: Duration = Duration::from_secs(60);

const METHODS: &[&str] = &[
    method::CARDS_LIST,
    method::CARDS_FEEDBACK,
    method::CARDS_PREVIEW,
    method::CARDS_EXECUTE,
    method::BRIEF_GET,
    method::PROACTIVE_METRICS,
    method::PROACTIVE_REFRESH,
    method::REMINDERS_LIST,
];

pub(crate) async fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let state = cx.state.clone();
    let table = cx.config.proactive.clone();
    let settings = blocking(move || Settings::parse(&table, settings::detect_local_offset)).await?;

    let exec = exec::Exec::new(&state)?;
    let executors = exec.executors();
    let paths = state.paths.clone();
    let policy = blocking(move || Ok(policy_gate(&paths))).await?;
    let store = state.store.clone();
    let key = blocking(move || approval_key(&store)).await?;
    let runner = ActionRunner::new(policy, executors)
        .with_key(key)
        .with_audit(state.store.clone());

    let mut engine = Proactive::new(state.store.clone(), settings.engine.clone());
    if settings.judge != JudgeMode::Off {
        engine = engine.with_judge(Arc::new(RoutedJudge::new(
            state.router.clone(),
            settings.judge,
        )));
    }
    let enabled = settings.enabled;
    tracing::info!(
        enabled,
        judge = ?settings.judge,
        brief_time = %settings.brief_time,
        utc_offset = %settings.offset,
        "proactive cards configured"
    );
    let core = Arc::new(Core::new(
        settings,
        Arc::new(engine),
        Arc::new(runner),
        Arc::new(exec),
        state.hub.clone(),
        state.store.clone(),
    ));
    if enabled {
        spawn_background(&state, &core);
    }
    Ok(Arc::new(ProactiveFeature { core }))
}

/// The policy gate proactive actions go through: the policy engine with the user's rules from
/// `<config>/policy.json` (built-in defaults when absent) and the process-wide kill switch.
///
/// The seam for the shared engine: once the daemon owns one policy engine (W3), return it here.
/// A rules file that cannot be read fails closed (every action is denied, with the reason).
/// The install's key for binding approvals to previews (created on first start, kept in the
/// encrypted store).
fn approval_key(store: &Store) -> Result<[u8; APPROVAL_KEY_LEN]> {
    let id = SourceId::new(APPROVAL_KEY_KEY);
    let stored = store
        .get_cursor(&id)?
        .and_then(|v| serde_json::from_value::<Vec<u8>>(v).ok())
        .and_then(|v| <[u8; APPROVAL_KEY_LEN]>::try_from(v).ok());
    if let Some(key) = stored {
        return Ok(key);
    }
    let key = new_approval_key()?;
    store.set_cursor(&id, json!(key.to_vec()))?;
    Ok(key)
}

pub(crate) fn policy_gate(paths: &Paths) -> Arc<dyn PolicyGate> {
    let path = paths.config.join("policy.json");
    let store = Arc::new(JsonFilePolicyStore::new(&path));
    match PolicyEngine::load(store, PolicyEngine::builder()) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e, "cannot read the action policy; proactive actions are denied");
            Arc::new(DenyAll(format!(
                "the action policy at {} cannot be read: {e}",
                path.display()
            )))
        }
    }
}

struct DenyAll(String);

impl PolicyGate for DenyAll {
    fn evaluate(&self, _: &ActionDescriptor) -> Decision {
        Decision::Deny(self.0.clone())
    }
    fn halted(&self) -> bool {
        KillSwitch::global().halted()
    }
}

// ---- seams (the daemon's pieces, faked in tests) ----

/// What the feature needs from the executors (`exec::Exec`).
pub(crate) trait ExecApi: Send + Sync {
    fn supports(&self, action: &Action) -> bool;
    fn artifact(&self, action: &Action, result_id: &str) -> Option<ActionArtifact>;
    fn pending_reminders(&self) -> Result<Vec<ReminderView>>;
    /// Reminders due at `now`, marked fired.
    fn take_due_reminders(&self, now: OffsetDateTime) -> Result<Vec<ReminderView>>;
}

impl ExecApi for exec::Exec {
    fn supports(&self, action: &Action) -> bool {
        exec::Exec::supports(self, action)
    }
    fn artifact(&self, action: &Action, result_id: &str) -> Option<ActionArtifact> {
        exec::Exec::artifact(self, action, result_id)
    }
    fn pending_reminders(&self) -> Result<Vec<ReminderView>> {
        exec::Exec::pending_reminders(self)
    }
    fn take_due_reminders(&self, now: OffsetDateTime) -> Result<Vec<ReminderView>> {
        exec::Exec::take_due_reminders(self, now)
    }
}

/// Where notifications go (the hub, or a recorder in tests).
pub(crate) trait Broadcast: Send + Sync {
    fn broadcast(&self, method: &'static str, params: Value);
}

impl Broadcast for Hub {
    fn broadcast(&self, method: &'static str, params: Value) {
        Hub::broadcast(self, method, params);
    }
}

// ---- the judge ----

/// The LLM judge over the router: a synthesis-tier provider that can follow a JSON schema; with
/// `judge = "local"` only on-device and local-network providers. The provider is chosen when the
/// engine asks for the judge's locality (once per refresh) and that same provider enriches, so the
/// engine's sensitive-evidence gate always matches where the text goes.
struct RoutedJudge {
    router: Arc<Router>,
    mode: JudgeMode,
    chosen: Mutex<Option<Arc<dyn Provider>>>,
}

impl RoutedJudge {
    fn new(router: Arc<Router>, mode: JudgeMode) -> Self {
        Self {
            router,
            mode,
            chosen: Mutex::new(None),
        }
    }

    fn pick(&self) -> Option<Arc<dyn Provider>> {
        let caps = Capabilities {
            json_schema: true,
            ..Default::default()
        };
        self.router
            .candidates(Tier::Synthesis, &caps)
            .into_iter()
            .find(|p| match self.mode {
                JudgeMode::Cloud => true,
                JudgeMode::Local | JudgeMode::Off => p.meta().locality != Locality::Cloud,
            })
    }
}

#[async_trait]
impl Judge for RoutedJudge {
    fn locality(&self) -> Locality {
        let p = self.pick();
        let locality = p.as_ref().map_or(Locality::OnDevice, |p| p.meta().locality);
        *self.chosen.lock().unwrap_or_else(PoisonError::into_inner) = p;
        locality
    }

    async fn enrich(&self, inputs: &[JudgeInput]) -> Result<Vec<Enrichment>> {
        // Never re-pick here: the engine filtered `inputs` for the locality reported above.
        let chosen = self
            .chosen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match chosen {
            Some(p) => LlmJudge::new(p).enrich(inputs).await,
            None => Ok(Vec::new()),
        }
    }
}

// ---- previews awaiting approval ----

struct PreviewEntry {
    prepared: PreparedAction,
    action: u32,
    at: OffsetDateTime,
}

/// Prepared actions by digest, for `cards.execute`.
#[derive(Default)]
struct Previews {
    map: HashMap<String, PreviewEntry>,
}

impl Previews {
    fn purge(&mut self, now: OffsetDateTime) {
        self.map
            .retain(|_, e| now - e.at < PREVIEW_TTL && e.at <= now);
    }

    fn insert(&mut self, entry: PreviewEntry, now: OffsetDateTime) {
        self.purge(now);
        while self.map.len() >= MAX_PREVIEWS {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.map.remove(&k),
                None => break,
            };
        }
        self.map.insert(entry.prepared.digest.clone(), entry);
    }

    /// Take the preview approved by `req` (put it back with [`Previews::put_back`] unless it
    /// executed). Unknown, expired or mismatched digests are refused.
    fn take(&mut self, req: &CardExecuteRequest, now: OffsetDateTime) -> Result<PreviewEntry> {
        self.purge(now);
        let entry = self.map.get(&req.digest).ok_or_else(|| {
            Error::Invalid(
                "unknown or expired preview: preview the action again (cards.preview) and approve that"
                    .into(),
            )
        })?;
        if entry.prepared.card_id != req.card_id || entry.action != req.action {
            return Err(Error::Invalid(
                "the preview digest belongs to another card or action: preview the action again"
                    .into(),
            ));
        }
        Ok(self.map.remove(&req.digest).expect("checked above"))
    }

    fn put_back(&mut self, entry: PreviewEntry) {
        self.map.insert(entry.prepared.digest.clone(), entry);
    }
}

// ---- the core ----

/// Everything the methods and background tasks share; independent of the daemon's `State`.
pub(crate) struct Core {
    settings: Settings,
    engine: Arc<Proactive>,
    runner: Arc<ActionRunner>,
    exec: Arc<dyn ExecApi>,
    out: Arc<dyn Broadcast>,
    store: Store,
    previews: Mutex<Previews>,
    /// Held while a refresh runs (one at a time).
    refreshing: tokio::sync::Mutex<()>,
    refreshes: AtomicU64,
}

impl Core {
    fn new(
        settings: Settings,
        engine: Arc<Proactive>,
        runner: Arc<ActionRunner>,
        exec: Arc<dyn ExecApi>,
        out: Arc<dyn Broadcast>,
        store: Store,
    ) -> Self {
        Self {
            settings,
            engine,
            runner,
            exec,
            out,
            store,
            previews: Mutex::new(Previews::default()),
            refreshing: tokio::sync::Mutex::new(()),
            refreshes: AtomicU64::new(0),
        }
    }

    fn previews(&self) -> std::sync::MutexGuard<'_, Previews> {
        self.previews.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn require_enabled(&self) -> Result<()> {
        if self.settings.enabled {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "proactive cards are off ([proactive] enabled = false)".into(),
            ))
        }
    }

    fn view(&self, card: &nugget_proactive::Card) -> CardView {
        convert::card_view(card, &|a| self.exec.supports(a))
    }

    fn changed(&self, reason: &str, open: usize, created: usize, resolved: usize) -> CardsChanged {
        let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        CardsChanged {
            reason: reason.into(),
            open: n(open),
            created: n(created),
            resolved: n(resolved),
        }
    }

    fn announce(&self, changed: &CardsChanged) {
        self.out.broadcast(
            notification::CARDS_CHANGED,
            serde_json::to_value(changed).unwrap_or(Value::Null),
        );
    }

    async fn open_count(&self) -> Result<usize> {
        let engine = self.engine.clone();
        blocking(move || Ok(engine.inbox()?.len())).await
    }

    /// Run a refresh now (waiting for one in progress), broadcasting `cards.changed` when the
    /// card set changed.
    async fn refresh(&self, now: OffsetDateTime) -> Result<CardsChanged> {
        let _one = self.refreshing.lock().await;
        let report = self.engine.refresh(now).await?;
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        let open = self.open_count().await?;
        let changed = self.changed(
            "refresh",
            open,
            report.created,
            report.resolved + report.expired,
        );
        if report_changed(&report) {
            self.announce(&changed);
        }
        tracing::debug!(?report, "proactive refresh");
        Ok(changed)
    }

    async fn refresh_logged(&self, now: OffsetDateTime) {
        if let Err(e) = self.refresh(now).await {
            tracing::warn!(error = %e, "proactive refresh failed");
        }
    }

    async fn list(&self, req: CardsListRequest) -> Result<CardsList> {
        if !self.settings.enabled {
            return Ok(CardsList {
                cards: Vec::new(),
                last_refresh: None,
                enabled: false,
            });
        }
        let engine = self.engine.clone();
        let include_closed = req.include_closed;
        let (inbox, mut rest, last_refresh) = blocking(move || {
            let inbox = engine.inbox()?;
            let rest = if include_closed {
                let shown: std::collections::HashSet<String> =
                    inbox.iter().map(|c| c.id.clone()).collect();
                engine
                    .all_cards()?
                    .into_iter()
                    .filter(|c| !shown.contains(&c.id))
                    .collect()
            } else {
                Vec::new()
            };
            Ok((inbox, rest, engine.last_refresh()?))
        })
        .await?;
        rest.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(CardsList {
            cards: inbox
                .iter()
                .chain(rest.iter())
                .map(|c| self.view(c))
                .collect(),
            last_refresh,
            enabled: true,
        })
    }

    async fn feedback(&self, req: CardFeedbackRequest, now: OffsetDateTime) -> Result<CardView> {
        self.require_enabled()?;
        enum Op {
            Accept,
            Dismiss(nugget_proactive::DismissReason),
            Snooze(OffsetDateTime),
            Seen,
            Done,
        }
        let op = match req.feedback.as_str() {
            "accept" => Op::Accept,
            "dismiss" => Op::Dismiss(convert::dismiss_reason(
                req.reason.as_deref(),
                req.reason_text.as_deref(),
            )?),
            "snooze" => Op::Snooze(
                req.until
                    .unwrap_or_else(|| self.settings.default_snooze_until(now)),
            ),
            "seen" => Op::Seen,
            "done" => Op::Done,
            other => {
                return Err(Error::Invalid(format!(
                    "unknown feedback {other:?} (accept, dismiss, snooze, seen, done)"
                )));
            }
        };
        let engine = self.engine.clone();
        let id = req.card_id;
        let (card, open, changed) = blocking(move || {
            let changed = match op {
                Op::Accept => engine.accept(&id, now).map(|()| true)?,
                Op::Dismiss(reason) => engine.dismiss(&id, reason, now).map(|()| true)?,
                Op::Snooze(until) => engine.snooze(&id, until, now).map(|()| true)?,
                Op::Seen => {
                    // Only a real transition is announced: clients mark cards seen when they
                    // show them, and must not be sent into a refetch loop.
                    let was_new = engine.card(&id)?.state == CardState::New;
                    engine.mark_seen(std::slice::from_ref(&id), now)?;
                    was_new
                }
                Op::Done => {
                    // Done means "handled it myself": an accept (it was useful), then done.
                    if engine.card(&id)?.state != CardState::Accepted {
                        engine.accept(&id, now)?;
                    }
                    engine.complete(&id, now)?;
                    true
                }
            };
            Ok((engine.card(&id)?, engine.inbox()?.len(), changed))
        })
        .await?;
        if changed {
            self.announce(&self.changed("feedback", open, 0, 0));
        }
        Ok(self.view(&card))
    }

    async fn preview(&self, req: CardPreviewRequest, now: OffsetDateTime) -> Result<CardPreview> {
        self.require_enabled()?;
        let index = usize::try_from(req.action).unwrap_or(usize::MAX);
        let prepared = self
            .engine
            .prepare_action(&self.runner, &req.card_id, index)
            .await?;
        let view = convert::preview(&prepared, req.action);
        self.previews().insert(
            PreviewEntry {
                prepared,
                action: req.action,
                at: now,
            },
            now,
        );
        Ok(view)
    }

    async fn execute(
        &self,
        req: CardExecuteRequest,
        now: OffsetDateTime,
    ) -> Result<CardExecuteResult> {
        self.require_enabled()?;
        // Taken out while it runs, so the same approval cannot execute twice concurrently.
        let entry = self.previews().take(&req, now)?;
        let approval = entry.prepared.approve(req.confirmed);
        let out = self
            .engine
            .run_action(&self.runner, &entry.prepared, &approval, now)
            .await;
        match out {
            Ok(ExecOutcome::Executed(id)) => {
                let artifact = self.exec.artifact(&entry.prepared.action, &id);
                let mut result = convert::outcome(ExecOutcome::Executed(id));
                result.artifact = artifact;
                let open = self.open_count().await.unwrap_or(0);
                self.announce(&self.changed("execute", open, 0, 1));
                Ok(result)
            }
            Ok(other) => {
                // Needs confirmation, denied or halted: the same preview may be retried.
                self.previews().put_back(entry);
                Ok(convert::outcome(other))
            }
            Err(e) => {
                self.previews().put_back(entry);
                Err(e)
            }
        }
    }

    async fn brief(&self, now: OffsetDateTime) -> Result<BriefView> {
        if !self.settings.enabled {
            return Ok(BriefView {
                date: convert::iso_date(self.settings.local_date(now)),
                generated_at: now,
                top: Vec::new(),
                meetings: Vec::new(),
                overdue: Vec::new(),
                renewals: Vec::new(),
                open_cards: 0,
            });
        }
        let engine = self.engine.clone();
        let b = blocking(move || engine.brief(now)).await?;
        Ok(convert::brief(&b))
    }

    async fn metrics(&self) -> Result<ProactiveMetrics> {
        let engine = self.engine.clone();
        Ok(convert::metrics(blocking(move || engine.metrics()).await?))
    }

    async fn reminders(&self) -> Result<Vec<ReminderView>> {
        let exec = self.exec.clone();
        let mut v = blocking(move || exec.pending_reminders()).await?;
        v.sort_by_key(|r| r.at);
        Ok(v)
    }

    /// Broadcast `reminder.due` for every reminder due at `now`.
    async fn fire_due_reminders(&self, now: OffsetDateTime) -> usize {
        let exec = self.exec.clone();
        match blocking(move || exec.take_due_reminders(now)).await {
            Ok(due) => {
                for r in &due {
                    self.out.broadcast(
                        notification::REMINDER_DUE,
                        serde_json::to_value(r).unwrap_or(Value::Null),
                    );
                }
                due.len()
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot read due reminders");
                0
            }
        }
    }

    async fn brief_mark(&self) -> Option<Date> {
        let store = self.store.clone();
        let v = blocking(move || store.get_cursor(&SourceId::new(BRIEF_MARK_KEY)))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot read the brief mark");
                None
            })?;
        parse_iso_date(v.get("date")?.as_str()?)
    }

    async fn set_brief_mark(&self, date: Date) -> Result<()> {
        let store = self.store.clone();
        let v = json!({ "date": convert::iso_date(date) });
        blocking(move || store.set_cursor(&SourceId::new(BRIEF_MARK_KEY), v)).await
    }

    /// Fire the morning brief if it is due at `now` (refresh, then `brief.ready`). Returns when it
    /// is due next.
    async fn brief_tick(&self, now: OffsetDateTime) -> OffsetDateTime {
        let last = self.brief_mark().await;
        let next = self.settings.next_brief_at(now, last);
        if next > now {
            return next;
        }
        self.refresh_logged(now).await;
        match self.brief(now).await {
            Ok(view) => {
                self.out.broadcast(
                    notification::BRIEF_READY,
                    serde_json::to_value(&view).unwrap_or(Value::Null),
                );
                let today = self.settings.local_date(now);
                if let Err(e) = self.set_brief_mark(today).await {
                    tracing::warn!(error = %e, "cannot record that the brief fired");
                }
                tracing::info!(date = %view.date, cards = view.top.len(), "morning brief ready");
                self.settings.next_brief_at(now, Some(today))
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot compose the morning brief; retrying");
                now + CLOCK_TICK
            }
        }
    }
}

/// Whether a refresh changed what the user sees.
fn report_changed(r: &RefreshReport) -> bool {
    r.created + r.resolved + r.expired + r.woken + r.judged > 0
}

fn parse_iso_date(s: &str) -> Option<Date> {
    let mut it = s.splitn(3, '-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u8 = it.next()?.parse().ok()?;
    let d: u8 = it.next()?.parse().ok()?;
    Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()
}

// ---- background work ----

fn spawn_background(state: &Arc<State>, core: &Arc<Core>) {
    // Capacity 1: pokes coalesce while the trigger loop is busy.
    let (tx, rx) = mpsc::channel(1);
    let stop = state.shutdown.clone();
    state.tasks.spawn(forward_ingest(
        state.ingest.subscribe(),
        tx.clone(),
        stop.clone(),
    ));
    state.tasks.spawn(forward_knowledge(
        state.knowledge.knowledge.subscribe(),
        tx,
        stop.clone(),
    ));
    state
        .tasks
        .spawn(trigger_loop(core.clone(), rx, STARTUP_DELAY, stop.clone()));
    state.tasks.spawn(clock_loop(core.clone(), stop));
}

fn poke(tx: &mpsc::Sender<()>) {
    // Full means a poke is already pending.
    let _ = tx.try_send(());
}

/// Whether an ingest event means the corpus changed.
fn ingest_changed(ev: &IngestEvent) -> bool {
    match ev {
        IngestEvent::Progress(p) => p.phase == ipc::IngestPhase::Indexing && p.done > 0,
        IngestEvent::SourceRemoved(_) => true,
        IngestEvent::SourceChanged(_) => false,
    }
}

async fn forward_ingest(
    mut rx: broadcast::Receiver<IngestEvent>,
    tx: mpsc::Sender<()>,
    stop: CancellationToken,
) {
    loop {
        let ev = tokio::select! {
            () = stop.cancelled() => return,
            ev = rx.recv() => ev,
        };
        match ev {
            Ok(ev) if ingest_changed(&ev) => poke(&tx),
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => poke(&tx),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn forward_knowledge<T: Clone>(
    mut rx: broadcast::Receiver<T>,
    tx: mpsc::Sender<()>,
    stop: CancellationToken,
) {
    loop {
        let ev = tokio::select! {
            () = stop.cancelled() => return,
            ev = rx.recv() => ev,
        };
        match ev {
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => poke(&tx),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Refresh `initial` after start, `debounce` after the last poke, and every `refresh` period.
async fn trigger_loop(
    core: Arc<Core>,
    pokes: mpsc::Receiver<()>,
    initial: Duration,
    stop: CancellationToken,
) {
    let mut pokes = Some(pokes);
    let mut debounce = Debounce::new(core.settings.debounce);
    debounce.at(Instant::now() + initial);
    let mut periodic = Instant::now() + core.settings.refresh;
    loop {
        let next = debounce.due().map_or(periodic, |d| d.min(periodic));
        let poked = async {
            match pokes.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = stop.cancelled() => return,
            p = poked => match p {
                Some(()) => debounce.poke(Instant::now()),
                None => pokes = None,
            },
            () = tokio::time::sleep_until(next) => {
                debounce.fired();
                core.refresh_logged(OffsetDateTime::now_utc()).await;
                periodic = Instant::now() + core.settings.refresh;
            }
        }
    }
}

/// The morning brief and due reminders.
async fn clock_loop(core: Arc<Core>, stop: CancellationToken) {
    loop {
        let now = OffsetDateTime::now_utc();
        core.fire_due_reminders(now).await;
        let next = core.brief_tick(now).await;
        let left = next - OffsetDateTime::now_utc();
        let wait = Duration::try_from(left)
            .unwrap_or(Duration::ZERO)
            .clamp(Duration::from_secs(1), CLOCK_TICK);
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

// ---- the feature ----

struct ProactiveFeature {
    core: Arc<Core>,
}

#[async_trait]
impl Feature for ProactiveFeature {
    fn name(&self) -> &'static str {
        "proactive"
    }

    fn methods(&self) -> &'static [&'static str] {
        METHODS
    }

    async fn call(&self, method: &str, params: Value, _: Notifier) -> Result<Value, RpcError> {
        self.core
            .call(method, params, OffsetDateTime::now_utc())
            .await
    }
}

impl Core {
    async fn call(
        &self,
        name: &str,
        params: Value,
        now: OffsetDateTime,
    ) -> Result<Value, RpcError> {
        let params = if params.is_null() { json!({}) } else { params };
        match name {
            method::CARDS_LIST => to_result(self.list(parse_params(params)?).await?),
            method::CARDS_FEEDBACK => to_result(self.feedback(parse_params(params)?, now).await?),
            method::CARDS_PREVIEW => to_result(self.preview(parse_params(params)?, now).await?),
            method::CARDS_EXECUTE => to_result(self.execute(parse_params(params)?, now).await?),
            method::BRIEF_GET => to_result(self.brief(now).await?),
            method::PROACTIVE_METRICS => to_result(self.metrics().await?),
            method::PROACTIVE_REFRESH => {
                self.require_enabled()?;
                to_result(self.refresh(now).await?)
            }
            method::REMINDERS_LIST => to_result(self.reminders().await?),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nugget_proactive::cards::STATE_VERSION;
    use nugget_proactive::{
        CandidateKind, Card, CardStore, Executors, Features, MemCardStore, ProactiveState,
        Reminder, ReminderExecutor,
    };
    use time::UtcOffset;

    use super::*;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(&'static str, Value)>>);

    impl Broadcast for Recorder {
        fn broadcast(&self, method: &'static str, params: Value) {
            self.0.lock().unwrap().push((method, params));
        }
    }

    impl Recorder {
        fn take(&self, method: &str) -> Vec<Value> {
            let mut all = self.0.lock().unwrap();
            let (hit, keep): (Vec<_>, Vec<_>) = all.drain(..).partition(|(m, _)| *m == method);
            *all = keep;
            hit.into_iter().map(|(_, v)| v).collect()
        }
    }

    #[derive(Default)]
    struct FakeExec {
        created: Mutex<Vec<Reminder>>,
        fired: Mutex<bool>,
    }

    #[async_trait]
    impl ReminderExecutor for FakeExec {
        async fn create_reminder(&self, r: &Reminder) -> Result<String> {
            self.created.lock().unwrap().push(r.clone());
            Ok("rem-1".into())
        }
    }

    impl FakeExec {
        fn view(&self) -> Vec<ReminderView> {
            self.created
                .lock()
                .unwrap()
                .iter()
                .map(|r| ReminderView {
                    id: "rem-1".into(),
                    text: r.text.clone(),
                    at: r.at,
                    about: None,
                    card_id: None,
                })
                .collect()
        }
    }

    impl ExecApi for FakeExec {
        fn supports(&self, a: &Action) -> bool {
            matches!(a, Action::Reminder(_))
        }
        fn artifact(&self, a: &Action, _: &str) -> Option<ActionArtifact> {
            match a {
                Action::Reminder(r) => Some(ActionArtifact {
                    kind: "reminder".into(),
                    path: None,
                    text: None,
                    url: None,
                    at: Some(r.at),
                }),
                _ => None,
            }
        }
        fn pending_reminders(&self) -> Result<Vec<ReminderView>> {
            Ok(self.view())
        }
        fn take_due_reminders(&self, now: OffsetDateTime) -> Result<Vec<ReminderView>> {
            let mut fired = self.fired.lock().unwrap();
            if *fired {
                return Ok(Vec::new());
            }
            let due: Vec<_> = self.view().into_iter().filter(|r| r.at <= now).collect();
            *fired = !due.is_empty();
            Ok(due)
        }
    }

    fn t0() -> OffsetDateTime {
        settings::local_at(
            Date::from_calendar_date(2026, time::Month::March, 10).unwrap(),
            time::Time::from_hms(9, 0, 0).unwrap(),
            UtcOffset::UTC,
        )
    }

    fn reminder_action() -> Action {
        Action::Reminder(Reminder {
            text: "Call Ana back".into(),
            at: t0() + time::Duration::hours(3),
            about: None,
        })
    }

    fn note_action() -> Action {
        Action::ResearchNote(nugget_proactive::ResearchNote {
            title: "Prep".into(),
            body: "notes".into(),
            sources: Vec::new(),
        })
    }

    fn card(id: &str) -> Card {
        let now = t0();
        Card {
            id: id.into(),
            dedup_key: format!("test:{id}"),
            kind: CandidateKind::FollowUp,
            title: format!("Card {id}"),
            detail: "detail".into(),
            evidence: Vec::new(),
            people: vec!["ana@example.com".into()],
            due: None,
            expires_at: None,
            // Ranks above the default `min_score` after a rescore, so the card stays in the
            // inbox once feedback rescored it.
            features: Features {
                urgency: 1.0,
                ..Features::default()
            },
            score: 0.5,
            rank: 0.5,
            factors: vec![nugget_proactive::Factor {
                name: "urgency".into(),
                value: 1.0,
                weight: 0.5,
                contribution: 0.5,
            }],
            actions: vec![reminder_action(), note_action()],
            enrichment: None,
            sensitive: false,
            state: CardState::New,
            created_at: now,
            updated_at: now,
            last_generated_at: now,
            first_seen_at: None,
            acted_at: None,
            snoozed_until: None,
            dismiss_reason: None,
            was_accepted: false,
            history: Vec::new(),
        }
    }

    struct Fixture {
        core: Core,
        out: Arc<Recorder>,
        exec: Arc<FakeExec>,
    }

    fn fixture(toml_src: &str, cards: &[&str]) -> Fixture {
        let table: toml::Table = toml::from_str(toml_src).unwrap();
        let settings = Settings::parse(&table, || UtcOffset::UTC).unwrap();
        let store = Store::open_in_memory().unwrap();
        let mem = Arc::new(MemCardStore::default());
        mem.save(&ProactiveState {
            version: STATE_VERSION,
            cards: cards
                .iter()
                .map(|id| ((*id).to_owned(), card(id)))
                .collect::<BTreeMap<_, _>>(),
            ..Default::default()
        })
        .unwrap();
        let engine = Proactive::new(store.clone(), settings.engine.clone())
            .with_graph(None)
            .with_card_store(mem);
        let exec = Arc::new(FakeExec::default());
        let policy = Arc::new(
            PolicyEngine::builder()
                .kill_switch(KillSwitch::new())
                .build(),
        );
        let runner = ActionRunner::new(
            policy,
            Executors {
                reminders: Some(exec.clone()),
                ..Default::default()
            },
        );
        let out = Arc::new(Recorder::default());
        let core = Core::new(
            settings,
            Arc::new(engine),
            Arc::new(runner),
            exec.clone(),
            out.clone(),
            store,
        );
        Fixture { core, out, exec }
    }

    fn feedback(card_id: &str, feedback: &str) -> CardFeedbackRequest {
        CardFeedbackRequest {
            card_id: card_id.into(),
            feedback: feedback.into(),
            reason: None,
            reason_text: None,
            until: None,
        }
    }

    fn exec_req(card_id: &str, action: u32, digest: &str) -> CardExecuteRequest {
        CardExecuteRequest {
            card_id: card_id.into(),
            action,
            digest: digest.into(),
            confirmed: false,
        }
    }

    fn code(r: Result<Value, RpcError>) -> i64 {
        r.unwrap_err().code
    }

    #[test]
    fn card_view_conversion() {
        let exec = FakeExec::default();
        let c = card("a");
        let v = convert::card_view(&c, &|a| exec.supports(a));
        assert_eq!(v.id, "a");
        assert_eq!(v.kind, "follow_up");
        assert_eq!(v.state, "new");
        assert_eq!(v.summary, None);
        assert_eq!(v.factors[0].name, "urgency");
        assert_eq!(v.actions.len(), 2);
        let a = &v.actions[0];
        assert_eq!(
            (a.index, a.kind.as_str(), a.supported),
            (0, "reminder", true)
        );
        assert_eq!(a.label, reminder_action().preview().summary);
        assert_eq!(
            a.risk,
            nugget_proactive::actions::risk_name(reminder_action().risk())
        );
        assert_eq!(
            (
                v.actions[1].index,
                v.actions[1].kind.as_str(),
                v.actions[1].supported
            ),
            (1, "research_note", false)
        );
        // Round-trips through the wire type.
        let back: CardView = serde_json::from_value(serde_json::to_value(&v).unwrap()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn small_conversions() {
        assert_eq!(
            convert::iso_date(Date::from_calendar_date(2026, time::Month::March, 9).unwrap()),
            "2026-03-09"
        );
        assert_eq!(
            parse_iso_date("2026-03-09"),
            Date::from_calendar_date(2026, time::Month::March, 9).ok()
        );
        assert_eq!(parse_iso_date("nope"), None);
        use nugget_proactive::DismissReason as R;
        assert_eq!(
            convert::dismiss_reason(Some("wrong_info"), None).unwrap(),
            R::WrongInfo
        );
        assert_eq!(
            convert::dismiss_reason(None, Some(" spam ")).unwrap(),
            R::Other("spam".into())
        );
        assert!(convert::dismiss_reason(Some("meh"), None).is_err());
        let r = convert::outcome(ExecOutcome::NeedsConfirmation("sure?".into()));
        assert_eq!(
            (r.outcome.as_str(), r.detail.as_deref()),
            ("needs_confirmation", Some("sure?"))
        );
        let r = convert::outcome(ExecOutcome::Halted);
        assert_eq!((r.outcome.as_str(), r.detail), ("halted", None));
        assert_eq!(
            convert::outcome(ExecOutcome::Denied("no".into())).outcome,
            "denied"
        );
        let mut m = nugget_proactive::Metrics {
            total: 3,
            shown: 2,
            accepted: 1,
            precision: Some(0.5),
            ..Default::default()
        };
        m.by_kind.insert(
            "renewal".into(),
            nugget_proactive::cards::KindMetrics {
                shown: 1,
                ..Default::default()
            },
        );
        let v = convert::metrics(m);
        assert_eq!(
            (v.total, v.shown, v.accepted, v.precision),
            (3, 2, 1, Some(0.5))
        );
        assert_eq!(v.by_kind["renewal"].shown, 1);
    }

    #[tokio::test]
    async fn list_and_feedback_change_state_and_broadcast() {
        let f = fixture("", &["a", "b"]);
        let list: CardsList = serde_json::from_value(
            f.core
                .call(method::CARDS_LIST, Value::Null, t0())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(list.enabled);
        assert_eq!(list.cards.len(), 2);

        // seen: announced once, not again for a card already seen.
        let v = f.core.feedback(feedback("a", "seen"), t0()).await.unwrap();
        assert_eq!(v.state, "seen");
        assert_eq!(f.out.take(notification::CARDS_CHANGED).len(), 1);
        f.core.feedback(feedback("a", "seen"), t0()).await.unwrap();
        assert!(f.out.take(notification::CARDS_CHANGED).is_empty());

        // snooze without `until`: tomorrow at the brief time.
        let v = f
            .core
            .feedback(feedback("a", "snooze"), t0())
            .await
            .unwrap();
        assert_eq!(v.state, "snoozed");
        assert_eq!(
            v.snoozed_until,
            Some(f.core.settings.default_snooze_until(t0()))
        );
        let changed: CardsChanged =
            serde_json::from_value(f.out.take(notification::CARDS_CHANGED).remove(0)).unwrap();
        assert_eq!((changed.reason.as_str(), changed.open), ("feedback", 1));

        let mut d = feedback("b", "dismiss");
        d.reason = Some("not_relevant".into());
        assert_eq!(f.core.feedback(d, t0()).await.unwrap().state, "dismissed");
        assert_eq!(f.out.take(notification::CARDS_CHANGED).len(), 1);

        // Closed cards only with include_closed.
        let list = f.core.list(CardsListRequest::default()).await.unwrap();
        assert!(list.cards.is_empty());
        let list = f
            .core
            .list(CardsListRequest {
                include_closed: true,
            })
            .await
            .unwrap();
        assert_eq!(list.cards.len(), 2);

        // Bad input.
        let err = f
            .core
            .feedback(feedback("a", "love"), t0())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
        let err = f
            .core
            .feedback(feedback("zzz", "accept"), t0())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
        assert_eq!(
            code(
                f.core
                    .call(
                        method::CARDS_FEEDBACK,
                        json!({"card_id": "zzz", "feedback": "accept"}),
                        t0()
                    )
                    .await
            ),
            nugget_ipc::code::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn done_accepts_then_completes() {
        let f = fixture("", &["a"]);
        let v = f.core.feedback(feedback("a", "done"), t0()).await.unwrap();
        assert_eq!(v.state, "done");
        let m = f.core.metrics().await.unwrap();
        assert_eq!(m.accepted, 1);
    }

    #[tokio::test]
    async fn preview_then_execute_is_bound_to_the_digest() {
        let f = fixture("", &["a", "b"]);
        let p = f
            .core
            .preview(
                CardPreviewRequest {
                    card_id: "a".into(),
                    action: 0,
                },
                t0(),
            )
            .await
            .unwrap();
        assert_eq!(
            (p.kind.as_str(), p.card_id.as_str(), p.action),
            ("reminder", "a", 0)
        );
        assert!(!p.diff.is_empty());
        assert_eq!(p.diff[0].op, "add");

        // Unknown digest.
        let err = f
            .core
            .execute(exec_req("a", 0, "deadbeef"), t0())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("preview"), "{err}");
        // Right digest, wrong card or action.
        for req in [exec_req("b", 0, &p.digest), exec_req("a", 1, &p.digest)] {
            let err = f.core.execute(req, t0()).await.unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{err}");
        }
        assert!(f.exec.created.lock().unwrap().is_empty(), "nothing ran");

        // The right one executes, once.
        let r = f
            .core
            .execute(exec_req("a", 0, &p.digest), t0())
            .await
            .unwrap();
        assert_eq!(
            (r.outcome.as_str(), r.detail.as_deref()),
            ("executed", Some("rem-1"))
        );
        assert_eq!(r.artifact.unwrap().kind, "reminder");
        assert_eq!(f.exec.created.lock().unwrap().len(), 1);
        let changed: CardsChanged =
            serde_json::from_value(f.out.take(notification::CARDS_CHANGED).remove(0)).unwrap();
        assert_eq!(changed.reason, "execute");
        let err = f
            .core
            .execute(exec_req("a", 0, &p.digest), t0())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "single use: {err}");
        let all = f.core.engine.card("a").unwrap();
        assert_eq!(all.state, CardState::Done);

        // Over the wire: invalid params.
        assert_eq!(
            code(
                f.core
                    .call(
                        method::CARDS_EXECUTE,
                        json!({"card_id": "a", "action": 0, "digest": "x"}),
                        t0()
                    )
                    .await
            ),
            nugget_ipc::code::INVALID_PARAMS
        );
    }

    #[tokio::test]
    async fn previews_expire() {
        let f = fixture("", &["a"]);
        let p = f
            .core
            .preview(
                CardPreviewRequest {
                    card_id: "a".into(),
                    action: 0,
                },
                t0(),
            )
            .await
            .unwrap();
        let later = t0() + time::Duration::hours(2);
        let err = f
            .core
            .execute(exec_req("a", 0, &p.digest), later)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
        assert!(f.exec.created.lock().unwrap().is_empty());
    }

    #[test]
    fn preview_cache_is_bounded() {
        let mut p = Previews::default();
        let prepared = |i: usize| PreparedAction {
            card_id: "a".into(),
            action_index: 0,
            action: reminder_action(),
            preview: reminder_action().preview(),
            digest: format!("d{i}"),
        };
        for i in 0..MAX_PREVIEWS + 5 {
            p.insert(
                PreviewEntry {
                    prepared: prepared(i),
                    action: 0,
                    at: t0() + time::Duration::seconds(i as i64),
                },
                t0() + time::Duration::seconds(i as i64),
            );
        }
        assert_eq!(p.map.len(), MAX_PREVIEWS);
        assert!(!p.map.contains_key("d0"), "oldest dropped first");
        assert!(p.map.contains_key(&format!("d{}", MAX_PREVIEWS + 4)));
    }

    #[tokio::test]
    async fn refresh_broadcasts_only_changes() {
        // An empty corpus: a seeded open card is resolved (its situation is gone).
        let f = fixture("", &["a"]);
        let c = f.core.refresh(t0()).await.unwrap();
        assert_eq!((c.reason.as_str(), c.resolved, c.open), ("refresh", 1, 0));
        assert_eq!(f.out.take(notification::CARDS_CHANGED).len(), 1);
        assert_eq!(f.core.refreshes.load(Ordering::Relaxed), 1);
        let list = f.core.list(CardsListRequest::default()).await.unwrap();
        assert_eq!(list.last_refresh, Some(t0()));
        // Nothing changed the second time.
        f.core.refresh(t0()).await.unwrap();
        assert!(f.out.take(notification::CARDS_CHANGED).is_empty());
    }

    #[tokio::test]
    async fn disabled_serves_empty_and_refuses_changes() {
        let f = fixture("enabled = false", &["a"]);
        let list = f.core.list(CardsListRequest::default()).await.unwrap();
        assert!(!list.enabled);
        assert!(list.cards.is_empty());
        assert!(matches!(
            f.core.feedback(feedback("a", "accept"), t0()).await,
            Err(Error::Unsupported(_))
        ));
        assert_eq!(
            code(
                f.core
                    .call(method::PROACTIVE_REFRESH, Value::Null, t0())
                    .await
            ),
            nugget_ipc::code::UNSUPPORTED
        );
        let b = f.core.brief(t0()).await.unwrap();
        assert_eq!((b.date.as_str(), b.open_cards), ("2026-03-10", 0));
        f.core
            .call(method::PROACTIVE_METRICS, Value::Null, t0())
            .await
            .unwrap();
        f.core
            .call(method::REMINDERS_LIST, Value::Null, t0())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn brief_fires_once_a_day_and_reminders_when_due() {
        let f = fixture("brief_time = \"08:00\"", &[]);
        // 09:00 UTC, not fired yet: fires now and schedules tomorrow.
        let next = f.core.brief_tick(t0()).await;
        let briefs = f.out.take(notification::BRIEF_READY);
        assert_eq!(briefs.len(), 1);
        let b: BriefView = serde_json::from_value(briefs[0].clone()).unwrap();
        assert_eq!(b.date, "2026-03-10");
        assert_eq!(next, t0() + time::Duration::hours(23));
        // The mark persists: a second tick (or a restart) the same day does not fire.
        assert_eq!(
            f.core.brief_tick(t0() + time::Duration::hours(1)).await,
            next
        );
        assert!(f.out.take(notification::BRIEF_READY).is_empty());

        // A reminder created by an executed action fires when due, once.
        f.exec.created.lock().unwrap().push(Reminder {
            text: "ping".into(),
            at: t0() + time::Duration::minutes(5),
            about: None,
        });
        assert_eq!(f.core.fire_due_reminders(t0()).await, 0);
        assert_eq!(f.core.reminders().await.unwrap().len(), 1);
        let later = t0() + time::Duration::minutes(6);
        assert_eq!(f.core.fire_due_reminders(later).await, 1);
        assert_eq!(f.out.take(notification::REMINDER_DUE).len(), 1);
        assert_eq!(f.core.fire_due_reminders(later).await, 0);
    }

    #[tokio::test]
    async fn trigger_loop_refreshes_at_start_and_after_quiet_pokes() {
        let mut f = fixture("", &[]);
        f.core.settings.debounce = Duration::from_millis(100);
        let core = Arc::new(f.core);
        let (tx, rx) = mpsc::channel(1);
        let stop = CancellationToken::new();
        let task = tokio::spawn(trigger_loop(
            core.clone(),
            rx,
            Duration::from_millis(10),
            stop.clone(),
        ));
        let count = || core.refreshes.load(Ordering::Relaxed);
        let wait_for = |n: u64| async move {
            for _ in 0..200 {
                if count() >= n {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        wait_for(1).await;
        assert_eq!(count(), 1, "startup refresh");
        for _ in 0..3 {
            poke(&tx);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(count(), 1, "still debouncing");
        wait_for(2).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(count(), 2, "one refresh for the burst");
        stop.cancel();
        task.await.unwrap();
    }

    #[test]
    fn ingest_events_that_count() {
        use nugget_core::ipc::{IngestPhase, IngestProgress};
        let p = |phase, done| {
            IngestEvent::Progress(IngestProgress {
                source: SourceId::new("s"),
                phase,
                done,
                total: None,
                error: None,
            })
        };
        assert!(ingest_changed(&p(IngestPhase::Indexing, 3)));
        assert!(!ingest_changed(&p(IngestPhase::Indexing, 0)));
        assert!(!ingest_changed(&p(IngestPhase::Idle, 0)));
        assert!(ingest_changed(&IngestEvent::SourceRemoved(SourceId::new(
            "s"
        ))));
    }

    #[test]
    fn unreadable_policy_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.config).unwrap();
        // No file: the engine with built-in defaults.
        let gate = policy_gate(&paths);
        let d = reminder_action().descriptor("t");
        assert!(!matches!(gate.evaluate(&d), Decision::Deny(_)));
        std::fs::write(paths.config.join("policy.json"), b"{not json").unwrap();
        let gate = policy_gate(&paths);
        assert!(matches!(gate.evaluate(&d), Decision::Deny(_)));
    }
}
