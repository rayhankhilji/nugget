#![allow(dead_code)]

//! Seeded store and graph fixtures. "Now" is Wednesday 2026-10-28 09:00 UTC; the user is
//! `me@nugget.dev`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_core::Result;
use nugget_core::capability::Locality;
use nugget_core::ids::{ItemKey, ItemRef, ProviderId, SourceId};
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
use serde_json::json;
use time::format_description::well_known::Rfc3339;
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

pub fn mail_ref(key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new("gmail:me"),
        key: ItemKey::new(key),
    }
}

/// Puts mail, one per line: `key|from|to|subject|days ago|labels|reply to|flags|body`. An empty
/// `from` or `to` is the user, empty labels are `INBOX` (`SENT` when from the user), `reply to`
/// is the key of the message answered, flags are `unsub`, `bulk` and `sensitive`. Message ids
/// are `<key@test>`; `\n` in the body is a newline.
pub fn put_mails(store: &Store, table: &str) {
    for line in table.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.splitn(9, '|').collect();
        let [key, from, to, subject, days, labels, reply, flags, body] = f[..] else {
            panic!("bad mail line: {line}")
        };
        let from = if from.is_empty() { ME } else { from };
        let to: Vec<&str> = if to.is_empty() {
            vec![ME]
        } else {
            to.split(',').collect()
        };
        let labels: Vec<&str> = match labels {
            "" if from == ME => vec!["SENT"],
            "" => vec!["INBOX"],
            l => l.split(',').collect(),
        };
        let date = now() - Duration::seconds_f64(days.parse::<f64>().unwrap() * 86_400.0);
        let body = body.replace("\\n", "\n");
        let mut extracted = json!({
            "from": from,
            "to": to,
            "subject": subject,
            "date": date.format(&Rfc3339).unwrap(),
            "message_id": format!("<{key}@test>"),
        });
        if !reply.is_empty() {
            extracted["in_reply_to"] = json!([format!("<{reply}@test>")]);
            extracted["references"] = extracted["in_reply_to"].clone();
        }
        if flags.contains("unsub") {
            extracted["list_unsubscribe"] = json!("<mailto:u@list.test>");
        }
        if flags.contains("bulk") {
            extracted["headers"] = json!({"Precedence": "bulk"});
        }
        let meta = json!({
            "source": {"labels": labels},
            "mime": "message/rfc822",
            "extracted": extracted,
            "sensitive": flags.contains("sensitive"),
            "url": format!("https://mail.example/{key}"),
        });
        let item = NewItem::new("gmail:me", key, content_hash(format!("{key}{body}")))
            .with_title(subject)
            .with_mime("message/rfc822")
            .with_text(body)
            .with_modified_at(date)
            .with_metadata(meta);
        store.upsert_item(&item).unwrap();
    }
}

