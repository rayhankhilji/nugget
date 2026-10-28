//! Small text helpers: whitespace normalisation, quote matching, mail addresses and domains.

use nugget_store::Chunk;

/// Longest quote stored as evidence (the graph contract asks for ≤ 300 characters).
pub const MAX_QUOTE_CHARS: usize = 300;

/// At most `max` characters of `s`.
pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

/// Collapse whitespace runs to one space, trim, and fold typographic quotes and dashes to ASCII,
/// so a quote copied by a model matches the source despite re-flowing.
pub fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        for c in word.chars() {
            out.push(match c {
                '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' => '\'',
                '\u{201C}' | '\u{201D}' | '\u{201F}' | '\u{2033}' => '"',
                '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
                '\u{00A0}' => ' ',
                c => c,
            });
        }
    }
    out
}

/// Whether `needle` occurs in `hay` after whitespace normalisation (case-sensitive: a quote
/// must be copied verbatim).
pub fn contains_verbatim(hay_normalized: &str, needle: &str) -> bool {
    let n = normalize_ws(needle);
    !n.is_empty() && hay_normalized.contains(&n)
}

/// Case-insensitive containment after whitespace normalisation (for names).
pub fn contains_ci(hay_lower_normalized: &str, needle: &str) -> bool {
    let n = normalize_ws(needle).to_lowercase();
    !n.is_empty() && hay_lower_normalized.contains(&n)
}

/// Find the first chunk containing `needle` (case-insensitive) and return its id with the line
/// around the match as a quote.
pub fn locate(chunks: &[Chunk], needle: &str) -> Option<(i64, String)> {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    for c in chunks {
        let lower = c.text.to_lowercase();
        if let Some(pos) = lower.find(&n) {
            // `to_lowercase` can change byte lengths; fall back to the needle if the offsets
            // do not line up with the original text.
            let quote = if lower.len() == c.text.len() && c.text.is_char_boundary(pos) {
                line_around(&c.text, pos)
            } else {
                needle.trim().to_owned()
            };
            return Some((c.id, quote));
        }
    }
    None
}

/// The line of `text` containing byte offset `pos`, cut to [`MAX_QUOTE_CHARS`].
fn line_around(text: &str, pos: usize) -> String {
    let start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let end = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
    truncate_chars(text[start..end].trim(), MAX_QUOTE_CHARS)
}

/// A parsed mail address: `"Name" <addr>`, `addr`, or a bare name. A trailing participation
/// status in parentheses (`(accepted)`, as the calendar extractor writes it) is returned
/// separately.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Address {
    pub name: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
}

pub fn parse_address(raw: &str) -> Address {
    let mut s = raw.trim();
    let mut status = None;
    if s.ends_with(')')
        && let Some(open) = s.rfind(" (")
    {
        let inner = &s[open + 2..s.len() - 1];
        if !inner.contains(['<', '>', '@']) {
            status = Some(inner.trim().to_lowercase());
            s = s[..open].trim();
        }
    }
    let (name, email) = match (s.rfind('<'), s.ends_with('>')) {
        (Some(lt), true) => {
            let addr = s[lt + 1..s.len() - 1].trim();
            (s[..lt].trim(), Some(addr))
        }
        _ if is_email(s) => ("", Some(s)),
        _ => (s, None),
    };
    let email = email
        .map(|e| {
            e.trim_start_matches("mailto:")
                .trim_start_matches("MAILTO:")
        })
        .filter(|e| is_email(e))
        .map(str::to_lowercase);
    let name = clean_person_name(name);
    let name = name.filter(|n| email.as_deref() != Some(n.to_lowercase().as_str()));
    Address {
        name,
        email,
        status,
    }
}

