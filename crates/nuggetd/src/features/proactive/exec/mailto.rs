//! `mailto:` links (RFC 6068) for drafted replies: the desktop opens one in the user's mail app,
//! which sends nothing until the user does.

use nugget_proactive::DraftReply;

/// Longest body put into the link; mail apps and OS URL handlers truncate or refuse very long
/// URLs. The full text is always in the artifact's `text`.
const MAX_BODY_CHARS: usize = 6_000;

/// `mailto:<to>?cc=..&subject=..&body=..`, every part percent-encoded (line breaks as `%0D%0A`).
/// `In-Reply-To` cannot be expressed in a `mailto:` link that mail apps honour; it is left out.
pub(crate) fn link(d: &DraftReply) -> String {
    let addrs = |list: &[String]| {
        list.iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .map(|a| encode(a, true))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut url = format!("mailto:{}", addrs(&d.to));
    let mut query = Vec::new();
    let cc = addrs(&d.cc);
    if !cc.is_empty() {
        query.push(format!("cc={cc}"));
    }
    if !d.subject.is_empty() {
        query.push(format!("subject={}", encode(&d.subject, false)));
    }
    if let Some(body) = d.body.as_deref().filter(|b| !b.is_empty()) {
        let body: String = body.chars().take(MAX_BODY_CHARS).collect();
        let crlf = body.replace("\r\n", "\n").replace('\n', "\r\n");
        query.push(format!("body={}", encode(&crlf, false)));
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

/// Percent-encode everything but RFC 3986 unreserved characters (and `@` in addresses).
fn encode(s: &str, address: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (address && b == b'@');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{ItemKey, ItemRef, SourceId};

    fn draft(body: Option<&str>) -> DraftReply {
        DraftReply {
            reply_to: ItemRef {
                source: SourceId::new("mail"),
                key: ItemKey::new("1"),
            },
            in_reply_to_message_id: Some("<x@y>".into()),
            to: vec!["ana@acme.io".into(), "bo+x@acme.io".into()],
            cc: vec!["c d@e.io".into()],
            subject: "Re: Pricing & terms?".into(),
            intent: "reply".into(),
            body: body.map(str::to_owned),
        }
    }

    #[test]
    fn encodes_every_part() {
        let url = link(&draft(Some("Hi Ana,\nSee you at 5 — ok?\n")));
        assert_eq!(
            url,
            "mailto:ana@acme.io,bo%2Bx@acme.io?cc=c%20d@e.io\
             &subject=Re%3A%20Pricing%20%26%20terms%3F\
             &body=Hi%20Ana%2C%0D%0ASee%20you%20at%205%20%E2%80%94%20ok%3F%0D%0A"
        );
    }

    #[test]
    fn hostile_parts_cannot_add_fields() {
        let mut d = draft(Some("x&bcc=evil@x.io"));
        d.to = vec!["a@b.io?bcc=evil@x.io".into()];
        let url = link(&d);
        assert!(!url.contains("&bcc="), "{url}");
        assert!(!url.contains("?bcc="), "{url}");
        assert_eq!(url.matches('?').count(), 1);
    }

    #[test]
    fn empty_parts_are_left_out() {
        let mut d = draft(None);
        d.cc.clear();
        d.subject.clear();
        assert_eq!(link(&d), "mailto:ana@acme.io,bo%2Bx@acme.io");
        let long = "a".repeat(MAX_BODY_CHARS + 100);
        let url = link(&draft(Some(&long)));
        assert_eq!(url.split("body=").nth(1).unwrap().len(), MAX_BODY_CHARS);
    }
}
