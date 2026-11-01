//! RFC 822 / MIME mail via `mail-parser`: headers into metadata, the text/plain body (or the
//! HTML body converted), quoted replies and signatures moved into metadata, attachment names.
//! The text starts with `From:` / `Date:` lines (see [`text_header`]); `metadata.text_header`
//! holds that exact prefix.

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
    let header = text_header(from.first().map(String::as_str), msg.date());
    if !header.is_empty() {
        text = if text.is_empty() {
            header.trim_end().to_owned()
        } else {
            format!("{header}{text}")
        };
        meta.insert("text_header".into(), Value::String(header));
    }

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

/// Longest sender kept in the text header.
const MAX_HEADER_FROM_CHARS: usize = 200;

/// `From: <sender>` and `Date: <YYYY-MM-DD>` lines (each only when known) and a blank line, put
/// before the body so a passage says who sent it and when; empty when neither is known. The date
/// is the calendar date in the header's own offset (as the sender wrote it).
fn text_header(from: Option<&str>, date: Option<&mail_parser::DateTime>) -> String {
    let mut out = String::new();
    if let Some(f) = from.map(util::normalize_ws).filter(|f| !f.is_empty()) {
        out.push_str("From: ");
        out.push_str(&util::truncate_chars(&f, MAX_HEADER_FROM_CHARS));
        out.push('\n');
    }
    if let Some(d) = date.filter(|d| d.is_valid()) {
        out.push_str(&format!(
            "Date: {:04}-{:02}-{:02}\n",
            d.year, d.month, d.day
        ));
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
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
    "写道:",
    "写道：",
    "寫道:",
    "寫道：",
    "كتب:",
];

/// The verb of a localised attribution whose line ends with the sender (`… <a@b>:`) instead:
/// `Am 27.10.2026 um 16:05 schrieb Paul <p@x>:`, `في … كتب Sam <s@x>:`.
const ATTRIBUTION_VERBS: &[&str] = &[
    " wrote ",
    " schrieb ",
    " a écrit ",
    " escribió ",
    " escreveu ",
    " scrisse ",
    " schreef ",
    " skrev ",
    " napisał ",
    "ने लिखा",
    "写道",
    "寫道",
    " كتب ",
    "،كتب ",
];

fn is_attribution(line: &str) -> bool {
    let t = line.trim();
    if t.len() >= 400 {
        return false;
    }
    if ATTRIBUTION_ENDINGS.iter().any(|e| t.ends_with(e)) {
        return true;
    }
    // Hindi Gmail: `… को Sam <s@x> ने लिखा:`; the sender's address keeps "मैंने लिखा:" ("I wrote:")
    // in the user's own text from matching.
    if t.ends_with("> ने लिखा:") && t.contains('@') {
        return true;
    }
    // `… Name <address>:` (or `：`): a localised header that ends with the sender. It needs the
    // verb, or a leading date as Japanese Gmail writes it (`2026年10月28日(水) 14:03 Sam <s@x>:`).
    let Some(head) = t.strip_suffix(':').or_else(|| t.strip_suffix('：')) else {
        return false;
    };
    let head = head.trim_end();
    if !(head.ends_with('>') && head.contains('@') && head.contains('<')) {
        return false;
    }
    ATTRIBUTION_VERBS.iter().any(|v| head.contains(v))
        || head.starts_with("كتب ")
        || (head.starts_with(|c: char| c.is_ascii_digit()) && head.contains('年'))
}

/// `-----Original Message-----` and friends: everything after is quoted.
fn is_original_marker(line: &str, next: Option<&str>) -> bool {
    let t = line.trim();
    let lower = t.to_lowercase();
    if t.starts_with("--") && ORIGINAL_MESSAGE.iter().any(|m| lower.contains(m)) {
        return true;
    }
    // Outlook: a line of underscores followed by a (localised) `From:` header block.
    t.len() >= 20 && t.chars().all(|c| c == '_') && next.is_some_and(is_from_header)
}

/// The `To:` line of a quoted header block.
const TO_HEADERS: &[&str] = &[
    "To:",
    "An:",
    "À :",
    "À:",
    "Para:",
    "A:",
    "Aan:",
    "Till:",
    "Do:",
    "收件人:",
    "收件人：",
    "宛先:",
    "宛先：",
    "إلى:",
    "प्रति:",
];

/// The `Subject:` line of a quoted header block.
const SUBJECT_HEADERS: &[&str] = &[
    "Subject:",
    "Betreff:",
    "Objet :",
    "Objet:",
    "Asunto:",
    "Assunto:",
    "Oggetto:",
    "Onderwerp:",
    "Ämne:",
    "Temat:",
    "主题:",
    "主题：",
    "主旨:",
    "主旨：",
    "件名:",
    "件名：",
    "الموضوع:",
    "विषय:",
];

/// Subject prefixes of a forwarded message.
const FORWARD_PREFIXES: &[&str] = &[
    "fw:",
    "fwd:",
    "wg:",
    "tr:",
    "rv:",
    "enc:",
    "i:",
    "doorst:",
    "vb:",
    "转发:",
    "转发：",
    "轉寄:",
    "転送:",
    "fw：",
];

/// Byte length of `line` up to the end of the first of `heads` it starts with.
fn starts_with_any(line: &str, heads: &[&str]) -> Option<usize> {
    let t = line.trim_start();
    heads
        .iter()
        .find(|h| t.starts_with(**h))
        .map(|h| line.len() - t.len() + h.len())
}

/// The subject of the header block that starts at `rest[0]` (its first 8 lines, up to a blank
/// one), when the block also names a recipient: `Some(subject)` for a real header block.
fn block_subject<'a>(rest: &[&'a str]) -> Option<&'a str> {
    let block: Vec<&str> = rest
        .iter()
        .take(8)
        .take_while(|l| !l.trim().is_empty())
        .copied()
        .collect();
    if !block
        .iter()
        .any(|l| starts_with_any(l, TO_HEADERS).is_some())
    {
        return None;
    }
    block
        .iter()
        .find_map(|l| starts_with_any(l, SUBJECT_HEADERS).map(|n| l[n..].trim()))
}