/// A plausible `local@domain.tld` address.
pub fn is_email(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && !s.contains(char::is_whitespace)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

/// Trim quotes and surrounding punctuation; turn `Last, First` into `First Last`.
pub fn clean_person_name(raw: &str) -> Option<String> {
    let s = raw
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
        .trim();
    if s.is_empty() || is_email(s) {
        return None;
    }
    let s = match s.split_once(", ") {
        Some((last, first))
            if !first.contains(',')
                && !last.contains(' ')
                && !first.is_empty()
                && first.split_whitespace().count() <= 2 =>
        {
            format!("{} {}", first.trim(), last.trim())
        }
        _ => s.to_owned(),
    };
    let s = normalize_ws(&s);
    (!s.is_empty() && s.chars().any(char::is_alphabetic)).then(|| truncate_chars(&s, 200))
}

/// Lower-cased domain of an address.
pub fn domain_of(email: &str) -> Option<String> {
    email
        .rsplit_once('@')
        .map(|(_, d)| d.trim().trim_end_matches('.').to_lowercase())
        .filter(|d| d.contains('.'))
}

/// Second-level labels under which registrations happen (`example.co.uk`).
const SECOND_LEVEL: &[&str] = &[
    "co", "com", "ac", "gov", "org", "net", "edu", "ne", "or", "go", "gv", "ltd", "plc",
];

/// The registrable part of a host (`mail.eu.acme.com` → `acme.com`, `x.acme.co.uk` →
/// `acme.co.uk`). A heuristic without the public-suffix list.
pub fn registrable_domain(domain: &str) -> String {
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        return labels.join(".");
    }
    let n = labels.len();
    let tld = labels[n - 1];
    let second = labels[n - 2];
    let keep = if tld.len() == 2 && SECOND_LEVEL.contains(&second) {
        3
    } else {
        2
    };
    labels[n - keep.min(n)..].join(".")
}

/// A display name for an organisation known only by its domain (`acme-labs.com` → `Acme Labs`).
pub fn org_name_from_domain(domain: &str) -> String {
    let reg = registrable_domain(domain);
    let label = reg.split('.').next().unwrap_or(&reg);
    let words: Vec<String> = label
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        reg
    } else {
        words.join(" ")
    }
}

/// Consumer mail providers: an address there says nothing about an employer.
const FREEMAIL: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "ymail.com",
    "rocketmail.com",
    "hotmail.com",
    "outlook.com",
    "live.com",
    "msn.com",
    "passport.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "aim.com",
    "proton.me",
    "protonmail.com",
    "protonmail.ch",
    "pm.me",
    "gmx.com",
    "gmx.net",
    "gmx.de",
    "web.de",
    "t-online.de",
    "freenet.de",
    "yandex.com",
    "yandex.ru",
    "mail.ru",
    "inbox.ru",
    "list.ru",
    "bk.ru",
    "rambler.ru",
    "zoho.com",
    "zohomail.com",
    "fastmail.com",
    "fastmail.fm",
    "hey.com",
    "tutanota.com",
    "tutanota.de",
    "tuta.io",
    "tuta.com",
    "qq.com",
    "163.com",
    "126.com",
    "yeah.net",
    "sina.com",
    "naver.com",
    "daum.net",
    "hanmail.net",
    "rediffmail.com",
    "mail.com",
    "email.com",
    "inbox.com",
    "hushmail.com",
    "duck.com",
    "privaterelay.appleid.com",
    "orange.fr",
    "wanadoo.fr",
    "free.fr",
    "laposte.net",
    "libero.it",
    "virgilio.it",
    "btinternet.com",
    "sky.com",
    "comcast.net",
    "verizon.net",
    "att.net",
    "sbcglobal.net",
    "bellsouth.net",
    "cox.net",
    "earthlink.net",
    "shaw.ca",
    "rogers.com",
    "bigpond.com",
    "optusnet.com.au",
    "seznam.cz",
    "wp.pl",
    "o2.pl",
    "interia.pl",
    "uol.com.br",
    "bol.com.br",
    "terra.com.br",
    "users.noreply.github.com",
];

/// Whether `domain` (or its registrable part) is a consumer mail provider. Matches the
/// `yahoo.*` / `hotmail.*` / `outlook.*` / `live.*` country variants too.
pub fn is_freemail(domain: &str, extra: &[String]) -> bool {
    let d = domain.to_lowercase();
    let reg = registrable_domain(&d);
    let hit = |x: &str| d == x || reg == x || d.ends_with(&format!(".{x}"));
    if FREEMAIL.iter().any(|f| hit(f)) || extra.iter().any(|f| hit(&f.to_lowercase())) {
        return true;
    }
    let label = reg.split('.').next().unwrap_or("");
    matches!(
        label,
        "yahoo" | "hotmail" | "outlook" | "live" | "gmx" | "yandex" | "aol" | "icloud"
    )
}

