//! Proactive cards and the morning brief (M5).
//!
//! Pipeline of [`Proactive::refresh`]:
//! 1. **Corpus.** Mail and calendar items are read from the encrypted store and normalised
//!    across connectors ([`corpus`]), with threads computed.
//! 2. **Deterministic generators** ([`generators`]): unanswered threads, graph commitments,
//!    meetings to prepare, renewals and trials, promised follow-ups. Each [`Candidate`] carries
//!    evidence (item refs and quotes) and a stable dedup key.
//! 3. **Scoring** ([`score`]): an online logistic model over urgency, importance (relationship
//!    strength from the graph and mail history), freshness and kind; updated from accept,
//!    dismiss and snooze; explainable (top factors on every card). On top of it, the
//!    [`feedback`] layer learns from the user's own feedback per sender, domain, kind and thread
//!    (reason-aware, decaying with a 30-day half-life, bounded) and adds to the log-odds; a
//!    thread dismissed as already handled is suppressed until something new happens in it. Each
//!    card lists these effects (`Card::learned`) with the counts behind them, and
//!    [`Proactive::reset_learning`] forgets them. Learning changes ranking and suppression only,
//!    never what an action does or whether it needs approval.
//! 4. **Optional LLM judge** ([`judge`]): enriches the top candidates (summary, suggested action,
//!    confidence). Local by default; cloud only when allowed and never for sensitive evidence. It
//!    cannot add candidates and only nudges the rank (20 % weight next to the model's 80 %).
//! 5. **Cards** ([`cards`]): merged by dedup key into the persisted card set with a state machine
//!    (new, seen, snoozed, accepted, dismissed, done, expired). Dismissed, done and expired cards
//!    are tombstones: the same situation never returns as a new card. Open cards whose situation
//!    disappeared (thread answered, meeting started) are resolved.
//!
//! **Actions** ([`actions`]) are typed (`DraftReply`, `CreateEvent`, `Reminder`,
//! `ArchiveThread`, `ResearchNote`), render a dry-run diff, declare a `RiskClass`, and run
//! through [`ActionRunner`] only with an [`Approval`] bound to the previewed digest and after
//! the [`PolicyGate`](nugget_policy::PolicyGate) agrees.
//!
//! The **morning brief** ([`brief`]) and **metrics** ([`Metrics`]: precision = accepted / shown,
//! time to action, dismiss reasons) are computed from the same state.
//!
//! The API is mostly synchronous over the store; async callers should run [`Proactive`] calls
//! other than `refresh` and `run_action` on a blocking thread.

pub mod actions;
pub mod brief;
pub mod candidate;
pub mod cards;
pub mod config;
pub mod corpus;
pub mod feedback;
pub mod generators;
pub mod judge;
pub mod score;
pub mod text;

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nugget_core::{Error, Result};
use nugget_graph::GraphStore;
use nugget_store::Store;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};

pub use actions::{
    APPROVAL_KEY_LEN, Action, ActionPreview, ActionRunner, Approval, ArchiveThread,
    CalendarExecutor, CreateEvent, DiffLine, DiffOp, DraftReply, DraftRequest, Drafter,
    ExecOutcome, Executors, MailExecutor, NoteExecutor, PreparedAction, Reminder, ReminderExecutor,
    ResearchNote, new_approval_key,
};
pub use brief::{BriefCard, BriefMeeting, MorningBrief};
pub use candidate::{Candidate, CandidateKind, EvidenceRef, Features};
pub use cards::{
    Card, CardState, CardStore, DismissReason, MemCardStore, Metrics, ProactiveState,
    StoreCardStore,
};
pub use config::ProactiveConfig;
pub use corpus::Corpus;
pub use feedback::{Direction, Effect, Learned, LearnedEntry, ResetScope, Scope};
pub use generators::GenContext;
pub use judge::{Enrichment, Judge, JudgeInput, LlmJudge, judge_allowed};
pub use score::{Factor, Feedback, ScoringModel};

/// What one refresh did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    /// Candidates the generators produced.
    pub generated: usize,
    pub created: usize,
    /// Existing open cards refreshed.
    pub updated: usize,
    /// Candidates matching a dismissed/done/expired card (not shown again).
    pub suppressed: usize,
    /// Open cards past their deadline.
    pub expired: usize,
    /// Open cards whose situation disappeared (e.g. the thread was answered).
    pub resolved: usize,
    /// Snoozed cards back in the inbox.
    pub woken: usize,
    /// Candidates enriched by the judge.
    pub judged: usize,
    /// Candidates kept from a cloud judge because they are sensitive or cloud is not allowed.
    pub judge_withheld: usize,
    pub judge_error: Option<String>,
}

