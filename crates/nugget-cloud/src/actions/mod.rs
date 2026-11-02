//! Outbound actions through Composio: send mail (Gmail, Outlook), reply in a thread, create or
//! update a calendar event, post a Slack message, create a Notion page.
//!
//! Nothing is sent without an explicit approval. An action is *proposed* (validated, turned
//! into the exact Composio call, assessed by `nugget-policy`, previewed), stored in
//! `pending_actions`, and only `POST /v1/actions/{id}/approve` with the preview's digest runs
//! it, once: approving twice, or retrying with the same idempotency key, never sends twice.
//! Every step is appended to the per-user hash chain in `action_audit` ([`store`]).
//!
//! Tool names and inputs are from Composio's toolkit pages (docs.composio.dev/toolkits/<slug>,
//! checked 2026-11-02). Outputs are only documented as `data`, so results are stored as given.

use std::fmt;

use nugget_policy::preview::display_text;
use nugget_policy::{
    ActionContext, ActionDescriptor, Decision, PolicyConfig, PolicyEngine, RiskClass,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

pub mod store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    SendMail,
    ReplyMail,
    CreateEvent,
    UpdateEvent,
    PostMessage,
    CreatePage,
}

impl ActionKind {
    pub const ALL: [ActionKind; 6] = [
        Self::SendMail,
        Self::ReplyMail,
        Self::CreateEvent,
        Self::UpdateEvent,
        Self::PostMessage,
        Self::CreatePage,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SendMail => "send_mail",
            Self::ReplyMail => "reply_mail",
            Self::CreateEvent => "create_event",
            Self::UpdateEvent => "update_event",
            Self::PostMessage => "post_message",
            Self::CreatePage => "create_page",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for ActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The input fields a user may change on a proposed action of `kind` (`PATCH
/// /v1/actions/{id}`). Where an action goes (thread, event, channel, parent page) is fixed at
/// proposal; only its content and recipients can be edited.
pub fn editable(kind: ActionKind) -> &'static [&'static str] {
    use ActionKind::*;
    match kind {
        SendMail => &["to", "cc", "bcc", "subject", "body"],
        ReplyMail => &["to", "body"],
        CreateEvent | UpdateEvent => &[
            "title",
            "start",
            "end",
            "timezone",
            "attendees",
            "location",
            "description",
        ],
        PostMessage => &["text"],
        CreatePage => &["title", "markdown"],
    }
}

/// `input` with the user's edit merged in. Only [`editable`] fields may be set; a `null`
/// removes an optional field.
pub fn merge_edit(kind: ActionKind, input: &Value, edit: &Value) -> Result<Value, Invalid> {
    let Some(changes) = edit.as_object() else {
        return Err(bad("the edit must be an object of fields"));
    };
    if changes.is_empty() {
        return Err(bad("nothing to change"));
    }
    let allowed = editable(kind);
    let mut merged = input.as_object().cloned().unwrap_or_default();
    for (k, v) in changes {
        if !allowed.contains(&k.as_str()) {
            return Err(bad(format!(
                "{k} cannot be edited on {kind}; editable: {}",
                allowed.join(", ")
            )));
        }
        if v.is_null() {
            merged.remove(k);
        } else {
            merged.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(merged))
}

/// The Composio tool that performs `kind` in `toolkit`, if this build supports it.
pub fn tool_for(kind: ActionKind, toolkit: &str) -> Option<&'static str> {
    use ActionKind::*;
    Some(match (kind, toolkit) {
        (SendMail, "gmail") => "GMAIL_SEND_EMAIL",
        (SendMail, "outlook") => "OUTLOOK_SEND_EMAIL",
        (ReplyMail, "gmail") => "GMAIL_REPLY_TO_THREAD",
        (ReplyMail, "outlook") => "OUTLOOK_REPLY_EMAIL",
        (CreateEvent, "googlecalendar") => "GOOGLECALENDAR_CREATE_EVENT",
        (CreateEvent, "outlook") => "OUTLOOK_CALENDAR_CREATE_EVENT",
        // PATCH changes only the fields sent; UPDATE_EVENT is a full replace that clears the rest.
        (UpdateEvent, "googlecalendar") => "GOOGLECALENDAR_PATCH_EVENT",
        (PostMessage, "slack") => "SLACK_SEND_MESSAGE",
        (CreatePage, "notion") => "NOTION_CREATE_NOTION_PAGE",
        _ => return None,
    })
}

// ---- inputs ---------------------------------------------------------------------------------------

const MAX_RECIPIENTS: usize = 50;
const MAX_SUBJECT: usize = 998;
const MAX_BODY: usize = 100_000;
const MAX_TITLE: usize = 500;
const MAX_ID: usize = 256;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMail {
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyMail {
    /// Gmail: the thread to reply in.
    pub thread_id: Option<String>,
    /// Outlook: the message to reply to.
    pub message_id: Option<String>,
    #[serde(default)]
    pub to: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFields {
    pub event_id: Option<String>,
    pub title: Option<String>,
    /// RFC 3339 (`2026-11-05T10:00:00Z`), or a local time without offset plus `timezone`.
    pub start: Option<String>,
    pub end: Option<String>,
    /// IANA zone (`Europe/London`) for local `start`/`end`.
    pub timezone: Option<String>,
    pub attendees: Option<Vec<String>>,
    pub location: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostMessage {
    pub channel: String,
    pub text: String,
    pub thread_ts: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePage {
    pub parent_id: String,
    pub title: String,
    pub markdown: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Invalid {
    #[error("{0}")]
    Bad(String),
    #[error("{kind} is not available for {toolkit}")]
    Unsupported { kind: String, toolkit: String },
}

fn bad(m: impl Into<String>) -> Invalid {
    Invalid::Bad(m.into())
}

fn check_email(e: &str) -> Result<String, Invalid> {
    let e = e.trim();
    let ok = e.len() <= 254
        && e.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')
        })
        && e.matches('@').count() == 1
        && !e
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || ",;<>\"()[]\\".contains(c));
    if ok {
        Ok(e.to_owned())
    } else {
        Err(bad(format!(
            "not an email address: {}",
            display_text(e, 80)
        )))
    }
}

fn check_emails(list: &[String], what: &str) -> Result<Vec<String>, Invalid> {
    if list.len() > MAX_RECIPIENTS {
        return Err(bad(format!("at most {MAX_RECIPIENTS} {what} recipients")));
    }
    list.iter().map(|e| check_email(e)).collect()
}

fn check_text(s: &str, what: &str, max: usize, required: bool) -> Result<String, Invalid> {
    if required && s.trim().is_empty() {
        return Err(bad(format!("{what} is empty")));
    }
    if s.chars().count() > max {
        return Err(bad(format!("{what} is longer than {max} characters")));
    }
    if s.chars().any(|c| c == '\0') {
        return Err(bad(format!("{what} contains a NUL character")));
    }
    Ok(s.to_owned())
}

/// Upstream ids (thread, message, event, channel, page): printable, no spaces or slashes.
fn check_id(s: &str, what: &str) -> Result<String, Invalid> {
    let s = s.trim();
    let ok = !s.is_empty()
        && s.len() <= MAX_ID
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.=:@#+".contains(c));
    if ok {
        Ok(s.to_owned())
    } else {
        Err(bad(format!("not a valid {what}")))
    }
}

fn check_zone(z: &str) -> Result<String, Invalid> {
    let ok = !z.is_empty()
        && z.len() <= 64
        && z.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c));
    if ok {
        Ok(z.to_owned())
    } else {
        Err(bad("not a time zone"))
    }
}

/// A point in time as the user gave it: an instant, or a wall-clock time in a named zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Instant(OffsetDateTime),
    Local(PrimitiveDateTime),
}

