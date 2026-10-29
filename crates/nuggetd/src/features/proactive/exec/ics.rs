//! RFC 5545 rendering of one event: TEXT escaping, CRLF line ends, lines folded at 75 octets,
//! UTC times. Every field comes from untrusted card content, so nothing is written raw.
//!
//! **Organizer.** RFC 5545 §3.8.4.3: `ORGANIZER` MUST be present in a group-scheduled event (one
//! with attendees) and MUST NOT be in an event only on the user's own calendar. So:
//! - attendees and a known organizer: `ORGANIZER;CN=<name>:mailto:<email>` plus the attendees;
//! - no attendees: no `ORGANIZER`;
//! - attendees but no organizer address: the event is written as a personal one, without
//!   `ATTENDEE` lines (servers refuse or misattribute a group event without an organizer), and
//!   the addresses are listed in the description so the user can invite them from their
//!   calendar app.

use nugget_proactive::CreateEvent;
use time::OffsetDateTime;

/// Product identifier written into every calendar object.
const PRODID: &str = "-//Nugget//Proactive//EN";
/// Longest line, in octets, before folding (RFC 5545 §3.1).
const FOLD_AT: usize = 75;
/// Longest summary / description kept (characters).
const MAX_SUMMARY: usize = 500;
const MAX_DESCRIPTION: usize = 20_000;
/// Longest organizer display name kept (characters).
const MAX_CN: usize = 200;

/// Who organizes the event: the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Organizer {
    /// A plain address (see [`attendee`]).
    pub email: String,
    pub name: Option<String>,
}

impl Organizer {
    /// `email` when it is a plain address, with the display name `name` (when not blank).
    pub(crate) fn new(email: &str, name: Option<&str>) -> Option<Self> {
        Some(Self {
            email: attendee(email)?,
            name: name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned),
        })
    }

    /// The `ORGANIZER` content line (unfolded).
    fn line(&self) -> String {
        let cn = self
            .name
            .as_deref()
            .map(|n| param_value(&clip(n, MAX_CN)))
            .filter(|v| !v.is_empty() && v != "\"\"");
        match cn {
            Some(cn) => format!("ORGANIZER;CN={cn}:mailto:{}", self.email),
            None => format!("ORGANIZER:mailto:{}", self.email),
        }
    }
}

/// Render `event` as a complete `VCALENDAR` with one `VEVENT`. `method` adds `METHOD:` (e.g.
/// `PUBLISH` for a file the OS opens; CalDAV servers refuse objects with one). `organizer` is
/// the user (see the module docs for when it is written).
pub(crate) fn render(
    event: &CreateEvent,
    uid: &str,
    stamp: OffsetDateTime,
    method: Option<&str>,
    organizer: Option<&Organizer>,
) -> String {
    let attendees: Vec<String> = event.attendees.iter().filter_map(|a| attendee(a)).collect();
    // Without an organizer the attendees cannot be scheduled: keep them in the description.
    let uninvited = (organizer.is_none() && !attendees.is_empty())
        .then(|| format!("Invite: {}", attendees.join(", ")));
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        format!("PRODID:{PRODID}"),
        "CALSCALE:GREGORIAN".to_owned(),
    ];
    if let Some(m) = method {
        lines.push(format!("METHOD:{m}"));
    }
    lines.push("BEGIN:VEVENT".to_owned());
    lines.push(format!("UID:{}", escape_text(uid)));
    lines.push(format!("DTSTAMP:{}", utc(stamp)));
    lines.push(format!("DTSTART:{}", utc(event.start)));
    // A zero-length or inverted range would be rejected by servers; keep DTEND after DTSTART.
    let end = if event.end > event.start {
        event.end
    } else {
        event.start + time::Duration::minutes(30)
    };
    lines.push(format!("DTEND:{}", utc(end)));
    lines.push(format!(
        "SUMMARY:{}",
        escape_text(&clip(&event.title, MAX_SUMMARY))
    ));
    let description = event
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .map(|d| clip(d, MAX_DESCRIPTION));
    let description = match (description, uninvited) {
        (Some(d), Some(u)) => Some(format!("{d}\n\n{u}")),
        (d, u) => d.or(u),
    };
    if let Some(d) = description {
        lines.push(format!("DESCRIPTION:{}", escape_text(&d)));
    }
    if let Some(o) = organizer.filter(|_| !attendees.is_empty()) {
        lines.push(o.line());
        for addr in &attendees {
            lines.push(format!(
                "ATTENDEE;ROLE=REQ-PARTICIPANT;RSVP=TRUE:mailto:{addr}"
            ));
        }
    }
    lines.push("END:VEVENT".to_owned());
    lines.push("END:VCALENDAR".to_owned());
    let mut out = String::new();
    for l in lines {
        out.push_str(&fold(&l));
        out.push_str("\r\n");
    }
    out
}

