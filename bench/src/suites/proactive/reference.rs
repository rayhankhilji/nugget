//! The reference engine: the design `docs/PLAN.md` §3 gives `nugget-proactive`, small enough to
//! read in one sitting. It exists so the benchmark has something to score before the real crate
//! lands, and so the real crate has a bar to clear.
//!
//! - **Deterministic candidate store.** Rules over threads, notes, calendar and receipts produce
//!   candidates of the five card kinds, each keyed by `(kind, subject)`. A key is shown at most
//!   once (dedup).
//! - **Filters before scoring.** Header-flagged bulk mail (`List-Unsubscribe`, `Auto-Submitted`,
//!   no-reply style senders) never becomes a candidate, nor does a renewal or charge notice from a
//!   merchant the user has no receipt from, nor a cancelled meeting.
//! - **Feedback-learned score.** A fixed prior per kind and for known correspondents, plus learned
//!   weights on a few features (sender, kind, internal or recurring meeting, renewal cadence),
//!   updated by logistic SGD on each accept or dismiss. Cards scoring below zero are not shown.
//! - **Typed actions with dry-run diffs.** Every card carries an action whose diff is computed
//!   before it is shown, and execution refuses without an approval for that exact card.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::engine::{
    Action, ActionKind, Approval, Card, DryRun, Execution, Feedback, ProactiveEngine,
};
use super::world::{
    Cadence, CardKind, Event, Item, Mail, commit_subject, day_of, event_subject, iso_date,
    pretty_date, sub_subject, thread_subject,
};

/// Local parts that mark automated senders.
const AUTOMATED_LOCAL_PARTS: [&str; 6] = [
    "noreply",
    "no-reply",
    "donotreply",
    "notifications",
    "alerts",
    "mailer-daemon",
];

#[derive(Debug, Clone)]
struct Msg {
    day: i32,
    outbound: bool,
    counterpart: String,
    subject: String,
    body: String,
    question: bool,
    /// Bulk or suspect: never a candidate.
    ignored: bool,
}

#[derive(Debug, Clone)]
struct Commitment {
    source: String,
    thread: Option<String>,
    made: i32,
    due: i32,
    text: String,
}

#[derive(Debug, Clone)]
struct Subscription {
    merchant: String,
    cadence: Cadence,
    renews_on: i32,
    amount_cents: u32,
}

#[derive(Debug, Clone)]
struct Issued {
    card: Card,
    features: Vec<String>,
}

pub struct ReferenceEngine {
    user: String,
    user_domain: String,
    learning: bool,
    known: HashSet<String>,
    receipt_domains: HashSet<String>,
    threads: BTreeMap<String, Vec<Msg>>,
    commitments: Vec<Commitment>,
    events: BTreeMap<String, Event>,
    cancelled: HashSet<String>,
    subs: BTreeMap<String, Subscription>,
    shown: HashSet<(CardKind, String)>,
    weights: HashMap<String, f64>,
    issued: HashMap<String, Issued>,
    next_card: usize,
    executed: HashSet<String>,
    sandbox: HashMap<String, String>,
    audit: Vec<Execution>,
}

impl ReferenceEngine {
    /// `learning: false` freezes the weights (the ablation arm).
    pub fn new(user: &str, learning: bool) -> Self {
        Self {
            user: user.to_owned(),
            user_domain: domain(user).to_owned(),
            learning,
            known: HashSet::new(),
            receipt_domains: HashSet::new(),
            threads: BTreeMap::new(),
            commitments: Vec::new(),
            events: BTreeMap::new(),
            cancelled: HashSet::new(),
            subs: BTreeMap::new(),
            shown: HashSet::new(),
            weights: HashMap::new(),
            issued: HashMap::new(),
            next_card: 0,
            executed: HashSet::new(),
            sandbox: HashMap::new(),
            audit: Vec::new(),
        }
    }

    fn observe_mail(&mut self, m: &Mail) {
        let outbound = m.from == self.user;
        if outbound {
            for to in &m.to {
                self.known.insert(to.clone());
            }
            if let Some(due) = promised_by(&m.body) {
                self.commitments.push(Commitment {
                    source: m.id.clone(),
                    thread: Some(m.thread.clone()),
                    made: m.day,
                    due,
                    text: m.body.clone(),
                });
            }
        }
        let counterpart = if outbound {
            m.to.first().cloned().unwrap_or_default()
        } else {
            m.from.clone()
        };
        let ignored = !outbound && (automated(m) || self.suspect_billing(m));
        self.threads.entry(m.thread.clone()).or_default().push(Msg {
            day: m.day,
            outbound,
            counterpart,
            subject: m.subject.clone(),
            body: m.body.clone(),
            question: asks(&m.body),
            ignored,
        });
    }

