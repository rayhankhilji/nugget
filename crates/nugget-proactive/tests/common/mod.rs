#![allow(dead_code)]

//! Seeded store and graph fixtures. "Now" is Wednesday 2026-10-28 09:00 UTC; the user is
//! `me@nugget.dev`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_core::Result;
use nugget_core::capability::Locality;
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, Evidence, GraphStore, Identifier, NewClaim, NewEntity,
};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, StopReason,
};
use nugget_policy::{ActionDescriptor, Decision, PolicyGate};
use nugget_proactive::{
    ArchiveThread, CalendarExecutor, CreateEvent, DraftReply, DraftRequest, Drafter, MailExecutor,
    NoteExecutor, ProactiveConfig, Reminder, ReminderExecutor, ResearchNote,
};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde_json::{Value, json};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

pub const ME: &str = "me@nugget.dev";

pub fn now() -> OffsetDateTime {
    datetime!(2026-10-28 09:00 UTC)
}

pub fn cfg() -> ProactiveConfig {
    ProactiveConfig {
        user_addresses: vec![ME.into()],
        ..Default::default()
    }
}

pub struct Mail<'a> {
    pub key: &'a str,
    pub from: &'a str,
    pub to: &'a [&'a str],
    pub subject: &'a str,
    pub body: &'a str,
    pub days_ago: f64,
    pub message_id: &'a str,
    pub in_reply_to: Option<&'a str>,
    pub labels: &'a [&'a str],
    pub extra: Value,
    pub sensitive: bool,
}

impl Default for Mail<'_> {
    fn default() -> Self {
        Self {
            key: "",
            from: "",
            to: &[ME],
            subject: "",
            body: "",
            days_ago: 2.0,
            message_id: "",
            in_reply_to: None,
            labels: &["INBOX"],
            extra: json!({}),
            sensitive: false,
        }
    }
}

pub fn put_mail(store: &Store, m: Mail<'_>) -> ItemRef {
    let date = now() - Duration::seconds_f64(m.days_ago * 86_400.0);
    let mut extracted = json!({
        "from": m.from,
        "to": m.to,
        "subject": m.subject,
        "date": date.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "message_id": m.message_id,
    });
    if let Some(r) = m.in_reply_to {
        extracted["in_reply_to"] = json!([r]);
        extracted["references"] = json!([r]);
    }
    if let Value::Object(extra) = &m.extra {
        for (k, v) in extra {
            extracted[k] = v.clone();
        }
    }
    let meta = json!({
        "source": {"labels": m.labels},
        "mime": "message/rfc822",
        "extracted": extracted,
        "sensitive": m.sensitive,
        "url": format!("https://mail.example/{}", m.key),
    });
    let item = NewItem::new(
        "gmail:me",
        m.key,
        content_hash(format!("{}{}", m.key, m.body)),
    )
    .with_title(m.subject)
    .with_mime("message/rfc822")
    .with_text(m.body)
    .with_modified_at(date)
    .with_metadata(meta);
    store.upsert_item(&item).unwrap();
    item.item
}

pub fn put_event(
    store: &Store,
    key: &str,
    title: &str,
    start: OffsetDateTime,
    attendees: &[&str],
) -> ItemRef {
    let fmt = |t: OffsetDateTime| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    };
    let meta = json!({
        "source": {
            "event_id": key,
            "ical_uid": format!("{key}@google.com"),
            "status": "confirmed",
            "start": {"date_time": fmt(start)},
            "end": {"date_time": fmt(start + Duration::hours(1))},
            "attendees": attendees.iter().map(|a| json!({"email": a, "name": null, "resource": false})).collect::<Vec<_>>(),
            "location": "Zoom",
        },
        "mime": "text/plain",
        "sensitive": false,
    });
    let item = NewItem::new("gcal:me", key, content_hash(key))
        .with_title(title)
        .with_mime("text/plain")
        .with_text(format!("{title}\nAttendees: {}", attendees.join(", ")))
        .with_metadata(meta);
    store.upsert_item(&item).unwrap();
    item.item
}

pub fn put_note(store: &Store, key: &str, title: &str, text: &str) -> ItemRef {
    let item = NewItem::new("files:notes", key, content_hash(text))
        .with_title(title)
        .with_mime("text/markdown")
        .with_text(text)
        .with_metadata(json!({"mime": "text/markdown", "sensitive": false}));
    store
        .upsert_item_with_chunks(&item, &[NewChunk::text(text)])
        .unwrap();
    item.item
}