pub struct Proactive {
    store: Store,
    graph: Option<Arc<dyn GraphStore>>,
    cfg: ProactiveConfig,
    /// The user's current UTC offset in seconds; replaces `cfg.utc_offset` (see
    /// [`Proactive::set_utc_offset`]).
    utc_offset: AtomicI32,
    cards: Arc<dyn CardStore>,
    judge: Option<Arc<dyn Judge>>,
    /// Serialises load-modify-save of the card state.
    lock: Mutex<()>,
    /// Cards with an action executing: one at a time per card.
    executing: Mutex<HashSet<String>>,
}

impl Proactive {
    /// Cards persisted in `store`, graph from `store`.
    pub fn new(store: Store, cfg: ProactiveConfig) -> Self {
        Self {
            graph: Some(Arc::new(store.graph())),
            cards: Arc::new(StoreCardStore::new(store.clone())),
            store,
            utc_offset: AtomicI32::new(cfg.utc_offset.whole_seconds()),
            cfg,
            judge: None,
            lock: Mutex::new(()),
            executing: Mutex::new(HashSet::new()),
        }
    }

    pub fn with_graph(mut self, graph: Option<Arc<dyn GraphStore>>) -> Self {
        self.graph = graph;
        self
    }

    pub fn with_card_store(mut self, cards: Arc<dyn CardStore>) -> Self {
        self.cards = cards;
        self
    }

    pub fn with_judge(mut self, judge: Arc<dyn Judge>) -> Self {
        self.judge = Some(judge);
        self
    }

    /// The configuration given to [`Proactive::new`]; its `utc_offset` is the initial one (see
    /// [`Proactive::utc_offset`]).
    pub fn config(&self) -> &ProactiveConfig {
        &self.cfg
    }

    /// The user's current UTC offset (for "today" and floating calendar times).
    pub fn utc_offset(&self) -> UtcOffset {
        UtcOffset::from_whole_seconds(self.utc_offset.load(Ordering::Relaxed))
            .unwrap_or(self.cfg.utc_offset)
    }

    /// Change the user's UTC offset (a daylight-saving change while running). Takes effect from
    /// the next call.
    pub fn set_utc_offset(&self, offset: UtcOffset) {
        self.utc_offset
            .store(offset.whole_seconds(), Ordering::Relaxed);
    }

