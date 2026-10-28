//! iCalendar (RFC 5545): events and tasks rendered as readable text, one section each; uid and
//! times in metadata. Hand-rolled line parser (unfolding, parameters, escapes); no recursion.

use nugget_core::{Error, Result};
use serde_json::{Map, Value, json};

use crate::util::{self, LINK_CAP, SectionTracker, TAG_CAP, TextBuf};
use crate::{Ctx, Extracted};

const MAX_COMPONENTS: usize = 5_000;
const MAX_ATTENDEES: usize = 500;
const MAX_DEPTH: usize = 16;
const MAX_VALUE_CHARS: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Unfold continuation lines (CRLF or LF followed by a space or tab).
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(cont) = line.strip_prefix([' ', '\t'])
            && let Some(last) = out.last_mut()
        {
            if last.len() < MAX_VALUE_CHARS * 4 {
                last.push_str(cont);
            }
            continue;
        }
        out.push(line.to_owned());
    }
    out
}

/// `NAME;P1=a;P2="b:c":value`.
fn parse_line(line: &str) -> Option<Prop> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in head.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' if !in_quotes => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let mut it = parts.into_iter();
    let name = it.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = it
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), v.trim().to_owned()))
        })
        .collect();
    Some(Prop {
        name,
        params,
        value: value.to_owned(),
    })
}

/// TEXT value escapes: `\n`, `\,`, `\;`, `\\`.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    util::truncate_chars(&out, MAX_VALUE_CHARS)
}

/// A parsed DATE / DATE-TIME.
struct When {
    /// ISO 8601 (`2024-05-01T09:00:00`, `Z` suffix for UTC; `2024-05-01` for dates).
    iso: String,
    /// Human form (`2024-05-01 09:00`).
    display: String,
    /// `UTC`, a TZID, or `None` for floating times and dates.
    tz: Option<String>,
    all_day: bool,
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_when(p: &Prop) -> Option<When> {
    let v = p.value.trim();
    let tzid = p
        .param("TZID")
        .map(|t| t.trim_start_matches('/').to_owned());
    let is_date = p
        .param("VALUE")
        .is_some_and(|x| x.eq_ignore_ascii_case("DATE"))
        || (v.len() == 8 && digits(v));
    let date = v.get(..8).filter(|d| digits(d))?;
    let (y, mo, d) = (&date[..4], &date[4..6], &date[6..8]);
    if is_date {
        let iso = format!("{y}-{mo}-{d}");
        return Some(When {
            display: iso.clone(),
            iso,
            tz: None,
            all_day: true,
        });
    }
    let rest = v.get(8..)?.strip_prefix('T')?;
    let (time, utc) = match rest.strip_suffix(['Z', 'z']) {
        Some(t) => (t, true),
        None => (rest, false),
    };
    if time.len() < 4 || !digits(time) {
        return None;
    }
    let (h, mi) = (&time[..2], &time[2..4]);
    let s = time.get(4..6).unwrap_or("00");
    let iso = format!("{y}-{mo}-{d}T{h}:{mi}:{s}{}", if utc { "Z" } else { "" });
    let display = if s == "00" {
        format!("{y}-{mo}-{d} {h}:{mi}")
    } else {
        format!("{y}-{mo}-{d} {h}:{mi}:{s}")
    };
    Some(When {
        iso,
        display,
        tz: if utc { Some("UTC".into()) } else { tzid },
        all_day: false,
    })
}

fn when_text(w: &When) -> String {
    match (&w.tz, w.all_day) {
        (_, true) => format!("{} (all day)", w.display),
        (Some(tz), _) => format!("{} ({tz})", w.display),
        (None, _) => format!("{} (floating time)", w.display),
    }
}

/// `CN="Bob" ...:mailto:bob@x` → `Bob <bob@x>`.
fn person(p: &Prop) -> String {
    let addr = p.value.trim();
    let addr = addr
        .strip_prefix("mailto:")
        .or_else(|| addr.strip_prefix("MAILTO:"))
        .unwrap_or(addr);
    let cn = p.param("CN").map(|c| c.trim_matches('"').trim());
    let mut s = match cn.filter(|c| !c.is_empty()) {
        Some(cn) if !addr.is_empty() && cn != addr => format!("{cn} <{addr}>"),
        Some(cn) => cn.to_owned(),
        None => addr.to_owned(),
    };
    if let Some(ps) = p.param("PARTSTAT") {
        let ps = ps.to_ascii_lowercase().replace('-', " ");
        if ps != "needs action" {
            s.push_str(&format!(" ({ps})"));
        }
    }
    util::truncate_chars(&s, 500)
}

#[derive(Default)]
struct Component {
    kind: String,
    props: Vec<Prop>,
    attendees: usize,
}

impl Component {
    fn get(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }

    fn text(&self, name: &str) -> Option<String> {
        self.get(name)
            .map(|p| unescape(&p.value))
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
}

pub(crate) fn extract(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    if util::looks_binary(data) {
        return Err(Error::Invalid("calendar: binary content".into()));
    }
    let text = util::decode_text(data, ctx.charset);
    let lines = unfold(&text);
    drop(text);

    let mut stack: Vec<String> = Vec::new();
    let mut saw_calendar = false;
    let mut cal_name = None;
    let mut components: Vec<Component> = Vec::new();
    let mut cur: Option<Component> = None;
    let mut dropped = 0usize;
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let Some(p) = parse_line(line) else { continue };
        match p.name.as_str() {
            "BEGIN" => {
                let kind = p.value.trim().to_ascii_uppercase();
                if stack.len() >= MAX_DEPTH {
                    return Err(Error::Invalid(
                        "calendar: components nested too deeply".into(),
                    ));
                }
                if kind == "VCALENDAR" {
                    saw_calendar = true;
                }
                if (kind == "VEVENT" || kind == "VTODO") && cur.is_none() {
                    if components.len() < MAX_COMPONENTS {
                        cur = Some(Component {
                            kind: kind.clone(),
                            ..Default::default()
                        });
                    } else {
                        dropped += 1;
                    }
                }
                stack.push(kind);
            }
            "END" => {
                let kind = p.value.trim().to_ascii_uppercase();
                if let Some(pos) = stack.iter().rposition(|k| *k == kind) {
                    stack.truncate(pos);
                }
                if cur.as_ref().is_some_and(|c| c.kind == kind) {
                    components.extend(cur.take());
                }
            }
            _ => {
                let top = stack.last().map(String::as_str);
                if let Some(c) = cur.as_mut() {
                    // Properties of the event itself, not of nested VALARMs.
                    if top == Some(c.kind.as_str()) && c.props.len() < 1_000 {
                        if p.name == "ATTENDEE" {
                            c.attendees += 1;
                            if c.attendees > MAX_ATTENDEES {
                                continue;
                            }
                        }
                        c.props.push(p);
                    }
                } else if top == Some("VCALENDAR") && p.name == "X-WR-CALNAME" {
                    cal_name = Some(unescape(&p.value).trim().to_owned());
                }
            }
        }
    }
    components.extend(cur.take());
    if !saw_calendar {
        return Err(Error::Invalid("calendar: no BEGIN:VCALENDAR".into()));
    }

    let mut buf = TextBuf::new(ctx.opts.max_text_chars);
    let mut tracker = SectionTracker::default();
    let mut tags = Vec::new();
    let mut links = Vec::new();
    let mut events_meta = Vec::new();
    for c in &components {
        if buf.is_full() {
            break;
        }
        let (rendered, m) = render(c, &mut tags, &mut links);
        let label = c.text("SUMMARY").unwrap_or_else(|| {
            if c.kind == "VTODO" {
                "Task".into()
            } else {
                "Event".into()
            }
        });
        buf.paragraph();
        tracker.start(
            vec![util::truncate_chars(&util::normalize_ws(&label), 200)],
            buf.len(),
        );
        buf.push(&rendered);
        events_meta.push(m);
    }
    let truncated = buf.is_full() || dropped > 0;
    let (text, cut) = buf.finish();
    let sections = tracker.finish(text.len());

