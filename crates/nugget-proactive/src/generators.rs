//! Deterministic candidate generators over the store and the knowledge graph.
//!
//! Each generator is a pure function of its [`GenContext`]: the same data and `now` always give
//! the same candidates with the same dedup keys.

use std::collections::{BTreeMap, HashSet};

use nugget_core::Result;
use nugget_core::ids::EntityId;
use nugget_graph::{Claim, ClaimObject, Entity, EntityKind, EntityQuery, GraphStore, Identifier};
use nugget_store::Store;
use serde_json::Value;
use time::{Duration, OffsetDateTime, PrimitiveDateTime, Time};

use crate::actions::{Action, ArchiveThread, CreateEvent, DraftReply, Reminder, ResearchNote};
use crate::candidate::{Candidate, CandidateKind, EvidenceRef, Features, freshness, urgency};
use crate::config::ProactiveConfig;
use crate::corpus::{CalEvent, Corpus, MailMessage};
use crate::text::{
    deadline_in, find_amounts, find_dates, find_promises, is_promise, own_text, parse_iso_date,
    relative_days, sentences, stable_hash, truncate,
};

const QUOTE_CHARS: usize = 240;

/// Everything a generator may read.
pub struct GenContext<'a> {
    pub corpus: &'a Corpus,
    pub graph: Option<&'a dyn GraphStore>,
    /// For related-notes search (meeting prep).
    pub store: Option<&'a Store>,
    pub cfg: &'a ProactiveConfig,
    pub now: OffsetDateTime,
}

impl GenContext<'_> {
    fn days_ago(&self, t: OffsetDateTime) -> f64 {
        (self.now - t).as_seconds_f64() / 86_400.0
    }

    /// Relationship strength in `[0, 1]` with the strongest of `emails`: graph claims and
    /// mentions of their person entity, and how often the user wrote to them.
    pub fn importance<'e>(&self, emails: impl IntoIterator<Item = &'e str>) -> f64 {
        let mut best: f64 = 0.0;
        for e in emails {
            let sent = self.corpus.sent_to(e) as f64;
            let mut s = (1.0 + sent).ln() / 11f64.ln();
            if let Some(g) = self.graph
                && let Ok(Some(id)) =
                    g.find_by_identifier(&Identifier::new("email", e).normalized())
            {
                let claims = g.claims_for(&id, false).map(|c| c.len()).unwrap_or(0);
                let mentions = g.mentions_for(&id, 200).map(|m| m.len()).unwrap_or(0);
                let n = (claims + mentions) as f64;
                s = s.max((1.0 + n).ln() / 51f64.ln());
            }
            best = best.max(s);
        }
        best.min(1.0)
    }
}

/// Run every generator. Candidates with the same dedup key are merged (the first wins).
pub fn generate_all(ctx: &GenContext<'_>) -> Result<Vec<Candidate>> {
    let mut all = Vec::new();
    all.extend(unanswered_threads(ctx));
    all.extend(commitments(ctx)?);
    all.extend(meetings(ctx));
    all.extend(renewals(ctx));
    all.extend(follow_ups(ctx));
    let mut seen = HashSet::new();
    all.retain(|c| seen.insert(c.dedup_key.clone()));
    Ok(all)
}

fn evidence(m: &MailMessage, quote: Option<String>) -> EvidenceRef {
    EvidenceRef {
        item: m.item.clone(),
        quote,
        url: m.url.clone(),
    }
}

/// The first substantive line of the user-written part of a body.
fn lead(body: &str) -> Option<String> {
    let own = own_text(body);
    let s = sentences(&own).into_iter().find(|s| {
        let l = s.to_lowercase();
        s.len() > 3 && !l.starts_with("hi") && !l.starts_with("hello") && !l.starts_with("dear")
    })?;
    Some(truncate(&s, QUOTE_CHARS))
}

fn asks_something(body: &str) -> bool {
    let own = own_text(body).to_lowercase();
    own.contains('?')
        || [
            "could you",
            "can you",
            "would you",
            "please",
            "let me know",
            "thoughts",
        ]
        .iter()
        .any(|p| own.contains(p))
}

fn reply_subject(s: &str) -> String {
    if s.to_lowercase().starts_with("re:") {
        s.to_owned()
    } else {
        format!("Re: {s}")
    }
}

// ---- unanswered threads ----