/// Addresses of machines rather than people (`noreply@`, `notifications@`, bounces).
pub fn is_automated(email: &str) -> bool {
    let local = email.split('@').next().unwrap_or("").to_lowercase();
    const EXACT: &[&str] = &[
        "mailer-daemon",
        "postmaster",
        "notifications",
        "notification",
        "notify",
        "alerts",
        "alert",
        "newsletter",
        "news",
        "updates",
        "digest",
        "billing",
        "receipts",
        "calendar-notification",
        "calendar-server",
    ];
    local.contains("noreply")
        || local.contains("no-reply")
        || local.contains("donotreply")
        || local.contains("do-not-reply")
        || local.starts_with("bounce")
        || EXACT.contains(&local.as_str())
}

/// A `snake_case` predicate: lowercase ASCII letters, digits and `_`, at most 48 characters.
pub fn normalize_predicate(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c.is_whitespace() || c == '-' || c == '_' || c == '.') && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_owned();
    (!out.is_empty() && out.len() <= 48 && out.starts_with(|c: char| c.is_ascii_alphabetic()))
        .then_some(out)
}

/// Phrases that mark text as trying to steer a model. Quotes containing them are never accepted
/// as evidence.
const INJECTION_MARKERS: &[&str] = &[
    "ignore previous instruction",
    "ignore all previous",
    "ignore the previous",
    "ignore prior instruction",
    "ignore above instruction",
    "disregard previous",
    "disregard all previous",
    "disregard the above",
    "forget previous instruction",
    "forget all previous",
    "new instructions:",
    "system prompt",
    "you are now",
    "as an ai",
    "assistant:",
    "</untrusted",
    "<<<end",
];

/// Whether `text` contains a prompt-injection marker (case-insensitive).
pub fn has_injection_marker(text: &str) -> bool {
    let t = normalize_ws(text).to_lowercase();
    INJECTION_MARKERS.iter().any(|m| t.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_parse() {
        let a = parse_address("Alice Smith <Alice@Acme.com>");
        assert_eq!(a.name.as_deref(), Some("Alice Smith"));
        assert_eq!(a.email.as_deref(), Some("alice@acme.com"));
        let b = parse_address("\"Smith, Bob\" <bob@x.org> (accepted)");
        assert_eq!(b.name.as_deref(), Some("Bob Smith"));
        assert_eq!(b.status.as_deref(), Some("accepted"));
        let c = parse_address("carol@example.com");
        assert_eq!(c.name, None);
        assert_eq!(c.email.as_deref(), Some("carol@example.com"));
        let d = parse_address("Dan Brown");
        assert_eq!(d.name.as_deref(), Some("Dan Brown"));
        assert_eq!(d.email, None);
        let e = parse_address("Erin (declined)");
        assert_eq!(e.status.as_deref(), Some("declined"));
    }

    #[test]
    fn domains() {
        assert_eq!(registrable_domain("mail.eu.acme.com"), "acme.com");
        assert_eq!(registrable_domain("x.acme.co.uk"), "acme.co.uk");
        assert_eq!(org_name_from_domain("acme-labs.io"), "Acme Labs");
        assert!(is_freemail("gmail.com", &[]));
        assert!(is_freemail("yahoo.co.uk", &[]));
        assert!(is_freemail("example.org", &["Example.org".into()]));
        assert!(!is_freemail("acme.com", &[]));
        assert!(is_automated("no-reply@github.com"));
        assert!(!is_automated("alice@acme.com"));
    }

    #[test]
    fn whitespace_and_quotes() {
        let hay = normalize_ws("She said \u{201C}hello\u{201D}\n   to   Bob\u{2019}s team.");
        assert!(contains_verbatim(&hay, "said \"hello\" to Bob's team"));
        assert!(!contains_verbatim(&hay, "said hello"));
        assert_eq!(normalize_predicate("Works At"), Some("works_at".into()));
        assert_eq!(normalize_predicate("  "), None);
        assert!(has_injection_marker(
            "Please IGNORE previous   instructions now"
        ));
    }
}