    /// A renewal or charge notice from a merchant the user has never paid.
    fn suspect_billing(&self, m: &Mail) -> bool {
        let text = format!("{} {}", m.subject, m.body).to_lowercase();
        let billing = ["renew", "charge", "subscription", "refund", "billing"]
            .iter()
            .any(|w| text.contains(w));
        billing && !self.receipt_domains.contains(domain(&m.from))
    }

    fn delivered(&self, c: &Commitment, before: i32) -> bool {
        let Some(t) = &c.thread else { return false };
        self.threads.get(t).is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m.outbound && m.day > c.made && m.day < before && {
                    let b = m.body.to_lowercase();
                    b.contains("attached") || b.contains("as promised") || b.contains("here is")
                }
            })
        })
    }

    fn score(&self, kind: CardKind, prior: f64, features: &[String]) -> f64 {
        let base = match kind {
            CardKind::UnansweredThread => 1.0,
            CardKind::FollowUp => 0.8,
            CardKind::CommitmentDue => 1.5,
            CardKind::MeetingPrep => 1.0,
            CardKind::Renewal => 0.8,
        };
        base + prior
            + features
                .iter()
                .map(|f| self.weights.get(f).copied().unwrap_or(0.0))
                .sum::<f64>()
    }

    fn candidates(&self, day: i32) -> Vec<(Card, Vec<String>)> {
        let mut out = Vec::new();
        let mut push = |kind: CardKind,
                        subject: String,
                        title: String,
                        prior: f64,
                        mut features: Vec<String>,
                        action: (ActionKind, String, String)| {
            features.push(format!("kind:{}", kind.as_str()));
            let score = self.score(kind, prior, &features);
            let (action_kind, target, after) = action;
            let card = Card {
                id: String::new(),
                kind,
                subject,
                title,
                score,
                action: Some(Action {
                    kind: action_kind,
                    dry_run: Some(DryRun {
                        target,
                        before: String::new(),
                        after,
                    }),
                }),
            };
            out.push((card, features));
        };

        for (thread, msgs) in &self.threads {
            let Some(last) = msgs.last() else { continue };
            if last.ignored || !last.question {
                continue;
            }
            let age = day - last.day;
            let known = if self.known.contains(&last.counterpart) {
                1.0
            } else {
                0.0
            };
            let who = name(&last.counterpart);
            if !last.outbound && (2..=7).contains(&age) {
                push(
                    CardKind::UnansweredThread,
                    thread_subject(thread),
                    format!("{who} is waiting on you: {}", last.subject),
                    known,
                    vec![format!("sender:{}", last.counterpart)],
                    (
                        ActionKind::DraftReply,
                        format!("drafts/{thread}"),
                        format!(
                            "To: {}\nSubject: Re: {}\n\nHi {who},\n\nThanks for your note. ",
                            last.counterpart, last.subject
                        ),
                    ),
                );
            } else if last.outbound && (4..=9).contains(&age) {
                push(
                    CardKind::FollowUp,
                    thread_subject(thread),
                    format!("No reply from {who}: {}", last.subject),
                    known,
                    vec![format!("sender:{}", last.counterpart)],
                    (
                        ActionKind::DraftReply,
                        format!("drafts/{thread}"),
                        format!(
                            "To: {}\nSubject: Re: {}\n\nHi {who},\n\nJust following up on my note from {}.",
                            last.counterpart,
                            last.subject,
                            pretty_date(last.day)
                        ),
                    ),
                );
            }
        }

        for c in &self.commitments {
            if !(c.due - 2..=c.due).contains(&day) || self.delivered(c, day) {
                continue;
            }
            let urgency = 0.25 * f64::from(2 - (c.due - day));
            push(
                CardKind::CommitmentDue,
                commit_subject(&c.source),
                format!("Due {}: {}", pretty_date(c.due), first_line(&c.text)),
                urgency,
                Vec::new(),
                (
                    ActionKind::CreateReminder,
                    format!("reminders/{}", c.source),
                    format!("Due {}: {}", iso_date(c.due), first_line(&c.text)),
                ),
            );
        }

        for (id, e) in &self.events {
            if self.cancelled.contains(id) || !(day..=day + 1).contains(&e.start_day) {
                continue;
            }
            let others: Vec<&String> = e.attendees.iter().filter(|a| **a != self.user).collect();
            if others.is_empty() {
                continue;
            }
            let mut features = vec![format!("organizer:{}", e.organizer)];
            if others.iter().all(|a| domain(a) == self.user_domain) {
                features.push("meeting:internal".to_owned());
            }
            if e.series.is_some() {
                features.push("meeting:recurring".to_owned());
            }
            let related: Vec<String> = self
                .threads
                .values()
                .filter_map(|msgs| msgs.last())
                .filter(|m| !m.ignored && others.iter().any(|a| **a == m.counterpart))
                .map(|m| format!("- {} ({})", m.subject, pretty_date(m.day)))
                .take(5)
                .collect();
            push(
                CardKind::MeetingPrep,
                event_subject(id),
                format!(
                    "Prepare: {} ({} {:02}:00)",
                    e.title,
                    pretty_date(e.start_day),
                    e.start_hour
                ),
                0.0,
                features,
                (
                    ActionKind::CreatePrepNote,
                    format!("notes/prep-{id}"),
                    format!(
                        "# Prep: {}\nWhen: {} {:02}:00\nWith: {}\nAgenda: {}\nRecent threads:\n{}",
                        e.title,
                        iso_date(e.start_day),
                        e.start_hour,
                        others
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        e.description,
                        related.join("\n")
                    ),
                ),
            );
        }

        for (dom, s) in &self.subs {
            if !(s.renews_on - 7..s.renews_on).contains(&day) {
                continue;
            }
            let cadence = match s.cadence {
                Cadence::Monthly => "monthly",
                Cadence::Annual => "annual",
            };
            push(
                CardKind::Renewal,
                sub_subject(dom),
                format!("{} renews on {}", s.merchant, pretty_date(s.renews_on)),
                0.0,
                vec![format!("renewal:{cadence}")],
                (
                    ActionKind::CreateReminder,
                    format!("reminders/renewal-{dom}"),
                    format!(
                        "Decide whether to keep {} ({cadence}, ${}.{:02}) before {}",
                        s.merchant,
                        s.amount_cents / 100,
                        s.amount_cents % 100,
                        iso_date(s.renews_on)
                    ),
                ),
            );
        }
        out
    }

    fn learning_rate(feature: &str) -> f64 {
        match feature.split(':').next().unwrap_or("") {
            "kind" => 0.1,
            "sender" | "organizer" => 1.5,
            "meeting" => 1.2,
            "renewal" => 1.0,
            _ => 0.5,
        }
    }
}

