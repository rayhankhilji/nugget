//! RFC 822 / MIME mail via `mail-parser`: headers into metadata, the text/plain body (or the
//! HTML body converted), quoted replies and signatures moved into metadata, attachment names.

use mail_parser::{
    Address, HeaderForm, HeaderValue, Message, MessageParser, MimeHeaders, PartType,
};
use nugget_core::{Error, Result};
use serde_json::{Map, Value, json};

use crate::util::{self, LINK_CAP, TAG_CAP};
use crate::{Ctx, Extracted, html};

/// Most `message/rfc822` parts (nested messages) accepted in one input.
const MAX_NESTED_MESSAGES: usize = 64;
const MAX_ATTACHMENTS: usize = 500;
/// Cap on quoted / signature text kept in metadata.
const MAX_SIDE_CHARS: usize = 100_000;
/// A trailing block after `-- ` longer than this is not treated as a signature.
const MAX_SIGNATURE_LINES: usize = 30;

/// Case-insensitive occurrences of `message/<sub>` (cheap pre-filter on the `/`).
fn count_ci(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .filter(|w| w[7] == b'/' && w.eq_ignore_ascii_case(needle))
        .count()
}

fn addresses(a: Option<&Address<'_>>) -> Vec<String> {
    let Some(a) = a else { return Vec::new() };
    a.iter()
        .take(500)
        .filter_map(|addr| {
            let name = addr.name().map(str::trim).filter(|n| !n.is_empty());
            let email = addr.address().map(str::trim).filter(|e| !e.is_empty());
            match (name, email) {
                (Some(n), Some(e)) => Some(format!("{n} <{e}>")),
                (None, Some(e)) => Some(e.to_owned()),
                (Some(n), None) => Some(n.to_owned()),
                (None, None) => None,
            }
        })
        .collect()
}

fn text_list(h: &HeaderValue<'_>) -> Vec<String> {
    if let Some(list) = h.as_text_list() {
        return list.iter().take(500).map(|s| s.to_string()).collect();
    }
    h.as_text().map(|s| vec![s.to_owned()]).unwrap_or_default()
}

/// Bulk / automated-mail headers kept verbatim (unfolded, RFC 2047 decoded) under snake_case keys.
const LIST_HEADERS: [(&str, &str); 4] = [
    ("list_unsubscribe", "List-Unsubscribe"),
    ("list_id", "List-Id"),
    ("precedence", "Precedence"),
    ("auto_submitted", "Auto-Submitted"),
];

/// First occurrence of `name` as unstructured text, whitespace-collapsed; `None` when absent or blank.
fn header_text(msg: &Message<'_>, name: &str) -> Option<String> {
    msg.header_as(name, HeaderForm::Text)
        .first()
        .and_then(HeaderValue::as_text)
        .map(util::normalize_ws)
        .filter(|s| !s.is_empty())
}