/// `YYYYMMDDTHHMMSSZ`.
pub(crate) fn utc(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// RFC 5545 §3.3.11 TEXT: `\` `;` `,` escaped, line breaks as `\n`, other controls dropped.
pub(crate) fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// RFC 5545 §3.1 `param-value`: `"` and control characters dropped (a quoted-string cannot hold
/// them); quoted when it holds `:`, `;` or `,`.
pub(crate) fn param_value(s: &str) -> String {
    let v: String = s
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| *c != '"' && !c.is_control())
        .collect();
    if v.contains([':', ';', ',']) {
        format!("\"{v}\"")
    } else {
        v
    }
}

/// Fold a content line: at most 75 octets per physical line, continuations start with a space,
/// never splitting a UTF-8 sequence.
pub(crate) fn fold(line: &str) -> String {
    if line.len() <= FOLD_AT {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len() + line.len() / FOLD_AT * 3);
    let mut used = 0;
    // Continuation lines carry a leading space, so they hold 74 octets of content.
    let mut limit = FOLD_AT;
    for c in line.chars() {
        let n = c.len_utf8();
        if used + n > limit {
            out.push_str("\r\n ");
            used = 1;
            limit = FOLD_AT;
        }
        out.push(c);
        used += n;
    }
    out
}

/// A plain `local@domain` address, or `None` (anything that could break the line or the URI).
pub(crate) fn attendee(addr: &str) -> Option<String> {
    let a = addr.trim();
    let a = a
        .strip_prefix("mailto:")
        .or_else(|| a.strip_prefix("MAILTO:"))
        .unwrap_or(a);
    let (local, domain) = a.rsplit_once('@')?;
    let ok = !local.is_empty()
        && !domain.is_empty()
        && a.len() <= 254
        && !a.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, ':' | ';' | ',' | '"' | '<' | '>' | '\\' | '(' | ')')
        });
    ok.then(|| a.to_owned())
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, Time, UtcOffset};

    fn at(day: u8, h: u8, m: u8, offset_h: i8) -> OffsetDateTime {
        Date::from_calendar_date(2026, Month::October, day)
            .unwrap()
            .with_time(Time::from_hms(h, m, 0).unwrap())
            .assume_offset(UtcOffset::from_hms(offset_h, 0, 0).unwrap())
    }

    fn event() -> CreateEvent {
        CreateEvent {
            title: "Pricing; review, \"pilot\" \\ v2".into(),
            start: at(29, 15, 0, 2),
            end: at(29, 15, 30, 2),
            attendees: vec![
                "ana@acme.io".into(),
                "evil@x.io\r\nATTENDEE:mailto:other@y.io".into(),
                "not an address".into(),
            ],
            description: Some("Line one\r\nLine two\nwith a tab\there".into()),
        }
    }

    #[test]
    fn renders_a_valid_event_with_crlf_and_utc_times() {
        let s = render(
            &event(),
            "abc@nugget",
            at(28, 9, 0, 0),
            Some("PUBLISH"),
            Some(&me()),
        );
        assert!(s.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"), "{s}");
        assert!(s.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
        // Every line ends in CRLF; no bare LF anywhere.
        assert_eq!(s.matches('\n').count(), s.matches("\r\n").count());
        assert!(s.contains("\r\nMETHOD:PUBLISH\r\n"));
        assert!(s.contains("\r\nUID:abc@nugget\r\n"));
        assert!(s.contains("\r\nDTSTAMP:20261028T090000Z\r\n"));
        assert!(s.contains("\r\nDTSTART:20261029T130000Z\r\n"));
        assert!(s.contains("\r\nDTEND:20261029T133000Z\r\n"));
        assert!(s.contains("\r\nSUMMARY:Pricing\\; review\\, \"pilot\" \\\\ v2\r\n"));
        assert!(s.contains("\r\nDESCRIPTION:Line one\\nLine two\\nwith a tab here\r\n"));
        assert!(s.contains("\r\nATTENDEE;ROLE=REQ-PARTICIPANT;RSVP=TRUE:mailto:ana@acme.io\r\n"));
        assert!(s.contains("\r\nORGANIZER;CN=Alex Morgan:mailto:alex@morgan.example\r\n"));
        // An injected line break cannot add properties.
        assert_eq!(s.matches("ATTENDEE").count(), 1, "{s}");
        assert!(!s.contains("other@y.io"));
        assert!(!s.contains("METHOD:PUBLISH\r\nMETHOD"));
        let caldav = render(&event(), "u", at(28, 9, 0, 0), None, Some(&me()));
        assert!(!caldav.contains("METHOD:"));
    }

    fn me() -> Organizer {
        Organizer::new("alex@morgan.example", Some("Alex Morgan")).unwrap()
    }

    #[test]
    fn organizer_only_with_attendees() {
        // No attendees: a personal event, no ORGANIZER.
        let mut e = event();
        e.attendees.clear();
        let s = render(&e, "u", at(28, 9, 0, 0), None, Some(&me()));
        assert!(!s.contains("ORGANIZER") && !s.contains("ATTENDEE"), "{s}");
        // Attendees but nobody to organize: no group event; the addresses go in the description.
        let s = render(&event(), "u", at(28, 9, 0, 0), Some("PUBLISH"), None);
        assert!(!s.contains("ORGANIZER") && !s.contains("ATTENDEE"), "{s}");
        assert!(
            s.contains(
                "DESCRIPTION:Line one\\nLine two\\nwith a tab here\\n\\nInvite: ana@acme.io\r\n"
            ),
            "{s}"
        );
        let mut e = event();
        e.description = None;
        let s = render(&e, "u", at(28, 9, 0, 0), None, None);
        assert!(s.contains("\r\nDESCRIPTION:Invite: ana@acme.io\r\n"), "{s}");
        // No name: no CN.
        let o = Organizer::new("mailto:alex@morgan.example", Some("  ")).unwrap();
        let s = render(&event(), "u", at(28, 9, 0, 0), None, Some(&o));
        assert!(
            s.contains("\r\nORGANIZER:mailto:alex@morgan.example\r\n"),
            "{s}"
        );
        assert_eq!(Organizer::new("not an address", Some("x")), None);
    }

    #[test]
    fn organizer_names_are_quoted_and_folded() {
        assert_eq!(param_value("Alex Morgan"), "Alex Morgan");
        assert_eq!(param_value("Morgan, Alex"), "\"Morgan, Alex\"");
        assert_eq!(param_value("a:b;c"), "\"a:b;c\"");
        assert_eq!(param_value("Alex \"AM\" Morgan"), "Alex AM Morgan");
        assert_eq!(
            param_value("Alex\r\nORGANIZER:x\u{7}"),
            "\"AlexORGANIZER:x\""
        );
        assert_eq!(param_value("a\tb"), "a b");
        // An injected line break cannot add a property; a quote cannot end the value early.
        let o = Organizer::new(
            "alex@morgan.example",
            Some("Evil\r\nATTENDEE:mailto:x@y.io\"; X"),
        )
        .unwrap();
        let s = render(&event(), "u", at(28, 9, 0, 0), None, Some(&o));
        assert!(
            s.contains(
                "\r\nORGANIZER;CN=\"EvilATTENDEE:mailto:x@y.io; X\":mailto:alex@morgan.example\r\n"
            ),
            "{s}"
        );
        assert_eq!(s.matches("\r\nATTENDEE").count(), 1, "{s}");
        // A long name folds at 75 octets and unfolds to the same line.
        let long = "Ågren-Östlund ".repeat(12);
        let o = Organizer::new("alex@morgan.example", Some(&long)).unwrap();
        let s = render(&event(), "u", at(28, 9, 0, 0), None, Some(&o));
        let start = s.find("ORGANIZER").unwrap();
        let end = s[start..].find("\r\nATTENDEE").unwrap() + start;
        let folded = &s[start..end];
        assert!(folded.contains("\r\n "), "{folded}");
        assert!(folded.split("\r\n").all(|l| l.len() <= 75), "{folded}");
        assert_eq!(
            folded.replace("\r\n ", ""),
            format!(
                "ORGANIZER;CN={}:mailto:alex@morgan.example",
                long.trim_end()
            )
        );
    }

    #[test]
    fn inverted_ranges_get_a_default_length() {
        let mut e = event();
        e.end = e.start;
        let s = render(&e, "u", at(28, 9, 0, 0), None, None);
        assert!(s.contains("DTEND:20261029T133000Z"), "{s}");
    }

    #[test]
    fn long_lines_fold_at_75_octets_without_splitting_characters() {
        let long = format!("SUMMARY:{}", "é".repeat(100));
        let folded = fold(&long);
        for (i, l) in folded.split("\r\n").enumerate() {
            assert!(l.len() <= 75, "line {i} has {} octets", l.len());
            if i > 0 {
                assert!(l.starts_with(' '));
            }
        }
        // Unfolding (drop CRLF + space) restores the line.
        assert_eq!(folded.replace("\r\n ", ""), long);
        assert_eq!(fold("short"), "short");
        let ascii = "x".repeat(200);
        let f = fold(&ascii);
        assert_eq!(f.split("\r\n").next().unwrap().len(), 75);
        assert_eq!(f.replace("\r\n ", ""), ascii);
    }

    #[test]
    fn escaping_and_attendees() {
        assert_eq!(escape_text("a\\b;c,d\ne\u{7}"), "a\\\\b\\;c\\,d\\ne");
        assert_eq!(attendee(" mailto:a@b.io ").as_deref(), Some("a@b.io"));
        assert_eq!(attendee("a;x@b.io"), None);
        assert_eq!(attendee("nobody"), None);
        assert_eq!(attendee("a@b.io>"), None);
    }
}