pub fn put_event(
    store: &Store,
    key: &str,
    title: &str,
    start: OffsetDateTime,
    attendees: &[&str],
) -> ItemRef {
    let fmt = |t: OffsetDateTime| t.format(&Rfc3339).unwrap();
    let people: Vec<_> = attendees
        .iter()
        .map(|a| json!({"email": a, "name": null, "resource": false}))
        .collect();
    let meta = json!({
        "source": {
            "event_id": key,
            "ical_uid": format!("{key}@google.com"),
            "status": "confirmed",
            "start": {"date_time": fmt(start)},
            "end": {"date_time": fmt(start + Duration::hours(1))},
            "attendees": people,
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

/// Item refs of the fixture, by role.
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

/// Ana's unanswered question (the fixture's only unanswered thread).
pub const ANA: &str = "m-ana-1|Ana Ruiz <ana@acme.io>||Pricing for the pilot|3|||\
|Hi,\\n\\nCould you send over the pricing for the pilot? We'd like to decide this week.\\n\\nThanks, Ana";

/// The full fixture: an unanswered and an answered thread, newsletters and automated mail
/// (never cards), mail too old and too fresh, a sent promise and a kept one, a renewal and a
/// trial (each after an earlier mail from the vendor) and a past receipt, events (external
/// tomorrow, internal, in 5 days), a note and graph commitments.
pub fn fixture() -> Fixture {
    let store = Store::open_in_memory().unwrap();
    put_mails(&store, ANA);
    put_mails(
        &store,
        "\
m-bob-1|Bob Lee <bob@globex.com>||Lunch next week?|4||||Are you free for lunch next Tuesday?
m-bob-2||bob@globex.com|Re: Lunch next week?|3.5||m-bob-1||Tuesday works, see you then.\\n\\nOn Sat, Bob wrote:\\n> Are you free
m-news-1|The Weekly <newsletter@weekly.news>||This week in AI|2|||unsub|Top stories... Unsubscribe here.
m-news-2|Deals <deals@shop.example>||Your exclusive offer?|2|INBOX,CATEGORY_PROMOTIONS|||Don't miss out, can you believe these prices?
m-noreply|GitHub <noreply@github.com>||[repo] New issue opened|2||||Someone opened an issue. Can you take a look?
m-bulk|Team Updates <carol@bigco.com>||Org update|2|||bulk|Please read the attached update?
m-old|Dan Wu <dan@initech.com>||Old question|20||||Any update?
m-fresh|Eve Park <eve@initech.com>||Quick one|0.1||||Got a minute?
m-promise||Frank Ode <frank@partner.org>|Partnership deck|2||||Great talking today. I'll send the deck by Friday.\\n\\nBest
m-kept-1||gina@partner.org|Notes|5||||I'll share my notes tomorrow.
m-kept-2||gina@partner.org|Re: Notes|4||m-kept-1||Here are my notes.
m-cloudy-receipt|Cloudy Billing <billing@cloudy.io>||Your receipt from Cloudy|26||||Thanks for your purchase. Pro plan, monthly: $49.00.
m-streamco-welcome|StreamCo <no-reply@streamco.tv>||Welcome to StreamCo|11||||Your account is ready. Enjoy watching.
m-renew|Cloudy Billing <billing@cloudy.io>||Your subscription renews soon|1|||unsub|Your Pro plan will renew on November 2, 2026. You will be charged $49.00 to the card on file.
m-trial|StreamCo <no-reply@streamco.tv>||Your free trial ends in 3 days|0.5||||Your free trial ends in 3 days. After that, your plan is €9,99 per month.
m-receipt-old|Shop <orders@shop.example>||Your receipt|27||||Thanks for your order of $20.00 on October 1, 2026.",
    );
    let room = "room-1@resource.calendar.google.com";
    let kickoff = now() + Duration::hours(26);
    let meeting = put_event(
        &store,
        "ev-acme",
        "Acme pilot kickoff",
        kickoff,
        &[ME, "ana@acme.io", room],
    );
    let standup = now() + Duration::hours(2);
    put_event(
        &store,
        "ev-standup",
        "Standup",
        standup,
        &[ME, "zed@nugget.dev"],
    );
    let review = now() + Duration::days(5);
    put_event(
        &store,
        "ev-later",
        "Quarterly review",
        review,
        &[ME, "ana@acme.io"],
    );
    let note_text = "Acme pilot: they need SSO and pricing by end of month.";
    let note = put_note(&store, "acme.md", "Acme pilot notes", note_text);
    seed_graph(&store);
    Fixture {
        store,
        unanswered: mail_ref("m-ana-1"),
        answered_inbound: mail_ref("m-bob-1"),
        promise: mail_ref("m-promise"),
        renewal: mail_ref("m-renew"),
        trial: mail_ref("m-trial"),
        meeting,
        note,
    }
}

/// Graph: the user, Ana (strong relationship), and commitments: mine due in 2 days, Ana's
/// overdue, one outside the horizon and a done one.
pub fn seed_graph(store: &Store) {
    let g = store.graph();
    let person = |name: &str, email: &str| {
        let mut e = NewEntity::new(EntityKind::Person, name);
        e.identifiers.push(Identifier::new("email", email));
        g.create_entity(&e).unwrap()
    };
    let me = person("Me Myself", ME);
    let ana = person("Ana Ruiz", "ana@acme.io");
    let claim = |s: &nugget_core::ids::EntityId, p: &str, o: ClaimObject, text: &str, q: &str| {
        let evidence = Evidence {
            item: mail_ref("m-ana-1"),
            chunk_id: None,
            quote: Some(q.into()),
        };
        g.add_claim(&NewClaim {
            subject: s.clone(),
            predicate: p.into(),
            object: o,
            text: text.into(),
            confidence: 0.9,
            status: ClaimStatus::Active,
            evidence: vec![evidence],
            valid_from: None,
            valid_to: None,
        })
        .unwrap();
    };
    let lit = |s: &str| ClaimObject::Literal(s.into());
    for i in 0..6 {
        claim(
            &ana,
            "works_on",
            lit(&format!("project {i}")),
            "Ana works on the pilot",
            "pilot",
        );
    }
    let commitment = |title: &str| {
        g.create_entity(&NewEntity::new(EntityKind::Commitment, title))
            .unwrap()
    };
    let c1 = commitment("Send SOC 2 report to Acme");
    let c1o = ClaimObject::Entity(c1.clone());
    claim(
        &me,
        "committed_to",
        c1o,
        "I committed to send the SOC 2 report",
        "send the SOC 2 report",
    );
    claim(
        &c1,
        "due_on",
        lit("2026-10-30"),
        "Due on 2026-10-30",
        "by Oct 30",
    );
    let c2 = commitment("Signed pilot agreement");
    let c2o = ClaimObject::Entity(c2.clone());
    claim(
        &ana,
        "owes",
        c2o,
        "Ana owes the signed agreement",
        "we'll sign",
    );
    claim(
        &c2,
        "due_on",
        lit("October 20, 2026"),
        "Due October 20",
        "by Oct 20",
    );
    let c3 = commitment("Annual plan");
    claim(&c3, "due_on", lit("2027-03-01"), "Due March", "March");
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
        let decision = self.decision.lock().unwrap().clone();
        decision.unwrap_or(Decision::Allow)
    }
    fn halted(&self) -> bool {
        *self.halted.lock().unwrap()
    }
}

/// Every executor; records `verb:what` per call.
#[derive(Default)]
pub struct RecordingExec {
    pub calls: Mutex<Vec<String>>,
}

impl RecordingExec {
    fn log(&self, verb: &str, what: &str, out: &str) -> Result<String> {
        self.calls.lock().unwrap().push(format!("{verb}:{what}"));
        Ok(out.into())
    }
}

#[async_trait]
impl Drafter for RecordingExec {
    async fn draft_reply(&self, req: &DraftRequest) -> Result<String> {
        self.log("draft", &req.subject, "")?;
        Ok(format!("Hi,\n\nHere it is.\n\n(re: {})", req.subject))
    }
}

#[async_trait]
impl MailExecutor for RecordingExec {
    async fn save_draft(&self, d: &DraftReply) -> Result<String> {
        self.log("save_draft", &d.subject, "draft-1")
    }
    async fn archive_thread(&self, a: &ArchiveThread) -> Result<String> {
        self.log("archive", &a.subject, "archived")
    }
}

#[async_trait]
impl CalendarExecutor for RecordingExec {
    async fn create_event(&self, e: &CreateEvent) -> Result<String> {
        self.log("event", &e.title, "ev-1")
    }
}

#[async_trait]
impl ReminderExecutor for RecordingExec {
    async fn create_reminder(&self, r: &Reminder) -> Result<String> {
        self.log("reminder", &r.text, "rem-1")
    }
}

#[async_trait]
impl NoteExecutor for RecordingExec {
    async fn write_note(&self, n: &ResearchNote) -> Result<String> {
        self.log("note", &n.title, "notes/prep.md")
    }
}

/// A chat provider that answers with a fixed text and records requests.
pub struct FakeProvider {
    pub id: ProviderId,
    pub locality: Locality,
    pub reply: String,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl FakeProvider {
    pub fn new(locality: Locality, reply: &str) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new("fake"),
            locality,
            reply: reply.into(),
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
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ChatEvent::TextDelta(self.reply.clone())),
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
