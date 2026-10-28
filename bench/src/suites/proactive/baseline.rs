//! The Sentient-style baseline arm: an LLM judge orders recent context and the top few items become
//! prepared actions (`docs/research/sentient-os-teardown.md` §2.8: the judge reads the last 7 days
//! plus the calendar, returns up to 8 items, at most 5 become actions, and ranking is purely the
//! LLM's ordering).
//!
//! The judge here is a deterministic fake that orders by recency only. To keep the comparison
//! about ranking rather than reading comprehension, it is given two things an LLM would get right
//! for free: it skips header-flagged newsletters and notifications and cancelled events, and it
//! types each item with the same simple mapping (inbound mail asks for a reply, the user's own
//! mail wants a follow-up or is a promise, an event wants prep, a receipt or a billing mail is a
//! renewal). It has no candidate store, so it keeps no memory between days (no dedup) and ignores
//! feedback.

use std::collections::HashSet;

use super::engine::{Action, ActionKind, Approval, Card, Execution, Feedback, ProactiveEngine};
use super::world::{CardKind, Item, commit_subject, event_subject, sub_subject, thread_subject};

/// Items the judge returns per day before at most `k` become actions.
pub const JUDGE_ITEMS: usize = 8;
/// Days of history in the judge's context.
pub const CONTEXT_DAYS: i32 = 7;

pub struct RecencyJudge {
    user: String,
    items: Vec<Item>,
    cancelled: HashSet<String>,
    issued: Vec<Card>,
    executed: HashSet<String>,
    audit: Vec<Execution>,
}

impl RecencyJudge {
    pub fn new(user: &str) -> Self {
        Self {
            user: user.to_owned(),
            items: Vec::new(),
            cancelled: HashSet::new(),
            issued: Vec::new(),
            executed: HashSet::new(),
            audit: Vec::new(),
        }
    }

    /// The judge's view of an item on `day`: its recency, and the card it would become.
    fn judge(&self, item: &Item, day: i32) -> Option<(i32, CardKind, String, String)> {
        let recent = |d: i32| (day - CONTEXT_DAYS..day).contains(&d);
        match item {
            Item::Mail(m) => {
                if !recent(m.day) || m.list_unsubscribe || m.auto_submitted || no_reply(&m.from) {
                    return None;
                }
                let text = format!("{} {}", m.subject, m.body).to_lowercase();
                let (kind, subject) = if m.from == self.user {
                    if text.contains("i'll ") && text.contains(" by ") {
                        (CardKind::CommitmentDue, commit_subject(&m.id))
                    } else {
                        (CardKind::FollowUp, thread_subject(&m.thread))
                    }
                } else if text.contains("renew") {
                    let dom = m.from.rsplit('@').next().unwrap_or_default();
                    (CardKind::Renewal, sub_subject(dom))
                } else {
                    (CardKind::UnansweredThread, thread_subject(&m.thread))
                };
                Some((m.day, kind, subject, m.subject.clone()))
            }
            Item::Event(e) => {
                let upcoming = (day..day + CONTEXT_DAYS).contains(&e.start_day);
                let others = e.attendees.iter().any(|a| *a != self.user);
                if !upcoming || !others || self.cancelled.contains(&e.id) {
                    return None;
                }
                // The calendar enters the context a week ahead.
                let recency = e.day.max(e.start_day - CONTEXT_DAYS);
                Some((
                    recency,
                    CardKind::MeetingPrep,
                    event_subject(&e.id),
                    e.title.clone(),
                ))
            }
            Item::Note(n) => recent(n.day).then(|| {
                (
                    n.day,
                    CardKind::CommitmentDue,
                    commit_subject(&n.id),
                    n.title.clone(),
                )
            }),
            Item::Receipt(r) => recent(r.day).then(|| {
                (
                    r.day,
                    CardKind::Renewal,
                    sub_subject(&r.merchant_domain),
                    r.description.clone(),
                )
            }),
            Item::EventCancelled(_) => None,
        }
    }
}

fn no_reply(addr: &str) -> bool {
    let local = addr.split('@').next().unwrap_or("").to_lowercase();
    ["noreply", "no-reply", "notifications", "alerts"]
        .iter()
        .any(|p| local.contains(p))
}

impl ProactiveEngine for RecencyJudge {
    fn name(&self) -> &'static str {
        "recency_judge"
    }

    fn observe(&mut self, item: &Item) {
        if let Item::EventCancelled(c) = item {
            self.cancelled.insert(c.event.clone());
        }
        self.items.push(item.clone());
    }

    fn brief(&mut self, day: i32, k: usize) -> Vec<Card> {
        let mut judged: Vec<(i32, &str, CardKind, String, String)> = self
            .items
            .iter()
            .filter_map(|i| {
                self.judge(i, day)
                    .map(|(recency, kind, subject, title)| (recency, i.id(), kind, subject, title))
            })
            .collect();
        // Most recent first; ties by id, newest first.
        judged.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(a.1)));
        judged.truncate(JUDGE_ITEMS);
        let cards: Vec<Card> = judged
            .into_iter()
            .take(k)
            .enumerate()
            .map(|(rank, (_, _, kind, subject, title))| Card {
                id: format!("j-{}", self.issued.len() + rank),
                kind,
                subject,
                title,
                score: -(rank as f64),
                action: Some(Action {
                    kind: match kind {
                        CardKind::UnansweredThread | CardKind::FollowUp => ActionKind::DraftReply,
                        CardKind::MeetingPrep => ActionKind::CreatePrepNote,
                        CardKind::CommitmentDue | CardKind::Renewal => ActionKind::CreateReminder,
                    },
                    // Prepared actions run when the card is tapped; there is no preview diff.
                    dry_run: None,
                }),
            })
            .collect();
        self.issued.extend(cards.iter().cloned());
        cards
    }

    fn feedback(&mut self, _card_id: &str, _feedback: Feedback) {}

    fn execute(&mut self, card_id: &str, approval: Option<&Approval>) -> Result<Execution, String> {
        let approval = approval.ok_or("not tapped")?;
        if approval.card_id != card_id || self.executed.contains(card_id) {
            return Err("approval is not for this card".into());
        }
        let card = self
            .issued
            .iter()
            .find(|c| c.id == card_id)
            .ok_or("unknown card")?;
        let exec = Execution {
            card_id: card_id.to_owned(),
            action: card
                .action
                .as_ref()
                .map_or(ActionKind::DraftReply, |a| a.kind),
            approval: Some(approval.token),
            applied: super::engine::DryRun {
                target: format!("prepared/{card_id}"),
                before: String::new(),
                after: card.title.clone(),
            },
        };
        self.executed.insert(card_id.to_owned());
        self.audit.push(exec.clone());
        Ok(exec)
    }

    fn audit_log(&self) -> Vec<Execution> {
        self.audit.clone()
    }
}
