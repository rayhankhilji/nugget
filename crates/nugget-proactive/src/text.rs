//! Deterministic text helpers: dates, money amounts, promise phrases, addresses, hashing.

pub use nugget_core::lang::find_dates;
use nugget_core::lang::{self, contains_word, normalize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Date, Duration, Month, Weekday};

/// First 16 hex chars of SHA-256 over the parts (joined with `\x1f`).
pub fn stable_hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            h.update([0x1f]);
        }
        h.update(p.as_bytes());
    }
    hex::encode(&h.finalize()[..8])
}

/// At most `max` chars, cut at a word boundary with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        None => s.to_owned(),
        Some((cut, _)) => {
            let head = &s[..cut];
            let head = head.rsplit_once(' ').map_or(head, |(h, _)| h);
            format!("{}…", head.trim_end())
        }
    }
}

/// A mail address with an optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    /// Lowercased.
    pub email: String,
}

impl Address {
    /// Parse `Name <a@b>`, `<a@b>`, `a@b` or `Name <a@b> (accepted)`. `None` without an `@`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let (Some(lt), Some(gt)) = (s.find('<'), s.rfind('>'))
            && lt < gt
        {
            let email = s[lt + 1..gt].trim().trim_start_matches("mailto:");
            if !email.contains('@') {
                return None;
            }
            let name = s[..lt].trim().trim_matches('"').trim();
            return Some(Self {
                name: (!name.is_empty()).then(|| name.to_owned()),
                email: email.to_lowercase(),
            });
        }
        let first = s.split_whitespace().next()?;
        let email = first
            .trim_start_matches("mailto:")
            .trim_start_matches("MAILTO:");
        email.contains('@').then(|| Self {
            name: None,
            email: email.to_lowercase(),
        })
    }

    pub fn domain(&self) -> &str {
        self.email.rsplit_once('@').map_or("", |(_, d)| d)
    }

    pub fn local(&self) -> &str {
        self.email.split_once('@').map_or("", |(l, _)| l)
    }

    /// Display name, or the local part of the address.
    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.local().to_owned())
    }
}