/// Inbound mail from real people, addressed to the user, with no later reply from the user,
/// 1–14 days old. Newsletters and automated senders never qualify.
pub fn unanswered_threads(ctx: &GenContext<'_>) -> Vec<Candidate> {
    let c = ctx.corpus;
    let mut out = Vec::new();
    for idxs in c.threads.values() {
        // People only: automated messages in a thread neither need nor count as a reply.
        let human: Vec<&MailMessage> = idxs
            .iter()
            .map(|&i| &c.mail[i])
            .filter(|m| m.from_user || m.automated_reason().is_none())
            .collect();
        let Some(last) = human.last() else { continue };
        if last.from_user {
            continue;
        }
        let Some(from) = &last.from else { continue };
        let age = ctx.days_ago(last.date);
        if age < ctx.cfg.unanswered_min_days as f64 || age > ctx.cfg.unanswered_max_days as f64 {
            continue;
        }
        let direct = last.to.iter().any(|a| c.is_user(&a.email));
        let cc = last.cc.iter().any(|a| c.is_user(&a.email));
        // Mailing-list traffic not addressed to the user is not "unanswered".
        if !c.user_addresses.is_empty() && !direct && !cc {
            continue;
        }
        let question = asks_something(&last.body);
        let quote = lead(&last.body).or_else(|| Some(truncate(&last.subject, QUOTE_CHARS)));
        let mut evidence_list = vec![evidence(last, quote.clone())];
        // The user's own earlier message gives context.
        if let Some(mine) = human.iter().rev().find(|m| m.from_user) {
            evidence_list.push(evidence(mine, lead(&mine.body)));
        }
        let msg_key = last
            .message_id
            .clone()
            .unwrap_or_else(|| last.item.key.as_str().to_owned());
        let dedup_key = format!("unanswered:{}", stable_hash(&[&last.thread, &msg_key]));
        let mut to = vec![from.email.clone()];
        let cc_list: Vec<String> = last
            .to
            .iter()
            .chain(&last.cc)
            .filter(|a| !c.is_user(&a.email) && a.email != from.email)
            .map(|a| a.email.clone())
            .collect();
        to.dedup();
        let actions = vec![
            Action::DraftReply(DraftReply {
                reply_to: last.item.clone(),
                in_reply_to_message_id: last.message_id.clone(),
                to,
                cc: cc_list,
                subject: reply_subject(&last.subject),
                intent: if question {
                    "Answer their question.".into()
                } else {
                    "Acknowledge and reply.".into()
                },
                body: None,
            }),
            Action::ArchiveThread(ArchiveThread {
                messages: idxs.iter().map(|&i| c.mail[i].item.clone()).collect(),
                subject: last.subject.clone(),
            }),
        ];
        let days = age.floor() as i64;
        out.push(Candidate {
            dedup_key,
            kind: CandidateKind::UnansweredThread,
            title: format!("Reply to {}: {}", from.display(), last.subject),
            detail: format!(
                "{} wrote {} day{} ago and hasn't heard back.",
                from.display(),
                days,
                if days == 1 { "" } else { "s" }
            ),
            evidence: evidence_list,
            people: vec![from.email.clone()],
            due: None,
            expires_at: Some(last.date + Duration::days(ctx.cfg.unanswered_max_days)),
            features: Features {
                // Waiting grows more pressing with time.
                urgency: (age / ctx.cfg.unanswered_max_days as f64).min(1.0) * 0.6,
                importance: ctx.importance([from.email.as_str()]),
                freshness: freshness(last.date, ctx.now),
                direct: f64::from(u8::from(direct)),
                question: f64::from(u8::from(question)),
                kind: Some(CandidateKind::UnansweredThread),
            },
            actions,
            sensitive: last.sensitive,
        });
    }
    out
}

// ---- commitments ----

#[derive(Debug, Clone, PartialEq)]
enum Owner {
    Me,
    Them(String),
    Unknown,
}

fn user_entity(ctx: &GenContext<'_>, g: &dyn GraphStore) -> Option<EntityId> {
    if let Some(id) = &ctx.cfg.user_entity {
        return g.get_entity(id).ok().flatten().map(|e| e.id);
    }
    ctx.corpus.user_addresses.iter().find_map(|a| {
        g.find_by_identifier(&Identifier::new("email", a).normalized())
            .ok()
            .flatten()
    })
}

