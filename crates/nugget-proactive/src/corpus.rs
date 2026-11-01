//! A read-only snapshot of the mail and calendar items in the store, normalised across
//! connectors (Gmail, IMAP, mbox/eml files; Google Calendar, CalDAV, `.ics` files).
//!
//! Mail fields are read from the item metadata that ingest writes (`extracted` headers from the
//! mail extractor, `source` from the connector: Gmail `labels`/`thread_id`, IMAP `folder`/`flags`).

use std::collections::{BTreeMap, HashMap};

use jiff::tz::TimeZone;
use nugget_core::Result;
use nugget_core::ids::ItemRef;
use nugget_store::{Item, Store};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::calendar::{Recurrence, resolve, zone_named};
use crate::config::ProactiveConfig;
use crate::text::{Address, is_generated_subject, normalize_subject, stable_hash};

/// Source ids with this prefix belong to this crate and are never read as content.
pub const OWN_SOURCE_PREFIX: &str = "proactive";

#[derive(Debug, Clone, PartialEq)]
pub struct MailMessage {
    pub item: ItemRef,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    /// Connector thread id (Gmail).
    pub thread_hint: Option<String>,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: OffsetDateTime,
    pub body: String,
    /// Gmail label ids, `X-Gmail-Labels` tags and IMAP flags, uppercased.
    pub labels: Vec<String>,
    pub folder: Option<String>,
    /// `List-Unsubscribe` / `List-Id` present.
    pub list_header: bool,
    /// `Precedence` header value, lowercased.
    pub precedence: Option<String>,
    /// `Auto-Submitted` header value, lowercased.
    pub auto_submitted: Option<String>,
    /// Ingest flagged it as junk.
    pub junk: bool,
    pub sensitive: bool,
    pub url: Option<String>,
    /// Filled by [`Corpus`]: whether the user sent it.
    pub from_user: bool,
    /// Filled by [`Corpus`]: thread key.
    pub thread: String,
}

impl MailMessage {
    pub fn participants(&self) -> impl Iterator<Item = &Address> {
        self.from.iter().chain(&self.to).chain(&self.cc)
    }

    /// Why this message looks machine-sent or bulk, if it does.
    pub fn automated_reason(&self) -> Option<&'static str> {
        if self.list_header {
            return Some("list-unsubscribe");
        }
        if self
            .precedence
            .as_deref()
            .is_some_and(|p| matches!(p, "bulk" | "list" | "junk"))
        {
            return Some("precedence");
        }
        if self.auto_submitted.as_deref().is_some_and(|a| a != "no") {
            return Some("auto-submitted");
        }
        if self.junk {
            return Some("junk");
        }
        if self.labels.iter().any(|l| {
            matches!(
                l.as_str(),
                "CATEGORY_PROMOTIONS"
                    | "CATEGORY_SOCIAL"
                    | "CATEGORY_UPDATES"
                    | "CATEGORY_FORUMS"
                    | "SPAM"
                    | "PROMOTIONS"
                    | "$JUNK"
                    | "JUNK"
            )
        }) {
            return Some("mail-category");
        }
        if let Some(f) = &self.from
            && automated_local(f.local())
        {
            return Some("automated-sender");
        }
        if is_generated_subject(&self.subject) {
            return Some("generated-subject");
        }
        let lower = self.body.to_lowercase();
        if lower.contains("unsubscribe")
            || lower.contains("manage your email preferences")
            || lower.contains("view this email in your browser")
        {
            return Some("unsubscribe-link");
        }
        None
    }
}