    /// The configuration with the current UTC offset.
    fn cfg_now(&self) -> Cow<'_, ProactiveConfig> {
        let offset = self.utc_offset();
        if offset == self.cfg.utc_offset {
            Cow::Borrowed(&self.cfg)
        } else {
            Cow::Owned(ProactiveConfig {
                utc_offset: offset,
                ..self.cfg.clone()
            })
        }
    }

    pub fn corpus(&self) -> Result<Corpus> {
        Corpus::load(&self.store, &self.cfg_now())
    }

    /// Run the deterministic generators (no scoring, no state change).
    pub fn candidates(&self, now: OffsetDateTime) -> Result<Vec<Candidate>> {
        let corpus = self.corpus()?;
        self.candidates_in(&corpus, now)
    }

    fn candidates_in(&self, corpus: &Corpus, now: OffsetDateTime) -> Result<Vec<Candidate>> {
        generators::generate_all(&GenContext {
            corpus,
            graph: self.graph.as_deref(),
            store: Some(&self.store),
            cfg: &self.cfg_now(),
            now,
        })
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Generate, score, enrich and merge candidates into the card set.
    pub async fn refresh(&self, now: OffsetDateTime) -> Result<RefreshReport> {
        let corpus = self.corpus()?;
        let cands = self.candidates_in(&corpus, now)?;
        let mut report = RefreshReport {
            generated: cands.len(),
            ..Default::default()
        };

        // Judge outside the lock, on a snapshot.
        let snapshot = self.cards.load()?;
        let mut enrichments = Vec::new();
        if let Some(judge) = &self.judge {
            let locality = judge.locality();
            let mut pending: Vec<(&Candidate, f64)> = cands
                .iter()
                .filter(|c| {
                    snapshot
                        .by_dedup_key(&c.dedup_key)
                        .is_none_or(|card| !card.state.is_terminal() && card.enrichment.is_none())
                })
                .filter_map(|c| {
                    let (p, _, effects) = score::learned_score(
                        &snapshot.model,
                        &snapshot.learned,
                        c.kind,
                        &c.features,
                        now,
                    );
                    // Suppressed candidates are not worth the judge's time.
                    (!score::hidden(&effects)).then_some((c, p))
                })
                .collect();
            pending.sort_by(|a, b| b.1.total_cmp(&a.1));
            let mut inputs = Vec::new();
            for (c, _) in pending {
                if inputs.len() == self.cfg.judge_top_k {
                    break;
                }
                if judge_allowed(locality, self.cfg.judge_allow_cloud, c.sensitive) {
                    inputs.push(JudgeInput::from_candidate(c));
                } else {
                    report.judge_withheld += 1;
                }
            }
            if !inputs.is_empty() {
                match judge.enrich(&inputs).await {
                    Ok(e) => {
                        let sent: HashSet<&str> = inputs.iter().map(|i| i.key.as_str()).collect();
                        // The judge cannot add candidates: unknown keys are dropped.
                        enrichments = e
                            .into_iter()
                            .filter(|e| sent.contains(e.dedup_key.as_str()))
                            .collect();
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "proactive judge failed; ranking without it");
                        report.judge_error = Some(e.to_string());
                    }
                }
            }
        }

        report.judged = enrichments.len();
        let _g = self.guard();
        let mut state = self.cards.load()?;
        state.migrate(now);
        let mut produced = HashSet::new();
        for cand in cands {
            let id = cards::card_id(&cand.dedup_key);
            produced.insert(id.clone());
            let enrichment = enrichments
                .iter()
                .find(|e| e.dedup_key == cand.dedup_key)
                .cloned();
            match state.cards.get_mut(&id) {
                Some(card) if card.state.is_terminal() => report.suppressed += 1,
                Some(card) => {
                    card.refresh_from(cand, now);
                    if enrichment.is_some() {
                        card.enrichment = enrichment;
                    }
                    report.updated += 1;
                }
                None => {
                    let mut card = Card::from_candidate(cand, now);
                    card.enrichment = enrichment;
                    state.cards.insert(id, card);
                    report.created += 1;
                }
            }
        }
        for card in state.cards.values_mut() {
            if card.state.is_terminal() {
                continue;
            }
            if card.wake(now) {
                report.woken += 1;
            }
            if card.expires_at.is_some_and(|e| e <= now) {
                card.expire(now);
                report.expired += 1;
            } else if !produced.contains(&card.id) && card.state != CardState::Snoozed {
                // The situation is gone (answered, followed through, cancelled).
                if card.state == CardState::Accepted {
                    card.complete(now)?;
                } else {
                    card.expire(now);
                }
                report.resolved += 1;
            }
            if let Some(e) = &card.enrichment {
                order_actions(card, e.suggested_action.clone());
            }
        }
        state.learned.prune(now);
        state.rescore_open(now);
        state.prune(now, self.cfg.tombstone_days);
        state.last_refresh = Some(now);
        state.version = cards::STATE_VERSION;
        self.cards.save(&state)?;
        Ok(report)
    }

    /// Open cards (new and seen), best first, at most `max_inbox`.
    pub fn inbox(&self) -> Result<Vec<Card>> {
        let state = self.cards.load()?;
        let mut v: Vec<Card> = state
            .cards
            .into_values()
            .filter(|c| c.state.is_open() && score::shown(c, &self.cfg))
            .collect();
        v.sort_by(|a, b| {
            b.rank
                .total_cmp(&a.rank)
                .then_with(|| a.due.cmp(&b.due))
                .then_with(|| a.id.cmp(&b.id))
        });
        v.truncate(self.cfg.max_inbox);
        Ok(v)
    }

    /// Every card, in any state.
    pub fn all_cards(&self) -> Result<Vec<Card>> {
        Ok(self.cards.load()?.cards.into_values().collect())
    }

    pub fn card(&self, id: &str) -> Result<Card> {
        self.cards
            .load()?
            .cards
            .remove(id)
            .ok_or_else(|| Error::NotFound(format!("card {id}")))
    }

    fn update<T>(
        &self,
        id: &str,
        now: OffsetDateTime,
        f: impl FnOnce(&mut Card, &mut ScoringModel) -> Result<T>,
    ) -> Result<T> {
        let _g = self.guard();
        let mut state = self.cards.load()?;
        state.migrate(now);
        let ProactiveState { cards, model, .. } = &mut state;
        let card = cards
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("card {id}")))?;
        let out = f(card, model)?;
        state.rescore_open(now);
        self.cards.save(&state)?;
        Ok(out)
    }

    fn learn(
        &self,
        id: &str,
        fb: Feedback,
        reason: Option<&DismissReason>,
        now: OffsetDateTime,
        f: impl FnOnce(&mut Card) -> Result<()>,
    ) -> Result<()> {
        let _g = self.guard();
        let mut state = self.cards.load()?;
        state.migrate(now);
        let card = state
            .cards
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("card {id}")))?;
        let already_accepted = card.was_accepted;
        f(card)?;
        // A sensitive card's title (its mail subject) is not kept as a thread label: the
        // learned layer outlives the card and is listed by `proactive.learning`.
        let title = if card.sensitive {
            String::new()
        } else {
            card.title.clone()
        };
        let (features, kind) = (card.features.clone(), card.kind);
        // One accept per card counts; repeated accepts are no-ops.
        if !(fb == Feedback::Accepted && already_accepted) {
            state
                .model
                .update_weighted(&features, fb, model_weight(fb, reason));
            state
                .learned
                .record(kind, &features, fb, reason, &title, now);
            state.feedback_log.push(cards::FeedbackEvent {
                card_id: id.to_owned(),
                kind,
                feedback: fb,
                reason: reason.map(|r| r.as_str().to_owned()),
                at: now,
            });
        }
        state.rescore_open(now);
        self.cards.save(&state)
    }

    /// The desktop showed these cards.
    pub fn mark_seen(&self, ids: &[String], now: OffsetDateTime) -> Result<()> {
        let _g = self.guard();
        let mut state = self.cards.load()?;
        for id in ids {
            if let Some(c) = state.cards.get_mut(id)
                && c.state == CardState::New
            {
                c.mark_seen(now)?;
            }
        }
        self.cards.save(&state)
    }

    pub fn accept(&self, id: &str, now: OffsetDateTime) -> Result<()> {
        self.learn(id, Feedback::Accepted, None, now, |c| c.accept(now))
    }

    pub fn dismiss(&self, id: &str, reason: DismissReason, now: OffsetDateTime) -> Result<()> {
        let r = reason.clone();
        self.learn(id, Feedback::Dismissed, Some(&r), now, |c| {
            c.dismiss(reason, now)
        })
    }

    pub fn snooze(&self, id: &str, until: OffsetDateTime, now: OffsetDateTime) -> Result<()> {
        self.learn(id, Feedback::Snoozed, None, now, |c| c.snooze(until, now))
    }

    /// Mark an accepted card done (the user handled it themselves).
    pub fn complete(&self, id: &str, now: OffsetDateTime) -> Result<()> {
        self.update(id, now, |c, _| c.complete(now))
    }

    /// Prepare action `index` of a card for approval (drafts content, renders the diff).
    pub async fn prepare_action(
        &self,
        runner: &ActionRunner,
        card_id: &str,
        index: usize,
    ) -> Result<PreparedAction> {
        let card = self.card(card_id)?;
        if card.state.is_terminal() {
            return Err(Error::Invalid(format!("card {card_id} is closed")));
        }
        let action = card
            .actions
            .get(index)
            .ok_or_else(|| Error::NotFound(format!("action {index} of card {card_id}")))?;
        let context: Vec<String> = card
            .evidence
            .iter()
            .filter_map(|e| e.quote.clone())
            .collect();
        runner.prepare(card_id, index, action, &context).await
    }

    /// Execute an approved action; on success the card is accepted (if it was not) and done.
    ///
    /// The action must be the one the card offers at `prepared.action_index` (only content
    /// `prepare` fills in may differ), and only one action per card runs at a time.
    pub async fn run_action(
        &self,
        runner: &ActionRunner,
        prepared: &PreparedAction,
        approval: &Approval,
        now: OffsetDateTime,
    ) -> Result<ExecOutcome> {
        let _running = ExecutingGuard::claim(&self.executing, &prepared.card_id)?;
        let card = self.card(&prepared.card_id)?;
        if card.state.is_terminal() {
            return Err(Error::Invalid(format!("card {} is closed", card.id)));
        }
        let offered = card
            .actions
            .get(prepared.action_index)
            .is_some_and(|a| prepared.matches_offered(a));
        if !offered {
            return Err(Error::Integrity(format!(
                "card {} does not offer this action",
                card.id
            )));
        }
        let out = runner.execute(prepared, approval).await?;
        if matches!(out, ExecOutcome::Executed(_)) {
            if card.state != CardState::Accepted {
                self.accept(&card.id, now)?;
            }
            self.complete(&card.id, now)?;
        }
        Ok(out)
    }

    /// When the last [`refresh`](Self::refresh) finished, if one ever ran.
    pub fn last_refresh(&self) -> Result<Option<OffsetDateTime>> {
        Ok(self.cards.load()?.last_refresh)
    }

    pub fn metrics(&self) -> Result<Metrics> {
        Ok(Metrics::compute(&self.cards.load()?))
    }

    pub fn model(&self) -> Result<ScoringModel> {
        Ok(self.cards.load()?.model)
    }

    /// What feedback taught the ranking, strongest adjustment first.
    pub fn learned(&self, now: OffsetDateTime) -> Result<Vec<LearnedEntry>> {
        Ok(self.cards.load()?.learned.list(now))
    }

    /// Forget what feedback taught the ranking (all, or one sender, domain, kind or thread) and
    /// rescore open cards, so suppressed cards come back. Returns how many learned entries were
    /// forgotten.
    ///
    /// `All` also puts the scoring model's weights back to the prior and its update count to
    /// zero; one kind also puts that kind's model weight back to its prior. The feedback log
    /// (the audit of what the user did) is kept.
    pub fn reset_learning(&self, scope: &ResetScope, now: OffsetDateTime) -> Result<usize> {
        let _g = self.guard();
        let mut state = self.cards.load()?;
        // Legacy per-sender weights become entries first, so they are forgotten like the rest.
        state.migrate(now);
        let n = state.learned.reset(scope);
        match scope {
            ResetScope::All => state.model.reset_learned(),
            ResetScope::One(Scope::Kind, key) => {
                if let Some(kind) = feedback::parse_kind(key.trim()) {
                    state.model.reset_kind(kind);
                }
            }
            ResetScope::One(..) => {}
        }
        state.rescore_open(now);
        self.cards.save(&state)?;
        Ok(n)
    }

    /// The morning brief from the current cards and calendar (run `refresh` first).
    pub fn brief(&self, now: OffsetDateTime) -> Result<MorningBrief> {
        let corpus = self.corpus()?;
        let state = self.cards.load()?;
        Ok(brief::compose(&state, &corpus, &self.cfg_now(), now))
    }
}

