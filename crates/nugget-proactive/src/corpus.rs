//! A read-only snapshot of the mail and calendar items in the store, normalised across
//! connectors (Gmail, IMAP, mbox/eml files; Google Calendar, CalDAV, `.ics` files).
//!
//! Mail fields are read from the item metadata that ingest writes (`extracted` headers from the
//! mail extractor, `source` from the connector: Gmail `labels`/`thread_id`, IMAP `folder`/`flags`).

use std::collections::{BTreeMap, HashMap};

use nugget_core::Result;
use nugget_core::ids::ItemRef;
use nugget_store::{Item, Store};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::config::ProactiveConfig;
use crate::text::{Address, normalize_subject, parse_iso_date, stable_hash};

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
    pub sensitive: bool,
    pub url: Option<String>,
}

/// Mail, events and other (note) items, with threads computed.
#[derive(Debug, Clone, Default)]
pub struct Corpus {
    pub mail: Vec<MailMessage>,
    pub events: Vec<CalEvent>,
    /// The user's addresses (configured or inferred), lowercased.
    pub user_addresses: Vec<String>,
    pub internal_domains: Vec<String>,
    /// Thread key → indices into `mail`, oldest first.
    pub threads: BTreeMap<String, Vec<usize>>,
}

impl Corpus {
    /// Read every non-proactive item in the store.
    ///
    /// This walks `source_counts` → `item_versions` → `get_item`, a full scan; see the crate
    /// docs for the additive store API requested to make it incremental.
    pub fn load(store: &Store, cfg: &ProactiveConfig) -> Result<Self> {
        let mut items = Vec::new();
        for sc in store.source_counts()? {
            if sc.source.as_str().starts_with(OWN_SOURCE_PREFIX) {
                continue;
            }
            for key in store.item_versions(&sc.source)?.into_keys() {
                let r = ItemRef {
                    source: sc.source.clone(),
                    key,
                };
                if let Some(item) = store.get_item(&r)? {
                    items.push(item);
                }
            }
        }
        Ok(Self::from_items(&items, cfg))
    }

    pub fn from_items(items: &[Item], cfg: &ProactiveConfig) -> Self {
        let mut mail = Vec::new();
        let mut events = Vec::new();
        for it in items {
            if let Some(m) = parse_mail(it) {
                mail.push(m);
            } else if let Some(e) = parse_event(it, cfg.utc_offset) {
                events.push(e);
            }
        }
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
        events.sort_by_key(|e| e.start);
        let internal_domains = cfg.internal_domains_for(&users);
        Self {
            mail,
            events,
            user_addresses: users,
            internal_domains,
            threads,
        }
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

/// A calendar time: RFC 3339, a floating `YYYY-MM-DDTHH:MM:SS` (read in `offset`), or a date.
fn parse_time(s: &str, offset: UtcOffset) -> Option<(OffsetDateTime, bool)> {
    let s = s.trim();
    if let Ok(t) = OffsetDateTime::parse(s, &Rfc3339) {
        return Some((t, false));
    }
    let date = parse_iso_date(s)?;
    if s.len() == 10 {
        return Some((
            PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_offset(offset),
            true,
        ));
    }
    let t = s.get(11..19).or_else(|| s.get(11..16))?;
    let mut parts = t.split(':').map(|p| p.parse::<u8>().ok());
    let (h, m) = (parts.next()??, parts.next()??);
    let sec = parts.next().flatten().unwrap_or(0);
    let time = Time::from_hms(h, m, sec).ok()?;
    let utc = s.ends_with('Z') || s.ends_with('z');
    let off = if utc { UtcOffset::UTC } else { offset };
    Some((PrimitiveDateTime::new(date, time).assume_offset(off), false))
}

fn parse_event(item: &Item, offset: UtcOffset) -> Option<CalEvent> {
    let meta = &item.metadata;
    let mime = mime(item);
    // Google Calendar: `source.start` is {date, date_time}.
    let gstart = get(meta, &["source", "start"]).filter(|v| v.is_object());
    let (start, end, all_day) = if let Some(s) = gstart {
        let pick = |v: &Value| {
            v.get("date_time")
                .and_then(Value::as_str)
                .or_else(|| v.get("date").and_then(Value::as_str))
                .and_then(|x| parse_time(x, offset))
        };
        let (start, all_day) = pick(s)?;
        let end = get(meta, &["source", "end"]).and_then(pick).map(|(t, _)| t);
        (start, end, all_day)
    } else {
        // iCalendar (CalDAV, .ics files): `extracted.start` is ISO.
        if !mime.contains("calendar") && get(meta, &["extracted", "start"]).is_none() {
            return None;
        }
        let (start, all_day) = parse_time(str_at(meta, &["extracted", "start"])?, offset)?;
        let end = str_at(meta, &["extracted", "end"])
            .and_then(|x| parse_time(x, offset))
            .map(|(t, _)| t);
        (start, end, all_day)
    };
    let mut attendees = Vec::new();
    for a in get(meta, &["source", "attendees"])
        .or_else(|| get(meta, &["extracted", "attendees"]))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match a {
            Value::String(s) => {
                if let Some(address) = Address::parse(s) {
                    attendees.push(Attendee {
                        address,
                        resource: false,
                    });
                }
            }
            Value::Object(o) => {
                if let Some(email) = o.get("email").and_then(Value::as_str) {
                    attendees.push(Attendee {
                        address: Address {
                            name: o.get("name").and_then(Value::as_str).map(str::to_owned),
                            email: email.to_lowercase(),
                        },
                        resource: o.get("resource").and_then(Value::as_bool).unwrap_or(false),
                    });
                }
            }
            _ => {}
        }
    }
    // Rooms and equipment, even when the connector did not flag them.
    for a in &mut attendees {
        a.resource |= a.address.domain().starts_with("resource.")
            || a.address
                .domain()
                .ends_with(".resource.calendar.google.com");
    }
    let organizer = match get(meta, &["source", "organizer"])
        .or_else(|| get(meta, &["extracted", "organizer"]))
    {
        Some(Value::String(s)) => Address::parse(s),
        Some(Value::Object(o)) => o.get("email").and_then(Value::as_str).map(|e| Address {
            name: o.get("name").and_then(Value::as_str).map(str::to_owned),
            email: e.to_lowercase(),
        }),
        _ => None,
    };
    let status = str_at(meta, &["source", "status"])
        .or_else(|| str_at(meta, &["extracted", "status"]))
        .unwrap_or("")
        .to_lowercase();
    let title = item
        .title
        .clone()
        .or_else(|| str_at(meta, &["extracted", "summary"]).map(str::to_owned))
        .unwrap_or_else(|| "(untitled event)".into());
    Some(CalEvent {
        item: item.item.clone(),
        uid: str_at(meta, &["source", "ical_uid"])
            .or_else(|| str_at(meta, &["extracted", "uid"]))
            .or_else(|| str_at(meta, &["source", "event_id"]))
            .map(str::to_owned),
        title,
        start,
        end,
        all_day,
        location: str_at(meta, &["source", "location"])
            .or_else(|| str_at(meta, &["extracted", "location"]))
            .map(str::to_owned),
        organizer,
        attendees,
        cancelled: status == "cancelled",
        sensitive: sensitive(meta),
        url: url(meta),
    })
}
