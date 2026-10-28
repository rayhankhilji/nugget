//! Typed actions with dry-run previews, policy-gated execution after explicit approval.
//!
//! The flow is always: [`ActionRunner::prepare`] (fills in generated content, e.g. a drafted
//! reply, and renders the [`ActionPreview`] the user sees) → the user approves that exact preview
//! ([`PreparedAction::approve`] returns an [`Approval`] bound to its digest) →
//! [`ActionRunner::execute`] (verifies the digest, asks the [`PolicyGate`], then calls the
//! executor). Nothing executes without an approval whose digest matches what was previewed.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use nugget_core::ids::ItemRef;
use nugget_core::{Error, Result};
use nugget_policy::{ActionDescriptor, Decision, PolicyGate, RiskClass};
use nugget_store::{AuditEntry, Store};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::text::truncate;

/// Draft a reply for the user to copy or open in their mail app. Never sends, and nothing is
/// written to the mailbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftReply {
    /// The message replied to.
    pub reply_to: ItemRef,
    pub in_reply_to_message_id: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    /// What the reply should do (fed to the drafter).
    pub intent: String,
    /// Filled by [`ActionRunner::prepare`] from the [`Drafter`] when empty.
    pub body: Option<String>,
}

/// Create a calendar event; invitations go out when it has attendees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateEvent {
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
    pub attendees: Vec<String>,
    pub description: Option<String>,
}

/// A local reminder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    pub text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub about: Option<ItemRef>,
}

/// Archive a mail thread (remove it from the inbox; nothing is deleted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveThread {
    pub messages: Vec<ItemRef>,
    pub subject: String,
}

/// Write a local note (e.g. meeting prep) into the vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchNote {
    pub title: String,
    /// Markdown body.
    pub body: String,
    pub sources: Vec<ItemRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    DraftReply(DraftReply),
    CreateEvent(CreateEvent),
    Reminder(Reminder),
    ArchiveThread(ArchiveThread),
    ResearchNote(ResearchNote),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffOp {
    Add,
    Remove,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub op: DiffOp,
    pub text: String,
}

/// What an action would change, for the user to read before approving.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionPreview {
    /// e.g. "Save a draft reply to Ana".
    pub summary: String,
    pub risk: RiskClass,
    /// Where the change lands, e.g. `mail:drafts`, `calendar`.
    pub target: String,
    pub diff: Vec<DiffLine>,
}

impl ActionPreview {
    /// Unified-diff style text (`+`, `-`, two spaces for context).
    pub fn render(&self) -> String {
        let mut s = format!(
            "# {} [{}] → {}\n",
            self.summary,
            risk_name(self.risk),
            self.target
        );
        for l in &self.diff {
            let p = match l.op {
                DiffOp::Add => "+ ",
                DiffOp::Remove => "- ",
                DiffOp::Context => "  ",
            };
            s.push_str(p);
            s.push_str(&l.text);
            s.push('\n');
        }
        s
    }
}

pub fn risk_name(r: RiskClass) -> &'static str {
    match r {
        RiskClass::Observe => "observe",
        RiskClass::Edit => "edit",
        RiskClass::Send => "send",
        RiskClass::Destroy => "destroy",
        RiskClass::Spend => "spend",
        RiskClass::Security => "security",
    }
}

fn add(text: impl Into<String>) -> DiffLine {
    DiffLine {
        op: DiffOp::Add,
        text: text.into(),
    }
}

fn ctx(text: impl Into<String>) -> DiffLine {
    DiffLine {
        op: DiffOp::Context,
        text: text.into(),
    }
}

fn rm(text: impl Into<String>) -> DiffLine {
    DiffLine {
        op: DiffOp::Remove,
        text: text.into(),
    }
}

