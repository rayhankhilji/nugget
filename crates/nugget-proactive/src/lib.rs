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
//!    dismiss and snooze; explainable (top factors on every card).
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
pub mod generators;
pub mod judge;
pub mod score;
pub mod text;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use nugget_core::{Error, Result};
use nugget_graph::GraphStore;
use nugget_store::Store;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use actions::{
    Action, ActionPreview, ActionRunner, Approval, ArchiveThread, CalendarExecutor, CreateEvent,
    DiffLine, DiffOp, DraftReply, DraftRequest, Drafter, ExecOutcome, Executors, MailExecutor,
    NoteExecutor, PreparedAction, Reminder, ReminderExecutor, ResearchNote,
};
pub use brief::{BriefCard, BriefMeeting, MorningBrief};
pub use candidate::{Candidate, CandidateKind, EvidenceRef, Features};
pub use cards::{
    Card, CardState, CardStore, DismissReason, MemCardStore, Metrics, ProactiveState,
    StoreCardStore,
};
pub use config::ProactiveConfig;
pub use corpus::Corpus;
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
    cards: Arc<dyn CardStore>,
    judge: Option<Arc<dyn Judge>>,
    /// Serialises load-modify-save of the card state.
    lock: Mutex<()>,
}

impl Proactive {
    /// Cards persisted in `store`, graph from `store`.
    pub fn new(store: Store, cfg: ProactiveConfig) -> Self {
        Self {
            graph: Some(Arc::new(store.graph())),
            cards: Arc::new(StoreCardStore::new(store.clone())),
            store,
            cfg,
            judge: None,
            lock: Mutex::new(()),
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

    pub fn config(&self) -> &ProactiveConfig {
        &self.cfg
    }

    pub fn corpus(&self) -> Result<Corpus> {
        Corpus::load(&self.store, &self.cfg)
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
            cfg: &self.cfg,
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
                .map(|c| (c, snapshot.model.score(&c.features)))
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
            card.rescore(&state.model);
        }
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
            .filter(|c| c.state.is_open())
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
        f: impl FnOnce(&mut Card, &mut ScoringModel) -> Result<T>,
    ) -> Result<T> {
        let _g = self.guard();
        let mut state = self.cards.load()?;
        let ProactiveState { cards, model, .. } = &mut state;
        let card = cards
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("card {id}")))?;
        let out = f(card, model)?;
        let model = state.model.clone();
        for c in state.cards.values_mut().filter(|c| !c.state.is_terminal()) {
            c.rescore(&model);
        }
        self.cards.save(&state)?;
        Ok(out)
    }

    fn learn(
        &self,
        id: &str,
        fb: Feedback,
        now: OffsetDateTime,
        f: impl FnOnce(&mut Card) -> Result<()>,
    ) -> Result<()> {
        let _g = self.guard();
        let mut state = self.cards.load()?;
        let card = state
            .cards
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("card {id}")))?;
        let already_accepted = card.was_accepted;
        f(card)?;
        let (features, kind) = (card.features.clone(), card.kind);
        // One accept per card counts; repeated accepts are no-ops.
        if !(fb == Feedback::Accepted && already_accepted) {
            state.model.update(&features, fb);
            state.feedback_log.push(cards::FeedbackEvent {
                card_id: id.to_owned(),
                kind,
                feedback: fb,
                at: now,
            });
        }
        let model = state.model.clone();
        for c in state.cards.values_mut().filter(|c| !c.state.is_terminal()) {
            c.rescore(&model);
        }
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
        self.learn(id, Feedback::Accepted, now, |c| c.accept(now))
    }

    pub fn dismiss(&self, id: &str, reason: DismissReason, now: OffsetDateTime) -> Result<()> {
        self.learn(id, Feedback::Dismissed, now, |c| c.dismiss(reason, now))
    }

    pub fn snooze(&self, id: &str, until: OffsetDateTime, now: OffsetDateTime) -> Result<()> {
        self.learn(id, Feedback::Snoozed, now, |c| c.snooze(until, now))
    }

    /// Mark an accepted card done (the user handled it themselves).
    pub fn complete(&self, id: &str, now: OffsetDateTime) -> Result<()> {
        self.update(id, |c, _| c.complete(now))
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
        runner.prepare(card_id, action, &context).await
    }

    /// Execute an approved action; on success the card is accepted (if it was not) and done.
    pub async fn run_action(
        &self,
        runner: &ActionRunner,
        prepared: &PreparedAction,
        approval: &Approval,
        now: OffsetDateTime,
    ) -> Result<ExecOutcome> {
        let card = self.card(&prepared.card_id)?;
        if card.state.is_terminal() {
            return Err(Error::Invalid(format!("card {} is closed", card.id)));
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

    /// The morning brief from the current cards and calendar (run `refresh` first).
    pub fn brief(&self, now: OffsetDateTime) -> Result<MorningBrief> {
        let corpus = self.corpus()?;
        let state = self.cards.load()?;
        Ok(brief::compose(&state, &corpus, &self.cfg, now))
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