/// Item keys of the fixture, by role.
pub struct Fixture {
    pub store: Store,
    pub unanswered: ItemRef,
    pub answered_inbound: ItemRef,
    pub promise: ItemRef,
    pub renewal: ItemRef,
    pub trial: ItemRef,
    pub meeting: ItemRef,
    pub note: ItemRef,
}

/// The full fixture: mail threads (answered and unanswered), newsletters and automated mail,
/// events, receipts, a sent promise, a note, and graph commitments.
pub fn fixture() -> Fixture {
    let store = Store::open_in_memory().unwrap();

    // 1. Unanswered: Ana asks a question 3 days ago.
    let unanswered = put_mail(
        &store,
        Mail {
            key: "m-ana-1",
            from: "Ana Ruiz <ana@acme.io>",
            subject: "Pricing for the pilot",
            body: "Hi,\n\nCould you send over the pricing for the pilot? We'd like to decide this week.\n\nThanks, Ana",
            days_ago: 3.0,
            message_id: "<ana-1@acme.io>",
            ..Default::default()
        },
    );

    // 2. Answered: Bob wrote, the user replied.
    let answered_inbound = put_mail(
        &store,
        Mail {
            key: "m-bob-1",
            from: "Bob Lee <bob@globex.com>",
            subject: "Lunch next week?",
            body: "Are you free for lunch next Tuesday?",
            days_ago: 4.0,
            message_id: "<bob-1@globex.com>",
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "m-bob-2",
            from: ME,
            to: &["bob@globex.com"],
            subject: "Re: Lunch next week?",
            body: "Tuesday works, see you then.\n\nOn Sat, Bob wrote:\n> Are you free",
            days_ago: 3.5,
            message_id: "<me-bob-2@nugget.dev>",
            in_reply_to: Some("<bob-1@globex.com>"),
            labels: &["SENT"],
            ..Default::default()
        },
    );

    // 3. Newsletters / automated mail that must never become unanswered cards.
    put_mail(
        &store,
        Mail {
            key: "m-news-1",
            from: "The Weekly <newsletter@weekly.news>",
            subject: "This week in AI",
            body: "Top stories... Unsubscribe here.",
            days_ago: 2.0,
            message_id: "<n1@weekly.news>",
            extra: json!({"list_unsubscribe": "<mailto:u@weekly.news>"}),
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "m-news-2",
            from: "Deals <deals@shop.example>",
            subject: "Your exclusive offer?",
            body: "Don't miss out, can you believe these prices?",
            days_ago: 2.0,
            message_id: "<n2@shop.example>",
            labels: &["INBOX", "CATEGORY_PROMOTIONS"],
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "m-noreply",
            from: "GitHub <noreply@github.com>",
            subject: "[repo] New issue opened",
            body: "Someone opened an issue. Can you take a look?",
            days_ago: 2.0,
            message_id: "<gh@github.com>",
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "m-bulk",
            from: "Team Updates <carol@bigco.com>",
            subject: "Org update",
            body: "Please read the attached update?",
            days_ago: 2.0,
            message_id: "<bulk@bigco.com>",
            extra: json!({"headers": {"Precedence": "bulk"}}),
            ..Default::default()
        },
    );
    // Too old (20 days) and too fresh (2 hours).
    put_mail(
        &store,
        Mail {
            key: "m-old",
            from: "Dan Wu <dan@initech.com>",
            subject: "Old question",
            body: "Any update?",
            days_ago: 20.0,
            message_id: "<old@initech.com>",
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "m-fresh",
            from: "Eve Park <eve@initech.com>",
            subject: "Quick one",
            body: "Got a minute?",
            days_ago: 0.1,
            message_id: "<fresh@initech.com>",
            ..Default::default()
        },
    );

    // 4. Promise in sent mail, no follow-through: "I'll send the deck by Friday."
    let promise = put_mail(
        &store,
        Mail {
            key: "m-promise",
            from: ME,
            to: &["Frank Ode <frank@partner.org>"],
            subject: "Partnership deck",
            body: "Great talking today. I'll send the deck by Friday.\n\nBest",
            days_ago: 2.0,
            message_id: "<me-promise@nugget.dev>",
            labels: &["SENT"],
            ..Default::default()
        },
    );
    // A kept promise (a later sent message in the same thread).
    put_mail(
        &store,
        Mail {
            key: "m-kept-1",
            from: ME,
            to: &["gina@partner.org"],
            subject: "Notes",
            body: "I'll share my notes tomorrow.",
            days_ago: 5.0,
            message_id: "<me-kept-1@nugget.dev>",
            labels: &["SENT"],
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "m-kept-2",
            from: ME,
            to: &["gina@partner.org"],
            subject: "Re: Notes",
            body: "Here are my notes.",
            days_ago: 4.0,
            message_id: "<me-kept-2@nugget.dev>",
            in_reply_to: Some("<me-kept-1@nugget.dev>"),
            labels: &["SENT"],
            ..Default::default()
        },
    );

    // 5. Receipts: a renewal in 5 days, a trial ending in 3 days, a plain past receipt.
    let renewal = put_mail(
        &store,
        Mail {
            key: "m-renew",
            from: "Cloudy Billing <billing@cloudy.io>",
            subject: "Your subscription renews soon",
            body: "Your Pro plan will renew on November 2, 2026. You will be charged $49.00 to the card on file.",
            days_ago: 1.0,
            message_id: "<renew@cloudy.io>",
            extra: json!({"list_unsubscribe": "<mailto:x@cloudy.io>"}),
            ..Default::default()
        },
    );
    let trial = put_mail(
        &store,
        Mail {
            key: "m-trial",
            from: "StreamCo <no-reply@streamco.tv>",
            subject: "Your free trial ends in 3 days",
            body: "Your free trial ends in 3 days. After that, your plan is €9,99 per month.",
            days_ago: 0.5,
            message_id: "<trial@streamco.tv>",
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "m-receipt-old",
            from: "Shop <orders@shop.example>",
            subject: "Your receipt",
            body: "Thanks for your order of $20.00 on October 1, 2026.",
            days_ago: 27.0,
            message_id: "<receipt@shop.example>",
            ..Default::default()
        },
    );

    // 6. Events: an external meeting tomorrow, an internal one, one in 5 days, a cancelled one.
    let meeting = put_event(
        &store,
        "ev-acme",
        "Acme pilot kickoff",
        now() + Duration::hours(26),
        &[ME, "ana@acme.io", "room-1@resource.calendar.google.com"],
    );
    put_event(
        &store,
        "ev-standup",
        "Standup",
        now() + Duration::hours(2),
        &[ME, "zed@nugget.dev"],
    );
    put_event(
        &store,
        "ev-later",
        "Quarterly review",
        now() + Duration::days(5),
        &[ME, "ana@acme.io"],
    );
    let note = put_note(
        &store,
        "acme.md",
        "Acme pilot notes",
        "Acme pilot: they need SSO and pricing by end of month.",
    );

    seed_graph(&store);

    Fixture {
        store,
        unanswered,
        answered_inbound,
        promise,
        renewal,
        trial,
        meeting,
        note,
    }
}