const LOCAL: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
const LOCAL_NO_SECS: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]");

fn parse_when(s: &str, zone: Option<&str>) -> Result<When, Invalid> {
    let s = s.trim();
    if let Ok(t) = OffsetDateTime::parse(s, &Rfc3339) {
        return Ok(When::Instant(t));
    }
    let local = PrimitiveDateTime::parse(s, LOCAL)
        .or_else(|_| PrimitiveDateTime::parse(s, LOCAL_NO_SECS))
        .map_err(|_| bad(format!("not a date and time: {}", display_text(s, 40))))?;
    if zone.is_none() {
        return Err(bad(
            "a time without an offset needs a timezone (or use RFC 3339)",
        ));
    }
    Ok(When::Local(local))
}

impl When {
    /// `YYYY-MM-DDTHH:MM:SS` and the zone it is in (UTC for an instant).
    fn wall(self, zone: Option<&str>) -> (String, String) {
        match self {
            When::Instant(t) => {
                let u = t.to_offset(UtcOffset::UTC);
                (
                    PrimitiveDateTime::new(u.date(), u.time())
                        .format(LOCAL)
                        .unwrap_or_default(),
                    "UTC".into(),
                )
            }
            When::Local(p) => (
                p.format(LOCAL).unwrap_or_default(),
                zone.unwrap_or("UTC").to_owned(),
            ),
        }
    }

    fn rfc3339(self) -> Option<String> {
        match self {
            When::Instant(t) => t.format(&Rfc3339).ok(),
            When::Local(_) => None,
        }
    }

    fn show(self, zone: Option<&str>) -> String {
        let (w, z) = self.wall(zone);
        format!("{} {z}", w.replace('T', " "))
    }

