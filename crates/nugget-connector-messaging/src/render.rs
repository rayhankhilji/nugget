//! Conversation → documents: one Markdown document per conversation per calendar month (UTC).

use std::fmt::Write as _;

use nugget_connector::{Content, Document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::model::{Attachment, Conversation, Message, Participant, clean_name, one_line};

/// A single message's text is cut beyond this many bytes (with a note).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// A month document larger than this is split into `<id>/<YYYY-MM>`, `<id>/<YYYY-MM>.2`, ...
pub const MAX_DOC_BYTES: usize = 2 * 1024 * 1024;
/// At most this many attachment entries are listed in one document's metadata.
const MAX_META_ATTACHMENTS: usize = 1000;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Per-source rendering options.
#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// `imessage`, `whatsapp`, `telegram`.
    pub service: &'static str,
    /// Mark every document `sensitive`.
    pub private: bool,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A YAML double-quoted scalar. Escapes everything YAML could read as structure or a line
/// break, so an untrusted name can never end the frontmatter.
pub fn yaml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Controls, Unicode line separators and the non-characters YAML refuses to read.
            c if c.is_control()
                || matches!(
                    c,
                    '\u{2028}' | '\u{2029}' | '\u{85}' | '\u{FEFF}' | '\u{FFFE}' | '\u{FFFF}'
                ) =>
            {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Inline Markdown text on one line, with emphasis/link/HTML characters escaped.
pub fn md_inline(s: &str) -> String {
    // Not cut: system lines can be long; names are already cut by `clean_name`.
    let line = one_line(s);
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#' | '|' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Cut `text` to [`MAX_MESSAGE_BYTES`] on a character boundary, noting how much was dropped.
pub fn cap_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.len() <= MAX_MESSAGE_BYTES {
        return text.into();
    }
    let mut end = MAX_MESSAGE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[… message truncated: {} more bytes]",
        &text[..end],
        text.len() - end
    )
    .into()
}

fn stamp(at: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        at.year(),
        at.month() as u8,
        at.day(),
        at.hour(),
        at.minute()
    )
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

fn attachment_label(a: &Attachment) -> String {
    let kind = match a.mime.as_deref().unwrap_or("") {
        m if m.starts_with("image/") => "photo",
        m if m.starts_with("video/") => "video",
        m if m.starts_with("audio/") => "audio",
        "text/vcard" | "text/x-vcard" => "contact",
        _ => "file",
    };
    match &a.name {
        Some(n) if !n.is_empty() => {
            let n = clean_name(n).replace('[', "(").replace(']', ")");
            format!("[{kind}: {n}]")
        }
        _ => format!("[{kind}]"),
    }
}

/// Whether a name reads as `Me`, the label of the user's own messages (`Me`, `ME.`, `Мe` with a
/// Cyrillic `М`, ...). Display names in an export are chosen by their owners.
fn looks_like_me(name: &str) -> bool {
    let mut letters = name.chars().filter(|c| c.is_alphanumeric());
    let (m, e) = (letters.next(), letters.next());
    letters.next().is_none()
        && matches!(
            m,
            Some('M' | 'm' | '\u{41C}' | '\u{43C}' | '\u{39C}' | '\u{FF2D}' | '\u{FF4D}')
        )
        && matches!(
            e,
            Some(
                'E' | 'e' | '\u{415}' | '\u{435}' | '\u{395}' | '\u{3B5}' | '\u{FF25}' | '\u{FF45}'
            )
        )
}

/// A participant's name as shown in a document. Someone else named like the user's own `Me`
/// label is marked, so their messages never read as the user's.
fn shown_name(p: &Participant) -> String {
    if p.me {
        "Me".into()
    } else if looks_like_me(&p.name) {
        format!("{} (not you)", p.name)
    } else {
        p.name.clone()
    }
}

fn author_name(conv: &Conversation, m: &Message) -> String {
    match m.author.and_then(|i| conv.participants.get(i)) {
        Some(p) => shown_name(p),
        None => "Unknown".into(),
    }
}

/// One message as Markdown. Every line of untrusted text is quoted with `> `.
fn render_message(conv: &Conversation, m: &Message) -> String {
    let mut out = String::new();
    if m.system {
        let who = m
            .author
            .and_then(|i| conv.participants.get(i))
            .filter(|_| !m.text.is_empty());
        let text = match who {
            // System lines that carry an author but whose text does not name them.
            Some(p) if !m.text.contains(&p.name) && !p.me => {
                format!("{}: {}", shown_name(p), m.text)
            }
            _ => m.text.clone(),
        };
        let _ = writeln!(out, "_{} · {}_", stamp(m.at), md_inline(&cap_text(&text)));
        out.push('\n');
        return out;
    }
    let _ = writeln!(
        out,
        "**{}** · {}",
        md_inline(&author_name(conv, m)),
        stamp(m.at)
    );
    let text = cap_text(&m.text);
    let text = text.trim_matches(is_line_break);
    if !text.is_empty() {
        let mut prev_cr = false;
        let mut line = String::new();
        let flush = |line: &mut String, out: &mut String| {
            let l = line.trim_end();
            if l.is_empty() {
                out.push_str(">\n");
            } else {
                let _ = writeln!(out, "> {l}");
            }
            line.clear();
        };
        for c in text.chars() {
            if c == '\n' && prev_cr {
                prev_cr = false;
                continue;
            }
            prev_cr = c == '\r';
            if is_line_break(c) {
                flush(&mut line, &mut out);
            } else if c.is_control() && c != '\t' {
                // Other C0/C1 controls are dropped.
            } else {
                line.push(c);
            }
        }
        flush(&mut line, &mut out);
    }
    for a in &m.attachments {
        let _ = writeln!(out, "> {}", attachment_label(a));
    }
    if m.deleted {
        out.push_str("> _(message deleted)_\n");
    }
    out.push('\n');
    out
}

struct Part<'a> {
    messages: Vec<&'a Message>,
    body: String,
}