fn is_forward_subject(subject: &str) -> bool {
    let lower = subject.to_lowercase();
    FORWARD_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Whether the quote starts at `rest[0]`, given the reply's last non-blank line `above`.
fn quote_starts(rest: &[&str], above: Option<&str>) -> bool {
    let line = rest[0];
    let next = rest.get(1).copied();
    let t = line.trim();
    let lower = t.to_lowercase();
    if t.starts_with("--") && ORIGINAL_MESSAGE.iter().any(|m| lower.contains(m)) {
        return true;
    }
    // A header block (after underscores or on its own) quotes the earlier mail, unless it is a
    // forward: then its text is what the user sent on, and stays the mail's own.
    let forwarded = |block: &[&str]| {
        above.is_some_and(is_forward_marker) || block_subject(block).is_some_and(is_forward_subject)
    };
    if is_original_marker(line, next) {
        return !forwarded(&rest[1..]);
    }
    is_header_block(line, next) && block_subject(rest).is_some() && !forwarded(rest)
}

/// `-----Original Message-----` in the languages mail clients write it in.
const ORIGINAL_MESSAGE: &[&str] = &[
    "original message",
    "ursprüngliche nachricht",
    "message d'origine",
    "message d’origine",
    "mensaje original",
    "mensagem original",
    "messaggio originale",
    "oorspronkelijk bericht",
    "原始邮件",
    "原始郵件",
    "元のメッセージ",
    "الرسالة الأصلية",
    "मूल संदेश",
];

/// The `From:` line of a quoted header block, in the languages Outlook writes it in.
const FROM_HEADERS: &[&str] = &[
    "From:",
    "Von:",
    "De :",
    "De:",
    "Da:",
    "Van:",
    "Från:",
    "Od:",
    "发件人:",
    "发件人：",
    "寄件者:",
    "寄件者：",
    "差出人:",
    "差出人：",
    "من:",
    "प्रेषक:",
];

/// The `Sent:`/`Date:` line that follows a quoted `From:` line.
const SENT_HEADERS: &[&str] = &[
    "Sent:",
    "Date:",
    "Gesendet:",
    "Datum:",
    "Envoyé :",
    "Envoyé:",
    "Date :",
    "Enviado:",
    "Enviado el:",
    "Fecha:",
    "Data:",
    "Inviato:",
    "Verzonden:",
    "Skickat:",
    "发送时间:",
    "发送时间：",
    "日期:",
    "日期：",
    "送信日時:",
    "送信日時：",
    "تاريخ الإرسال:",
    "التاريخ:",
    "भेजा गया:",
    "दिनांक:",
];

fn is_from_header(line: &str) -> bool {
    let t = line.trim_start();
    FROM_HEADERS.iter().any(|h| t.starts_with(h))
}

/// Outlook's plain-text reply block without a separator: a `From:` line, then `Sent:` (localised),
/// once the reply has new text above it. A forwarded message's header block is not a quote: its
/// text is what the user forwarded.
fn is_header_block(line: &str, next: Option<&str>) -> bool {
    is_from_header(line)
        && next.is_some_and(|n| SENT_HEADERS.iter().any(|h| n.trim_start().starts_with(h)))
}

/// `---------- Forwarded message ---------`, `Begin forwarded message:` and their translations
/// (a marker line of its own, not a sentence that mentions forwarding).
fn is_forward_marker(line: &str) -> bool {
    let t = line.trim();
    let lower = t.to_lowercase();
    let dashed = t.starts_with("---") && t.trim_end_matches(':').ends_with("--");
    let begin = [
        "begin forwarded message",
        "anfang der weitergeleiteten nachricht",
        "début du message réexpédié",
        "inicio del mensaje reenviado",
        "início da mensagem encaminhada",
    ]
    .iter()
    .any(|m| lower.starts_with(m));
    begin
        || dashed
            && [
                "forward",
                "weitergeleitet",
                "transféré",
                "reenviado",
                "encaminhad",
                "inoltrat",
                "doorgestuurd",
                "转发",
                "轉寄",
                "転送",
                "توجيه",
                "अग्रेषित",
                "फ़ॉरवर्ड",
            ]
            .iter()
            .any(|m| lower.contains(m))
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
        if !main.iter().all(|l| l.trim().is_empty())
            && quote_starts(
                &lines[i..],
                main.iter().rev().find(|l| !l.trim().is_empty()).copied(),
            )
        {
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

    /// LL2 (GG7-9): the reply headers mail clients write in the nine UI languages, as in QA
    /// GG7's corpus, mark the quote below them.
    #[test]
    fn localised_attributions() {
        let headers = [
            "On Tue, Oct 27, 2026 at 3:12 PM Sam Okafor <sam.okafor@example.net> wrote:",
            "Am 27.10.2026 um 16:05 schrieb Sam Okafor <sam.okafor@example.net>:",
            "Le mar. 27 oct. 2026 à 15:42, Sam Okafor <sam.okafor@example.net> a écrit :",
            "El mar, 28 oct 2026 a las 14:03, Sam Okafor (<sam@example.net>) escribió:",
            "Em ter., 27 de out. de 2026 às 14:03, Sam Okafor <sam.okafor@example.net> escreveu:",
            "बुध, 28 अक्तू॰ 2026, 4:12 pm को Sam Okafor <sam.okafor@example.net> ने लिखा:",
            "2026年10月28日(水) 14:03 Sam Okafor <sam.okafor@example.net>:",
            "在 2026年10月27日 星期二 15:20，Sam Okafor <sam.okafor@example.net> 写道：",
            "في الثلاثاء ٢٧ أكتوبر ٢٠٢٦ الساعة ٣:١٥ م، كتب Sam Okafor <sam.okafor@example.net>:",
        ];
        for h in headers {
            let s = split_reply(&format!(
                "Danke, erledigt.\n\n{h}\n> Können Sie mir das Formular schicken?\n"
            ));
            assert_eq!(s.main, "Danke, erledigt.", "{h}");
            assert!(s.quoted.starts_with(h.trim()), "{h}: {:?}", s.quoted);
            assert!(s.quoted.ends_with("Formular schicken?"), "{h}");
        }
        // Not attributions: a colon line naming someone, a list lead-in, a date without a sender.
        for line in [
            "Contact Sam Okafor <sam@example.net>:",
            "尊敬的 Sam Okafor：",
            "Am 27.10.2026 um 16:05:",
            "رجاءً أرسل الملف إلى Sam <sam@example.net>:",
            "أرسل الكتب إلى Sam <sam@example.net>:",
            "पिछले हफ़्ते मैंने लिखा:",
        ] {
            let s = split_reply(&format!("Hi\n{line}\n> x\n"));
            assert!(s.main.contains(line), "{line}");
        }
    }

    #[test]
    fn localised_outlook_blocks() {
        for (marker, from, sent) in [
            (
                "-----Ursprüngliche Nachricht-----",
                "Von: Paul",
                "Gesendet: Dienstag, 27. Oktober 2026 16:05",
            ),
            (
                "-----Message d'origine-----",
                "De : Paul",
                "Envoyé : mardi 27 octobre 2026 16:05",
            ),
            (
                "-----Mensagem original-----",
                "De: Paul",
                "Enviada em: terça-feira",
            ),
            (
                "-----原始邮件-----",
                "发件人: Paul",
                "发送时间: 2026年10月27日 16:05",
            ),
            (
                "-----元のメッセージ-----",
                "差出人: Paul",
                "送信日時: 2026年10月27日 16:05",
            ),
        ] {
            let s = split_reply(&format!(
                "Erledigt.\n\n{marker}\n{from}\n{sent}\nold question?"
            ));
            assert_eq!(s.main, "Erledigt.", "{marker}");
            assert!(s.quoted.ends_with("old question?"), "{marker}");
        }
        // Underscores, then a localised `From:`.
        let s = split_reply(&format!(
            "Ok\n{}\nVon: Paul\nGesendet: heute\nalt",
            "_".repeat(32)
        ));
        assert_eq!(s.main, "Ok");
        // No separator: a `From:`, `Sent:`, `To:`, `Subject:` block in any language.
        for (from, sent, to, subject) in [
            (
                "From: Paul",
                "Sent: Tuesday",
                "To: Sam",
                "Subject: RE: Plan",
            ),
            (
                "Von: Paul",
                "Gesendet: Dienstag",
                "An: Sam",
                "Betreff: AW: Plan",
            ),
            (
                "من: بول",
                "تاريخ الإرسال: الثلاثاء",
                "إلى: سام",
                "الموضوع: رد: الخطة",
            ),
        ] {
            let s = split_reply(&format!("Ok\n\n{from}\n{sent}\n{to}\n{subject}\n\nalt"));
            assert_eq!(s.main, "Ok", "{from}");
            assert!(s.quoted.ends_with("alt"), "{from}");
        }
        // Forwards stay the mail's own text: after a marker line, or with a forward subject
        // (with or without the underscore separator).
        for fwd in [
            "---------- Forwarded message ---------",
            "---------- Weitergeleitete Nachricht ----------",
            "Begin forwarded message:",
        ] {
            let s = split_reply(&format!(
                "FYI\n\n{fwd}\nFrom: Paul\nDate: Tue\nTo: Sam\nSubject: Invoice\n\nthe invoice"
            ));
            assert!(s.main.ends_with("the invoice"), "{fwd}");
        }
        for sep in ["", "________________________________\n"] {
            for (from, sent, to, subject) in [
                (
                    "From: Paul <p@x>",
                    "Date: Tue",
                    "To: Sam",
                    "Subject: FW: Invoice 4471",
                ),
                (
                    "Von: Paul",
                    "Gesendet: Dienstag",
                    "An: Sam",
                    "Betreff: WG: Rechnung 4471",
                ),
                (
                    "De : Paul",
                    "Envoyé : mardi",
                    "À : Sam",
                    "Objet : TR: Facture 4471",
                ),
            ] {
                let s = split_reply(&format!(
                    "Zur Info.\n\n{sep}{from}\n{sent}\n{to}\n{subject}\n\nInvoice 4471 is due."
                ));
                assert!(s.main.ends_with("Invoice 4471 is due."), "{sep}{subject}");
                assert!(s.quoted.is_empty(), "{subject}");
            }
        }
        // Not header blocks: the user's own `From:`/`Data:` lines with no recipient and subject.
        for own in [
            "Here is the export.\n\nFrom: prod-db-1\nData: orders_2026.csv\nThe totals match the ledger.",
            "Itinerário:\n\nDe: São Paulo (GRU)\nData: 12/11/2026\nPara: Lisboa (LIS)\nVoo TP 88",
        ] {
            let s = split_reply(own);
            assert!(s.quoted.is_empty(), "{own}");
        }
        // A sentence that mentions forwarding is no marker: the quote below it still goes.
        let s = split_reply(
            "See you then. Looking forward to it.\n\nFrom: Paul\nSent: Tue\nTo: Sam\nSubject: RE: Lunch\n\nold",
        );
        assert_eq!(s.main, "See you then. Looking forward to it.");
        // The block opens the mail: nothing above it, so it is the mail itself.
        let s = split_reply("Von: Paul\nGesendet: heute\nText");
        assert!(s.main.ends_with("Text"));
    }

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
