//! `proactive --backend real`: the real `nugget-proactive` crate behind the [`ProactiveEngine`]
//! adapter.
//!
//! Each engine owns an encrypted on-disk [`Store`] in a temporary directory. The world's items
//! are written into it in the shape the crate's corpus reader expects from ingest:
//!
//! - **mail** (`gmail:<user>`, `message/rfc822`): `extracted.{from, to, subject, date,
//!   message_id, in_reply_to, references}`, Gmail-style `source.{labels, thread_id}` (`SENT` for
//!   the user's own mail, `INBOX` otherwise), and the bulk headers the world carries
//!   (`list_unsubscribe`, `auto_submitted`);
//! - **receipts** become the receipt mail a merchant sends (`receipts@<merchant domain>`), with
//!   the next billing date when the receipt is for a subscription;
//! - **events** (`gcal:<user>`): Google Calendar-style `source.{start, end, attendees,
//!   organizer, status, ical_uid}`; a cancellation rewrites the event with `status: cancelled`,
//!   as a calendar sync would;
//! - **notes** (`files:notes`, Markdown, with a text chunk so related-notes search finds them).
//!
//! Each morning the adapter calls [`Proactive::refresh`] at 07:00 UTC, then shows the top `k` of
//! [`Proactive::inbox`]. Accept and dismiss go to [`Proactive::accept`] and
//! [`Proactive::dismiss`] (with the simulated user's reason: not relevant, already handled, not
//! important), so the crate learns from the same noisy user as the reference engine. Every shown
//! action is prepared through [`ActionRunner::prepare`] (the dry-run diff is the crate's rendered
//! preview) and executed through [`Proactive::run_action`] with an approval bound to that
//! preview's digest, the default [`PolicyEngine`] and sandbox executors that record what they
//! were asked to apply. No network: the drafter is a deterministic stub and
//! no LLM judge is configured.
//!
//! The crate's card kinds map to the benchmark's `(kind, subject)` through each card's first
//! evidence item, whose store key is the world item id:
//!
//! | crate kind | benchmark kind | subject |
//! |---|---|---|
//! | `unanswered_thread` | `unanswered_thread` | `thread:<world thread of the message>` |
//! | `follow_up`, `awaiting:` key (the user's unanswered question) | `follow_up` | `thread:<world thread of the message>` |
//! | `follow_up` (a promise in sent mail) | `commitment_due` | `commit:<mail id>` |
//! | `commitment` (graph, or a dated todo in a note) | `commitment_due` | `commit:<evidence item id>` |
//! | `meeting_prep` | `meeting_prep` | `event:<event id>` |
//! | `renewal` | `renewal` | `sub:<sender domain>` |

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use nugget_policy::PolicyEngine;
use nugget_proactive as np;
use nugget_proactive::{
    ActionRunner, CandidateKind, CardStore, DismissReason, ExecOutcome, Executors, Proactive,
    ProactiveConfig, ProactiveState, ScoringModel, StoreCardStore,
};
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use super::engine::{
    self, Action, ActionKind, Approval, Card, DryRun, Execution, Feedback, ProactiveEngine,
};
use super::world::{
    self, CardKind, Event, EventCancelled, Item, Mail, Note, Receipt, commit_subject,
    event_subject, iso_date, sub_subject, thread_subject,
};
use crate::corpus::Rng;
use crate::runner::BenchError;

const MAIL_SOURCE: &str = "gmail:bench";
const CAL_SOURCE: &str = "gcal:bench";
const NOTE_SOURCE: &str = "files:notes";
/// The morning brief is composed at this hour (UTC) of each simulated day.
pub const BRIEF_HOUR: i64 = 7;

/// Midnight UTC of a world day.
fn day_start(day: i32) -> OffsetDateTime {
    let (y, m, d) = world::START_DATE;
    let date = time::Date::from_calendar_date(y, time::Month::try_from(m).expect("month"), d)
        .expect("start date");
    date.midnight().assume_utc() + Duration::days(i64::from(day) - 1)
}