/// Every document for one conversation, in key order.
pub fn conversation_docs(conv: &Conversation, opts: RenderOptions) -> Vec<Document> {
    let mut docs = Vec::new();
    let mut i = 0;
    let msgs = &conv.messages;
    while i < msgs.len() {
        let (y, mo) = (msgs[i].at.year(), msgs[i].at.month() as u8);
        let mut j = i;
        while j < msgs.len() && msgs[j].at.year() == y && msgs[j].at.month() as u8 == mo {
            j += 1;
        }
        month_docs(conv, &msgs[i..j], y, mo, opts, &mut docs);
        i = j;
    }
    docs
}

fn month_docs(
    conv: &Conversation,
    msgs: &[Message],
    year: i32,
    month: u8,
    opts: RenderOptions,
    docs: &mut Vec<Document>,
) {
    // Leave room for the header (frontmatter with participants and the heading).
    let budget = MAX_DOC_BYTES
        .saturating_sub(64 * 1024)
        .max(MAX_MESSAGE_BYTES * 2);
    let mut parts: Vec<Part<'_>> = vec![Part {
        messages: Vec::new(),
        body: String::new(),
    }];
    for m in msgs {
        let block = render_message(conv, m);
        let cur = parts.last_mut().expect("one part");
        if !cur.messages.is_empty() && cur.body.len() + block.len() > budget {
            parts.push(Part {
                messages: Vec::new(),
                body: String::new(),
            });
        }
        let cur = parts.last_mut().expect("one part");
        cur.body.push_str(&block);
        cur.messages.push(m);
    }
    let total = parts.len();
    for (n, part) in parts.into_iter().enumerate() {
        docs.push(part_doc(conv, part, year, month, n + 1, total, opts));
    }
}

/// Participants shown for a document: everyone in a one-to-one chat, the authors in a group
/// (a large group's member list would swamp every month).
fn doc_participants<'a>(conv: &'a Conversation, msgs: &[&Message]) -> Vec<&'a Participant> {
    if !conv.is_group {
        return conv.participants.iter().collect();
    }
    let mut used = vec![false; conv.participants.len()];
    for m in msgs {
        if let Some(i) = m.author
            && let Some(u) = used.get_mut(i)
        {
            *u = true;
        }
    }
    conv.participants
        .iter()
        .zip(used)
        .filter_map(|(p, u)| u.then_some(p))
        .collect()
}