/// A due date from a literal: ISO date / RFC 3339, or written dates ("Nov 3, 2026").
fn literal_date(s: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let date = parse_iso_date(s).or_else(|| find_dates(s, now.date()).into_iter().next())?;
    // Due at the end of that day (UTC is enough at day granularity).
    Some(PrimitiveDateTime::new(date, Time::from_hms(23, 59, 0).ok()?).assume_utc())
}

fn attr_due(e: &Entity, now: OffsetDateTime) -> Option<OffsetDateTime> {
    ["due_on", "due", "deadline", "due_date", "due_by"]
        .iter()
        .find_map(|k| e.attributes.get(*k).and_then(Value::as_str))
        .and_then(|s| literal_date(s, now))
}

fn closed(e: &Entity) -> bool {
    e.attributes
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| {
            matches!(
                s.to_lowercase().as_str(),
                "done" | "completed" | "complete" | "cancelled" | "canceled" | "closed"
            )
        })
}

fn claim_evidence(claims: &[&Claim], corpus: &Corpus) -> Vec<EvidenceRef> {
    let mut out: Vec<EvidenceRef> = Vec::new();
    for c in claims {
        for ev in &c.evidence {
            if out.iter().any(|o| o.item == ev.item) {
                continue;
            }
            let url = corpus
                .mail
                .iter()
                .find(|m| m.item == ev.item)
                .and_then(|m| m.url.clone());
            out.push(EvidenceRef {
                item: ev.item.clone(),
                quote: ev
                    .quote
                    .clone()
                    .or_else(|| Some(truncate(&c.text, QUOTE_CHARS))),
                url,
            });
        }
    }
    out
}