/// When the brief for `day` is composed.
pub fn brief_time(day: i32) -> OffsetDateTime {
    day_start(day) + Duration::hours(BRIEF_HOUR)
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

fn domain(addr: &str) -> &str {
    addr.rsplit_once('@').map_or("", |(_, d)| d)
}

fn store_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Card persistence that never keeps what was learned: the learning-off ablation. Feedback still
/// moves cards through the state machine (so dedup and tombstones work), but every save resets
/// the scoring model to its prior and forgets the per sender, domain, kind and thread entries.
struct FrozenModel(StoreCardStore);

impl CardStore for FrozenModel {
    fn load(&self) -> nugget_core::Result<ProactiveState> {
        self.0.load()
    }

    fn save(&self, state: &ProactiveState) -> nugget_core::Result<()> {
        let mut s = state.clone();
        s.model = ScoringModel::default();
        s.learned = np::Learned::default();
        self.0.save(&s)
    }
}

/// What the sandbox executors were asked to apply, in order: the rendered preview of the action
/// they received (the crate's own diff renderer, so fidelity compares like with like).
#[derive(Clone, Default)]
struct Sandbox {
    applied: Arc<Mutex<Vec<(ActionKind, String)>>>,
}

impl Sandbox {
    fn record(&self, action: np::Action) -> String {
        let mut log = self.applied.lock().unwrap_or_else(PoisonError::into_inner);
        log.push((action_kind(&action), action.preview().render()));
        format!("sandbox-{}", log.len())
    }

    fn len(&self) -> usize {
        self.applied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn since(&self, n: usize) -> Vec<(ActionKind, String)> {
        self.applied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(n..)
            .map(<[_]>::to_vec)
            .unwrap_or_default()
    }
}

#[async_trait]
impl np::MailExecutor for Sandbox {
    async fn save_draft(&self, draft: &np::DraftReply) -> nugget_core::Result<String> {
        Ok(self.record(np::Action::DraftReply(draft.clone())))
    }
    async fn archive_thread(&self, archive: &np::ArchiveThread) -> nugget_core::Result<String> {
        Ok(self.record(np::Action::ArchiveThread(archive.clone())))
    }
}

#[async_trait]
impl np::CalendarExecutor for Sandbox {
    async fn create_event(&self, event: &np::CreateEvent) -> nugget_core::Result<String> {
        Ok(self.record(np::Action::CreateEvent(event.clone())))
    }
}

#[async_trait]
impl np::ReminderExecutor for Sandbox {
    async fn create_reminder(&self, reminder: &np::Reminder) -> nugget_core::Result<String> {
        Ok(self.record(np::Action::Reminder(reminder.clone())))
    }
}

#[async_trait]
impl np::NoteExecutor for Sandbox {
    async fn write_note(&self, note: &np::ResearchNote) -> nugget_core::Result<String> {
        Ok(self.record(np::Action::ResearchNote(note.clone())))
    }
}

/// A deterministic drafter: the reply states the card's intent. No model, no network.
struct StubDrafter;

#[async_trait]
impl np::Drafter for StubDrafter {
    async fn draft_reply(&self, req: &np::DraftRequest) -> nugget_core::Result<String> {
        Ok(format!(
            "Hi,\n\n{}\n\nBest,\nSam",
            req.intent.trim().trim_end_matches('.')
        ))
    }
}

/// The simulated user's dismiss reason as the crate's (the Today page's) reason.
pub fn dismiss_reason(reason: engine::DismissReason) -> DismissReason {
    match reason {
        engine::DismissReason::NotRelevant => DismissReason::NotRelevant,
        engine::DismissReason::AlreadyHandled => DismissReason::AlreadyHandled,
        engine::DismissReason::NotImportant => DismissReason::NotImportant,
        engine::DismissReason::WrongInfo => DismissReason::WrongInfo,
    }
}

fn action_kind(a: &np::Action) -> ActionKind {
    match a {
        np::Action::DraftReply(_) => ActionKind::DraftReply,
        np::Action::CreateEvent(_) => ActionKind::CreateEvent,
        np::Action::Reminder(_) => ActionKind::CreateReminder,
        np::Action::ArchiveThread(_) => ActionKind::ArchiveThread,
        np::Action::ResearchNote(_) => ActionKind::CreatePrepNote,
    }
}

/// The real crate as a [`ProactiveEngine`].
pub struct RealEngine {
    learning: bool,
    user: String,
    rt: tokio::runtime::Runtime,
    store: Store,
    proactive: Proactive,
    runner: ActionRunner,
    sandbox: Sandbox,
    /// World mail id → world thread id.
    mail_thread: HashMap<String, String>,
    /// World mail id → sender domain.
    mail_domain: HashMap<String, String>,
    /// World thread → message ids so far, oldest first.
    thread_msgs: HashMap<String, Vec<String>>,
    events: HashMap<String, Event>,
    seq: u32,
    now: OffsetDateTime,
    /// Card id → its first action, prepared (content filled in, preview rendered).
    prepared: HashMap<String, np::PreparedAction>,
    /// Applied state of each action target, for `before`.
    applied: HashMap<String, String>,
    audit: Vec<Execution>,
    errors: Vec<String>,
    // Last: removed after the store is dropped.
    _home: tempfile::TempDir,
}

impl RealEngine {
    /// A fresh encrypted store in a temporary directory. `learning: false` is the ablation: the
    /// scoring model stays at its prior.
    pub fn new(user: &str, learning: bool, seed: u64) -> Result<Self, BenchError> {
        let home = tempfile::Builder::new()
            .prefix("nugget-bench-proactive-")
            .tempdir()?;
        let mut rng = Rng::new(seed ^ 0x7072_6F61_6374);
        let mut key = [0u8; 32];
        for chunk in key.chunks_mut(8) {
            chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
        }
        let fail = |e: nugget_core::Error| BenchError::Failed(format!("nugget-proactive: {e}"));
        let store =
            Store::open(home.path().join("nugget.db"), &StoreKey::from_bytes(key)).map_err(fail)?;
        if !store.is_encrypted() {
            return Err(BenchError::Failed("store is not encrypted".into()));
        }
        let cfg = ProactiveConfig {
            user_addresses: vec![user.to_owned()],
            ..ProactiveConfig::default()
        };
        let mut proactive = Proactive::new(store.clone(), cfg);
        if !learning {
            proactive = proactive
                .with_card_store(Arc::new(FrozenModel(StoreCardStore::new(store.clone()))));
        }
        let sandbox = Sandbox::default();
        let executors = Executors {
            drafter: Some(Arc::new(StubDrafter)),
            mail: Some(Arc::new(sandbox.clone())),
            calendar: Some(Arc::new(sandbox.clone())),
            reminders: Some(Arc::new(sandbox.clone())),
            notes: Some(Arc::new(sandbox.clone())),
        };
        let runner = ActionRunner::new(Arc::new(PolicyEngine::default()), executors)
            .with_audit(store.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self {
            learning,
            user: user.to_owned(),
            rt,
            store,
            proactive,
            runner,
            sandbox,
            mail_thread: HashMap::new(),
            mail_domain: HashMap::new(),
            thread_msgs: HashMap::new(),
            events: HashMap::new(),
            seq: 0,
            now: brief_time(1),
            prepared: HashMap::new(),
            applied: HashMap::new(),
            audit: Vec::new(),
            errors: Vec::new(),
            _home: home,
        })
    }

    /// Store and crate errors seen while driving the engine (the trait's methods cannot fail).
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The crate under test, for assertions on what it learned.
    pub fn proactive(&self) -> &Proactive {
        &self.proactive
    }

    /// The crate's own metrics (precision = accepted / shown, by kind).
    pub fn crate_metrics(&self) -> Option<np::Metrics> {
        self.proactive.metrics().ok()
    }

    fn put(&mut self, item: &NewItem, chunk: Option<&str>) {
        let r = match chunk {
            Some(text) => self
                .store
                .upsert_item_with_chunks(item, &[NewChunk::text(text)])
                .map(|_| ()),
            None => self.store.upsert_item(item).map(|_| ()),
        };
        if let Err(e) = r {
            self.errors.push(format!("store {}: {e}", item.item.key));
        }
    }

    /// A time on `day` for the next item: working hours, spread by arrival order.
    fn arrival(&mut self, day: i32) -> OffsetDateTime {
        self.seq += 1;
        day_start(day) + Duration::hours(8 + i64::from(self.seq % 10)) + Duration::minutes(7)
    }

    fn write_mail(
        &mut self,
        m: &Mail,
        date: OffsetDateTime,
        labels: &[&str],
        thread: &str,
        extra: &[(&str, Value)],
    ) {
        let message_id = format!("<{}@mail.bench.test>", m.id);
        let prior = self.thread_msgs.entry(thread.to_owned()).or_default();
        let mut extracted = json!({
            "from": m.from,
            "to": m.to,
            "subject": m.subject,
            "date": rfc3339(date),
            "message_id": message_id,
        });
        if let Some(last) = prior.last() {
            extracted["in_reply_to"] = json!([last]);
            extracted["references"] = json!(prior);
        }
        prior.push(message_id);
        for (k, v) in extra {
            extracted[*k] = v.clone();
        }
        let meta = json!({
            "source": {"labels": labels, "thread_id": thread},
            "mime": "message/rfc822",
            "extracted": extracted,
            "sensitive": false,
        });
        let item = NewItem::new(
            MAIL_SOURCE,
            m.id.as_str(),
            content_hash(format!("{}\n{}\n{}", m.id, m.subject, m.body)),
        )
        .with_title(m.subject.as_str())
        .with_mime("message/rfc822")
        .with_text(m.body.as_str())
        .with_modified_at(date)
        .with_metadata(meta);
        self.mail_thread.insert(m.id.clone(), thread.to_owned());
        self.mail_domain
            .insert(m.id.clone(), domain(&m.from).to_lowercase());
        self.put(&item, None);
    }

    fn observe_mail(&mut self, m: &Mail) {
        let date = self.arrival(m.day);
        let outbound = m.from.eq_ignore_ascii_case(&self.user);
        let mut extra = Vec::new();
        if m.list_unsubscribe {
            extra.push((
                "list_unsubscribe",
                json!(format!("<mailto:unsubscribe@{}>", domain(&m.from))),
            ));
        }
        if m.auto_submitted {
            extra.push(("auto_submitted", json!("auto-generated")));
        }
        let labels: &[&str] = if outbound { &["SENT"] } else { &["INBOX"] };
        self.write_mail(m, date, labels, &m.thread.clone(), &extra);
    }

    /// The receipt mail a merchant sends for this purchase.
    fn observe_receipt(&mut self, r: &Receipt) {
        let date = self.arrival(r.day);
        let mut body = format!(
            "Thanks for your purchase. {}: ${}.{:02}.",
            r.description,
            r.amount_cents / 100,
            r.amount_cents % 100
        );
        if let (Some(cadence), Some(renews)) = (r.cadence, r.renews_on) {
            let c = match cadence {
                world::Cadence::Monthly => "monthly",
                world::Cadence::Annual => "annual",
            };
            body.push_str(&format!(
                " Your {c} subscription renews on {}.",
                iso_date(renews)
            ));
        }
        let mail = Mail {
            id: r.id.clone(),
            day: r.day,
            thread: format!("receipt-{}", r.id),
            from: format!("receipts@{}", r.merchant_domain),
            to: vec![self.user.clone()],
            subject: format!("Your receipt from {}", r.merchant),
            body,
            list_unsubscribe: false,
            auto_submitted: false,
        };
        let thread = mail.thread.clone();
        self.write_mail(&mail, date, &["INBOX"], &thread, &[]);
    }

    fn write_event(&mut self, e: &Event, cancelled: bool) {
        let start = day_start(e.start_day) + Duration::hours(i64::from(e.start_hour));
        let end = start + Duration::minutes(i64::from(e.minutes));
        let attendees: Vec<Value> = e
            .attendees
            .iter()
            .map(|a| json!({"email": a, "resource": false}))
            .collect();
        let meta = json!({
            "source": {
                "event_id": e.id,
                "ical_uid": format!("{}@cal.bench.test", e.id),
                "status": if cancelled { "cancelled" } else { "confirmed" },
                "start": {"date_time": rfc3339(start)},
                "end": {"date_time": rfc3339(end)},
                "attendees": attendees,
                "organizer": {"email": e.organizer},
                "location": e.location,
                "recurring_event_id": e.series,
            },
            "mime": "text/plain",
            "sensitive": false,
        });
        let text = format!(
            "{}\n{}\nAttendees: {}",
            e.title,
            e.description,
            e.attendees.join(", ")
        );
        let item = NewItem::new(
            CAL_SOURCE,
            e.id.as_str(),
            content_hash(format!("{}\n{text}\n{cancelled}", e.id)),
        )
        .with_title(e.title.as_str())
        .with_mime("text/plain")
        .with_text(text)
        .with_metadata(meta);
        self.put(&item, None);
    }

    fn observe_cancellation(&mut self, c: &EventCancelled) {
        match self.events.get(&c.event).cloned() {
            Some(e) => self.write_event(&e, true),
            None => self
                .errors
                .push(format!("cancellation {} of an unknown event", c.id)),
        }
    }

    fn observe_note(&mut self, n: &Note) {
        let item = NewItem::new(
            NOTE_SOURCE,
            n.id.as_str(),
            content_hash(format!("{}\n{}", n.title, n.body)),
        )
        .with_title(n.title.as_str())
        .with_mime("text/markdown")
        .with_text(n.body.as_str())
        .with_modified_at(self.arrival(n.day))
        .with_metadata(json!({"mime": "text/markdown", "sensitive": false}));
        let body = n.body.clone();
        self.put(&item, Some(&body));
    }

    /// The benchmark's `(kind, subject)` for a crate card.
    fn map_card(&self, c: &np::Card) -> (CardKind, String) {
        let key = c
            .evidence
            .first()
            .map(|e| e.item.key.as_str().to_owned())
            .unwrap_or_else(|| c.id.clone());
        match c.kind {
            CandidateKind::UnansweredThread => (
                CardKind::UnansweredThread,
                thread_subject(self.mail_thread.get(&key).unwrap_or(&key)),
            ),
            // The user's unanswered question (`awaiting:` cards) is the benchmark's follow-up.
            CandidateKind::FollowUp if c.dedup_key.starts_with("awaiting:") => (
                CardKind::FollowUp,
                thread_subject(self.mail_thread.get(&key).unwrap_or(&key)),
            ),
            CandidateKind::FollowUp | CandidateKind::Commitment => {
                (CardKind::CommitmentDue, commit_subject(&key))
            }
            CandidateKind::MeetingPrep => (CardKind::MeetingPrep, event_subject(&key)),
            CandidateKind::Renewal => {
                let d = self.mail_domain.get(&key).cloned().unwrap_or_else(|| {
                    c.people
                        .first()
                        .map(|p| domain(p).to_owned())
                        .unwrap_or_default()
                });
                (CardKind::Renewal, sub_subject(&d))
            }
        }
    }

    fn target(prepared: &np::PreparedAction) -> String {
        format!("{}/{}", prepared.preview.target, prepared.card_id)
    }
}

impl ProactiveEngine for RealEngine {
    fn name(&self) -> &'static str {
        if self.learning {
            "nugget-proactive"
        } else {
            "nugget-proactive_frozen"
        }
    }

    fn observe(&mut self, item: &Item) {
        match item {
            Item::Mail(m) => self.observe_mail(m),
            Item::Event(e) => {
                self.events.insert(e.id.clone(), e.clone());
                self.write_event(e, false);
            }
            Item::EventCancelled(c) => self.observe_cancellation(c),
            Item::Note(n) => self.observe_note(n),
            Item::Receipt(r) => self.observe_receipt(r),
        }
    }

    fn brief(&mut self, day: i32, k: usize) -> Vec<Card> {
        self.now = brief_time(day);
        let now = self.now;
        if let Err(e) = self.rt.block_on(self.proactive.refresh(now)) {
            self.errors.push(format!("refresh day {day}: {e}"));
            return Vec::new();
        }
        let inbox = match self.proactive.inbox() {
            Ok(v) => v,
            Err(e) => {
                self.errors.push(format!("inbox day {day}: {e}"));
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for c in inbox.into_iter().take(k) {
            let (kind, subject) = self.map_card(&c);
            let action = if c.actions.is_empty() {
                None
            } else {
                let prepared =
                    self.rt
                        .block_on(self.proactive.prepare_action(&self.runner, &c.id, 0));
                match prepared {
                    Ok(p) => {
                        let target = Self::target(&p);
                        let dry_run = DryRun {
                            before: self.applied.get(&target).cloned().unwrap_or_default(),
                            after: p.preview.render(),
                            target,
                        };
                        let kind = action_kind(&p.action);
                        self.prepared.insert(c.id.clone(), p);
                        Some(Action {
                            kind,
                            dry_run: Some(dry_run),
                        })
                    }
                    Err(e) => {
                        self.errors.push(format!("prepare {}: {e}", c.id));
                        Some(Action {
                            kind: action_kind(&c.actions[0]),
                            dry_run: None,
                        })
                    }
                }
            };
            out.push(Card {
                id: c.id.clone(),
                kind,
                subject,
                title: c.title.clone(),
                score: c.rank,
                action,
            });
        }
        out
    }

    fn feedback(&mut self, card_id: &str, feedback: Feedback) {
        let r = match feedback {
            Feedback::Accepted => self.proactive.accept(card_id, self.now),
            Feedback::Dismissed(reason) => {
                self.proactive
                    .dismiss(card_id, dismiss_reason(reason), self.now)
            }
        };
        if let Err(e) = r {
            self.errors.push(format!("feedback {card_id}: {e}"));
        }
    }

    fn execute(&mut self, card_id: &str, approval: Option<&Approval>) -> Result<Execution, String> {
        let prepared = self
            .prepared
            .get(card_id)
            .cloned()
            .ok_or_else(|| format!("card {card_id} has no prepared action"))?;
        // The user approved the preview of the card the approval names. With no approval the
        // adapter still asks the crate, with an approval bound to nothing, so the crate's gate
        // (not the adapter) is what refuses.
        let crate_approval = match approval {
            Some(a) => self
                .prepared
                .get(&a.card_id)
                .map(|p| p.approve(true))
                .unwrap_or(np::Approval {
                    digest: String::new(),
                    confirmed: true,
                }),
            None => np::Approval {
                digest: String::new(),
                confirmed: false,
            },
        };
        let before_log = self.sandbox.len();
        let outcome = self.rt.block_on(self.proactive.run_action(
            &self.runner,
            &prepared,
            &crate_approval,
            self.now,
        ));
        // Whatever the executors applied during this call is audited, refused or not.
        let target = Self::target(&prepared);
        let mut result = Err(String::from("nothing executed"));
        for (kind, after) in self.sandbox.since(before_log) {
            let before = self.applied.insert(target.clone(), after.clone());
            let exec = Execution {
                card_id: card_id.to_owned(),
                action: kind,
                approval: approval.map(|a| a.token),
                applied: DryRun {
                    target: target.clone(),
                    before: before.unwrap_or_default(),
                    after,
                },
            };
            self.audit.push(exec.clone());
            result = Ok(exec);
        }
        match outcome.map_err(store_err)? {
            ExecOutcome::Executed(_) => result,
            ExecOutcome::NeedsConfirmation(r) => Err(format!("needs confirmation: {r}")),
            ExecOutcome::Denied(r) => Err(format!("denied: {r}")),
            ExecOutcome::Halted => Err("halted".into()),
        }
    }

    fn audit_log(&self) -> Vec<Execution> {
        self.audit.clone()
    }
}