fn fmt_time(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| t.to_string())
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DraftReply(_) => "draft_reply",
            Self::CreateEvent(_) => "create_event",
            Self::Reminder(_) => "reminder",
            Self::ArchiveThread(_) => "archive_thread",
            Self::ResearchNote(_) => "research_note",
        }
    }

    /// The executor's declared risk (the policy engine may raise it).
    pub fn risk(&self) -> RiskClass {
        match self {
            // A draft stays on the device; nothing leaves it until the user sends it themselves.
            Self::DraftReply(_) => RiskClass::Edit,
            Self::CreateEvent(e) if !e.attendees.is_empty() => RiskClass::Send,
            Self::CreateEvent(_) | Self::Reminder(_) | Self::ResearchNote(_) => RiskClass::Edit,
            // Changes the mailbox on the server; also always needs explicit confirmation
            // ([`Action::requires_confirmation`]).
            Self::ArchiveThread(_) => RiskClass::Destroy,
        }
    }

    /// A reason the user must explicitly confirm before this action runs, whatever the policy
    /// says ([`ActionRunner::execute`] answers [`ExecOutcome::NeedsConfirmation`] until the
    /// approval is `confirmed`).
    pub fn requires_confirmation(&self) -> Option<&'static str> {
        match self {
            Self::ArchiveThread(_) => Some(
                "Archiving changes your mailbox: the thread leaves the inbox on the mail server \
                 (nothing is deleted).",
            ),
            _ => None,
        }
    }

    fn target(&self) -> (&'static str, &'static str) {
        match self {
            Self::DraftReply(_) => ("mail", "save_draft"),
            Self::CreateEvent(_) => ("calendar", "create_event"),
            Self::Reminder(_) => ("reminders", "create_reminder"),
            Self::ArchiveThread(_) => ("mail", "archive_thread"),
            Self::ResearchNote(_) => ("vault", "write_note"),
        }
    }

    /// The dry-run diff.
    pub fn preview(&self) -> ActionPreview {
        let risk = self.risk();
        let (summary, target, diff) = match self {
            Self::DraftReply(d) => {
                let mut diff = vec![add(format!("To: {}", d.to.join(", ")))];
                if !d.cc.is_empty() {
                    diff.push(add(format!("Cc: {}", d.cc.join(", "))));
                }
                diff.push(add(format!("Subject: {}", d.subject)));
                diff.push(add(""));
                match &d.body {
                    Some(b) => diff.extend(b.lines().map(add)),
                    None => diff.push(ctx(format!("(body to be drafted: {})", d.intent))),
                }
                diff.push(ctx(
                    "Nothing is sent: copy the draft or open it in your mail app to send it yourself.",
                ));
                (
                    format!("Draft a reply to {}", d.to.join(", ")),
                    "mail:compose".to_owned(),
                    diff,
                )
            }
            Self::CreateEvent(e) => {
                let mut diff = vec![
                    add(format!("Event: {}", e.title)),
                    add(format!("When: {} – {}", fmt_time(e.start), fmt_time(e.end))),
                ];
                if !e.attendees.is_empty() {
                    diff.push(add(format!("Invite: {}", e.attendees.join(", "))));
                }
                if let Some(d) = &e.description {
                    diff.extend(d.lines().map(add));
                }
                if !e.attendees.is_empty() {
                    diff.push(ctx("Invitations will be sent to the attendees."));
                }
                (
                    format!("Create calendar event \"{}\"", e.title),
                    "calendar".to_owned(),
                    diff,
                )
            }
            Self::Reminder(r) => (
                format!("Remind me {}", fmt_time(r.at)),
                "reminders".to_owned(),
                vec![add(format!("Reminder at {}: {}", fmt_time(r.at), r.text))],
            ),
            Self::ArchiveThread(a) => (
                format!("Archive \"{}\"", a.subject),
                "mail:inbox".to_owned(),
                vec![
                    rm(format!(
                        "Inbox: {} ({} message{})",
                        a.subject,
                        a.messages.len(),
                        if a.messages.len() == 1 { "" } else { "s" }
                    )),
                    add(format!("Archive: {}", a.subject)),
                    ctx("Messages are kept; they only leave the inbox."),
                ],
            ),
            Self::ResearchNote(n) => {
                let mut diff = vec![add(format!("New note: {}", n.title))];
                diff.extend(n.body.lines().map(add));
                (
                    format!("Write note \"{}\"", n.title),
                    "vault".to_owned(),
                    diff,
                )
            }
        };
        ActionPreview {
            summary,
            risk,
            target,
            diff,
        }
    }

    /// What the policy engine is asked about.
    pub fn descriptor(&self, task_id: &str) -> ActionDescriptor {
        let (app, verb) = self.target();
        let (label, text) = match self {
            Self::DraftReply(d) => (d.subject.clone(), d.body.clone().unwrap_or_default()),
            Self::CreateEvent(e) => (e.title.clone(), e.attendees.join(", ")),
            Self::Reminder(r) => (r.text.clone(), r.text.clone()),
            Self::ArchiveThread(a) => (a.subject.clone(), String::new()),
            Self::ResearchNote(n) => (n.title.clone(), n.body.clone()),
        };
        ActionDescriptor {
            target_app: app.into(),
            verb: verb.into(),
            target_label: Some(truncate(&label, 120)),
            text_preview: (!text.is_empty()).then(|| truncate(&text, 200)),
            declared_risk: self.risk(),
            task_id: task_id.into(),
        }
    }
}