/// Graph commitments (entities of kind `commitment`, and `owes`/`committed_to` claims with a
/// literal object) that are due within the horizon, overdue, or recent promises of the user.
pub fn commitments(ctx: &GenContext<'_>) -> Result<Vec<Candidate>> {
    let Some(g) = ctx.graph else {
        return Ok(Vec::new());
    };
    let me = user_entity(ctx, g);
    let horizon = ctx.now + Duration::days(ctx.cfg.commitment_horizon_days);
    let stale = ctx.now - Duration::days(ctx.cfg.commitment_overdue_max_days);
    let mut out = Vec::new();

    let mut offset = 0;
    let mut entities = Vec::new();
    loop {
        let page = g.list_entities(&EntityQuery {
            text: None,
            kind: Some(EntityKind::Commitment),
            limit: 200,
            offset,
        })?;
        let n = page.len();
        entities.extend(page);
        if n < 200 || entities.len() >= 2000 {
            break;
        }
        offset += n;
    }

    for summary in entities {
        let Some(e) = g.get_entity(&summary.id)? else {
            continue;
        };
        if e.id != summary.id || closed(&e) {
            continue;
        }
        let claims = g.claims_for(&e.id, false)?;
        let mut due = attr_due(&e, ctx.now);
        let mut owner = Owner::Unknown;
        let mut used: Vec<&Claim> = Vec::new();
        for c in &claims {
            match (c.predicate.as_str(), &c.object) {
                ("due_on", ClaimObject::Literal(s)) if c.subject == e.id => {
                    if due.is_none() {
                        due = literal_date(s, ctx.now);
                    }
                    used.push(c);
                }
                ("committed_to" | "owes", ClaimObject::Entity(obj)) if *obj == e.id => {
                    if me.as_ref() == Some(&c.subject) {
                        owner = Owner::Me;
                    } else if owner == Owner::Unknown {
                        let name = g
                            .get_entity(&c.subject)?
                            .map_or_else(|| c.subject.to_string(), |p| p.name);
                        owner = Owner::Them(name);
                    }
                    used.push(c);
                }
                _ => {
                    if c.valid_to.is_some() && due.is_none() {
                        due = c.valid_to;
                    }
                    used.push(c);
                }
            }
        }
        if owner == Owner::Unknown
            && (is_promise(&e.name) || used.iter().any(|c| is_promise(&c.text)))
        {
            owner = Owner::Me;
        }
        if let Some(c) = commitment_candidate(
            ctx,
            &e.id.to_string(),
            &e.name,
            due,
            e.created_at,
            &owner,
            &used,
            horizon,
            stale,
        ) {
            out.push(c);
        }
    }

    // Literal commitments of the user: `me owes "the Q3 report"`.
    if let Some(me) = &me {
        for c in g.claims_for(me, false)? {
            if c.subject != *me || !matches!(c.predicate.as_str(), "owes" | "committed_to") {
                continue;
            }
            let ClaimObject::Literal(what) = &c.object else {
                continue;
            };
            let due = c.valid_to.or_else(|| literal_date(what, ctx.now));
            if let Some(cand) = commitment_candidate(
                ctx,
                &format!("claim:{}", c.id),
                what,
                due,
                c.created_at,
                &Owner::Me,
                &[&c],
                horizon,
                stale,
            ) {
                out.push(cand);
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn commitment_candidate(
    ctx: &GenContext<'_>,
    id: &str,
    name: &str,
    due: Option<OffsetDateTime>,
    created: OffsetDateTime,
    owner: &Owner,
    claims: &[&Claim],
    horizon: OffsetDateTime,
    stale: OffsetDateTime,
) -> Option<Candidate> {
    match due {
        Some(d) if d > horizon || d < stale => return None,
        // Undated: only the user's own, recent promises.
        None if *owner != Owner::Me || ctx.days_ago(created) > ctx.cfg.card_ttl_days as f64 => {
            return None;
        }
        _ => {}
    }
    let overdue = due.is_some_and(|d| d < ctx.now);
    let when = due.map(|d| d.date().to_string());
    let title = match (owner, overdue, &when) {
        (Owner::Them(who), _, Some(w)) => format!("Waiting on {who}: {name} (due {w})"),
        (Owner::Them(who), _, None) => format!("Waiting on {who}: {name}"),
        (_, true, Some(w)) => format!("Overdue since {w}: {name}"),
        (_, false, Some(w)) => format!("Due {w}: {name}"),
        (_, _, None) => format!("You committed to: {name}"),
    };
    let evidence = claim_evidence(claims, ctx.corpus);
    let sensitive = evidence.iter().any(|ev| {
        ctx.corpus
            .mail
            .iter()
            .find(|m| m.item == ev.item)
            .is_none_or(|m| m.sensitive)
    });
    let remind_at = match due {
        Some(d) if d - Duration::days(1) > ctx.now => d - Duration::days(1),
        _ => ctx.now + Duration::hours(2),
    };
    let mut actions = vec![Action::Reminder(Reminder {
        text: name.to_owned(),
        at: remind_at,
        about: evidence.first().map(|e| e.item.clone()),
    })];
    if matches!(owner, Owner::Me | Owner::Unknown) {
        let start = (remind_at + Duration::hours(1))
            .replace_minute(0)
            .unwrap_or(remind_at);
        actions.push(Action::CreateEvent(CreateEvent {
            title: format!("Focus: {name}"),
            start,
            end: start + Duration::hours(1),
            attendees: Vec::new(),
            description: when.as_ref().map(|w| format!("Due {w}.")),
        }));
    }
    Some(Candidate {
        dedup_key: format!("commitment:{}", stable_hash(&[id])),
        kind: CandidateKind::Commitment,
        title,
        detail: claims
            .first()
            .map(|c| truncate(&c.text, QUOTE_CHARS))
            .unwrap_or_else(|| name.to_owned()),
        evidence,
        people: match owner {
            Owner::Them(w) => vec![w.clone()],
            _ => Vec::new(),
        },
        due,
        expires_at: Some(
            due.map_or(created + Duration::days(ctx.cfg.card_ttl_days), |d| {
                d + Duration::days(ctx.cfg.commitment_overdue_max_days)
            }),
        ),
        features: Features {
            urgency: urgency(due, ctx.now),
            importance: 0.5,
            freshness: freshness(created, ctx.now),
            direct: f64::from(u8::from(*owner == Owner::Me)),
            question: 0.0,
            kind: Some(CandidateKind::Commitment),
        },
        actions,
        sensitive,
    })
}

// ---- meetings ----

/// Events in the next 24–48 h with attendees outside the user's organisation, with the recent
/// threads involving those people and related notes.
pub fn meetings(ctx: &GenContext<'_>) -> Vec<Candidate> {
    let c = ctx.corpus;
    let horizon = ctx.now + Duration::hours(ctx.cfg.meeting_horizon_hours);
    let mut out = Vec::new();
    for ev in &c.events {
        if ev.cancelled || ev.all_day || ev.start <= ctx.now || ev.start > horizon {
            continue;
        }
        let external: Vec<String> = ev
            .attendees
            .iter()
            .filter(|a| !a.resource && c.is_external(&a.address))
            .map(|a| a.address.email.clone())
            .collect();
        if external.is_empty() {
            continue;
        }
        let threads = related_threads(c, &external, ctx.now);
        let notes = related_notes(ctx, ev);
        let mut evidence = vec![EvidenceRef {
            item: ev.item.clone(),
            quote: Some(truncate(&ev.title, QUOTE_CHARS)),
            url: ev.url.clone(),
        }];
        evidence.extend(threads.iter().map(|m| evidence_of_thread(m)));
        evidence.extend(notes.iter().cloned());
        let names: Vec<String> = ev
            .attendees
            .iter()
            .filter(|a| !a.resource && c.is_external(&a.address))
            .map(|a| a.address.display())
            .collect();
        let mut body = format!(
            "## {}\n\nWhen: {}\n",
            ev.title,
            ev.start
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        );
        if let Some(l) = &ev.location {
            body.push_str(&format!("Where: {l}\n"));
        }
        body.push_str(&format!("External: {}\n", names.join(", ")));
        if !threads.is_empty() {
            body.push_str("\n### Recent threads\n");
            for m in &threads {
                body.push_str(&format!("- {} ({})\n", m.subject, m.date.date()));
            }
        }
        if !notes.is_empty() {
            body.push_str("\n### Related notes\n");
            for n in &notes {
                body.push_str(&format!("- {}\n", n.quote.clone().unwrap_or_default()));
            }
        }
        body.push_str("\n### Goals\n- \n\n### Questions\n- \n");
        let sensitive = ev.sensitive || threads.iter().any(|m| m.sensitive);
        let hours = ((ev.start - ctx.now).as_seconds_f64() / 3600.0).round() as i64;
        let key_id = ev.uid.clone().unwrap_or_else(|| ev.item.key.to_string());
        out.push(Candidate {
            dedup_key: format!(
                "meeting:{}",
                stable_hash(&[&key_id, &ev.start.unix_timestamp().to_string()])
            ),
            kind: CandidateKind::MeetingPrep,
            title: format!("Prepare for {} (in {hours} h)", ev.title),
            detail: format!(
                "With {}. {} related thread{}, {} note{}.",
                names.join(", "),
                threads.len(),
                if threads.len() == 1 { "" } else { "s" },
                notes.len(),
                if notes.len() == 1 { "" } else { "s" },
            ),
            evidence,
            people: external.clone(),
            due: Some(ev.start),
            expires_at: Some(ev.start),
            features: Features {
                urgency: urgency(Some(ev.start), ctx.now),
                importance: ctx.importance(external.iter().map(String::as_str)),
                freshness: 1.0,
                direct: 1.0,
                question: 0.0,
                kind: Some(CandidateKind::MeetingPrep),
            },
            actions: vec![Action::ResearchNote(ResearchNote {
                title: format!("Prep: {}", ev.title),
                body,
                sources: std::iter::once(ev.item.clone())
                    .chain(threads.iter().map(|m| m.item.clone()))
                    .collect(),
            })],
            sensitive,
        });
    }
    out
}

fn evidence_of_thread(m: &MailMessage) -> EvidenceRef {
    evidence(m, Some(truncate(&m.subject, QUOTE_CHARS)))
}

/// The latest message of each of up to 3 recent threads (30 days) involving any of `emails`.
fn related_threads<'c>(
    c: &'c Corpus,
    emails: &[String],
    now: OffsetDateTime,
) -> Vec<&'c MailMessage> {
    let mut latest: BTreeMap<&str, &MailMessage> = BTreeMap::new();
    for m in &c.mail {
        if m.date > now || now - m.date > Duration::days(30) || m.automated_reason().is_some() {
            continue;
        }
        if m.participants().any(|a| emails.contains(&a.email)) {
            latest.insert(m.thread.as_str(), m);
        }
    }
    let mut v: Vec<&MailMessage> = latest.into_values().collect();
    v.sort_by_key(|m| std::cmp::Reverse(m.date));
    v.truncate(3);
    v
}

/// Up to 3 non-mail, non-calendar items matching the event title.
fn related_notes(ctx: &GenContext<'_>, ev: &CalEvent) -> Vec<EvidenceRef> {
    let Some(store) = ctx.store else {
        return Vec::new();
    };
    let hits = match store.search_text(&ev.title, 12) {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(error = %e, "related-notes search failed");
            return Vec::new();
        }
    };
    let mut out: Vec<EvidenceRef> = Vec::new();
    for h in hits {
        if h.item == ev.item
            || ctx.corpus.is_mail_item(&h.item)
            || ctx.corpus.is_event_item(&h.item)
            || out.iter().any(|o| o.item == h.item)
        {
            continue;
        }
        out.push(EvidenceRef {
            item: h.item,
            quote: Some(truncate(
                &h.title
                    .map_or(h.snippet.clone(), |t| format!("{t}: {}", h.snippet)),
                QUOTE_CHARS,
            )),
            url: None,
        });
        if out.len() == 3 {
            break;
        }
    }
    out
}

// ---- renewals ----

const RENEWAL_WORDS: &[&str] = &[
    "renew",
    "renewal",
    "auto-renew",
    "subscription",
    "will be charged",
    "next billing",
    "next payment",
    "billing date",
    "trial ends",
    "trial will end",
    "trial expires",
    "free trial",
];

/// Receipts and notices with an upcoming renewal, charge or trial end (amount and date parsed).
pub fn renewals(ctx: &GenContext<'_>) -> Vec<Candidate> {
    let horizon = ctx.now + Duration::days(ctx.cfg.renewal_horizon_days);
    let mut out: BTreeMap<String, Candidate> = BTreeMap::new();
    for m in &ctx.corpus.mail {
        if m.from_user
            || ctx.days_ago(m.date) > ctx.cfg.renewal_lookback_days as f64
            || m.date > ctx.now
        {
            continue;
        }
        let text = format!("{}\n{}", m.subject, own_text(&m.body));
        let lower = text.to_lowercase();
        if !RENEWAL_WORDS.iter().any(|w| lower.contains(w)) {
            continue;
        }
        let trial = lower.contains("trial");
        // The sentence that names the renewal carries its date.
        let sents = sentences(&text);
        let key_sents: Vec<&String> = sents
            .iter()
            .filter(|s| {
                let l = s.to_lowercase();
                RENEWAL_WORDS.iter().any(|w| l.contains(w))
            })
            .collect();
        let written = m.date.date();
        let date = key_sents.iter().find_map(|s| {
            find_dates(s, written)
                .into_iter()
                .find(|d| *d >= written)
                .or_else(|| relative_days(s, written))
        });
        let Some(date) = date else { continue };
        let Ok(at) = Time::from_hms(9, 0, 0)
            .map(|t| PrimitiveDateTime::new(date, t).assume_offset(ctx.cfg.utc_offset))
        else {
            continue;
        };
        if at < ctx.now - Duration::days(1) || at > horizon {
            continue;
        }
        let amount = key_sents
            .iter()
            .find_map(|s| find_amounts(s).into_iter().next())
            .or_else(|| find_amounts(&text).into_iter().next());
        let vendor = m
            .from
            .as_ref()
            .map(|f| {
                f.name.clone().unwrap_or_else(|| {
                    let d = f.domain();
                    let parts: Vec<&str> = d.split('.').collect();
                    parts
                        .len()
                        .checked_sub(2)
                        .map_or(d, |i| parts[i])
                        .to_owned()
                })
            })
            .unwrap_or_else(|| "Subscription".into());
        let vendor_key = m
            .from
            .as_ref()
            .map_or(vendor.clone(), |f| f.domain().to_owned());
        let dedup_key = format!(
            "renewal:{}",
            stable_hash(&[
                &vendor_key,
                &date.to_string(),
                if trial { "trial" } else { "renewal" }
            ])
        );
        let money = amount
            .as_ref()
            .map(|a| format!(" — {a}"))
            .unwrap_or_default();
        let title = if trial {
            format!("{vendor} trial ends {date}{money}")
        } else {
            format!("{vendor} renews {date}{money}")
        };
        let quote = key_sents.first().map(|s| truncate(s, QUOTE_CHARS));
        let remind = (at - Duration::days(1)).max(ctx.now + Duration::hours(1));
        let cand = Candidate {
            dedup_key: dedup_key.clone(),
            kind: CandidateKind::Renewal,
            title,
            detail: if trial {
                "Cancel before it ends if you don't want to be charged.".into()
            } else {
                "Keep it, or cancel before the charge.".into()
            },
            evidence: vec![evidence(m, quote)],
            people: m.from.iter().map(|f| f.email.clone()).collect(),
            due: Some(at),
            expires_at: Some(at + Duration::days(1)),
            features: Features {
                urgency: urgency(Some(at), ctx.now),
                importance: (amount.as_ref().map_or(0.0, |a| a.minor as f64) / 10_000.0).min(1.0)
                    * 0.6
                    + 0.2,
                freshness: freshness(m.date, ctx.now),
                direct: 1.0,
                question: 0.0,
                kind: Some(CandidateKind::Renewal),
            },
            actions: vec![Action::Reminder(Reminder {
                text: format!(
                    "{} {vendor}{money}",
                    if trial { "Trial ends:" } else { "Renewal:" }
                ),
                at: remind,
                about: Some(m.item.clone()),
            })],
            sensitive: m.sensitive,
        };
        // Several notices for the same renewal: keep the most recent.
        out.insert(dedup_key, cand);
    }
    out.into_values().collect()
}

// ---- follow-ups ----

/// Promises the user made in sent mail ("I'll send…") with no later message from the user in
/// that thread.
pub fn follow_ups(ctx: &GenContext<'_>) -> Vec<Candidate> {
    let c = ctx.corpus;
    let mut out = Vec::new();
    for (i, m) in c.mail.iter().enumerate() {
        if !m.from_user || m.date > ctx.now {
            continue;
        }
        let age = ctx.days_ago(m.date);
        if age > ctx.cfg.followup_max_days as f64 {
            continue;
        }
        let promises = find_promises(&m.body);
        let Some(promise) = promises.first() else {
            continue;
        };
        let followed = c
            .thread_of(i)
            .iter()
            .any(|&j| c.mail[j].from_user && c.mail[j].date > m.date);
        if followed {
            continue;
        }
        let due = deadline_in(promise, m.date.date()).and_then(|d| {
            Time::from_hms(17, 0, 0)
                .ok()
                .map(|t| PrimitiveDateTime::new(d, t).assume_offset(ctx.cfg.utc_offset))
        });
        // Without a deadline, a promise becomes worth a nudge after a day.
        if due.is_none() && age < 1.0 {
            continue;
        }
        let recipients: Vec<String> =
            m.to.iter()
                .filter(|a| !c.is_user(&a.email))
                .map(|a| a.email.clone())
                .collect();
        let who =
            m.to.iter()
                .find(|a| !c.is_user(&a.email))
                .map_or_else(|| "someone".to_owned(), |a| a.display());
        let quote = truncate(promise, QUOTE_CHARS);
        let mut actions = Vec::new();
        if !recipients.is_empty() {
            actions.push(Action::DraftReply(DraftReply {
                reply_to: m.item.clone(),
                in_reply_to_message_id: m.message_id.clone(),
                to: recipients.clone(),
                cc: Vec::new(),
                subject: reply_subject(&m.subject),
                intent: format!("Follow through on: \"{quote}\""),
                body: None,
            }));
        }
        actions.push(Action::Reminder(Reminder {
            text: quote.clone(),
            at: due
                .filter(|d| *d > ctx.now)
                .unwrap_or(ctx.now + Duration::hours(2)),
            about: Some(m.item.clone()),
        }));
        out.push(Candidate {
            dedup_key: format!(
                "followup:{}",
                stable_hash(&[m.item.source.as_str(), m.item.key.as_str(), promise])
            ),
            kind: CandidateKind::FollowUp,
            title: format!("You told {who}: \"{}\"", truncate(promise, 80)),
            detail: format!("In \"{}\" on {}.", m.subject, m.date.date()),
            evidence: vec![evidence(m, Some(quote))],
            people: recipients.clone(),
            due,
            expires_at: Some(
                due.map_or(m.date + Duration::days(ctx.cfg.followup_max_days), |d| {
                    d + Duration::days(7)
                }),
            ),
            features: Features {
                urgency: urgency(due, ctx.now).max(0.2),
                importance: ctx.importance(recipients.iter().map(String::as_str)),
                freshness: freshness(m.date, ctx.now),
                direct: 1.0,
                question: 0.0,
                kind: Some(CandidateKind::FollowUp),
            },
            actions,
            sensitive: m.sensitive,
        });
    }
    out
}