fn automated_local(local: &str) -> bool {
    let l = local.to_lowercase();
    const EXACT: &[&str] = &[
        "info",
        "news",
        "hello",
        "team",
        "support",
        "billing",
        "receipts",
        "receipt",
        "orders",
        "updates",
        "alerts",
        "notifications",
        "marketing",
        "digest",
        "admin",
        "postmaster",
        // Role mailboxes (RFC 2142 and common practice): not a person writing to the user.
        "account",
        "accounts",
        "service",
        "services",
        "customerservice",
        "events",
        "sales",
        "careers",
        "jobs",
        "contact",
        "help",
        "helpdesk",
        "feedback",
        "survey",
        "deals",
        "offers",
        "promo",
        "promotions",
        "shop",
        "store",
        "payments",
        "invoices",
        "membership",
        "members",
        "community",
        "webmaster",
        "security",
    ];
    const PARTS: &[&str] = &[
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "mailer-daemon",
        "bounce",
        "newsletter",
        "notification",
        "automated",
        "calendar-notification",
    ];
    EXACT.contains(&l.as_str()) || PARTS.iter().any(|p| l.contains(p))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attendee {
    pub address: Address,
    pub resource: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalEvent {
    pub item: ItemRef,
    pub uid: Option<String>,
    pub title: String,
    pub start: OffsetDateTime,
    pub end: Option<OffsetDateTime>,
    pub all_day: bool,
    pub location: Option<String>,
    pub organizer: Option<Address>,
    pub attendees: Vec<Attendee>,
    pub cancelled: bool,
    /// The user declined it.
    pub declined: bool,
    /// Attendees who declined (lowercased addresses).
    pub declined_by: Vec<String>,
    pub sensitive: bool,
    pub url: Option<String>,
    /// A repeating series (expanded by [`Corpus::events_between`]).
    pub recurrence: Option<Recurrence>,
    /// iCalendar version: `(RECURRENCE-ID, SEQUENCE, DTSTAMP)`; `None` for connector events.
    pub version: Option<(Option<OffsetDateTime>, i64, Option<OffsetDateTime>)>,
}

impl CalEvent {
    /// Whether it touches `[from, to)`.
    fn overlaps(&self, from: OffsetDateTime, to: OffsetDateTime) -> bool {
        self.start < to
            && match self.end.filter(|e| *e > self.start) {
                Some(end) => end > from,
                None => self.start >= from,
            }
    }
}

/// A text item that is neither mail nor an event: a note, a document.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteDoc {
    pub item: ItemRef,
    pub title: Option<String>,
    pub text: String,
    /// Last modified.
    pub date: OffsetDateTime,
    pub sensitive: bool,
    pub url: Option<String>,
}

/// Mail, events and other (note) items, with threads computed.
#[derive(Debug, Clone, Default)]
pub struct Corpus {
    pub mail: Vec<MailMessage>,
    pub events: Vec<CalEvent>,
    /// Text notes (Markdown and plain text), by item key.
    pub notes: Vec<NoteDoc>,
    /// The user's addresses (configured or inferred), lowercased.
    pub user_addresses: Vec<String>,
    pub internal_domains: Vec<String>,
    /// Thread key → indices into `mail`, oldest first.
    pub threads: BTreeMap<String, Vec<usize>>,
}

/// Items parsed so far by [`Corpus::load`] / [`Corpus::from_items`], before threading.
#[derive(Default)]
struct Parts {
    mail: Vec<MailMessage>,
    events: Vec<CalEvent>,
    notes: Vec<NoteDoc>,
}

impl Parts {
    fn push(&mut self, it: &Item, cfg: &ProactiveConfig) {
        if let Some(m) = parse_mail(it) {
            self.mail.push(m);
        } else if let Some(e) = parse_events(it, &cfg.zone()) {
            self.events.extend(e);
        } else if let Some(n) = parse_note(it) {
            self.notes.push(n);
        }
    }
}

impl Corpus {
    /// Read every non-proactive item in the store.
    ///
    /// Walks each source in key order a page at a time (`Store::items_page`, II5), parsing each
    /// item as it comes and keeping only the parsed form, so the raw items (with their metadata
    /// trees) are never all in memory at once. Still a full scan; making it incremental is the
    /// crate's follow-up.
    pub fn load(store: &Store, cfg: &ProactiveConfig) -> Result<Self> {
        const PAGE: usize = 500;
        let mut parts = Parts::default();
        for sc in store.source_counts()? {
            if sc.source.as_str().starts_with(OWN_SOURCE_PREFIX) {
                continue;
            }
            let mut after = None;
            loop {
                let (page, next) = store.items_page(&sc.source, after.as_ref(), PAGE)?;
                for it in &page {
                    parts.push(it, cfg);
                }
                match next {
                    Some(key) => after = Some(key),
                    None => break,
                }
            }
        }
        Ok(Self::assemble(parts, cfg))
    }

    pub fn from_items(items: &[Item], cfg: &ProactiveConfig) -> Self {
        let mut parts = Parts::default();
        for it in items {
            parts.push(it, cfg);
        }
        Self::assemble(parts, cfg)
    }

    fn assemble(parts: Parts, cfg: &ProactiveConfig) -> Self {
        let Parts {
            mut mail,
            events,
            mut notes,
        } = parts;
        notes.sort_by(|a, b| a.item.key.cmp(&b.item.key));
        let mut users = cfg.user_set();
        if users.is_empty() {
            for m in &mail {
                if is_sent_folder(m)
                    && let Some(f) = &m.from
                    && !users.contains(&f.email)
                {
                    users.push(f.email.clone());
                }
            }
        }
        for m in &mut mail {
            m.from_user =
                is_sent_folder(m) || m.from.as_ref().is_some_and(|f| users.contains(&f.email));
        }
        mail.sort_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then_with(|| a.item.key.cmp(&b.item.key))
        });
        let threads = thread(&mut mail, &users);
        let mut events = current_versions(events, &users);
        events.sort_by_key(|e| e.start);
        let internal_domains = cfg.internal_domains_for(&users);
        Self {
            mail,
            events,
            notes,
            user_addresses: users,
            internal_domains,
            threads,
        }
    }

    /// Events touching `[from, to)`, with each repeating series expanded into its occurrences,
    /// by start.
    pub fn events_between(&self, from: OffsetDateTime, to: OffsetDateTime) -> Vec<CalEvent> {
        let mut out = Vec::new();
        for e in &self.events {
            let Some(r) = &e.recurrence else {
                if e.overlaps(from, to) {
                    out.push(e.clone());
                }
                continue;
            };
            let len = e.end.map(|end| end - e.start);
            let lead = len.unwrap_or_default().max(time::Duration::ZERO);
            for start in r.between(from - lead, to) {
                let occ = CalEvent {
                    start,
                    end: len.map(|l| start + l),
                    recurrence: None,
                    ..e.clone()
                };
                if occ.overlaps(from, to) {
                    out.push(occ);
                }
            }
        }
        out.sort_by_key(|e| e.start);
        out
    }

    pub fn is_user(&self, email: &str) -> bool {
        self.user_addresses.iter().any(|u| u == email)
    }

    /// Not the user and not at an internal domain.
    pub fn is_external(&self, a: &Address) -> bool {
        !self.is_user(&a.email) && !self.internal_domains.iter().any(|d| d == a.domain())
    }

    pub fn thread_of(&self, idx: usize) -> &[usize] {
        self.threads
            .get(&self.mail[idx].thread)
            .map_or(&[], Vec::as_slice)
    }

    pub fn is_mail_item(&self, r: &ItemRef) -> bool {
        self.mail.iter().any(|m| &m.item == r)
    }

    pub fn is_event_item(&self, r: &ItemRef) -> bool {
        self.events.iter().any(|e| &e.item == r)
    }

    /// How many messages the user sent to `email` (a relationship signal).
    pub fn sent_to(&self, email: &str) -> usize {
        self.mail
            .iter()
            .filter(|m| m.from_user && m.to.iter().chain(&m.cc).any(|a| a.email == email))
            .count()
    }
}