/// Strip `Re:`, `Fwd:` etc. and collapse whitespace; lowercase.
pub fn normalize_subject(s: &str) -> String {
    let mut s = s.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let mut stripped = false;
        for p in ["re:", "fw:", "fwd:", "aw:", "sv:", "re :", "fwd :"] {
            if lower.starts_with(p) {
                s = s[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The body without quoted replies: stops at `On … wrote:` / `-----Original Message-----` and
/// drops `>` lines.
pub fn own_text(body: &str) -> String {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let lower = t.to_ascii_lowercase();
        if (lower.starts_with("on ") && lower.ends_with("wrote:"))
            || lower.starts_with("-----original message")
            || lower.starts_with("from:") && !out.is_empty()
        {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// Split into sentences (on `.`, `!`, `?`, `؟`, `।` followed by whitespace, on `。！？`, and on
/// line breaks).
pub fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut cur = String::new();
        let chars: Vec<char> = line.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            cur.push(c);
            let end = matches!(c, '。' | '！' | '？')
                || (matches!(c, '.' | '!' | '?' | '؟' | '।')
                    && chars.get(i + 1).is_none_or(|n| n.is_whitespace()));
            if end {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_owned());
                }
                cur.clear();
            }
        }
        let t = cur.trim();
        if !t.is_empty() {
            out.push(t.to_owned());
        }
    }
    out
}

/// Phrases that ask the reader for something (an answer, a decision, an action).
const ASKS: &[&str] = &[
    "could you",
    "can you",
    "would you",
    "will you",
    "would it be possible",
    "are you able",
    "do you have time",
    "let me know",
    "let us know",
    "i need you to",
    "need your",
    "i'd like your",
    "i would like your",
    "your thoughts",
    "get back to me",
    "can we",
    "could we",
    "shall we",
    // de
    "könnten sie",
    "können sie",
    "könntest du",
    "kannst du",
    "würden sie",
    "würdest du",
    "gib mir bescheid",
    "geben sie mir bescheid",
    "lass mich wissen",
    "lassen sie mich wissen",
    // fr
    "pourriez-vous",
    "pouvez-vous",
    "pourrais-tu",
    "peux-tu",
    "tenez-moi au courant",
    "tiens-moi au courant",
    "dites-moi",
    "dis-moi",
    // es
    "podrías",
    "podría usted",
    "puedes",
    "avísame",
    "confírmame",
    "le agradecería",
    "te agradecería",
    // pt
    "pode me",
    "pode nos",
    "você pode",
    "poderia",
    "me avise",
    "me confirme",
    "me diga",
    // hi
    "बताइए",
    "बताइये",
    "बताएं",
    "बताएँ",
    "दीजिए",
    "दीजिये",
    "भेजिए",
    // ja
    "いただけます",
    "いただけません",
    "いただければ",
    "いただけない",
    // zh
    "麻烦你",
    "麻烦您",
    "能否",
    "可否",
    "能不能",
    // ar
    "هل يمكنك",
    "هل يمكنكم",
];

/// Words that turn what follows into a request ("please", "bitte", "por favor", "请", …).
const PLEASE: &[&str] = &[
    "please",
    "bitte",
    "por favor",
    "por gentileza",
    "s'il vous plaît",
    "s'il te plaît",
    "veuillez",
    "merci de",
    "कृपया",
    "कृप्या",
    "يرجى",
    "نرجو",
    "ارجو",
    "الرجاء",
    "من فضلك",
    "请",
];

/// "please …" that does not request anything.
const PLEASE_NOT_ASKING: &[&str] = &[
    "please find",
    "please see attached",
    "please see below",
    "please note",
    "please ignore",
    "please disregard",
    "please do not reply",
    "please don't reply",
    "bitte schön",
    "bitte sehr",
    "bitte beachten",
    "bitte finden sie anbei",
    "veuillez trouver",
    "merci de votre",
    "merci de vos",
    "merci de ton",
    "merci de ta",
    "merci de tes",
    "merci de m'avoir",
    "merci de nous avoir",
    "merci de l'",
    "merci de la ",
    "कृपया ध्यान",
    "कृपया संलग्न",
    "请查收",
    "请见附件",
    "请参阅",
    "请注意",
];

/// "Yes" and "thanks" before a "please" that then does not ask ("yes please").
const YES_THANKS: &[&str] = &[
    "yes",
    "thanks",
    "ja",
    "danke",
    "oui",
    "merci",
    "sí",
    "si",
    "sim",
    "gracias",
    "obrigado",
    "obrigada",
    "हाँ",
    "好的",
    "谢谢",
    "نعم",
    "شكرا",
];

/// Negations that make a "please" a prohibition ("please don't share"): right after it, or
/// near the end in verb-final word order.
const NEG_HEAD: &[&str] = &[
    "don't", "dont", "not", "never", "nicht", "kein", "keine", "ne", "no", "não", "nao", "न", "मत",
    "عدم", "لا",
];
const NEG_TAIL: &[&str] = &["nicht", "न", "मत"];

fn please_asks(l: &str, at: usize, please: &str) -> bool {
    // "申请", "邀请", "请假": not "please".
    if please == "请"
        && (l[..at].ends_with(['申', '邀', '聘', '宴'])
            || l[at + please.len()..].starts_with(['假', '柬']))
    {
        return false;
    }
    let before = l[..at].trim_end_matches([' ', ',', '，']);
    let rest = &l[at..];
    let after = rest[please.len()..].trim_start_matches([' ', ',', '!', '.', '،', '，', '、']);
    let words: Vec<&str> = after
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation() && c != '\''))
        .collect();
    let prohibition = words
        .iter()
        .take(2)
        .any(|w| NEG_HEAD.contains(w) || w.starts_with("n'"))
        || words.iter().rev().take(3).any(|w| NEG_TAIL.contains(w))
        || after.starts_with(['勿', '别'])
        || after.starts_with("不要");
    !after.is_empty()
        && !prohibition
        && !before
            .rsplit([' ', ','])
            .next()
            .is_some_and(|w| YES_THANKS.contains(&w))
        && !PLEASE_NOT_ASKING.iter().any(|p| rest.starts_with(p))
}