fn part_doc(
    conv: &Conversation,
    part: Part<'_>,
    year: i32,
    month: u8,
    n: usize,
    total: usize,
    opts: RenderOptions,
) -> Document {
    let period = format!("{year:04}-{month:02}");
    let month_name = MONTHS[usize::from(month.clamp(1, 12)) - 1];
    let name = clean_name(&conv.name);
    let suffix = if total > 1 {
        format!(" (part {n})")
    } else {
        String::new()
    };
    let title = format!("{name} — {month_name} {year}{suffix}");
    let people = doc_participants(conv, &part.messages);

    let mut text = String::with_capacity(part.body.len() + 512);
    text.push_str("---\ntype: conversation\n");
    let _ = writeln!(text, "service: {}", opts.service);
    let _ = writeln!(text, "conversation: {}", yaml_quote(&name));
    let names: Vec<String> = people
        .iter()
        .filter(|p| !p.me)
        .map(|p| yaml_quote(&clean_name(&shown_name(p))))
        .collect();
    let _ = writeln!(text, "participants: [{}]", names.join(", "));
    let _ = writeln!(text, "period: {period}");
    if total > 1 {
        let _ = writeln!(text, "part: {n}");
    }
    text.push_str("---\n");
    let _ = writeln!(text, "# {}\n", md_inline(&title));
    text.push_str(&part.body);

    let first = part.messages.first().map(|m| m.at);
    let last = part.messages.last().map(|m| m.at);
    let participants: Vec<Value> = people
        .iter()
        .map(|p| json!({ "name": p.name, "handle": p.handle, "me": p.me }))
        .collect();
    let contacts: Vec<Value> = people
        .iter()
        .filter(|p| !p.me && !(p.phones.is_empty() && p.emails.is_empty()))
        .map(|p| json!({ "name": p.name, "phones": p.phones, "emails": p.emails }))
        .collect();
    let attachments: Vec<Value> = part
        .messages
        .iter()
        .flat_map(|m| &m.attachments)
        .take(MAX_META_ATTACHMENTS)
        .map(|a| json!({ "name": a.name, "mime": a.mime, "size": a.size }))
        .collect();
    let mut metadata = json!({
        "service": opts.service,
        "thread_id": conv.id,
        "conversation": name,
        "is_group": conv.is_group,
        "period": period,
        "message_count": part.messages.len(),
        "first_at": first.map(rfc3339),
        "last_at": last.map(rfc3339),
        "participants": participants,
        "contacts": contacts,
        "attachments": attachments,
        "sensitive": opts.private,
    });
    if total > 1 {
        metadata["part"] = json!(n);
    }

    // The version covers the metadata too, so toggling `private` re-sends the documents.
    let mut hashed = text.clone().into_bytes();
    hashed.push(0);
    hashed.extend_from_slice(metadata.to_string().as_bytes());
    let key = if n == 1 {
        format!("{}/{period}", conv.id)
    } else {
        format!("{}/{period}.{n}", conv.id)
    };
    let mut doc = Document::new(key, "text/markdown", Content::Text(text));
    doc.title = Some(title);
    doc.version = Some(sha256_hex(&hashed));
    doc.modified_at = last;
    doc.url = conv.url.clone();
    doc.metadata = metadata;
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn conv() -> Conversation {
        let mut c = Conversation::new("wa:family", "Family");
        c.is_group = true;
        let alice = c.participant("Alice Smith", Some("+15551234567"), false);
        let me = c.participant("Tanuj", None, true);
        c.messages.push(Message::new(
            datetime!(2024-05-01 9:12 UTC),
            Some(alice),
            "Hello\nsecond line",
        ));
        let mut m = Message::new(datetime!(2024-05-01 9:13 UTC), Some(me), "");
        m.attachments.push(Attachment {
            name: Some("IMG-20240501-WA0001.jpg".into()),
            mime: Some("image/jpeg".into()),
            size: None,
        });
        c.messages.push(m);
        c.messages.push(Message::system(
            datetime!(2024-06-02 10:00 UTC),
            "Alice added Bob",
        ));
        c
    }

    const OPTS: RenderOptions = RenderOptions {
        service: "whatsapp",
        private: true,
    };

    #[test]
    fn renders_contract_shape() {
        let docs = conversation_docs(&conv(), OPTS);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].key.0, "wa:family/2024-05");
        assert_eq!(docs[1].key.0, "wa:family/2024-06");
        assert_eq!(docs[0].title.as_deref(), Some("Family — May 2024"));
        let Content::Text(t) = &docs[0].content else {
            panic!()
        };
        let want = "---\ntype: conversation\nservice: whatsapp\nconversation: \"Family\"\n\
participants: [\"Alice Smith\"]\nperiod: 2024-05\n---\n# Family — May 2024\n\n\
**Alice Smith** · 2024-05-01 09:12\n> Hello\n> second line\n\n\
**Me** · 2024-05-01 09:13\n> [photo: IMG-20240501-WA0001.jpg]\n\n";
        assert_eq!(t, want);
        let md = &docs[0].metadata;
        assert_eq!(md["message_count"], 2);
        assert_eq!(md["sensitive"], true);
        assert_eq!(md["contacts"][0]["phones"][0], "+15551234567");
        assert_eq!(md["first_at"], "2024-05-01T09:12:00Z");
        let Content::Text(t) = &docs[1].content else {
            panic!()
        };
        assert!(
            t.ends_with("_2024-06-02 10:00 · Alice added Bob_\n\n"),
            "{t}"
        );
        // Stable.
        assert_eq!(docs, conversation_docs(&conv(), OPTS));
    }

    #[test]
    fn untrusted_text_cannot_escape() {
        let mut c = Conversation::new("wa:x", "Evil\"\n---\nname");
        let a = c.participant("**x**\n# h", None, false);
        c.messages.push(Message::new(
            datetime!(2024-01-01 0:00 UTC),
            Some(a),
            "---\n# heading\r\nline\u{2028}sep",
        ));
        let docs = conversation_docs(&c, OPTS);
        let Content::Text(t) = &docs[0].content else {
            panic!()
        };
        assert_eq!(t.matches("\n---\n").count(), 1, "{t}");
        assert!(t.contains("conversation: \"Evil\\\" --- name\""), "{t}");
        assert!(t.contains("> ---\n> # heading\n> line\n> sep\n"), "{t}");
        for line in t.lines().skip_while(|l| !l.starts_with("# ")).skip(1) {
            assert!(
                line.is_empty() || line.starts_with("> ") || line == ">" || line.starts_with("**"),
                "{line:?}"
            );
        }
    }

    /// Regression: a group member whose display name is `Me` (Telegram and WhatsApp names are
    /// chosen by their owners) rendered exactly like the user's own messages.
    #[test]
    fn others_named_me_never_read_as_the_user() {
        let mut c = Conversation::new("tg:9", "Group");
        c.is_group = true;
        let me = c.participant("Tanuj", None, true);
        let fake = c.participant("Me", Some("user666"), false);
        let cyr = c.participant("\u{41C}e.", Some("user667"), false);
        let mel = c.participant("Mel", Some("user668"), false);
        for (i, a) in [me, fake, cyr, mel].into_iter().enumerate() {
            c.messages.push(Message::new(
                datetime!(2024-01-01 0:00 UTC) + time::Duration::minutes(i as i64),
                Some(a),
                "I'll pay",
            ));
        }
        let docs = conversation_docs(&c, OPTS);
        let Content::Text(t) = &docs[0].content else {
            panic!()
        };
        assert_eq!(t.matches("**Me** ·").count(), 1, "{t}");
        assert!(t.contains("**Me (not you)** · 2024-01-01 00:01"), "{t}");
        assert!(
            t.contains("**\u{41C}e. (not you)** · 2024-01-01 00:02"),
            "{t}"
        );
        assert!(t.contains("**Mel** · 2024-01-01 00:03"), "{t}");
        assert!(t.contains("participants: [\"Me (not you)\""), "{t}");
    }

    #[test]
    fn long_messages_and_big_months_are_capped() {
        let mut c = Conversation::new("tg:1", "Big");
        let a = c.participant("A", None, false);
        c.messages.push(Message::new(
            datetime!(2024-01-01 0:00 UTC),
            Some(a),
            "x".repeat(MAX_MESSAGE_BYTES + 10),
        ));
        for i in 0..80 {
            c.messages.push(Message::new(
                datetime!(2024-01-02 0:00 UTC) + time::Duration::minutes(i),
                Some(a),
                "y".repeat(60_000),
            ));
        }
        let docs = conversation_docs(&c, OPTS);
        let keys: Vec<&str> = docs.iter().map(|d| d.key.0.as_str()).collect();
        assert_eq!(keys, ["tg:1/2024-01", "tg:1/2024-01.2", "tg:1/2024-01.3"]);
        for d in &docs {
            let Content::Text(t) = &d.content else {
                panic!()
            };
            assert!(t.len() <= MAX_DOC_BYTES, "{}", t.len());
        }
        let Content::Text(t) = &docs[0].content else {
            panic!()
        };
        assert!(t.contains("[… message truncated: 10 more bytes]"));
        let n: u64 = docs
            .iter()
            .map(|d| d.metadata["message_count"].as_u64().unwrap())
            .sum();
        assert_eq!(n, 81);
    }

    #[test]
    fn yaml_quoting() {
        assert_eq!(
            yaml_quote("a\"b\\c\u{1}\u{2028}"),
            "\"a\\\"b\\\\c\\u0001\\u2028\""
        );
    }

    /// Regression: U+FFFE / U+FFFF are not allowed in YAML, so an untrusted name holding one
    /// made the whole frontmatter unreadable.
    #[test]
    fn yaml_quoting_escapes_non_characters() {
        assert_eq!(yaml_quote("a\u{FFFE}b\u{FFFF}"), "\"a\\uFFFEb\\uFFFF\"");
    }

    /// Regression: system lines were cut to 200 characters by the name cleaner.
    #[test]
    fn long_system_lines_are_not_cut() {
        let mut c = Conversation::new("tg:1", "X");
        let long = format!("Alice changed the group description: {}", "d".repeat(500));
        c.messages.push(Message::system(
            datetime!(2024-01-01 0:00 UTC),
            long.clone(),
        ));
        let docs = conversation_docs(&c, OPTS);
        let Content::Text(t) = &docs[0].content else {
            panic!()
        };
        assert!(t.contains(&long), "{t}");
    }
}