fn is_sent_folder(m: &MailMessage) -> bool {
    m.labels.iter().any(|l| l == "SENT")
        || m.folder.as_deref().is_some_and(|f| {
            let f = f.to_lowercase();
            f == "sent"
                || f.ends_with("/sent")
                || f.contains("sent items")
                || f.contains("sent mail")
        })
}

/// Group messages into threads: connector thread ids, then `Message-ID` / `In-Reply-To` /
/// `References` links, then (for messages with no links) normalised subject plus the set of
/// people other than the user.
fn thread(mail: &mut [MailMessage], users: &[String]) -> BTreeMap<String, Vec<usize>> {
    let n = mail.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    fn union(p: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            p[ra.max(rb)] = ra.min(rb);
        }
    }
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut link = |key: String, i: usize, parent: &mut Vec<usize>| match by_key.get(&key) {
        Some(&j) => union(parent, i, j),
        None => {
            by_key.insert(key, i);
        }
    };
    for (i, m) in mail.iter().enumerate() {
        if let Some(t) = &m.thread_hint {
            link(format!("t:{t}"), i, &mut parent);
        }
        if let Some(id) = &m.message_id {
            link(format!("m:{}", norm_id(id)), i, &mut parent);
        }
        for r in m.in_reply_to.iter().chain(&m.references) {
            link(format!("m:{}", norm_id(r)), i, &mut parent);
        }
        let linked =
            m.thread_hint.is_some() || !m.in_reply_to.is_empty() || !m.references.is_empty();
        let subj = normalize_subject(&m.subject);
        if !subj.is_empty() && (!linked || m.subject.to_lowercase().starts_with("re")) {
            let mut others: Vec<&str> = m
                .participants()
                .map(|a| a.email.as_str())
                .filter(|e| !users.iter().any(|u| u == e))
                .collect();
            others.sort();
            others.dedup();
            link(format!("s:{subj}|{}", others.join(",")), i, &mut parent);
        }
    }
    let mut threads: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut root_key: HashMap<usize, String> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        let key = root_key
            .entry(r)
            .or_insert_with(|| {
                let root = &mail[r];
                stable_hash(&[root.item.source.as_str(), root.item.key.as_str()])
            })
            .clone();
        mail[i].thread = key.clone();
        threads.entry(key).or_default().push(i);
    }
    threads
}