// ---- executors ----

/// What the drafter gets to write a reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftRequest {
    pub reply_to: ItemRef,
    pub to: Vec<String>,
    pub subject: String,
    pub intent: String,
    /// Evidence quotes from the card.
    pub context: Vec<String>,
}

/// Writes reply text. Implemented by `nugget-draft` (voice profile, retrieval, sensitive gate).
#[async_trait]
pub trait Drafter: Send + Sync {
    async fn draft_reply(&self, req: &DraftRequest) -> Result<String>;
}

/// Mailbox changes (Gmail / IMAP connectors).
#[async_trait]
pub trait MailExecutor: Send + Sync {
    /// Keep a draft for the user to copy or open in their mail app; returns the draft's id.
    /// Must never send it, nor write it to the mailbox.
    async fn save_draft(&self, draft: &DraftReply) -> Result<String>;
    async fn archive_thread(&self, archive: &ArchiveThread) -> Result<String>;
}

#[async_trait]
pub trait CalendarExecutor: Send + Sync {
    /// Returns the new event's id.
    async fn create_event(&self, event: &CreateEvent) -> Result<String>;
}

#[async_trait]
pub trait ReminderExecutor: Send + Sync {
    async fn create_reminder(&self, reminder: &Reminder) -> Result<String>;
}

#[async_trait]
pub trait NoteExecutor: Send + Sync {
    /// Returns the note's path or id.
    async fn write_note(&self, note: &ResearchNote) -> Result<String>;
}

/// The executors available; a missing one makes its actions `Unsupported`.
#[derive(Clone, Default)]
pub struct Executors {
    pub drafter: Option<Arc<dyn Drafter>>,
    pub mail: Option<Arc<dyn MailExecutor>>,
    pub calendar: Option<Arc<dyn CalendarExecutor>>,
    pub reminders: Option<Arc<dyn ReminderExecutor>>,
    pub notes: Option<Arc<dyn NoteExecutor>>,
}

/// An action ready for approval: content filled in and its preview rendered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedAction {
    pub card_id: String,
    /// Position of the action in the card's actions.
    pub action_index: usize,
    pub action: Action,
    pub preview: ActionPreview,
    /// HMAC-SHA-256 (keyed by the [`ActionRunner`]) over the card, the action's position, the
    /// action and its preview; approvals are bound to it, and only the runner can mint one.
    pub digest: String,
}

impl PreparedAction {
    /// Whether this is the card's offered `action`, ignoring content `prepare` filled in (a
    /// drafted reply body).
    pub fn matches_offered(&self, offered: &Action) -> bool {
        match (&self.action, offered) {
            (Action::DraftReply(mine), Action::DraftReply(theirs)) if theirs.body.is_none() => {
                let mut mine = mine.clone();
                mine.body = None;
                &mine == theirs
            }
            (mine, theirs) => mine == theirs,
        }
    }

    /// The user approved this exact preview. `confirmed` answers a policy `Confirm` (the UI
    /// shows the policy's reason and the user confirmed it).
    pub fn approve(&self, confirmed: bool) -> Approval {
        Approval {
            digest: self.digest.clone(),
            confirmed,
        }
    }
}

/// Length of the key approvals are bound with.
pub const APPROVAL_KEY_LEN: usize = 32;

/// A fresh random key for [`ActionRunner::with_key`].
pub fn new_approval_key() -> Result<[u8; APPROVAL_KEY_LEN]> {
    let mut key = [0u8; APPROVAL_KEY_LEN];
    getrandom::fill(&mut key).map_err(|e| Error::Other(format!("approval key: {e}")))?;
    Ok(key)
}

/// Approvals remembered as used (older ones belong to closed cards, which refuse actions).
const USED_APPROVALS: usize = 4096;