/// Polite closings that ask nothing ("よろしくお願いします"), and what may stand around them.
const JA_CLOSING: &[&str] = &[
    "ご査収のほど",
    "ご査収ください",
    "ご了承のほど",
    "ご了承ください",
    "今後とも",
    "引き続き",
    "以上",
    "皆様",
    "皆さま",
    "どうぞ",
    "何卒",
    "よろしく",
    "宜しく",
    "お願い",
    "申し上げます",
    "いたします",
    "致します",
    "します",
    "ください",
];

/// A Japanese request (`…ください`, `…お願いします`) that is more than a polite closing and not
/// a prohibition (`…しないでください`).
fn japanese_asks(l: &str) -> bool {
    if !(l.contains("ください") || l.contains("お願い"))
        || [
            "ないでください",
            "ないようお願い",
            "ないようにお願い",
            "ご遠慮",
        ]
        .iter()
        .any(|p| l.contains(p))
    {
        return false;
    }
    let rest = JA_CLOSING
        .iter()
        .fold(l.to_owned(), |r, p| r.replace(p, ""));
    rest.chars().any(char::is_alphanumeric)
}

/// Whether one sentence asks the reader something: a question, or a request.
pub fn sentence_asks(sentence: &str) -> bool {
    let l = normalize(sentence);
    let l = l.trim();
    let end = l.trim_end_matches(['"', '\'', ')', ']', '*', '_']);
    if end.ends_with(['?', '？', '؟']) || l.starts_with('¿') {
        return true;
    }
    if ASKS.iter().any(|p| contains_word(l, &normalize(p))) || japanese_asks(l) {
        return true;
    }
    PLEASE.iter().any(|p| {
        let p = normalize(p);
        lang::word_hits(l, &p).any(|at| please_asks(l, at, &p))
    })
}

/// Whether the author's own text (quotes removed) asks the reader something or requests an
/// action. Acknowledgements ("Done, you're on from the 1st."), FYIs and thanks do not.
pub fn asks_something(body: &str) -> bool {
    sentences(&own_text(body)).iter().any(|s| sentence_asks(s))
}

/// Subjects of mail that calendars and mail servers generate on a person's behalf: invitation
/// replies and updates, auto-replies, bounces. They never need a reply.
pub fn is_generated_subject(subject: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "accepted:",
        "declined:",
        "tentative:",
        "tentatively accepted:",
        "cancelled:",
        "canceled:",
        "cancelled event:",
        "canceled event:",
        "event cancelled",
        "event canceled",
        "invitation:",
        "updated invitation",
        "invitation updated",
        "updated event:",
        "new event:",
        "automatic reply",
        "auto reply",
        "auto-reply",
        "autoreply",
        "out of office",
        "undeliverable:",
        "undelivered mail",
        "delivery status notification",
        "read:",
    ];
    let l = subject.trim().to_lowercase();
    PREFIXES.iter().any(|p| l.starts_with(p))
}