pub(crate) fn extract(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    let nested = count_ci(data, b"message/rfc822") + count_ci(data, b"message/global");
    if nested > MAX_NESTED_MESSAGES {
        return Err(Error::Invalid(format!(
            "mail: {nested} nested messages, over the {MAX_NESTED_MESSAGES} limit"
        )));
    }
    let msg = MessageParser::default()
        .parse(data)
        .ok_or_else(|| Error::Invalid("mail: no RFC 822 headers".into()))?;
    if msg.headers().is_empty() {
        return Err(Error::Invalid("mail: no RFC 822 headers".into()));
    }

    let mut meta = util::obj();
    let from = addresses(msg.from());
    if let Some(f) = from.first() {
        meta.insert("from".into(), Value::String(f.clone()));
    }
    for (key, list) in [
        ("to", addresses(msg.to())),
        ("cc", addresses(msg.cc())),
        ("bcc", addresses(msg.bcc())),
        ("reply_to", addresses(msg.reply_to())),
    ] {
        if !list.is_empty() {
            meta.insert(key.into(), json!(list));
        }
    }
    if let Some(d) = msg.date() {
        meta.insert("date".into(), Value::String(d.to_rfc3339()));
    }
    let subject = msg.subject().map(util::normalize_ws);
    if let Some(s) = &subject {
        meta.insert("subject".into(), Value::String(s.clone()));
    }
    if let Some(id) = msg.message_id() {
        meta.insert("message_id".into(), Value::String(id.to_owned()));
    }
    for (key, list) in [
        ("in_reply_to", text_list(msg.in_reply_to())),
        ("references", text_list(msg.references())),
    ] {
        if !list.is_empty() {
            meta.insert(key.into(), json!(list));
        }
    }
    for (key, name) in LIST_HEADERS {
        if let Some(v) = header_text(&msg, name) {
            meta.insert(key.into(), Value::String(v));
        }
    }

    let mut tags = Vec::new();
    if let Some(labels) = msg.header_raw("X-Gmail-Labels") {
        for l in labels.split(',') {
            let l = l.trim().trim_matches('"');
            if !l.is_empty() {
                util::push_unique_ci(&mut tags, l, TAG_CAP);
            }
        }
    }
    for k in text_list(msg.keywords()) {
        for t in k.split(',') {
            util::push_unique_ci(&mut tags, t.trim(), TAG_CAP);
        }
    }

    let attachments = attachments(&msg);
    if !attachments.is_empty() {
        meta.insert("attachments".into(), Value::Array(attachments));
    }

    let mut links = Vec::new();
    let mut quoted = String::new();
    let mut signature = String::new();
    let plain: Vec<&str> = msg
        .text_body
        .iter()
        .filter_map(|&id| msg.parts.get(id as usize))
        .filter_map(|p| match &p.body {
            PartType::Text(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect();
    let body = if !plain.is_empty() {
        plain.join("\n\n")
    } else {
        let html_src: Vec<&str> = msg
            .html_body
            .iter()
            .filter_map(|&id| msg.parts.get(id as usize))
            .filter_map(|p| match &p.body {
                PartType::Html(t) => Some(t.as_ref()),
                _ => None,
            })
            .collect();
        let out = html::convert(&html_src.join("\n"), ctx.opts.max_text_chars, true);
        links = out.links;
        quoted = out.quoted;
        signature = out.signature;
        out.text
    };
    let body = body.replace("\r\n", "\n");
    let split = split_reply(&body);
    let mut text = split.main;
    append_block(&mut quoted, &split.quoted);
    append_block(&mut signature, &split.signature);
    if text.trim().is_empty() {
        // Nothing but quotes: keep the whole body rather than an empty document.
        text = body.trim().to_owned();
        quoted.clear();
        signature.clear();
    }
    if !quoted.trim().is_empty() {
        meta.insert(
            "quoted".into(),
            Value::String(util::truncate_chars(quoted.trim(), MAX_SIDE_CHARS)),
        );
    }
    if !signature.trim().is_empty() {
        meta.insert(
            "signature".into(),
            Value::String(util::truncate_chars(signature.trim(), MAX_SIDE_CHARS)),
        );
    }
    util::find_urls(&text, |_, u| util::push_unique(&mut links, u, LINK_CAP));

    Ok(Extracted {
        title: subject.filter(|s| !s.is_empty()),
        text,
        sections: Vec::new(),
        links,
        tags,
        metadata: Value::Object(meta),
        truncated: false,
    })
}

fn append_block(dst: &mut String, src: &str) {
    if src.trim().is_empty() {
        return;
    }
    if !dst.is_empty() {
        dst.push_str("\n\n");
    }
    dst.push_str(src.trim());
}

/// Attachment names (and types and sizes); contents are never extracted.
fn attachments(msg: &Message<'_>) -> Vec<Value> {
    msg.attachments()
        .take(MAX_ATTACHMENTS)
        .map(|part| {
            let mut m = Map::new();
            let name = part
                .attachment_name()
                .map(str::to_owned)
                .or_else(|| part.message().and_then(|n| n.subject().map(str::to_owned)));
            if let Some(n) = name {
                m.insert("name".into(), Value::String(util::truncate_chars(&n, 500)));
            }
            if let Some(ct) = part.content_type() {
                let t = match ct.subtype() {
                    Some(sub) => format!("{}/{}", ct.ctype(), sub),
                    None => ct.ctype().to_owned(),
                };
                m.insert("content_type".into(), Value::String(t.to_ascii_lowercase()));
            }
            m.insert("size".into(), Value::from(part.contents().len()));
            Value::Object(m)
        })
        .collect()
}

pub(crate) struct Split {
    pub main: String,
    pub quoted: String,
    pub signature: String,
}

const ATTRIBUTION_ENDINGS: &[&str] = &[
    "wrote:",
    "writes:",
    "schrieb:",
    "a écrit :",
    "a écrit:",
    "escribió:",
    "scrisse:",
    "escreveu:",
    "skrev:",
    "schreef:",
    "napisał:",
];

fn is_attribution(line: &str) -> bool {
    let t = line.trim();
    t.len() < 400 && ATTRIBUTION_ENDINGS.iter().any(|e| t.ends_with(e))
}

/// `-----Original Message-----` and friends: everything after is quoted.
fn is_original_marker(line: &str, next: Option<&str>) -> bool {
    let t = line.trim();
    let lower = t.to_lowercase();
    if t.starts_with("--")
        && (lower.contains("original message")
            || lower.contains("ursprüngliche nachricht")
            || lower.contains("message d'origine")
            || lower.contains("mensaje original"))
    {
        return true;
    }
    // Outlook: a line of underscores followed by a `From:` header block.
    t.len() >= 20
        && t.chars().all(|c| c == '_')
        && next.is_some_and(|n| n.trim_start().starts_with("From:"))
}

fn is_mobile_signature(line: &str) -> bool {
    let t = line.trim();
    t.len() < 80
        && (t.starts_with("Sent from my ")
            || t.starts_with("Get Outlook for ")
            || t.starts_with("Sent from Mail for ")
            || t.starts_with("Sent via "))
}

/// Split a plain-text body into new content, quoted replies and the signature.
pub(crate) fn split_reply(body: &str) -> Split {
    let lines: Vec<&str> = body.lines().collect();
    let mut main: Vec<&str> = Vec::new();
    let mut quoted: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let next = lines.get(i + 1).copied();
        if !main.iter().all(|l| l.trim().is_empty()) && is_original_marker(line, next) {
            quoted.extend(lines[i..].iter().map(|l| (*l).to_owned()));
            break;
        }
        if line.trim_start().starts_with('>') {
            let t = line.trim_start();
            let stripped = t.strip_prefix("> ").or_else(|| t.strip_prefix('>'));
            quoted.push(stripped.unwrap_or(t).to_owned());
            i += 1;
            continue;
        }
        // "On <date>, <who> wrote:" (possibly wrapped onto a second line) before a quote block.
        let attribution_lines = if is_attribution(line) {
            Some(1)
        } else if next.is_some_and(|n| is_attribution(n) && n.trim().len() < 40)
            && !line.trim().is_empty()
        {
            Some(2)
        } else {
            None
        };
        if let Some(n) = attribution_lines {
            let mut j = i + n;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j >= lines.len() || lines[j].trim_start().starts_with('>') {
                quoted.extend(lines[i..i + n].iter().map(|l| l.trim().to_owned()));
                i += n;
                continue;
            }
        }
        main.push(line);
        i += 1;
    }

    let mut signature: Vec<&str> = Vec::new();
    if let Some(pos) = main.iter().rposition(|l| *l == "-- " || *l == "--") {
        if main.len() - pos - 1 <= MAX_SIGNATURE_LINES {
            signature = main.split_off(pos + 1);
            main.pop();
        }
    } else if let Some(pos) = main.iter().rposition(|l| !l.trim().is_empty())
        && is_mobile_signature(main[pos])
    {
        signature = main.split_off(pos);
    }

    Split {
        main: collapse_blank_lines(&main),
        quoted: collapse_blank_lines(&quoted),
        signature: collapse_blank_lines(&signature),
    }
}

fn collapse_blank_lines<S: AsRef<str>>(lines: &[S]) -> String {
    let mut out = String::new();
    let mut blank = false;
    for l in lines {
        let l = l.as_ref().trim_end();
        if l.trim().is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank { "\n\n" } else { "\n" });
        }
        blank = false;
        out.push_str(l);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quotes_and_signature() {
        let s = split_reply(
            "Sounds good.\n\nSee you then.\n-- \nAlice\nACME\n\nOn Mon, 1 Jan 2024, Bob <b@x.org> wrote:\n> Lunch?\n> Noon?\n",
        );
        assert_eq!(s.main, "Sounds good.\n\nSee you then.");
        assert_eq!(s.signature, "Alice\nACME");
        assert_eq!(
            s.quoted,
            "On Mon, 1 Jan 2024, Bob <b@x.org> wrote:\nLunch?\nNoon?"
        );
    }

    #[test]
    fn interleaved_and_outlook() {
        let s = split_reply("> q1\nanswer 1\n> q2\nanswer 2\n");
        assert_eq!(s.main, "answer 1\nanswer 2");
        assert_eq!(s.quoted, "q1\nq2");
        let s = split_reply("New text\n\n-----Original Message-----\nFrom: x\nold");
        assert_eq!(s.main, "New text");
        assert!(s.quoted.starts_with("-----Original Message-----"));
        let s = split_reply("Ok\n\nSent from my iPhone");
        assert_eq!(s.main, "Ok");
        assert_eq!(s.signature, "Sent from my iPhone");
    }

    #[test]
    fn wrapped_attribution() {
        let s = split_reply(
            "Yes\n\nOn Tue, Feb 2, 2024 at 9:00 AM Carol Example <carol@example.com>\nwrote:\n\n> hi\n",
        );
        assert_eq!(s.main, "Yes");
        assert!(s.quoted.contains("Carol Example"));
    }

    fn meta(raw: &str) -> Map<String, Value> {
        let opts = crate::ExtractOptions::default();
        let ctx = Ctx {
            opts: &opts,
            mime: "message/rfc822",
            charset: None,
            path: None,
            ocr: None,
        };
        match extract(raw.as_bytes(), &ctx).unwrap().metadata {
            Value::Object(m) => m,
            other => panic!("metadata not an object: {other}"),
        }
    }

    #[test]
    fn list_headers_into_metadata() {
        let m = meta(
            "From: News <news@example.com>\r\nTo: a@example.org\r\nSubject: Weekly\r\n\
             List-Unsubscribe: <mailto:unsub@example.com?subject=unsubscribe>,\r\n\
             \t<https://example.com/unsub?id=1>\r\n\
             List-Id: Example News <news.example.com>\r\n\
             Precedence:  bulk \r\n\
             Auto-Submitted: =?utf-8?q?auto-generated?=\r\n\r\nHello\r\n",
        );
        assert_eq!(
            m["list_unsubscribe"],
            "<mailto:unsub@example.com?subject=unsubscribe>, <https://example.com/unsub?id=1>"
        );
        assert_eq!(m["list_id"], "Example News <news.example.com>");
        assert_eq!(m["precedence"], "bulk");
        assert_eq!(m["auto_submitted"], "auto-generated");
    }

    #[test]
    fn list_headers_absent() {
        let m = meta("From: a@example.com\r\nSubject: Hi\r\n\r\nHello\r\n");
        for (key, _) in LIST_HEADERS {
            assert!(!m.contains_key(key), "{key} should be absent");
        }
        assert_eq!(m["subject"], "Hi");
    }

    #[test]
    fn list_headers_case_insensitive() {
        let m = meta(
            "from: a@example.com\r\nLIST-UNSUBSCRIBE: <mailto:u@example.com>\r\n\
             list-id: <l.example.com>\r\nPRECEDENCE: list\r\nauto-submitted: auto-replied\r\n\
             \r\nHello\r\n",
        );
        assert_eq!(m["list_unsubscribe"], "<mailto:u@example.com>");
        assert_eq!(m["list_id"], "<l.example.com>");
        assert_eq!(m["precedence"], "list");
        assert_eq!(m["auto_submitted"], "auto-replied");
    }
}