impl ProactiveEngine for ReferenceEngine {
    fn name(&self) -> &'static str {
        if self.learning {
            "reference"
        } else {
            "reference_frozen"
        }
    }

    fn observe(&mut self, item: &Item) {
        match item {
            Item::Mail(m) => self.observe_mail(m),
            Item::Event(e) => {
                self.events.insert(e.id.clone(), e.clone());
            }
            Item::EventCancelled(c) => {
                self.cancelled.insert(c.event.clone());
            }
            Item::Note(n) => {
                for line in n.body.lines() {
                    if line.trim_start().starts_with("- [x]") {
                        continue;
                    }
                    if let Some(due) = due_date(line) {
                        self.commitments.push(Commitment {
                            source: n.id.clone(),
                            thread: None,
                            made: n.day,
                            due,
                            text: line.trim_start_matches("- [ ]").trim().to_owned(),
                        });
                    }
                }
            }
            Item::Receipt(r) => {
                self.receipt_domains.insert(r.merchant_domain.clone());
                if let (Some(cadence), Some(renews_on)) = (r.cadence, r.renews_on) {
                    self.subs.insert(
                        r.merchant_domain.clone(),
                        Subscription {
                            merchant: r.merchant.clone(),
                            cadence,
                            renews_on,
                            amount_cents: r.amount_cents,
                        },
                    );
                }
            }
        }
    }

    fn brief(&mut self, day: i32, k: usize) -> Vec<Card> {
        let mut cands: Vec<(Card, Vec<String>)> = self
            .candidates(day)
            .into_iter()
            .filter(|(c, _)| c.score > 0.0 && !self.shown.contains(&(c.kind, c.subject.clone())))
            .collect();
        cands.sort_by(|a, b| {
            b.0.score
                .total_cmp(&a.0.score)
                .then_with(|| a.0.key().cmp(&b.0.key()))
        });
        cands.truncate(k);
        let mut out = Vec::with_capacity(cands.len());
        for (mut card, features) in cands {
            self.next_card += 1;
            card.id = format!("c-{}", self.next_card);
            self.shown.insert((card.kind, card.subject.clone()));
            self.issued.insert(
                card.id.clone(),
                Issued {
                    card: card.clone(),
                    features,
                },
            );
            out.push(card);
        }
        out
    }

    fn feedback(&mut self, card_id: &str, feedback: Feedback) {
        if !self.learning {
            return;
        }
        let Some(issued) = self.issued.get(card_id) else {
            return;
        };
        let y = if feedback == Feedback::Accepted {
            1.0
        } else {
            0.0
        };
        let p = 1.0 / (1.0 + (-issued.card.score).exp());
        for f in issued.features.clone() {
            *self.weights.entry(f.clone()).or_insert(0.0) += Self::learning_rate(&f) * (y - p);
        }
    }

    fn execute(&mut self, card_id: &str, approval: Option<&Approval>) -> Result<Execution, String> {
        let approval = approval.ok_or("no approval")?;
        if approval.card_id != card_id {
            return Err("approval is for another card".into());
        }
        let issued = self.issued.get(card_id).ok_or("unknown card")?;
        if self.executed.contains(card_id) {
            return Err("already executed".into());
        }
        let action = issued.card.action.clone().ok_or("card has no action")?;
        let preview = action.dry_run.ok_or("action has no dry run")?;
        let before = self
            .sandbox
            .get(&preview.target)
            .cloned()
            .unwrap_or_default();
        if before != preview.before {
            return Err("target changed since the preview".into());
        }
        self.sandbox
            .insert(preview.target.clone(), preview.after.clone());
        self.executed.insert(card_id.to_owned());
        let exec = Execution {
            card_id: card_id.to_owned(),
            action: action.kind,
            approval: Some(approval.token),
            applied: DryRun {
                target: preview.target,
                before,
                after: preview.after,
            },
        };
        self.audit.push(exec.clone());
        Ok(exec)
    }

    fn audit_log(&self) -> Vec<Execution> {
        self.audit.clone()
    }
}