/// Phrases with which the user promises to do something.
const PROMISES: &[&str] = &[
    "i'll send",
    "i will send",
    "i'll get back",
    "i will get back",
    "i'll follow up",
    "i will follow up",
    "i'll share",
    "i will share",
    "i'll circle back",
    "i'll check",
    "i will check",
    "let me check",
    "i'll look into",
    "i will look into",
    "i'll have",
    "i'll forward",
    "i will forward",
    "i'll get you",
    "i'll put together",
    "i'll draft",
    "i'll email",
    "i'll confirm",
    "i will confirm",
    "i'll review",
    "i will review",
    "i'll update you",
    "i'll let you know",
    "i will let you know",
    // de
    "ich schicke",
    "ich schick",
    "ich sende",
    "ich werde",
    "ich melde mich",
    "ich kümmere mich",
    "ich gebe dir bescheid",
    "ich gebe ihnen bescheid",
    // fr
    "je t'envoie",
    "je vous envoie",
    "je t'enverrai",
    "je vous enverrai",
    "je vais t'",
    "je vais vous",
    "je vais te",
    "je reviens vers",
    "je te tiens au courant",
    "je vous tiens au courant",
    "je m'en occupe",
    // es
    "te envío",
    "le envío",
    "te enviaré",
    "le enviaré",
    "te mando",
    "le mando",
    "voy a enviar",
    "voy a mandar",
    "te aviso",
    "le aviso",
    "me encargo",
    // pt
    "vou te enviar",
    "vou enviar",
    "vou te mandar",
    "vou mandar",
    "te envio",
    "lhe envio",
    "vou verificar",
    "vou confirmar",
    // hi
    "भेज दूँगा",
    "भेज दूंगा",
    "भेज दूँगी",
    "भेज दूंगी",
    "भेजूँगा",
    "भेजूंगा",
    "भेजूँगी",
    "भेजूंगी",
    "बता दूँगा",
    "बता दूंगा",
    "बता दूँगी",
    "बता दूंगी",
    // ja
    "送ります",
    "お送りします",
    "送付します",
    "ご連絡します",
    "共有します",
    // zh
    "我会",
    "我将",
    "发给你",
    "发给您",
    // ar
    "سأرسل",
    "سوف أرسل",
    "سأبلغك",
    "سأخبرك",
];

/// Words of something sent along now ("ich schicke dir anbei"), not promised.
const ATTACHED: &[&str] = &[
    "anbei",
    "im anhang",
    "ci-joint",
    "pièce jointe",
    "adjunto",
    "em anexo",
];

pub fn is_promise(text: &str) -> bool {
    let l = normalize(text);
    PROMISES.iter().any(|p| l.contains(&normalize(p))) && !ATTACHED.iter().any(|a| l.contains(a))
}

/// Sentences of the user's own text that promise something.
pub fn find_promises(text: &str) -> Vec<String> {
    sentences(&own_text(text))
        .into_iter()
        .filter(|s| is_promise(s))
        .collect()
}

const NEXT_WEEK: &[&str] = &[
    "next week",
    "nächste woche",
    "nächsten woche",
    "semaine prochaine",
    "próxima semana",
    "semana que vem",
    "अगले हफ्ते",
    "अगले सप्ताह",
    "下周",
    "来週",
    "الاسبوع القادم",
];

/// A deadline phrase in a promise, relative to the day it was written ("by Friday", "tomorrow",
/// "next week", "end of day", an explicit date), in any of the languages.
pub fn deadline_in(sentence: &str, written: Date) -> Option<Date> {
    let l = normalize(sentence);
    if let Some(d) = lang::relative_day(&l, written) {
        return Some(d);
    }
    if l.contains("eod") || l.contains("end of day") {
        return Some(written);
    }
    if l.contains("end of week") || l.contains("end of the week") || l.contains("eow") {
        return Some(next_weekday(written, Weekday::Friday, true));
    }
    if let Some(d) = find_dates(&l, written).into_iter().next() {
        return Some(d);
    }
    if let Some(wd) = lang::weekday_in(&l) {
        return Some(next_weekday(written, wd, false));
    }
    if NEXT_WEEK.iter().any(|w| l.contains(w)) {
        return Some(written + Duration::days(7));
    }
    if l.contains("end of month") || l.contains("end of the month") {
        let (y, m) = (written.year(), written.month());
        return Date::from_calendar_date(y, m, m.length(y)).ok();
    }
    None
}

/// The next `wd` after `from` (or `from` itself when `inclusive` and it matches).
fn next_weekday(from: Date, wd: Weekday, inclusive: bool) -> Date {
    let mut d = if inclusive {
        from
    } else {
        from + Duration::days(1)
    };
    while d.weekday() != wd {
        d += Duration::days(1);
    }
    d
}

/// Parse an ISO date (`2026-11-03`, also the date part of RFC 3339).
pub fn parse_iso_date(s: &str) -> Option<Date> {
    let s = s.trim();
    let d = s.get(..10)?;
    let b = d.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = d[..4].parse().ok()?;
    let m: u8 = d[5..7].parse().ok()?;
    let day: u8 = d[8..10].parse().ok()?;
    Date::from_calendar_date(y, Month::try_from(m).ok()?, day).ok()
}