/// Graph: the user, Ana (strong relationship), and commitments.
pub fn seed_graph(store: &Store) {
    let g = store.graph();
    let mut me = NewEntity::new(EntityKind::Person, "Me Myself");
    me.identifiers.push(Identifier::new("email", ME));
    let me = g.create_entity(&me).unwrap();
    let mut ana = NewEntity::new(EntityKind::Person, "Ana Ruiz");
    ana.identifiers
        .push(Identifier::new("email", "ana@acme.io"));
    let ana = g.create_entity(&ana).unwrap();
    let ev = |key: &str, quote: &str| Evidence {
        item: ItemRef {
            source: nugget_core::ids::SourceId::new("gmail:me"),
            key: nugget_core::ids::ItemKey::new(key),
        },
        chunk_id: None,
        quote: Some(quote.into()),
    };
    let claim =
        |s: &nugget_core::ids::EntityId, p: &str, o: ClaimObject, text: &str, e: Evidence| {
            NewClaim {
                subject: s.clone(),
                predicate: p.into(),
                object: o,
                text: text.into(),
                confidence: 0.9,
                status: ClaimStatus::Active,
                evidence: vec![e],
                valid_from: None,
                valid_to: None,
            }
        };
    for i in 0..6 {
        g.add_claim(&claim(
            &ana,
            "works_on",
            ClaimObject::Literal(format!("project {i}")),
            "Ana works on the pilot",
            ev("m-ana-1", "pilot"),
        ))
        .unwrap();
    }

    // My commitment, due in 2 days.
    let c1 = g
        .create_entity(&NewEntity::new(
            EntityKind::Commitment,
            "Send SOC 2 report to Acme",
        ))
        .unwrap();
    g.add_claim(&claim(
        &me,
        "committed_to",
        ClaimObject::Entity(c1.clone()),
        "I committed to send the SOC 2 report",
        ev("m-ana-1", "send the SOC 2 report"),
    ))
    .unwrap();
    g.add_claim(&claim(
        &c1,
        "due_on",
        ClaimObject::Literal("2026-10-30".into()),
        "Due on 2026-10-30",
        ev("m-ana-1", "by Oct 30"),
    ))
    .unwrap();

    // Overdue commitment owed by Ana.
    let c2 = g
        .create_entity(&NewEntity::new(
            EntityKind::Commitment,
            "Signed pilot agreement",
        ))
        .unwrap();
    g.add_claim(&claim(
        &ana,
        "owes",
        ClaimObject::Entity(c2.clone()),
        "Ana owes the signed agreement",
        ev("m-ana-1", "we'll sign"),
    ))
    .unwrap();
    g.add_claim(&claim(
        &c2,
        "due_on",
        ClaimObject::Literal("October 20, 2026".into()),
        "Due October 20",
        ev("m-ana-1", "by Oct 20"),
    ))
    .unwrap();

    // Far-future commitment (outside the horizon) and a done one.
    let c3 = g
        .create_entity(&NewEntity::new(EntityKind::Commitment, "Annual plan"))
        .unwrap();
    g.add_claim(&claim(
        &c3,
        "due_on",
        ClaimObject::Literal("2027-03-01".into()),
        "Due March",
        ev("m-ana-1", "March"),
    ))
    .unwrap();
    let mut done = NewEntity::new(EntityKind::Commitment, "Old task");
    done.attributes = json!({"status": "done", "due": "2026-10-29"});
    g.create_entity(&done).unwrap();
}