fn domain(addr: &str) -> &str {
    addr.rsplit('@').next().unwrap_or("")
}

fn name(addr: &str) -> String {
    let local = addr.split('@').next().unwrap_or(addr);
    let mut c = local.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn automated(m: &Mail) -> bool {
    let local = m.from.split('@').next().unwrap_or("").to_lowercase();
    m.list_unsubscribe
        || m.auto_submitted
        || AUTOMATED_LOCAL_PARTS.iter().any(|p| local.contains(p))
}

fn asks(body: &str) -> bool {
    let b = body.to_lowercase();
    b.contains('?')
        || ["could you", "can you", "let me know", "please confirm"]
            .iter()
            .any(|p| b.contains(p))
}

/// `I'll ... by YYYY-MM-DD` in the user's own mail.
fn promised_by(body: &str) -> Option<i32> {
    let b = body.to_lowercase();
    if !(b.contains("i'll ") || b.contains("i will ")) {
        return None;
    }
    let at = b.find(" by ")?;
    iso_at(&body[at + 4..])
}

/// `due YYYY-MM-DD` in a note line.
fn due_date(line: &str) -> Option<i32> {
    let at = line.find("due ")?;
    iso_at(&line[at + 4..])
}

fn iso_at(s: &str) -> Option<i32> {
    s.get(..10).and_then(day_of)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_promises_and_due_dates() {
        assert_eq!(
            promised_by("Happy to. I'll send the deck by 2026-09-09."),
            Some(9)
        );
        assert_eq!(promised_by("Could you reply by 2026-09-09?"), None);
        assert_eq!(due_date("- [ ] Send the lease (due 2026-09-10)"), Some(10));
        assert_eq!(due_date("no date here"), None);
    }

    #[test]
    fn flags_automated_mail_and_questions() {
        let mut m = Mail {
            id: "m".into(),
            day: 1,
            thread: "t".into(),
            from: "no-reply@parcelpost.test".into(),
            to: vec![],
            subject: String::new(),
            body: String::new(),
            list_unsubscribe: false,
            auto_submitted: false,
        };
        assert!(automated(&m));
        m.from = "kim@northwind.test".into();
        assert!(!automated(&m));
        m.list_unsubscribe = true;
        assert!(automated(&m));
        assert!(asks("Can you confirm the date"));
        assert!(!asks("Thanks, all done."));
    }
}