    /// Comparable when both are of the same kind (both instants, or both local in one zone).
    fn before(self, other: When) -> Option<bool> {
        match (self, other) {
            (When::Instant(a), When::Instant(b)) => Some(a < b),
            (When::Local(a), When::Local(b)) => Some(a < b),
            _ => None,
        }
    }
}

// ---- the planned call -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// What the user sees before approving: the fields that go out, and for an update what changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub fields: Vec<Field>,
    pub diff: Vec<Change>,
}

/// The exact Composio call an approval releases, and how it is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub kind: ActionKind,
    pub toolkit: String,
    pub tool: &'static str,
    pub arguments: Value,
    pub summary: String,
    pub preview: Preview,
    /// For the policy engine: who or what it goes to.
    pub target_label: String,
}

/// Known values of an event before an update (from the synced item), for the diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventBefore {
    pub title: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub location: Option<String>,
    pub attendees: Option<String>,
}

const SHOW: usize = 200;

fn field(label: &str, value: &str) -> Field {
    Field {
        label: label.into(),
        value: display_text(value, SHOW),
    }
}

fn list_preview(v: &[String]) -> String {
    v.join(", ")
}

fn input<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, Invalid> {
    if !v.is_object() {
        return Err(bad("input must be an object"));
    }
    serde_json::from_value(v.clone()).map_err(|e| bad(format!("bad input: {e}")))
}

fn strip_nulls(mut v: Value) -> Value {
    if let Value::Object(m) = &mut v {
        m.retain(|_, x| !x.is_null());
    }
    v
}