fn norm_id(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).to_lowercase()
}

// ---- metadata access ----

fn get<'a>(meta: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut v = meta;
    for p in path {
        v = v.get(p)?;
    }
    (!v.is_null()).then_some(v)
}

fn str_at<'a>(meta: &'a Value, path: &[&str]) -> Option<&'a str> {
    get(meta, path)?.as_str()
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// A header value from `extracted`, `extracted.headers`, `source` or `source.headers`, under
/// its snake_case or hyphenated, any-case name.
fn header(meta: &Value, name: &str) -> Option<String> {
    let snake = name.to_lowercase().replace('-', "_");
    let hyphen = name.to_lowercase();
    for base in [
        get(meta, &["extracted"]),
        get(meta, &["extracted", "headers"]),
        get(meta, &["source"]),
        get(meta, &["source", "headers"]),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(obj) = base.as_object() {
            for (k, v) in obj {
                let kl = k.to_lowercase();
                if kl == snake || kl == hyphen {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        Value::Array(a) => a
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                        Value::Bool(true) => "true".into(),
                        _ => continue,
                    };
                    if !s.trim().is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

fn sensitive(meta: &Value) -> bool {
    // Same rule as retrieval: only an explicit `false` means not sensitive.
    meta.get("sensitive") != Some(&Value::Bool(false))
}

fn url(meta: &Value) -> Option<String> {
    str_at(meta, &["url"]).map(str::to_owned)
}

fn mime(item: &Item) -> String {
    item.mime
        .clone()
        .or_else(|| str_at(&item.metadata, &["mime"]).map(str::to_owned))
        .unwrap_or_default()
        .to_lowercase()
}

fn parse_mail(item: &Item) -> Option<MailMessage> {
    let meta = &item.metadata;
    let mime = mime(item);
    let ex = get(meta, &["extracted"]);
    let is_mail = mime.starts_with("message/rfc822")
        || ex.is_some_and(|e| e.get("from").is_some() && e.get("message_id").is_some());
    if !is_mail {
        return None;
    }
    let from = str_at(meta, &["extracted", "from"])
        .or_else(|| str_at(meta, &["source", "from"]))
        .and_then(Address::parse);
    let addrs = |k: &str| -> Vec<Address> {
        strings(get(meta, &["extracted", k]))
            .iter()
            .filter_map(|s| Address::parse(s))
            .collect()
    };
    let date = str_at(meta, &["extracted", "date"])
        .and_then(|d| OffsetDateTime::parse(d, &Rfc3339).ok())
        .or(item.source_modified_at)
        .or_else(|| {
            get(meta, &["source", "internal_date"])
                .and_then(Value::as_i64)
                .and_then(|ms| {
                    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
                })
        })
        .unwrap_or(item.created_at);
    let subject = str_at(meta, &["extracted", "subject"])
        .or_else(|| str_at(meta, &["source", "subject"]))
        .or(item.title.as_deref())
        .unwrap_or("")
        .to_owned();
    let body = item
        .text
        .clone()
        .or_else(|| str_at(meta, &["source", "snippet"]).map(str::to_owned))
        .unwrap_or_default();
    let mut labels: Vec<String> = strings(get(meta, &["source", "labels"]));
    labels.extend(strings(get(meta, &["tags"])));
    labels.extend(strings(get(meta, &["source", "flags"])));
    let labels = labels
        .into_iter()
        .map(|l| l.trim().trim_start_matches('\\').to_uppercase())
        .collect();
    Some(MailMessage {
        item: item.item.clone(),
        message_id: str_at(meta, &["extracted", "message_id"])
            .or_else(|| str_at(meta, &["source", "message_id"]))
            .map(str::to_owned),
        in_reply_to: strings(get(meta, &["extracted", "in_reply_to"])),
        references: strings(get(meta, &["extracted", "references"])),
        thread_hint: str_at(meta, &["source", "thread_id"]).map(str::to_owned),
        from,
        to: addrs("to"),
        cc: addrs("cc"),
        subject,
        date,
        body,
        labels,
        folder: str_at(meta, &["source", "folder"]).map(str::to_owned),
        list_header: header(meta, "List-Unsubscribe").is_some()
            || header(meta, "List-Id").is_some(),
        precedence: header(meta, "Precedence").map(|p| p.trim().to_lowercase()),
        auto_submitted: header(meta, "Auto-Submitted").map(|p| p.trim().to_lowercase()),
        junk: meta.get("junk") == Some(&Value::Bool(true)),
        sensitive: sensitive(meta),
        url: url(meta),
        from_user: false,
        thread: String::new(),
    })
}

/// A Markdown or plain-text item with text (notes, documents).
fn parse_note(item: &Item) -> Option<NoteDoc> {
    let mime = mime(item);
    let texty = mime.starts_with("text/markdown")
        || mime.starts_with("text/x-markdown")
        || mime.starts_with("text/plain")
        || (mime.is_empty()
            && [".md", ".markdown", ".txt"]
                .iter()
                .any(|e| item.item.key.as_str().to_lowercase().ends_with(e)));
    let text = item.text.clone().filter(|t| !t.trim().is_empty())?;
    if !texty {
        return None;
    }
    Some(NoteDoc {
        item: item.item.clone(),
        title: item.title.clone(),
        text,
        date: item.source_modified_at.unwrap_or(item.created_at),
        sensitive: sensitive(&item.metadata),
        url: url(&item.metadata),
    })
}

/// The calendar events of an item: one per VEVENT of an iCalendar item (`extracted.events`),
/// or the connector's own event (Google: `source.start` is `{date, date_time}`). Times without
/// an offset are read in their TZID, else in the user's zone `tz`.
fn parse_events(item: &Item, tz: &TimeZone) -> Option<Vec<CalEvent>> {
    let meta = &item.metadata;
    if let Some(s) = get(meta, &["source", "start"]).filter(|v| v.is_object()) {
        let pick = |v: &Value| {
            v.get("date_time")
                .and_then(Value::as_str)
                .or_else(|| v.get("date").and_then(Value::as_str))
                .and_then(|x| resolve(x, tz))
        };
        let (start, all_day, _) = pick(s)?;
        let end = get(meta, &["source", "end"])
            .and_then(pick)
            .map(|(t, ..)| t);
        let src = get(meta, &["source"])?;
        let mut e = event_fields(item, src, tz);
        e.uid = str_at(meta, &["source", "ical_uid"])
            .or_else(|| str_at(meta, &["source", "event_id"]))
            .map(str::to_owned);
        e.start = start;
        e.end = end;
        e.all_day = all_day;
        e.version = None;
        e.recurrence = None;
        return Some(vec![e]);
    }
    let mime = mime(item);
    let evs = get(meta, &["extracted", "events"]).and_then(Value::as_array);
    if !mime.contains("calendar") && evs.is_none() {
        return None;
    }
    let cancel_all = str_at(meta, &["extracted", "method"]) == Some("CANCEL");
    let single = evs.map_or(0, Vec::len) == 1;
    let out: Vec<CalEvent> = evs
        .into_iter()
        .flatten()
        .filter(|ev| ev.get("kind").and_then(Value::as_str) != Some("task"))
        .filter_map(|ev| {
            let mut e = event_fields(item, ev, tz);
            if ev.get("start").is_none() {
                return None;
            }
            e.cancelled |= cancel_all;
            if !single {
                // A file of many events: each is its own (the item title is the calendar's).
                e.title = ev
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled event)")
                    .to_owned();
            }
            Some(e)
        })
        .collect();
    Some(out)
}

/// The fields of one event object (an `extracted.events[i]` or a connector's `source`).
fn event_fields(item: &Item, ev: &Value, user_tz: &TimeZone) -> CalEvent {
    let s = |k: &str| ev.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    let zone_of = |k: &str| {
        s(k).filter(|z| *z != "UTC")
            .and_then(zone_named)
            .unwrap_or_else(|| user_tz.clone())
    };
    let tz = zone_of("timezone");
    let start = s("start").and_then(|x| resolve(x, &tz));
    let end = s("end")
        .and_then(|x| resolve(x, &zone_of("end_timezone")))
        .map(|(t, ..)| t);
    let times = |k: &str| -> Vec<OffsetDateTime> {
        strings(ev.get(k))
            .iter()
            .filter_map(|x| resolve(x, &tz).map(|(t, ..)| t))
            .collect()
    };
    let recurrence = match (s("rrule"), &start) {
        (Some(r), Some((_, _, wall))) => {
            Recurrence::parse(r, *wall, tz.clone(), times("exdate"), times("rdate"))
        }
        _ => None,
    };
    let mut attendees = Vec::new();
    let mut declined_by = Vec::new();
    for a in ev
        .get("attendees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (address, resource, response) = match a {
            Value::String(s) => {
                let response = s.trim_end().ends_with("(declined)").then_some("declined");
                (Address::parse(s), false, response)
            }
            Value::Object(o) => (
                o.get("email").and_then(Value::as_str).map(|email| Address {
                    name: o.get("name").and_then(Value::as_str).map(str::to_owned),
                    email: email.to_lowercase(),
                }),
                o.get("resource").and_then(Value::as_bool).unwrap_or(false),
                o.get("response").and_then(Value::as_str),
            ),
            _ => continue,
        };
        let Some(address) = address else { continue };
        if response == Some("declined") {
            declined_by.push(address.email.clone());
        }
        // Rooms and equipment, even when the connector did not flag them.
        let resource = resource
            || address.domain().starts_with("resource.")
            || address.domain().ends_with(".resource.calendar.google.com");
        attendees.push(Attendee { address, resource });
    }
    let organizer = match ev.get("organizer") {
        Some(Value::String(s)) => Address::parse(s),
        Some(Value::Object(o)) => o.get("email").and_then(Value::as_str).map(|e| Address {
            name: o.get("name").and_then(Value::as_str).map(str::to_owned),
            email: e.to_lowercase(),
        }),
        _ => None,
    };
    let title = item
        .title
        .clone()
        .or_else(|| s("summary").map(str::to_owned))
        .unwrap_or_else(|| "(untitled event)".into());
    let (start, all_day) = start.map_or((item.created_at, false), |(t, d, _)| (t, d));
    CalEvent {
        item: item.item.clone(),
        uid: s("uid").map(str::to_owned),
        title,
        start,
        end,
        all_day,
        location: s("location").map(str::to_owned),
        organizer,
        attendees,
        cancelled: s("status").is_some_and(|v| v.eq_ignore_ascii_case("cancelled")),
        declined: false,
        declined_by,
        sensitive: sensitive(&item.metadata),
        url: url(&item.metadata),
        recurrence,
        version: Some((
            s("recurrence_id")
                .and_then(|x| resolve(x, &tz))
                .map(|(t, ..)| t),
            ev.get("sequence").and_then(Value::as_i64).unwrap_or(0),
            s("dtstamp").and_then(|x| resolve(x, &tz)).map(|(t, ..)| t),
        )),
    }
}

/// One event per iCalendar UID and RECURRENCE-ID: the highest SEQUENCE (then DTSTAMP, then a
/// cancellation) wins. Cancelled and user-declined events are dropped; an instance that was
/// moved, cancelled or declined is taken out of its series.
fn current_versions(events: Vec<CalEvent>, users: &[String]) -> Vec<CalEvent> {
    let mut best: BTreeMap<(String, Option<OffsetDateTime>), CalEvent> = BTreeMap::new();
    let mut out = Vec::new();
    for mut e in events {
        e.declined = e.declined_by.iter().any(|a| users.contains(a));
        let (Some(uid), Some((rid, ..))) = (e.uid.clone(), e.version) else {
            if !e.cancelled && !e.declined {
                out.push(e);
            }
            continue;
        };
        let rank = |e: &CalEvent| e.version.map(|(_, seq, stamp)| (seq, stamp, e.cancelled));
        match best.get(&(uid.clone(), rid)) {
            Some(cur) if rank(cur) >= rank(&e) => {}
            _ => {
                best.insert((uid, rid), e);
            }
        }
    }
    let mut excluded: HashMap<String, Vec<OffsetDateTime>> = HashMap::new();
    for (uid, rid) in best.keys() {
        if let Some(rid) = rid {
            excluded.entry(uid.clone()).or_default().push(*rid);
        }
    }
    for ((uid, _), mut e) in best {
        if let Some(r) = &mut e.recurrence {
            r.exdates
                .extend(excluded.get(&uid).into_iter().flatten().copied());
        }
        if !e.cancelled && !e.declined {
            out.push(e);
        }
    }
    out
}