/// `tomorrow`, `the day after` or `in N days` (any language) → a day after `reference`.
pub fn relative_days(text: &str, reference: Date) -> Option<Date> {
    lang::relative_day(text, reference).filter(|d| *d > reference)
}

/// A money amount in minor units (cents).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    /// ISO 4217 code.
    pub currency: String,
    pub minor: i64,
}

/// As English `Intl.NumberFormat` currency style writes it (`$1,299.00`, `¥1,299`,
/// `CHF 12.99`), so the English fallback matches what the app shows.
impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self.currency.as_str() {
            "USD" => "$".to_owned(),
            "EUR" => "€".to_owned(),
            "GBP" => "£".to_owned(),
            "JPY" => "¥".to_owned(),
            "INR" => "₹".to_owned(),
            "CAD" => "CA$".to_owned(),
            "AUD" => "A$".to_owned(),
            "NZD" => "NZ$".to_owned(),
            c => format!("{c}\u{a0}"),
        };
        let sign = if self.minor < 0 { "-" } else { "" };
        let abs = self.minor.unsigned_abs();
        let digits = (abs / 100).to_string();
        let mut whole = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                whole.push(',');
            }
            whole.push(c);
        }
        if self.currency == "JPY" {
            write!(f, "{sign}{prefix}{whole}")
        } else {
            write!(f, "{sign}{prefix}{whole}.{:02}", abs % 100)
        }
    }
}

const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A date as English `Intl.DateTimeFormat` `dateStyle: "medium"` writes it (`Nov 3, 2026`).
pub fn en_date(d: Date) -> String {
    format!(
        "{} {}, {}",
        MONTH_ABBR[d.month() as usize - 1],
        d.day(),
        d.year()
    )
}

/// A time as English `timeStyle: "short"` writes it (`2:05\u{202f}PM`).
pub fn en_time(t: time::Time) -> String {
    let h = match t.hour() % 12 {
        0 => 12,
        h => h,
    };
    let ampm = if t.hour() < 12 { "AM" } else { "PM" };
    format!("{h}:{:02}\u{202f}{ampm}", t.minute())
}

const SYMBOLS: &[(&str, &str)] = &[
    ("US$", "USD"),
    ("CA$", "CAD"),
    ("C$", "CAD"),
    ("A$", "AUD"),
    ("AU$", "AUD"),
    ("$", "USD"),
    ("€", "EUR"),
    ("£", "GBP"),
    ("¥", "JPY"),
    ("₹", "INR"),
];

const CODES: &[&str] = &[
    "USD", "EUR", "GBP", "CAD", "AUD", "JPY", "INR", "CHF", "SEK", "NOK", "DKK", "NZD",
];