fn digest(
    key: &[u8; APPROVAL_KEY_LEN],
    card_id: &str,
    index: usize,
    action: &Action,
    preview: &ActionPreview,
) -> Result<String> {
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .map_err(|e| Error::Other(format!("approval key: {e}")))?;
    h.update(card_id.as_bytes());
    h.update(&[0]);
    h.update(index.to_string().as_bytes());
    h.update(&[0]);
    h.update(&serde_json::to_vec(action)?);
    h.update(&[0]);
    h.update(&serde_json::to_vec(preview)?);
    Ok(hex::encode(h.finalize().into_bytes()))
}

/// Digests of approvals that executed or are executing, oldest first.
#[derive(Default)]
struct UsedApprovals {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl UsedApprovals {
    /// Claim `digest`; false when it was already used.
    fn claim(&mut self, digest: &str) -> bool {
        if !self.set.insert(digest.to_owned()) {
            return false;
        }
        self.order.push_back(digest.to_owned());
        while self.order.len() > USED_APPROVALS {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    fn release(&mut self, digest: &str) {
        if self.set.remove(digest) {
            self.order.retain(|d| d != digest);
        }
    }
}

/// The user's explicit approval of one prepared action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub digest: String,
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "detail", rename_all = "snake_case")]
pub enum ExecOutcome {
    /// Done; the executor's result id.
    Executed(String),
    /// The policy wants explicit confirmation of this reason; re-approve with `confirmed`.
    NeedsConfirmation(String),
    Denied(String),
    /// The kill switch is engaged.
    Halted,
}

/// Prepares and executes actions through the policy gate.
pub struct ActionRunner {
    policy: Arc<dyn PolicyGate>,
    executors: Executors,
    audit: Option<Store>,
    key: [u8; APPROVAL_KEY_LEN],
    used: Mutex<UsedApprovals>,
}

impl ActionRunner {
    /// Approvals are bound with a random key for this runner; use [`ActionRunner::with_key`] to
    /// keep one per install.
    pub fn new(policy: Arc<dyn PolicyGate>, executors: Executors) -> Self {
        let key = new_approval_key().unwrap_or_else(|e| {
            // Without randomness, no approval can be trusted: refuse every one.
            tracing::error!(error = %e, "no approval key; actions are refused");
            [0; APPROVAL_KEY_LEN]
        });
        Self {
            policy,
            executors,
            audit: None,
            key,
            used: Mutex::new(UsedApprovals::default()),
        }
    }

    /// Bind approvals with this key (e.g. the install's, kept in the encrypted store).
    pub fn with_key(mut self, key: [u8; APPROVAL_KEY_LEN]) -> Self {
        self.key = key;
        self
    }

    /// Record every decision and execution in the store's audit log.
    pub fn with_audit(mut self, store: Store) -> Self {
        self.audit = Some(store);
        self
    }

    /// Fill in generated content (the reply body) and render the preview. Executes nothing.
    pub async fn prepare(
        &self,
        card_id: &str,
        action_index: usize,
        action: &Action,
        context: &[String],
    ) -> Result<PreparedAction> {
        let mut action = action.clone();
        if let Action::DraftReply(d) = &mut action
            && d.body.is_none()
        {
            let drafter = self
                .executors
                .drafter
                .as_ref()
                .ok_or_else(|| Error::Unsupported("no drafter configured".into()))?;
            let body = drafter
                .draft_reply(&DraftRequest {
                    reply_to: d.reply_to.clone(),
                    to: d.to.clone(),
                    subject: d.subject.clone(),
                    intent: d.intent.clone(),
                    context: context.to_vec(),
                })
                .await?;
            d.body = Some(body);
        }
        let preview = action.preview();
        let digest = digest(&self.key, card_id, action_index, &action, &preview)?;
        Ok(PreparedAction {
            card_id: card_id.to_owned(),
            action_index,
            action,
            preview,
            digest,
        })
    }