/// Validates `input` for `kind` in `toolkit` and plans the call. `before` is the event as last
/// synced (update_event only).
pub fn plan(
    kind: ActionKind,
    toolkit: &str,
    input_v: &Value,
    before: Option<&EventBefore>,
) -> Result<Plan, Invalid> {
    let tool = tool_for(kind, toolkit).ok_or_else(|| Invalid::Unsupported {
        kind: kind.as_str().into(),
        toolkit: toolkit.into(),
    })?;
    let mut diff = Vec::new();
    let (arguments, summary, fields, target): (Value, String, Vec<Field>, String) = match kind {
        ActionKind::SendMail => {
            let i: SendMail = input(input_v)?;
            let to = check_emails(&i.to, "to")?;
            let cc = check_emails(&i.cc, "cc")?;
            let bcc = check_emails(&i.bcc, "bcc")?;
            if to.is_empty() {
                return Err(bad("add at least one recipient"));
            }
            let subject = check_text(&i.subject, "subject", MAX_SUBJECT, false)?;
            if subject.contains(['\r', '\n']) {
                return Err(bad("the subject must be one line"));
            }
            let body = check_text(&i.body, "body", MAX_BODY, true)?;
            let args = if toolkit == "gmail" {
                json!({"recipient_email": to[0], "extra_recipients": &to[1..], "cc": cc, "bcc": bcc,
                       "subject": subject, "body": body, "is_html": false})
            } else {
                json!({"to": to.join(","), "cc_emails": cc, "bcc_emails": bcc, "subject": subject,
                       "body": body, "is_html": false})
            };
            let mut f = vec![field("To", &list_preview(&to))];
            if !cc.is_empty() {
                f.push(field("Cc", &list_preview(&cc)));
            }
            if !bcc.is_empty() {
                f.push(field("Bcc", &list_preview(&bcc)));
            }
            f.push(field("Subject", &subject));
            f.push(field("Message", &body));
            let all: Vec<String> = to.iter().chain(&cc).chain(&bcc).cloned().collect();
            (
                args,
                format!(
                    "Send an email to {}: “{}”",
                    display_text(&list_preview(&to), 120),
                    display_text(&subject, 80)
                ),
                f,
                all.join(", "),
            )
        }
        ActionKind::ReplyMail => {
            let i: ReplyMail = input(input_v)?;
            let body = check_text(&i.body, "body", MAX_BODY, true)?;
            let to = check_emails(&i.to, "to")?;
            if toolkit == "gmail" {
                let thread = check_id(i.thread_id.as_deref().unwrap_or(""), "thread id")?;
                if i.message_id.is_some() {
                    return Err(bad("Gmail replies take thread_id, not message_id"));
                }
                // The tool needs at least one recipient.
                if to.is_empty() {
                    return Err(bad("add the recipient of the reply"));
                }
                (
                    json!({"thread_id": thread, "message_body": body, "recipient_email": to[0],
                           "extra_recipients": &to[1..], "is_html": false}),
                    format!(
                        "Reply in a Gmail thread to {}",
                        display_text(&list_preview(&to), 120)
                    ),
                    vec![
                        field("Thread", &thread),
                        field("To", &list_preview(&to)),
                        field("Message", &body),
                    ],
                    to.join(", "),
                )
            } else {
                let msg = check_id(i.message_id.as_deref().unwrap_or(""), "message id")?;
                if i.thread_id.is_some() || !to.is_empty() {
                    return Err(bad(
                        "Outlook replies go to the sender of message_id; leave out thread_id and to",
                    ));
                }
                (
                    json!({"message_id": msg, "comment": body, "is_html": false}),
                    "Reply to an Outlook message".to_owned(),
                    vec![field("In reply to", &msg), field("Message", &body)],
                    "sender of the message".to_owned(),
                )
            }
        }
        ActionKind::CreateEvent => {
            let i: EventFields = input(input_v)?;
            if i.event_id.is_some() {
                return Err(bad("create_event takes no event_id (use update_event)"));
            }
            let zone = i.timezone.as_deref().map(check_zone).transpose()?;
            let title = check_text(i.title.as_deref().unwrap_or(""), "title", MAX_TITLE, true)?;
            let start = parse_when(
                i.start.as_deref().ok_or_else(|| bad("start is required"))?,
                zone.as_deref(),
            )?;
            let end = parse_when(
                i.end.as_deref().ok_or_else(|| bad("end is required"))?,
                zone.as_deref(),
            )?;
            if start.before(end) != Some(true) {
                return Err(bad(
                    "end must be after start (give both as RFC 3339, or both local)",
                ));
            }
            let attendees = check_emails(i.attendees.as_deref().unwrap_or(&[]), "attendee")?;
            let location = i
                .location
                .as_deref()
                .map(|l| check_text(l, "location", MAX_TITLE, false))
                .transpose()?;
            let description = i
                .description
                .as_deref()
                .map(|d| check_text(d, "description", MAX_BODY, false))
                .transpose()?;
            let (s, sz) = start.wall(zone.as_deref());
            let (e, ez) = end.wall(zone.as_deref());
            if sz != ez {
                return Err(bad("start and end must be in the same time zone"));
            }
            let args = if toolkit == "googlecalendar" {
                strip_nulls(json!({
                    "calendar_id": "primary", "summary": title, "start_datetime": s, "end_datetime": e,
                    "timezone": sz, "attendees": attendees, "location": location, "description": description,
                    // Composio's defaults add a Meet link and email everyone; be explicit.
                    "create_meeting_room": false,
                    "send_updates": if attendees.is_empty() { "none" } else { "all" },
                }))
            } else {
                if !attendees.is_empty() {
                    // `attendees_info`'s shape is not documented; refuse rather than guess.
                    return Err(bad(
                        "inviting attendees to Outlook events is not supported yet",
                    ));
                }
                strip_nulls(json!({
                    "subject": title, "start_datetime": s, "end_datetime": e, "time_zone": sz,
                    "location": location, "body": description, "is_html": false,
                }))
            };
            let mut f = vec![
                field("Title", &title),
                field("Starts", &start.show(zone.as_deref())),
                field("Ends", &end.show(zone.as_deref())),
            ];
            if let Some(l) = &location {
                f.push(field("Where", l));
            }
            if !attendees.is_empty() {
                f.push(field("Invites", &list_preview(&attendees)));
                f.push(field(
                    "Notifications",
                    "Invitations are emailed to every attendee",
                ));
            }
            if let Some(d) = &description {
                f.push(field("Description", d));
            }
            for (k, v) in [
                ("title", Some(title.clone())),
                ("start", Some(start.show(zone.as_deref()))),
                ("end", Some(end.show(zone.as_deref()))),
                ("location", location.clone()),
                (
                    "attendees",
                    (!attendees.is_empty()).then(|| list_preview(&attendees)),
                ),
            ] {
                if v.is_some() {
                    diff.push(Change {
                        field: k.into(),
                        before: None,
                        after: v.map(|v| display_text(&v, SHOW)),
                    });
                }
            }
            (
                args,
                format!("Create the event “{}”", display_text(&title, 80)),
                f,
                if attendees.is_empty() {
                    "your calendar".into()
                } else {
                    attendees.join(", ")
                },
            )
        }
        ActionKind::UpdateEvent => {
            let i: EventFields = input(input_v)?;
            let event = check_id(i.event_id.as_deref().unwrap_or(""), "event id")?;
            let zone = i.timezone.as_deref().map(check_zone).transpose()?;
            let mut args = Map::new();
            args.insert("calendar_id".into(), json!("primary"));
            args.insert("event_id".into(), json!(event));
            let before = before.cloned().unwrap_or_default();
            let mut f = vec![field("Event", &event)];
            let mut push = |name: &str, label: &str, old: Option<String>, new: String| {
                f.push(field(label, &new));
                diff.push(Change {
                    field: name.into(),
                    before: old.map(|o| display_text(&o, SHOW)),
                    after: Some(display_text(&new, SHOW)),
                });
            };
            if let Some(t) = &i.title {
                let t = check_text(t, "title", MAX_TITLE, true)?;
                args.insert("summary".into(), json!(t));
                push("title", "Title", before.title.clone(), t);
            }
            let mut times = Vec::new();
            for (key, arg, label, old, v) in [
                (
                    "start",
                    "start_time",
                    "Starts",
                    before.start.clone(),
                    &i.start,
                ),
                ("end", "end_time", "Ends", before.end.clone(), &i.end),
            ] {
                if let Some(v) = v {
                    let w = parse_when(v, zone.as_deref())?;
                    let value = match (w.rfc3339(), w) {
                        (Some(r), _) => r,
                        (None, When::Local(p)) => p.format(LOCAL).unwrap_or_default(),
                        (None, When::Instant(_)) => unreachable!("instants format"),
                    };
                    args.insert(arg.into(), json!(value));
                    times.push(w);
                    push(key, label, old, w.show(zone.as_deref()));
                }
            }
            if let [a, b] = times[..]
                && a.before(b) != Some(true)
            {
                return Err(bad("end must be after start"));
            }
            if let Some(z) = &zone {
                args.insert("timezone".into(), json!(z));
            }
            if let Some(l) = &i.location {
                let l = check_text(l, "location", MAX_TITLE, false)?;
                args.insert("location".into(), json!(l));
                push("location", "Where", before.location.clone(), l);
            }
            if let Some(d) = &i.description {
                let d = check_text(d, "description", MAX_BODY, false)?;
                args.insert("description".into(), json!(d));
                push("description", "Description", None, d);
            }
            let mut target = "your calendar".to_owned();
            if let Some(a) = &i.attendees {
                let a = check_emails(a, "attendee")?;
                // PATCH replaces the attendee list; the diff shows old and new in full.
                args.insert("attendees".into(), json!(a));
                push(
                    "attendees",
                    "Attendees",
                    before.attendees.clone(),
                    list_preview(&a),
                );
                if !a.is_empty() {
                    target = a.join(", ");
                }
            }
            args.insert("send_updates".into(), json!("all"));
            if args.len() <= 3 {
                return Err(bad("nothing to change"));
            }
            f.push(field(
                "Notifications",
                "Attendees are emailed about the change",
            ));
            (
                Value::Object(args),
                format!(
                    "Update the event “{}”",
                    display_text(before.title.as_deref().unwrap_or(&event), 80)
                ),
                f,
                target,
            )
        }
        ActionKind::PostMessage => {
            let i: PostMessage = input(input_v)?;
            let channel = check_id(&i.channel, "channel")?;
            let text = check_text(&i.text, "text", 40_000, true)?;
            let thread = i
                .thread_ts
                .as_deref()
                .map(|t| check_id(t, "thread"))
                .transpose()?;
            let mut f = vec![field("Channel", &channel)];
            if let Some(t) = &thread {
                f.push(field("In thread", t));
            }
            f.push(field("Message", &text));
            (
                // `markdown_text`, not `text`: SLACK_SEND_MESSAGE has no `text` input.
                strip_nulls(
                    json!({"channel": channel, "markdown_text": text, "thread_ts": thread}),
                ),
                format!("Post a Slack message in {}", display_text(&channel, 80)),
                f,
                channel.clone(),
            )
        }
        ActionKind::CreatePage => {
            let i: CreatePage = input(input_v)?;
            let parent = check_id(&i.parent_id, "parent page id")?;
            let title = check_text(&i.title, "title", MAX_TITLE, true)?;
            let md = i
                .markdown
                .as_deref()
                .map(|m| check_text(m, "markdown", MAX_BODY, false))
                .transpose()?;
            let mut f = vec![field("Under", &parent), field("Title", &title)];
            if let Some(m) = &md {
                f.push(field("Content", m));
            }
            (
                strip_nulls(json!({"parent_id": parent, "title": title, "markdown": md})),
                format!("Create the Notion page “{}”", display_text(&title, 80)),
                f,
                parent.clone(),
            )
        }
    };
    // SEC1: shown text cannot open or close a prompt's `<source>` tag, wherever it is quoted.
    let tag_safe = |v: String| crate::ask::neutralise_tags(&v);
    let fields = fields
        .into_iter()
        .map(|f| Field {
            label: f.label,
            value: tag_safe(f.value),
        })
        .collect();
    let diff = diff
        .into_iter()
        .map(|c| Change {
            field: c.field,
            before: c.before.map(tag_safe),
            after: c.after.map(tag_safe),
        })
        .collect();
    Ok(Plan {
        kind,
        toolkit: toolkit.into(),
        tool,
        arguments,
        summary: tag_safe(summary),
        preview: Preview { fields, diff },
        target_label: target,
    })
}