    let mut meta = util::obj();
    if let Some(n) = cal_name.as_ref().filter(|n| !n.is_empty()) {
        meta.insert("calendar_name".into(), Value::String(n.clone()));
    }
    if let Some(Value::Object(first)) = events_meta.first() {
        for key in ["uid", "start", "end", "timezone", "all_day"] {
            if let Some(v) = first.get(key) {
                meta.insert(key.into(), v.clone());
            }
        }
    }
    if dropped > 0 {
        meta.insert("events_dropped".into(), Value::from(dropped));
    }
    let title = if components.len() == 1 {
        components[0].text("SUMMARY")
    } else {
        cal_name
            .filter(|n| !n.is_empty())
            .or_else(|| components.first().and_then(|c| c.text("SUMMARY")))
    };
    meta.insert("events".into(), Value::Array(events_meta));
    Ok(Extracted {
        title,
        text,
        sections,
        links,
        tags,
        metadata: Value::Object(meta),
        truncated: truncated || cut,
    })
}

fn render(c: &Component, tags: &mut Vec<String>, links: &mut Vec<String>) -> (String, Value) {
    let mut lines = Vec::new();
    let mut m = Map::new();
    let is_task = c.kind == "VTODO";
    m.insert(
        "kind".into(),
        Value::from(if is_task { "task" } else { "event" }),
    );
    let summary = c.text("SUMMARY");
    lines.push(format!(
        "{}: {}",
        if is_task { "Task" } else { "Event" },
        summary.as_deref().unwrap_or("(untitled)")
    ));
    if let Some(s) = &summary {
        m.insert("summary".into(), Value::String(s.clone()));
    }
    if let Some(uid) = c.text("UID") {
        m.insert("uid".into(), Value::String(uid));
    }
    let start = c.get("DTSTART").and_then(parse_when);
    let end = c.get("DTEND").or_else(|| c.get("DUE")).and_then(parse_when);
    if let Some(s) = &start {
        lines.push(format!("Start: {}", when_text(s)));
        m.insert("start".into(), Value::String(s.iso.clone()));
        if let Some(tz) = &s.tz {
            m.insert("timezone".into(), Value::String(tz.clone()));
        }
        m.insert("all_day".into(), Value::Bool(s.all_day));
    }
    if let Some(e) = &end {
        let label = if is_task && c.get("DTEND").is_none() {
            "Due"
        } else {
            "End"
        };
        lines.push(format!("{label}: {}", when_text(e)));
        m.insert("end".into(), Value::String(e.iso.clone()));
        if let Some(tz) = &e.tz {
            m.insert("end_timezone".into(), Value::String(tz.clone()));
        }
    }
    if let Some(d) = c.text("DURATION") {
        lines.push(format!("Duration: {d}"));
        m.insert("duration".into(), Value::String(d));
    }
    if let Some(r) = c.text("RRULE") {
        lines.push(format!("Repeats: {r}"));
        m.insert("rrule".into(), Value::String(r));
    }
    if let Some(r) = c.get("RECURRENCE-ID").and_then(parse_when) {
        m.insert("recurrence_id".into(), Value::String(r.iso));
    }
    if let Some(l) = c.text("LOCATION") {
        lines.push(format!("Location: {l}"));
        m.insert("location".into(), Value::String(l));
    }
    if let Some(o) = c.get("ORGANIZER") {
        let o = person(o);
        lines.push(format!("Organizer: {o}"));
        m.insert("organizer".into(), Value::String(o));
    }
    let attendees: Vec<String> = c
        .props
        .iter()
        .filter(|p| p.name == "ATTENDEE")
        .map(person)
        .collect();
    if !attendees.is_empty() {
        lines.push(format!("Attendees: {}", attendees.join(", ")));
        m.insert("attendees".into(), json!(attendees));
    }
    if let Some(s) = c.text("STATUS") {
        lines.push(format!("Status: {}", s.to_lowercase()));
        m.insert("status".into(), Value::String(s));
    }
    if let Some(u) = c.text("URL") {
        util::push_unique(links, &u, LINK_CAP);
        m.insert("url".into(), Value::String(u));
    }
    for cat in c.props.iter().filter(|p| p.name == "CATEGORIES") {
        for t in unescape(&cat.value).split(',') {
            util::push_unique_ci(tags, t.trim(), TAG_CAP);
        }
    }
    let mut text = lines.join("\n");
    if let Some(d) = c.text("DESCRIPTION") {
        text.push_str("\n\n");
        text.push_str(&d);
        util::find_urls(&d, |_, u| util::push_unique(links, u, LINK_CAP));
    }
    (text, Value::Object(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_times() {
        let p =
            parse_line("ATTENDEE;CN=\"Doe, Jane\";PARTSTAT=ACCEPTED:mailto:jane@x.org").unwrap();
        assert_eq!(p.name, "ATTENDEE");
        assert_eq!(person(&p), "Doe, Jane <jane@x.org> (accepted)");
        let w =
            parse_when(&parse_line("DTSTART;TZID=Europe/Berlin:20240501T090000").unwrap()).unwrap();
        assert_eq!(w.iso, "2024-05-01T09:00:00");
        assert_eq!(when_text(&w), "2024-05-01 09:00 (Europe/Berlin)");
        let w = parse_when(&parse_line("DTSTART:20240501T090000Z").unwrap()).unwrap();
        assert_eq!(w.iso, "2024-05-01T09:00:00Z");
        let w = parse_when(&parse_line("DTSTART;VALUE=DATE:20240501").unwrap()).unwrap();
        assert!(w.all_day);
        assert!(parse_when(&parse_line("DTSTART:garbage").unwrap()).is_none());
        assert_eq!(unescape("a\\, b\\nc\\\\"), "a, b\nc\\");
        assert_eq!(unfold("A:b\r\n c\r\nD:e"), ["A:bc", "D:e"]);
    }
}