    /// Execute after the user's approval: the approval must match the preview's digest, the
    /// kill switch must be off, an executor must be configured, and the policy must allow it.
    /// A policy `Confirm`, or the action's own [`Action::requires_confirmation`], needs an
    /// approval with `confirmed`.
    pub async fn execute(
        &self,
        prepared: &PreparedAction,
        approval: &Approval,
    ) -> Result<ExecOutcome> {
        let expected = digest(
            &self.key,
            &prepared.card_id,
            prepared.action_index,
            &prepared.action,
            &prepared.preview,
        )?;
        if self.key == [0; APPROVAL_KEY_LEN]
            || expected != prepared.digest
            || approval.digest != expected
        {
            return Err(Error::Integrity(
                "approval does not match the previewed action".into(),
            ));
        }
        // Single shot: an approval runs once. It is released again when nothing ran (policy
        // asked for confirmation, denied, halted, or the executor failed).
        if !self.used().claim(&expected) {
            return Err(Error::Integrity("this approval was already used".into()));
        }
        let out = self.execute_claimed(prepared, approval).await;
        if !matches!(out, Ok(ExecOutcome::Executed(_))) {
            self.used().release(&expected);
        }
        out
    }

    fn used(&self) -> std::sync::MutexGuard<'_, UsedApprovals> {
        self.used.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn execute_claimed(
        &self,
        prepared: &PreparedAction,
        approval: &Approval,
    ) -> Result<ExecOutcome> {
        let task = format!("proactive:{}", prepared.card_id);
        if self.policy.halted() {
            self.audit("halted", &task, prepared, json!({}));
            return Ok(ExecOutcome::Halted);
        }
        let missing = |what: &str| Error::Unsupported(format!("no {what} executor configured"));
        // Resolve the executor first: nothing to confirm for an action that cannot run here.
        let ex = &self.executors;
        let what = match &prepared.action {
            Action::DraftReply(_) | Action::ArchiveThread(_) => ex.mail.is_none().then_some("mail"),
            Action::CreateEvent(_) => ex.calendar.is_none().then_some("calendar"),
            Action::Reminder(_) => ex.reminders.is_none().then_some("reminder"),
            Action::ResearchNote(_) => ex.notes.is_none().then_some("note"),
        };
        if let Some(what) = what {
            return Err(missing(what));
        }
        let descriptor = prepared.action.descriptor(&task);
        let policy_reason = match self.policy.evaluate(&descriptor) {
            Decision::Allow => None,
            Decision::Confirm(reason) => Some(reason),
            Decision::Deny(reason) => {
                self.audit("denied", &task, prepared, json!({"reason": reason}));
                return Ok(ExecOutcome::Denied(reason));
            }
        };
        let reason =
            policy_reason.or_else(|| prepared.action.requires_confirmation().map(str::to_owned));
        if let Some(reason) = reason
            && !approval.confirmed
        {
            self.audit(
                "needs_confirmation",
                &task,
                prepared,
                json!({"reason": reason}),
            );
            return Ok(ExecOutcome::NeedsConfirmation(reason));
        }
        let id = match &prepared.action {
            Action::DraftReply(d) => {
                ex.mail
                    .as_ref()
                    .ok_or_else(|| missing("mail"))?
                    .save_draft(d)
                    .await?
            }
            Action::ArchiveThread(a) => {
                ex.mail
                    .as_ref()
                    .ok_or_else(|| missing("mail"))?
                    .archive_thread(a)
                    .await?
            }
            Action::CreateEvent(e) => {
                ex.calendar
                    .as_ref()
                    .ok_or_else(|| missing("calendar"))?
                    .create_event(e)
                    .await?
            }
            Action::Reminder(r) => {
                ex.reminders
                    .as_ref()
                    .ok_or_else(|| missing("reminder"))?
                    .create_reminder(r)
                    .await?
            }
            Action::ResearchNote(n) => {
                ex.notes
                    .as_ref()
                    .ok_or_else(|| missing("note"))?
                    .write_note(n)
                    .await?
            }
        };
        self.audit("executed", &task, prepared, json!({"result": id}));
        Ok(ExecOutcome::Executed(id))
    }

    fn audit(&self, what: &str, task: &str, p: &PreparedAction, mut detail: serde_json::Value) {
        let Some(store) = &self.audit else { return };
        detail["digest"] = json!(p.digest);
        detail["risk"] = json!(p.preview.risk);
        detail["summary"] = json!(p.preview.summary);
        let entry = AuditEntry {
            actor: "proactive".into(),
            action: format!("action.{}.{what}", p.action.name()),
            target: task.into(),
            detail,
        };
        if let Err(e) = store.append_audit(&entry) {
            tracing::warn!(error = %e, "cannot write proactive audit entry");
        }
    }
}