/// JSON with object keys sorted at every level, so equal values always hash equally.
pub fn canonical(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                let mut out = Map::new();
                for k in keys {
                    out.insert(k.clone(), sort(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    // serde_json's map keeps insertion order only with `preserve_order`; sorted either way.
    sort(v).to_string()
}

/// What an approval is bound to: the account, the tool and its exact arguments.
pub fn digest(account: &str, tool: &str, arguments: &Value) -> String {
    let body = canonical(&json!({"account": account, "tool": tool, "arguments": arguments}));
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

/// The policy's view of a planned action. Every action here is Send-class at least, so the
/// default rules answer Confirm (or Deny); an approval is required whatever they say.
pub fn assess(
    engine: &PolicyEngine,
    plan: &Plan,
    task: &str,
) -> (RiskClass, Decision, Option<String>) {
    let preview = plan
        .preview
        .fields
        .iter()
        .map(|f| format!("{}: “{}”", f.label, f.value))
        .collect::<Vec<_>>()
        .join(" · ");
    let action = ActionDescriptor {
        target_app: plan.toolkit.clone(),
        verb: plan.kind.as_str().into(),
        target_label: Some(display_text(&plan.target_label, 200)),
        text_preview: Some(format!(
            "{}{}",
            nugget_policy::preview::FIELDS_PREFIX,
            display_text(&preview, nugget_policy::preview::FIELDS_PREVIEW_MAX)
        )),
        declared_risk: RiskClass::Send,
        task_id: task.into(),
    };
    let a = engine.assess(&action, &ActionContext::default());
    let decision = match a.decision {
        Decision::Deny(r) => Decision::Deny(r),
        Decision::Confirm(r) => Decision::Confirm(r),
        // Never auto-approved: the cloud always asks.
        Decision::Allow => Decision::Confirm("approve before it is sent".into()),
    };
    (a.risk, decision, a.rule)
}

pub fn default_policy() -> PolicyEngine {
    PolicyEngine::new(PolicyConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(kind: ActionKind, toolkit: &str, v: Value) -> Result<Plan, Invalid> {
        plan(kind, toolkit, &v, None)
    }

    #[test]
    fn send_mail_maps_per_toolkit() {
        let v =
            json!({"to": ["a@x.com", "b@y.org"], "cc": ["c@z.io"], "subject": "Q3", "body": "Hi"});
        let g = p(ActionKind::SendMail, "gmail", v.clone()).unwrap();
        assert_eq!(g.tool, "GMAIL_SEND_EMAIL");
        assert_eq!(g.arguments["recipient_email"], "a@x.com");
        assert_eq!(g.arguments["extra_recipients"], json!(["b@y.org"]));
        assert_eq!(g.arguments["cc"], json!(["c@z.io"]));
        assert_eq!(g.arguments["is_html"], false);
        let o = p(ActionKind::SendMail, "outlook", v).unwrap();
        assert_eq!(o.tool, "OUTLOOK_SEND_EMAIL");
        assert_eq!(o.arguments["to"], "a@x.com,b@y.org");
        assert_eq!(o.arguments["cc_emails"], json!(["c@z.io"]));
        assert!(o.summary.contains("a@x.com, b@y.org") && o.summary.contains("Q3"));
        assert_eq!(o.preview.fields[0], field("To", "a@x.com, b@y.org"));
    }

    #[test]
    fn invalid_inputs_are_refused() {
        let cases = [
            (
                ActionKind::SendMail,
                "gmail",
                json!({"to": [], "subject": "s", "body": "b"}),
            ),
            (
                ActionKind::SendMail,
                "gmail",
                json!({"to": ["a@x.com, evil@y.com"], "subject": "s", "body": "b"}),
            ),
            (
                ActionKind::SendMail,
                "gmail",
                json!({"to": ["a@x.com"], "subject": "s\nBcc: e@v.il", "body": "b"}),
            ),
            (
                ActionKind::SendMail,
                "gmail",
                json!({"to": ["a@x.com"], "subject": "s", "body": " "}),
            ),
            (
                ActionKind::SendMail,
                "gmail",
                json!({"to": ["a@x.com"], "subject": "s", "body": "b", "extra": 1}),
            ),
            (ActionKind::SendMail, "gmail", json!("not an object")),
            (
                ActionKind::ReplyMail,
                "gmail",
                json!({"body": "b", "to": ["a@x.com"]}),
            ),
            (
                ActionKind::ReplyMail,
                "outlook",
                json!({"message_id": "m1", "body": "b", "to": ["a@x.com"]}),
            ),
            (
                ActionKind::CreateEvent,
                "googlecalendar",
                json!({"title": "t", "start": "2026-11-05T11:00:00Z", "end": "2026-11-05T10:00:00Z"}),
            ),
            (
                ActionKind::CreateEvent,
                "googlecalendar",
                json!({"title": "t", "start": "2026-11-05T10:00", "end": "2026-11-05T11:00"}),
            ),
            (
                ActionKind::CreateEvent,
                "outlook",
                json!({"title": "t", "start": "2026-11-05T10:00:00Z", "end": "2026-11-05T11:00:00Z", "attendees": ["a@x.com"]}),
            ),
            (
                ActionKind::UpdateEvent,
                "googlecalendar",
                json!({"event_id": "e1"}),
            ),
            (
                ActionKind::UpdateEvent,
                "googlecalendar",
                json!({"event_id": "../e1", "title": "x"}),
            ),
            (
                ActionKind::PostMessage,
                "slack",
                json!({"channel": "C1", "text": ""}),
            ),
            (
                ActionKind::CreatePage,
                "notion",
                json!({"parent_id": "p 1", "title": "t"}),
            ),
        ];
        for (k, t, v) in cases {
            assert!(
                matches!(p(k, t, v.clone()), Err(Invalid::Bad(_))),
                "{k} {t} {v}"
            );
        }
        assert!(matches!(
            p(ActionKind::PostMessage, "gmail", json!({})),
            Err(Invalid::Unsupported { .. })
        ));
        assert!(matches!(
            p(ActionKind::UpdateEvent, "outlook", json!({})),
            Err(Invalid::Unsupported { .. })
        ));
    }

    #[test]
    fn replies() {
        let g = p(
            ActionKind::ReplyMail,
            "gmail",
            json!({"thread_id": "t1", "to": ["a@x.com"], "body": "Thanks"}),
        )
        .unwrap();
        assert_eq!(g.tool, "GMAIL_REPLY_TO_THREAD");
        assert_eq!(g.arguments["thread_id"], "t1");
        assert_eq!(g.arguments["message_body"], "Thanks");
        assert_eq!(g.arguments["recipient_email"], "a@x.com");
        let o = p(
            ActionKind::ReplyMail,
            "outlook",
            json!({"message_id": "AAMk=", "body": "Thanks"}),
        )
        .unwrap();
        assert_eq!(o.tool, "OUTLOOK_REPLY_EMAIL");
        assert_eq!(
            o.arguments,
            json!({"message_id": "AAMk=", "comment": "Thanks", "is_html": false})
        );
    }

    #[test]
    fn events() {
        let g = p(
            ActionKind::CreateEvent,
            "googlecalendar",
            json!({"title": "Dentist", "start": "2026-11-05T10:00:00+01:00", "end": "2026-11-05T10:30:00+01:00",
                   "attendees": ["a@x.com"], "location": "High St"}),
        )
        .unwrap();
        assert_eq!(g.tool, "GOOGLECALENDAR_CREATE_EVENT");
        assert_eq!(g.arguments["start_datetime"], "2026-11-05T09:00:00");
        assert_eq!(g.arguments["end_datetime"], "2026-11-05T09:30:00");
        assert_eq!(g.arguments["timezone"], "UTC");
        assert_eq!(g.arguments["create_meeting_room"], false);
        assert_eq!(g.arguments["send_updates"], "all");
        assert!(g.preview.fields.iter().any(|f| f.label == "Notifications"));
        assert!(g.preview.diff.iter().all(|c| c.before.is_none()));
        let local = p(
            ActionKind::CreateEvent,
            "outlook",
            json!({"title": "Standup", "start": "2026-11-05T09:00", "end": "2026-11-05T09:15", "timezone": "Europe/London"}),
        )
        .unwrap();
        assert_eq!(local.tool, "OUTLOOK_CALENDAR_CREATE_EVENT");
        assert_eq!(local.arguments["start_datetime"], "2026-11-05T09:00:00");
        assert_eq!(local.arguments["time_zone"], "Europe/London");
        assert!(local.arguments.get("location").is_none(), "nulls dropped");

        let before = EventBefore {
            title: Some("Dentist".into()),
            start: Some("2026-11-05T10:00:00Z".into()),
            location: Some("High St".into()),
            ..Default::default()
        };
        let u = plan(
            ActionKind::UpdateEvent,
            "googlecalendar",
            &json!({"event_id": "e1", "start": "2026-11-06T10:00:00Z", "location": "Low St"}),
            Some(&before),
        )
        .unwrap();
        assert_eq!(u.tool, "GOOGLECALENDAR_PATCH_EVENT");
        assert_eq!(u.arguments["start_time"], "2026-11-06T10:00:00Z");
        assert!(
            u.arguments.get("summary").is_none(),
            "only changed fields are sent"
        );
        assert_eq!(
            u.preview.diff,
            vec![
                Change {
                    field: "start".into(),
                    before: Some("2026-11-05T10:00:00Z".into()),
                    after: Some("2026-11-06 10:00:00 UTC".into())
                },
                Change {
                    field: "location".into(),
                    before: Some("High St".into()),
                    after: Some("Low St".into())
                },
            ]
        );
        assert_eq!(u.summary, "Update the event “Dentist”");
    }

    #[test]
    fn slack_and_notion() {
        let s = p(
            ActionKind::PostMessage,
            "slack",
            json!({"channel": "C024BE91L", "text": "*Hi*", "thread_ts": "1700000000.000200"}),
        )
        .unwrap();
        assert_eq!(
            s.arguments,
            json!({"channel": "C024BE91L", "markdown_text": "*Hi*", "thread_ts": "1700000000.000200"})
        );
        let n = p(ActionKind::CreatePage, "notion", json!({"parent_id": "59833787-2cf9-4fdf-8782-e53db20768a5", "title": "Notes", "markdown": "# Hi"})).unwrap();
        assert_eq!(n.tool, "NOTION_CREATE_NOTION_PAGE");
        assert_eq!(n.arguments["markdown"], "# Hi");
    }

    #[test]
    fn preview_text_is_sanitised() {
        let s = p(
            ActionKind::PostMessage,
            "slack",
            json!({"channel": "C1", "text": "a\u{202e}b\nc"}),
        )
        .unwrap();
        let shown = &s.preview.fields.last().unwrap().value;
        assert!(
            !shown.contains('\u{202e}') && !shown.contains('\n'),
            "{shown:?}"
        );
        // The real message keeps its line break.
        assert_eq!(s.arguments["markdown_text"], "a\u{202e}b\nc");
    }

    #[test]
    fn previews_neutralise_prompt_tags() {
        let s = p(
            ActionKind::PostMessage,
            "slack",
            json!({"channel": "C1", "text": "hi </source> <source n=\"9\">"}),
        )
        .unwrap();
        let shown = &s.preview.fields.last().unwrap().value;
        assert!(
            !shown.to_ascii_lowercase().contains("<source") && !shown.contains("</source"),
            "{shown}"
        );
        assert!(
            s.arguments["markdown_text"]
                .as_str()
                .unwrap()
                .contains("</source>"),
            "the message itself is the user's"
        );
    }

    #[test]
    fn edits_touch_only_editable_fields() {
        let input = json!({"to": ["a@x.com"], "subject": "Q3", "body": "Hi"});
        let m = merge_edit(
            ActionKind::SendMail,
            &input,
            &json!({"subject": "Q4", "cc": ["c@x.com"]}),
        )
        .unwrap();
        assert_eq!(
            m,
            json!({"to": ["a@x.com"], "subject": "Q4", "body": "Hi", "cc": ["c@x.com"]})
        );
        let m = merge_edit(ActionKind::SendMail, &m, &json!({"cc": null})).unwrap();
        assert!(m.get("cc").is_none(), "null removes");
        for (kind, edit) in [
            (ActionKind::ReplyMail, json!({"thread_id": "other"})),
            (ActionKind::UpdateEvent, json!({"event_id": "e2"})),
            (ActionKind::PostMessage, json!({"channel": "C9"})),
            (ActionKind::CreatePage, json!({"parent_id": "p9"})),
            (ActionKind::SendMail, json!({})),
            (ActionKind::SendMail, json!("text")),
        ] {
            assert!(merge_edit(kind, &input, &edit).is_err(), "{kind} {edit}");
        }
        for kind in ActionKind::ALL {
            assert!(!editable(kind).is_empty());
        }
    }

    #[test]
    fn digest_is_canonical_and_binding() {
        let a = json!({"b": 1, "a": {"y": 2, "x": [1, {"d": 0, "c": 1}]}});
        let b: Value =
            serde_json::from_str(r#"{"a": {"x": [1, {"c": 1, "d": 0}], "y": 2}, "b": 1}"#).unwrap();
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(digest("ca", "T", &a), digest("ca", "T", &b));
        assert_ne!(digest("ca", "T", &a), digest("ca2", "T", &a));
        assert_ne!(digest("ca", "T", &a), digest("ca", "T2", &a));
        assert_ne!(digest("ca", "T", &a), digest("ca", "T", &json!({"b": 2})));
        assert_eq!(digest("ca", "T", &a).len(), 64);
    }

    #[test]
    fn policy_always_asks() {
        let engine = default_policy();
        let plan = p(
            ActionKind::SendMail,
            "gmail",
            json!({"to": ["a@x.com"], "subject": "s", "body": "b"}),
        )
        .unwrap();
        let (risk, decision, _) = assess(&engine, &plan, "t1");
        assert!(risk >= RiskClass::Send);
        assert!(matches!(decision, Decision::Confirm(_)), "{decision:?}");
        let page = p(
            ActionKind::CreatePage,
            "notion",
            json!({"parent_id": "p1", "title": "t"}),
        )
        .unwrap();
        assert!(matches!(
            assess(&engine, &page, "t2").1,
            Decision::Confirm(_)
        ));
    }
}