// ---- fakes ----

#[derive(Default)]
pub struct RecordingPolicy {
    pub decision: Mutex<Option<Decision>>,
    pub halted: Mutex<bool>,
    pub seen: Mutex<Vec<ActionDescriptor>>,
}

impl PolicyGate for RecordingPolicy {
    fn evaluate(&self, action: &ActionDescriptor) -> Decision {
        self.seen.lock().unwrap().push(action.clone());
        self.decision
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Decision::Allow)
    }
    fn halted(&self) -> bool {
        *self.halted.lock().unwrap()
    }
}

#[derive(Default)]
pub struct RecordingExec {
    pub calls: Mutex<Vec<String>>,
}

#[async_trait]
impl Drafter for RecordingExec {
    async fn draft_reply(&self, req: &DraftRequest) -> Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("draft:{}", req.subject));
        Ok(format!("Hi,\n\nHere it is.\n\n(re: {})", req.subject))
    }
}

#[async_trait]
impl MailExecutor for RecordingExec {
    async fn save_draft(&self, d: &DraftReply) -> Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("save_draft:{}", d.subject));
        Ok("draft-1".into())
    }
    async fn archive_thread(&self, a: &ArchiveThread) -> Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("archive:{}", a.subject));
        Ok("archived".into())
    }
}

#[async_trait]
impl CalendarExecutor for RecordingExec {
    async fn create_event(&self, e: &CreateEvent) -> Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("event:{}", e.title));
        Ok("ev-1".into())
    }
}

#[async_trait]
impl ReminderExecutor for RecordingExec {
    async fn create_reminder(&self, r: &Reminder) -> Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("reminder:{}", r.text));
        Ok("rem-1".into())
    }
}

#[async_trait]
impl NoteExecutor for RecordingExec {
    async fn write_note(&self, n: &ResearchNote) -> Result<String> {
        self.calls.lock().unwrap().push(format!("note:{}", n.title));
        Ok("notes/prep.md".into())
    }
}

/// A chat provider that answers with a fixed text and records requests.
pub struct FakeProvider {
    pub id: ProviderId,
    pub locality: Locality,
    pub reply: Mutex<String>,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl FakeProvider {
    pub fn new(locality: Locality, reply: &str) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new("fake"),
            locality,
            reply: Mutex::new(reply.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "fake".into(),
            locality: self.locality,
            context_tokens: None,
        }
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        self.requests.lock().unwrap().push(req);
        let text = self.reply.lock().unwrap().clone();
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ChatEvent::TextDelta(text)),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ])))
    }
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(nugget_core::Error::Unsupported("embed".into()))
    }
}