/// Marks a card as executing until dropped.
struct ExecutingGuard<'a> {
    set: &'a Mutex<HashSet<String>>,
    card_id: String,
}

impl<'a> ExecutingGuard<'a> {
    fn claim(set: &'a Mutex<HashSet<String>>, card_id: &str) -> Result<Self> {
        let mut s = set.lock().unwrap_or_else(PoisonError::into_inner);
        if !s.insert(card_id.to_owned()) {
            return Err(Error::Invalid(format!(
                "an action for card {card_id} is already running"
            )));
        }
        Ok(Self {
            set,
            card_id: card_id.to_owned(),
        })
    }
}

impl Drop for ExecutingGuard<'_> {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.card_id);
    }
}

/// How much one piece of feedback moves the shared model's weights: dismissals that say
/// something about this one card (already handled, wrong info) move them less.
fn model_weight(fb: Feedback, reason: Option<&DismissReason>) -> f64 {
    match (fb, reason) {
        (Feedback::Dismissed, Some(DismissReason::AlreadyHandled)) => 0.5,
        (Feedback::Dismissed, Some(DismissReason::WrongInfo)) => 0.25,
        (Feedback::Dismissed, Some(DismissReason::Other(_))) => 0.75,
        _ => 1.0,
    }
}

/// Move the judge's suggested action (if the card offers it) to the front.
fn order_actions(card: &mut Card, suggested: Option<String>) {
    if let Some(s) = suggested
        && let Some(i) = card.actions.iter().position(|a| a.name() == s)
    {
        let a = card.actions.remove(i);
        card.actions.insert(0, a);
    }
}
