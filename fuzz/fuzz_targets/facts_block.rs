//! Grounded facts block: packed retrieval context (untrusted passage text, titles and item dates)
//! split into facts and rendered for the draft prompt.
//!
//! The first byte holds flags: bit 7 picks raw mode (the first field is the packed text as is),
//! else the passages are rendered as `nugget_retrieve`'s packer does (`[n] Title (source)`
//! headers, blank lines between passages) around bodies that may themselves hold header-like
//! lines; bits 0-6 mark citations 1-7 sensitive. The rest, as UTF-8 (lossy), is split at NUL
//! into fields: in raw mode the packed text, then `title, date, body` per citation (up to 7).
//!
//! Invariants: the section has exactly one `<context>` fence and no other angle bracket, every
//! line inside it is one fact (`F<id> (`), ids run 1.. with superseded facts pointing at a newer
//! one, and (rendered mode) every line of a sensitive passage's body ends up in a fact marked
//! sensitive, so no passage text can shed its item's sensitivity by looking like a header.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{Citation, ContextResponse, DraftContext};
use nugget_draft::Grounder;
use nugget_draft::prompt_facts::{facts, facts_section};
use nugget_voice::VoiceHints;

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collapse(s: &str) -> String {
    one_line(s)
}

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else {
        return;
    };
    let raw = flags & 0x80 != 0;
    let input = String::from_utf8_lossy(rest);
    let mut fields = input.split('\0');
    let raw_text = if raw { fields.next().unwrap_or("") } else { "" };
    let fields: Vec<&str> = fields.collect();

    let mut citations = Vec::new();
    let mut bodies: Vec<(bool, &str)> = Vec::new();
    let mut text = String::new();
    for triple in fields.chunks(3).take(7) {
        let (title, date, body) = match triple {
            [t, d, b] => (*t, *d, *b),
            [t, d] => (*t, *d, ""),
            [t] => (*t, "", ""),
            _ => continue,
        };
        let body = body.trim();
        if !raw && body.is_empty() {
            // The packer skips empty passages.
            continue;
        }
        let n = citations.len() as u32 + 1;
        let sensitive = flags & (1 << (n - 1)) != 0;
        let title = Some(title.to_owned()).filter(|t| !t.trim().is_empty());
        let c = Citation {
            n,
            item: ItemRef {
                source: SourceId::new("notes"),
                key: ItemKey::new(format!("k{n}")),
            },
            chunk_id: i64::from(n),
            title: title.clone(),
            url: None,
            sensitive,
            date: Some(date.to_owned()).filter(|d| !d.is_empty()),
        };
        if !raw {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            let shown = title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(c.item.key.as_str());
            text.push_str(&format!("[{n}] {} ({})\n{body}", one_line(shown), "notes"));
            bodies.push((sensitive, body));
        }
        citations.push(c);
    }
    let context = ContextResponse {
        text: if raw { raw_text.to_owned() } else { text },
        citations,
        excluded_sensitive: 0,
    };

    let all = facts(&context);
    for (i, f) in all.iter().enumerate() {
        assert_eq!(f.id, i + 1);
        if let Some(by) = f.superseded_by {
            assert!(by < f.id, "superseded by an older fact: {f:?}");
        }
        assert!(!f.body.contains('\n') && !f.title.contains('\n'));
    }

    let section = facts_section(&context);
    if all.is_empty() {
        assert!(!section.contains('<') && !section.contains('>'));
    } else {
        assert_eq!(section.matches('<').count(), 2, "{section}");
        assert_eq!(section.matches('>').count(), 2, "{section}");
        let open = section.find("<context>\n").expect("fence opens") + "<context>\n".len();
        let close = section.find("\n</context>").expect("fence closes");
        assert!(open <= close);
        for line in section[open..close].lines() {
            let id = line.strip_prefix('F').expect("one fact per line");
            let digits = id.chars().take_while(char::is_ascii_digit).count();
            assert!(
                digits > 0 && id[digits..].starts_with(" ("),
                "forged line: {line:?}"
            );
        }
    }

    // No passage text sheds its item's sensitivity.
    for (sensitive, body) in &bodies {
        if !sensitive {
            continue;
        }
        for line in body.lines() {
            let line = collapse(line);
            if line.is_empty() {
                continue;
            }
            assert!(
                all.iter().any(|f| f.sensitive && f.body.contains(&line)),
                "sensitive text {line:?} left its item: {all:?}"
            );
        }
    }

    // The grounding corpus reads the same facts.
    let _ = Grounder::for_draft(
        &DraftContext::default(),
        &context,
        None,
        &VoiceHints::default(),
    );
});