/// Parse `1,299.00`, `9,99`, `12` into minor units.
fn parse_number(s: &str) -> Option<i64> {
    let s = s.trim_end_matches(|c: char| !c.is_ascii_digit());
    if s.is_empty() || !s.chars().next()?.is_ascii_digit() {
        return None;
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
    {
        return None;
    }
    // Decimal separator: the last `.` or `,` followed by exactly two digits.
    let (int, frac) = match s.rfind(['.', ',']) {
        Some(p) if s.len() - p - 1 == 2 => (&s[..p], &s[p + 1..]),
        _ => (s, "00"),
    };
    let int: String = int.chars().filter(char::is_ascii_digit).collect();
    let int: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac: i64 = frac.parse().ok()?;
    int.checked_mul(100)?.checked_add(frac)
}

/// Every money amount in `text`, in order.
pub fn find_amounts(text: &str) -> Vec<Money> {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    let mut after_number = false;
    for (i, raw) in toks.iter().enumerate() {
        let t = raw.trim_start_matches(['(', '[', '"']);
        if std::mem::take(&mut after_number) {
            continue;
        }
        // Symbol after a space: 12,99 €
        if let Some(minor) = parse_number(t)
            && let Some((_, code)) = toks.get(i + 1).and_then(|n| {
                let n = n.trim_end_matches(['.', ',', ')']);
                SYMBOLS.iter().find(|(s, _)| *s != "$" && *s == n)
            })
        {
            out.push(Money {
                currency: (*code).into(),
                minor,
            });
            after_number = true;
            continue;
        }
        // Symbol prefix: $12.99
        if let Some((sym, code)) = SYMBOLS.iter().find(|(s, _)| t.starts_with(s)) {
            let rest = &t[sym.len()..];
            let num = if rest.is_empty() {
                toks.get(i + 1).copied().unwrap_or("")
            } else {
                rest
            };
            if let Some(minor) = parse_number(num) {
                out.push(Money {
                    currency: (*code).into(),
                    minor,
                });
                continue;
            }
        }
        // Symbol suffix: 9,99€
        if let Some((sym, code)) = SYMBOLS
            .iter()
            .filter(|(s, _)| *s != "$")
            .find(|(s, _)| t.trim_end_matches(['.', ',', ')']).ends_with(s))
        {
            let num = t.trim_end_matches(['.', ',', ')']);
            if let Some(minor) = parse_number(&num[..num.len() - sym.len()]) {
                out.push(Money {
                    currency: (*code).into(),
                    minor,
                });
                continue;
            }
        }
        // ISO code before or after: USD 12.99 / 12.99 USD
        let bare = t.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if CODES.contains(&bare) {
            if let Some(minor) = toks.get(i + 1).and_then(|n| parse_number(n)) {
                out.push(Money {
                    currency: bare.into(),
                    minor,
                });
            }
        } else if let Some(code) = toks
            .get(i + 1)
            .map(|n| n.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|n| CODES.contains(n))
            && !toks
                .get(i.wrapping_sub(1))
                .is_some_and(|p| CODES.contains(&p.trim_matches(|c: char| !c.is_alphanumeric())))
            && let Some(minor) = parse_number(t)
        {
            out.push(Money {
                currency: code.into(),
                minor,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn dates() {
        let r = date!(2026 - 10 - 28);
        assert_eq!(
            find_dates("renews on November 3, 2026.", r),
            vec![date!(2026 - 11 - 03)]
        );
        assert_eq!(find_dates("on Nov 3", r), vec![date!(2026 - 11 - 03)]);
        assert_eq!(find_dates("by 3rd Nov", r), vec![date!(2026 - 11 - 03)]);
        assert_eq!(find_dates("Jan 5", r), vec![date!(2027 - 01 - 05)]);
        assert_eq!(find_dates("due 2026-11-02", r), vec![date!(2026 - 11 - 02)]);
        assert!(find_dates("may I ask", r).is_empty());
        assert_eq!(
            relative_days("trial ends in 3 days", r),
            Some(date!(2026 - 10 - 31))
        );
    }

    #[test]
    fn deadlines() {
        let wed = date!(2026 - 10 - 28);
        assert_eq!(
            deadline_in("I'll send it by Friday", wed),
            Some(date!(2026 - 10 - 30))
        );
        assert_eq!(
            deadline_in("tomorrow works", wed),
            Some(date!(2026 - 10 - 29))
        );
        assert_eq!(deadline_in("no date here", wed), None);
    }

    #[test]
    fn multilingual_deadlines_and_promises() {
        let mon = date!(2026 - 10 - 26);
        for (text, due) in [
            (
                "Ich schicke dir die überarbeiteten Folien bis Montag, 2. November.",
                Some(date!(2026 - 11 - 02)),
            ),
            ("Je t'envoie le devis demain.", Some(date!(2026 - 10 - 27))),
            (
                "Te envío el contrato el 13 de noviembre.",
                Some(date!(2026 - 11 - 13)),
            ),
            (
                "Vou te enviar os slides até sexta.",
                Some(date!(2026 - 10 - 30)),
            ),
            ("我会在11月5日前发给你。", Some(date!(2026 - 11 - 05))),
            ("Ich schicke dir anbei die Folien.", None),
        ] {
            assert_eq!(is_promise(text), due.is_some(), "{text}");
            if due.is_some() {
                assert_eq!(deadline_in(text, mon), due, "{text}");
            }
        }
        assert_eq!(find_amounts("Zahlung erhalten: 12,99 €.")[0].minor, 1299);
        assert_eq!(
            find_dates("Ihr Abonnement verlängert sich am 3. November 2026.", mon),
            vec![date!(2026 - 11 - 03)]
        );
    }

    #[test]
    fn amounts() {
        assert_eq!(
            find_amounts("You will be charged $12.99 on"),
            vec![Money {
                currency: "USD".into(),
                minor: 1299
            }]
        );
        assert_eq!(find_amounts("Total: 9,99€")[0].minor, 999);
        assert_eq!(find_amounts("EUR 1,299.00")[0].minor, 129_900);
        assert_eq!(find_amounts("120 GBP per year")[0].currency, "GBP");
        assert_eq!(find_amounts("$ 5")[0].minor, 500);
        assert_eq!(
            Money {
                currency: "USD".into(),
                minor: 1299
            }
            .to_string(),
            "$12.99"
        );
    }

    #[test]
    fn addresses_and_promises() {
        let a = Address::parse("\"Ana Ruiz\" <Ana@Acme.io>").unwrap();
        assert_eq!(a.email, "ana@acme.io");
        assert_eq!(a.name.as_deref(), Some("Ana Ruiz"));
        assert_eq!(
            Address::parse("bob@x.org (accepted)").unwrap().email,
            "bob@x.org"
        );
        assert!(Address::parse("Room 4").is_none());
        let p = find_promises(
            "Thanks! I'll send the deck by Friday.\n\nOn Mon, Ana wrote:\n> I'll check",
        );
        assert_eq!(p, vec!["I'll send the deck by Friday.".to_string()]);
        assert_eq!(normalize_subject("Re: Fwd:  Q3  Plan"), "q3 plan");
    }

    #[test]
    fn asking() {
        for yes in [
            "Hi, could you send the numbers before Friday?",
            "Any update?",
            "Please send the signed copy by Monday.",
            "Let me know which variant you prefer.",
            "I'd like your view before Friday.",
            "Hi,\n\nThoughts on the draft? \n\nOn Mon, Bob wrote:\n> ok",
            "Bitte teilen Sie mir mit, ob Ihnen der Termin passt.",
            "Por favor, confírmame la dirección antes del miércoles.",
            "Merci de me confirmer d'ici vendredi.",
            "Pode me confirmar até sexta.",
            "कृपया बताइए कि आपको कवर वाला विकल्प चाहिए या नहीं।",
            "ご検討のほどよろしくお願い申し上げます。",
            "请在周五前确认地址。",
            "يرجى تأكيد العنوان قبل الأربعاء.",
            "هل وصلك الملف؟",
        ] {
            assert!(asks_something(yes), "{yes}");
        }
        for no in [
            "Done, you're on from the 1st.",
            "Yes please, the usual.",
            "Thanks for the help yesterday, it worked.",
            "Please find attached the minutes from the call.",
            "Heads up: the office is closed on Monday.",
            "Sharing the agenda ahead of our meeting. Talk soon.",
            "Got them!\n\nOn Sun, Carol wrote:\n> Did you get the photos?",
            "See https://example.com/a?b=c for details.",
            "We scanned it as best we can, you're all set.",
            "Your salary will be $118,500. Please don't share this letter.",
            "Please do not forward.",
            "Bitte nicht weiterleiten.",
            "Bitte leiten Sie diesen Brief nicht weiter.",
            "Merci de votre réponse rapide.",
            "Por favor, no comparta esta carta.",
            "引き続きよろしくお願いいたします。",
            "请勿转发。",
            "我已经提交了申请。",
        ] {
            assert!(!asks_something(no), "{no}");
        }
        assert!(is_generated_subject("Accepted: Weekly sync @ Mon"));
        assert!(is_generated_subject("Updated invitation: All hands"));
        assert!(is_generated_subject("Automatic reply: Out of office"));
        assert!(!is_generated_subject("Re: Contract renewal terms"));
    }
}
