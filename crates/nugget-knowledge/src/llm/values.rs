//! Value checks of the LLM gate (QA GG5-1, GG5-15).

use time::OffsetDateTime;

use super::{RELATIVE_DATE_WORDS, day_of, is_weekday, month_of, normalize_date};
use crate::text::is_email;

/// Why `s` holds a credential, account or financial value, if it does.
pub(crate) fn sensitive_value(s: &str) -> Option<&'static str> {
    let plain = plain(s);
    let s = plain.as_str();
    if let Some(why) = credential(s) {
        return Some(why);
    }
    let masked = mask_dates(s);
    for run in digit_runs(&masked) {
        if run.total >= 7 || run.longest >= 6 {
            return Some(if run.plus || (run.groups >= 2 && run.total <= 15) {
                "phone or account number"
            } else {
                "long number"
            });
        }
    }
    if is_amount(s) {
        return Some("currency amount");
    }
    if s.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | ',' | ';'))
        .any(|t| t.contains('@') && is_email(t.trim_matches(|c: char| !c.is_alphanumeric())))
    {
        return Some("email address");
    }
    None
}

/// Why a quote may not be shown: it holds a card number, an IBAN or an identifier-like token.
pub(crate) fn credential_in_quote(q: &str) -> Option<&'static str> {
    credential(&plain(q))
}

/// Zero code points of the Unicode decimal digit blocks [`plain`] reads as ASCII digits
/// (Arabic-Indic, Extended Arabic-Indic, NKo, the Indic scripts, Thai, Lao, Tibetan, Myanmar,
/// Khmer, Mongolian and others, fullwidth, and the mathematical digits).
const DIGIT_ZEROS: &[u32] = &[
    0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66,
    0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6,
];

/// `s` as the value checks read it (security review 3, ER-2, ER-3).
fn plain(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c == '<' {
            // `<b>`, `</span>`, `<td class="x">`: dropped.
            let mut j = i;
            if chars.get(j) == Some(&'/') {
                j += 1;
            }
            if chars.get(j).is_some_and(char::is_ascii_alphabetic)
                && let Some(len) = chars[j..]
                    .iter()
                    .take(80)
                    .position(|&d| matches!(d, '>' | '<' | '@'))
                && chars[j + len] == '>'
            {
                i = j + len + 1;
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                continue;
            }
        }
        let c = match u32::from(c) {
            0xFF01..=0xFF5E => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
            cp => DIGIT_ZEROS
                .iter()
                .find(|&&z| (z..z + 10).contains(&cp))
                .and_then(|&z| char::from_digit(cp - z, 10))
                .unwrap_or(c),
        };
        match c {
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' => {}
            '\u{E0000}'..='\u{E007F}' => {}
            '*' | '_' | '`' | '~' => {}
            '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE58}' | '\u{FE63}' => out.push('-'),
            c if c.is_whitespace() => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            c => out.push(c),
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out
}

fn credential(s: &str) -> Option<&'static str> {
    let masked = mask_dates(s);
    for run in digit_runs(&masked) {
        if (13..=19).contains(&run.digits.len()) && luhn(&run.digits) {
            return Some("card number");
        }
    }
    if has_iban(s) {
        return Some("IBAN");
    }
    if s.split_whitespace().any(id_token) {
        return Some("account or identifier");
    }
    None
}

/// A run of digits joined by single separators (` `, `-`, `.`, `/`, or up to two of
/// `(`, `)`, `+`, ` `).
#[derive(Debug)]
struct Run {
    digits: String,
    groups: usize,
    longest: usize,
    total: usize,
    plus: bool,
}

fn digit_runs(s: &str) -> Vec<Run> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let plus =
            (i > 0 && chars[i - 1] == '+') || (i > 1 && chars[i - 1] == '(' && chars[i - 2] == '+');
        let mut run = Run {
            digits: String::new(),
            groups: 0,
            longest: 0,
            total: 0,
            plus,
        };
        loop {
            let start = run.digits.len();
            while i < chars.len() && chars[i].is_ascii_digit() {
                run.digits.push(chars[i]);
                i += 1;
            }
            let len = run.digits.len() - start;
            run.groups += 1;
            run.longest = run.longest.max(len);
            // Up to three separators, then more digits: the same run.
            let mut j = i;
            while j < chars.len()
                && j - i < 3
                && matches!(chars[j], ' ' | '-' | '.' | '/' | '(' | ')')
            {
                j += 1;
            }
            let gap = &chars[i..j];
            let spaced_one = gap.iter().filter(|c| **c != ' ').count() <= 1
                && gap.first() == Some(&' ')
                && gap.last() == Some(&' ');
            let joined = j > i
                && j < chars.len()
                && chars[j].is_ascii_digit()
                && (j - i == 1
                    || (j - i == 2 && gap.iter().any(|c| matches!(c, '(' | ')')))
                    || (j - i == 3 && spaced_one));
            if !joined {
                break;
            }
            i = j;
        }
        run.total = run.digits.len();
        out.push(run);
    }
    out
}

/// `s` with dates (`2026-06-03`, `03/06/2026`, `3.6.2026`, `05/30`) and times (`13:00`,
/// `09:30:15`) replaced by letters, so they never count as long numbers.
fn mask_dates(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (k, tok) in s.split(' ').enumerate() {
        if k > 0 {
            out.push(' ');
        }
        let core = tok.trim_matches(|c: char| !c.is_alphanumeric());
        if is_numeric_date(core) || is_time(core) {
            out.push_str(&tok.replace(|c: char| c.is_ascii_digit(), "d"));
        } else if let Some((d, rest)) = core.split_once('T')
            && is_numeric_date(d)
            && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            // An ISO date-time.
            out.push_str(&tok.replace(|c: char| c.is_ascii_digit(), "d"));
        } else {
            out.push_str(tok);
        }
    }
    out
}

/// `2026-06-03`, `2026/06/03`, `03/06/2026`, `3.6.26`, `05/30`, `6-3`.
fn is_numeric_date(t: &str) -> bool {
    let Some(sep) = t.chars().find(|c| matches!(c, '-' | '/' | '.')) else {
        return false;
    };
    let parts: Vec<&str> = t.split(sep).collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    let n: Vec<u32> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    let small = |x: u32, max: u32| (1..=max).contains(&x);
    match (parts.len(), parts[0].len()) {
        (3, 4) => parts[1].len() <= 2 && parts[2].len() <= 2 && small(n[1], 12) && small(n[2], 31),
        (3, _) => {
            parts[0].len() <= 2
                && parts[1].len() <= 2
                && matches!(parts[2].len(), 2 | 4)
                && small(n[0], 31)
                && small(n[1], 31)
        }
        (2, _) => {
            sep == '/'
                && parts[0].len() <= 2
                && parts[1].len() <= 2
                && small(n[0], 31)
                && small(n[1], 31)
        }
        _ => false,
    }
}

/// `13:00`, `9:30:15`, `10:00am`.
fn is_time(t: &str) -> bool {
    let t = t
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .trim_end_matches(['a', 'p', 'm', 'A', 'P', 'M']);
    let parts: Vec<&str> = t.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| (1..=2).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()))
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0;
    for (i, c) in digits.chars().rev().enumerate() {
        let mut d = c.to_digit(10).unwrap_or(0);
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

/// An IBAN: two letters, two check digits and 11–30 letters or digits (groups separated by
/// single spaces allowed), whose mod-97 check holds.
fn has_iban(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len() {
        if i > 0 && chars[i - 1].is_alphanumeric() {
            continue;
        }
        let head: String = chars[i..].iter().take(4).collect();
        let h: Vec<char> = head.chars().collect();
        if h.len() < 4
            || !h[0].is_ascii_uppercase()
            || !h[1].is_ascii_uppercase()
            || !h[2].is_ascii_digit()
            || !h[3].is_ascii_digit()
        {
            continue;
        }
        let mut compact = String::new();
        let mut j = i;
        while j < chars.len() && compact.len() < 34 {
            let c = chars[j];
            if c.is_ascii_alphanumeric() {
                compact.push(c.to_ascii_uppercase());
            } else if !(c == ' ' && chars.get(j + 1).is_some_and(char::is_ascii_alphanumeric)) {
                break;
            }
            j += 1;
        }
        if (15..=34)
            .rev()
            .any(|n| n <= compact.len() && iban_ok(&compact[..n]))
        {
            return true;
        }
    }
    false
}

fn iban_ok(c: &str) -> bool {
    let rearranged = format!("{}{}", &c[4..], &c[..4]);
    let mut rem: u64 = 0;
    for ch in rearranged.chars() {
        let v = match ch {
            '0'..='9' => u64::from(ch as u8 - b'0'),
            'A'..='Z' => u64::from(ch as u8 - b'A') + 10,
            _ => return false,
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

/// An account- or identifier-like token.
fn id_token(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| !c.is_alphanumeric());
    if is_numeric_date(t) || t.split_once('T').is_some_and(|(d, _)| is_numeric_date(d)) {
        return false;
    }
    let digits = t.chars().filter(char::is_ascii_digit).count();
    let letters = t.chars().filter(char::is_ascii_alphabetic).count();
    digits >= 6 && letters >= 1
}

/// Codes that make a number an amount.
const CURRENCY_CODES: &[&str] = &[
    "usd", "eur", "gbp", "chf", "jpy", "cad", "aud", "nzd", "inr", "cny", "sek", "nok", "dkk",
    "pln", "dollars", "dollar", "euros", "euro", "pounds", "pence", "quid", "bucks",
];

fn is_amount(s: &str) -> bool {
    if s.contains(['$', '€', '£', '¥', '₹', '₩', '₽', '¢']) {
        return true;
    }
    let lower = s.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != ',' && c != '.')
        .filter(|w| !w.is_empty())
        .collect();
    let numeric = |w: &str| w.chars().next().is_some_and(|c| c.is_ascii_digit());
    for (i, w) in words.iter().enumerate() {
        let w = w.trim_matches(|c: char| c == ',' || c == '.');
        let near_number =
            (i > 0 && numeric(words[i - 1])) || words.get(i + 1).is_some_and(|n| numeric(n));
        if CURRENCY_CODES.contains(&w) && near_number {
            return true;
        }
        // Thousands separators: 7,450 / 210,437.55.
        if numeric(w)
            && let Some((head, tail)) = w.split_once(',')
            && head.len() <= 3
            && head.chars().all(|c| c.is_ascii_digit())
            && tail.split(',').all(|g| {
                let g = g.split('.').next().unwrap_or(g);
                g.len() == 3 && g.chars().all(|c| c.is_ascii_digit())
            })
        {
            return true;
        }
    }
    false
}

/// Whether `s` is a date or time and nothing else: what the date predicates take.
pub(crate) fn is_date_value(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || sensitive_amount_or_card(t) {
        return false;
    }
    if normalize_date(t, OffsetDateTime::UNIX_EPOCH).is_some() {
        return true;
    }
    let mut any = false;
    for tok in t.split_whitespace() {
        let core = tok.trim_matches(|c: char| !c.is_alphanumeric());
        if core.is_empty() {
            continue;
        }
        if is_numeric_date(core) || is_time(core) {
            any = true;
            continue;
        }
        for w in core
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let year = w.len() == 4 && w.parse::<i32>().is_ok_and(|y| (1900..=2200).contains(&y));
            let hour = w
                .strip_suffix("am")
                .or_else(|| w.strip_suffix("pm"))
                .is_some_and(|h| {
                    !h.is_empty() && h.len() <= 2 && h.chars().all(|c| c.is_ascii_digit())
                });
            if is_weekday(w)
                || month_of(w).is_some()
                || day_of(w).is_some()
                || year
                || hour
                || RELATIVE_DATE_WORDS.contains(&w)
            {
                any = true;
            } else if !matches!(
                w,
                "on" | "by"
                    | "the"
                    | "of"
                    | "at"
                    | "before"
                    | "after"
                    | "end"
                    | "start"
                    | "mid"
                    | "early"
                    | "late"
                    | "until"
                    | "from"
                    | "to"
                    | "and"
                    | "or"
                    | "in"
                    | "around"
            ) {
                return false;
            }
        }
    }
    any
}

fn sensitive_amount_or_card(s: &str) -> bool {
    is_amount(s) || credential(s).is_some()
}

/// Predicates an item that looks medical or financial may still yield.
pub(crate) const STRUCTURAL: &[&str] = &[
    "attended",
    "organized",
    "held_at",
    "works_at",
    "works_on",
    "member_of",
    "part_of",
    "located_in",
    "reports_to",
    "manages",
    "leads",
];

/// One of these in an item's text or title makes it medical or financial.
pub(crate) const STRONG_TERMS: &[&str] = &[
    "diagnosis",
    "diagnosed",
    "prescription",
    "prescribed",
    "dosage",
    "medication",
    "anaemia",
    "anemia",
    "ferritin",
    "haemoglobin",
    "hemoglobin",
    "blood test",
    "blood results",
    "therapy session",
    "counselling session",
    "counseling session",
    "payslip",
    "pay slip",
    "gross salary",
    "net pay",
    "gross pay",
    "national insurance",
    "tax code",
    "mortgage",
    "account number",
    "sort code",
    "iban",
    "card number",
    "debit card",
    "credit card",
    "overdraft",
];

/// Two different ones of these make an item medical or financial ("balance" alone is a
/// work-life balance); one in a claim's object refuses the claim (outside [`STRUCTURAL`]).
pub(crate) const WEAK_TERMS: &[&str] = &[
    "mg",
    "dose",
    "tablet",
    "tablets",
    "therapy",
    "therapist",
    "counselling",
    "counseling",
    "symptoms",
    "salary",
    "balance",
    "statement",
    "loan",
    "interest rate",
    "repayment",
    "pension",
    "bank",
];

/// Folder names (any path segment of the item key, case-insensitive) that mark an item as
/// medical or financial.
pub(crate) const PRIVATE_FOLDERS: &[&str] = &[
    "health", "medical", "finance", "finances", "banking", "bank", "payslips", "tax", "taxes",
];

fn words_of(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

fn has_term(words: &[String], term: &str) -> bool {
    let t: Vec<&str> = term.split(' ').collect();
    words
        .windows(t.len())
        .any(|w| w.iter().zip(&t).all(|(a, b)| a == b))
}

/// Whether an item looks medical or financial (see the module docs).
pub(crate) fn health_or_finance_item(key: &str, title: Option<&str>, text: &str) -> bool {
    let folder = key
        .split(['/', '\\', ':'])
        .rev()
        .skip(1)
        .any(|seg| PRIVATE_FOLDERS.contains(&seg.to_lowercase().as_str()));
    if folder {
        return true;
    }
    let mut words = words_of(title.unwrap_or_default());
    words.extend(words_of(text));
    if STRONG_TERMS.iter().any(|t| has_term(&words, t)) {
        return true;
    }
    WEAK_TERMS.iter().filter(|t| has_term(&words, t)).count() >= 2
}

/// The health or finance term in a claim's object, if any.
pub(crate) fn health_or_finance_term(s: &str) -> Option<&'static str> {
    let words = words_of(s);
    STRONG_TERMS
        .iter()
        .chain(WEAK_TERMS)
        .find(|t| has_term(&words, t))
        .copied()
}

/// Words a job title contains at least one of (lower case, whole words; `head` covers "Head
/// of ..."). Every `has_role` object of the bench gold and every title of the GG5 gold has one.
pub(crate) const JOB_TITLE_WORDS: &[&str] = &[
    // engineering and product
    "engineer",
    "developer",
    "programmer",
    "architect",
    "scientist",
    "designer",
    "analyst",
    "researcher",
    "technician",
    "administrator",
    "operator",
    "specialist",
    // management and leadership
    "manager",
    "director",
    "lead",
    "head",
    "chief",
    "officer",
    "principal",
    "staff",
    "president",
    "vp",
    "svp",
    "evp",
    "avp",
    "ceo",
    "cto",
    "cfo",
    "coo",
    "cio",
    "cmo",
    "cpo",
    "ciso",
    "executive",
    "supervisor",
    "coordinator",
    "chair",
    "chairman",
    "chairwoman",
    "chairperson",
    "founder",
    "cofounder",
    "owner",
    "partner",
    "associate",
    "consultant",
    "advisor",
    "adviser",
    "representative",
    "rep",
    "agent",
    "secretary",
    "treasurer",
    "assistant",
    "fellow",
    // legal, finance, people
    "counsel",
    "lawyer",
    "solicitor",
    "attorney",
    "barrister",
    "paralegal",
    "accountant",
    "auditor",
    "recruiter",
    "mentor",
    "volunteer",
    "intern",
    "trainee",
    "apprentice",
    "student",
    // care, teaching and other trades
    "nurse",
    "doctor",
    "physician",
    "surgeon",
    "gp",
    "pharmacist",
    "dentist",
    "therapist",
    "counsellor",
    "counselor",
    "teacher",
    "lecturer",
    "professor",
    "tutor",
    "captain",
    "coach",
    "editor",
    "writer",
    "journalist",
    "producer",
    "librarian",
];

/// First words that make a "title" a phrase: pronouns and common verbs.
const NOT_TITLE_START: &[&str] = &[
    "i",
    "he",
    "she",
    "they",
    "we",
    "you",
    "it",
    "his",
    "her",
    "their",
    "my",
    "our",
    "your",
    "its",
    "this",
    "that",
    "these",
    "those",
    "is",
    "was",
    "are",
    "were",
    "be",
    "been",
    "being",
    "has",
    "have",
    "had",
    "will",
    "would",
    "can",
    "could",
    "should",
    "may",
    "might",
    "must",
    "do",
    "does",
    "did",
    "joined",
    "joining",
    "join",
    "leads",
    "leading",
    "led",
    "works",
    "working",
    "worked",
    "manages",
    "managing",
    "managed",
    "said",
    "says",
    "asked",
    "support",
    "supports",
    "supporting",
    "met",
    "meet",
    "sent",
    "send",
    "became",
    "become",
    "becoming",
    "promoted",
    "moved",
    "start",
    "started",
    "starting",
    "left",
    "leaving",
    "running",
    "runs",
    "ran",
    "got",
    "get",
    "make",
    "made",
    "take",
    "took",
    "going",
    "went",
];

/// Why `title` is not a job title, if it is not (`names`: normalised names of the item's people
/// and organisations, the user's included).
pub(crate) fn title_problem(title: &str, names: &[String]) -> Option<&'static str> {
    let t = title.trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.is_empty() || words.len() > 6 {
        return Some("not a job title: length");
    }
    if t.chars().any(|c| c.is_ascii_digit()) || is_amount(t) || t.contains('@') {
        return Some("not a job title: number, amount or address");
    }
    if t.contains(['!', '?', ';', ':', '"', '\u{201c}', '\u{201d}']) || t.contains(". ") {
        return Some("not a job title: a sentence");
    }
    if is_date_value(t) {
        return Some("not a job title: a date");
    }
    let n = nugget_graph::normalize_name(t);
    if names.contains(&n) {
        return Some("not a job title: a name");
    }
    let first = words[0]
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if NOT_TITLE_START.contains(&first.as_str()) {
        return Some("not a job title: starts with a verb or pronoun");
    }
    let has_title_word = t
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| JOB_TITLE_WORDS.contains(&w));
    if !has_title_word {
        return Some("not a job title: no title word");
    }
    None
}

/// Words that make a quote hypothetical, a request or a plan rather than a statement of fact.
const HYPOTHETICAL_WORDS: &[&str] = &[
    "would",
    "could",
    "might",
    "should",
    "if",
    "unless",
    "whether",
    "maybe",
    "perhaps",
    "possibly",
    "proposed",
    "potential",
    "potentially",
    "hypothetically",
    "considering",
    "nominated",
    "nomination",
];

/// Phrases with the same effect.
const HYPOTHETICAL_PHRASES: &[&str] = &[
    "case for",
    "plan to",
    "plans to",
    "planning to",
    "planned to",
    "hope to",
    "hopes to",
    "hoping to",
    "want to",
    "wants to",
    "aim to",
    "aims to",
    "intend to",
    "intends to",
    "in line for",
    "up for",
];

/// Predicates a hypothetical quote cannot support (states: titles, employers, membership...).
pub(crate) fn hypothesis_matters(predicate: &str) -> bool {
    !matches!(
        predicate,
        "committed_to" | "due_on" | "prefers" | "dislikes" | "attended" | "organized" | "held_at"
    )
}

/// The hypothetical cue in a quote, if any.
pub(crate) fn hypothetical(quote: &str) -> Option<&'static str> {
    let words = words_of(quote);
    if let Some(w) = HYPOTHETICAL_WORDS
        .iter()
        .find(|w| words.iter().any(|x| x == *w))
        .or_else(|| HYPOTHETICAL_PHRASES.iter().find(|p| has_term(&words, p)))
    {
        return Some(w);
    }
    words
        .iter()
        .enumerate()
        .filter(|(_, w)| matches!(w.as_str(), "asked" | "ask" | "asking"))
        .any(|(i, _)| words.iter().skip(i + 1).take(5).any(|w| w == "to"))
        .then_some("a request")
}

/// Words of a field label.
pub(crate) const FIELD_LABEL_WORDS: &[&str] = &[
    "account",
    "balance",
    "salary",
    "prescription",
    "diagnosis",
    "dosage",
    "statement",
    "payment",
    "payments",
    "invoice",
    "total",
    "amount",
    "card",
    "number",
    "reference",
    "gross",
    "net",
    "tax",
    "mortgage",
    "policy",
    "expiry",
    "borrower",
    "lender",
    "property",
    "deadline",
    "milestones",
    "date",
    "subject",
    "results",
];

/// The field-label word in a name, if any.
pub(crate) fn field_label(name: &str) -> Option<&'static str> {
    let words = words_of(name);
    FIELD_LABEL_WORDS
        .iter()
        .find(|w| words.iter().any(|x| x == *w))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_accounts_and_amounts_are_found() {
        for (s, why) in [
            ("4000 0566 5566 5556", "card number"),
            ("NCU-4471-0093-218", "account or identifier"),
            ("GB82 WEST 1234 5698 7654 32", "IBAN"),
            ("210,437.55", "currency amount"),
            ("450 GBP", "currency amount"),
            ("mara.okafor@mailbox.example", "email address"),
            ("+44 7700 900123", "phone or account number"),
            ("12345678", "long number"),
        ] {
            assert_eq!(sensitive_value(s), Some(why), "{s}");
        }
        for s in [
            "Head of Product",
            "2026-06-03 13:00",
            "05/30",
            "Harbor go-no-go review (launch 3 Dec)",
            "Room 101",
            "4000",
            "COVID-19",
        ] {
            assert_eq!(sensitive_value(s), None, "{s}");
        }
        // A number that only looks like a card (Luhn fails) is still a long number.
        assert_eq!(sensitive_value("4000 0566 5566 5557"), Some("long number"));
    }

    /// Security review 3 (ER-2, ER-3): digits of other scripts, fullwidth forms, spaced
    /// separators, line breaks and Markdown or HTML formatting do not hide a value.
    #[test]
    fn formatting_and_other_digits_do_not_hide_values() {
        for (s, why) in [
            ("４０００ ０５６６ ５５６６ ５５５６", "card number"),
            ("Karte ٤٠٠٠ ٠٥٦٦ ٥٥٦٦ ٥٥٥٦", "card number"),
            ("4000 - 0566 - 5566 - 5556", "card number"),
            (
                "4000 \u{2013} 0566 \u{2013} 5566 \u{2013} 5556",
                "card number",
            ),
            ("4000\n0566\n5566\n5556", "card number"),
            ("**4000** **0566** **5566** **5556**", "card number"),
            (
                "<td>4000</td><td>0566</td><td>5566</td><td>5556</td>",
                "card number",
            ),
            ("4000\u{200B}0566\u{00AD}5566 5556", "card number"),
            ("IBAN **DE89** 3704 0044 0532 0130 00", "IBAN"),
            ("ＤＥ８９ ３７０４ ００４４ ０５３２ ０１３０ ００", "IBAN"),
            ("Kontonummer ０７７００９００１２３", "long number"),
            ("٠٧٧٠٠ ٩٠٠١٢٣", "phone or account number"),
            ("＄40,000", "currency amount"),
        ] {
            assert_eq!(sensitive_value(s), Some(why), "{s}");
            if matches!(why, "card number" | "IBAN") {
                assert_eq!(credential_in_quote(&format!("Sam: {s}.")), Some(why), "{s}");
            }
        }
        // Addresses in angle brackets are not tags.
        assert_eq!(
            sensitive_value("Mara <mara@corvid.example>"),
            Some("email address")
        );
        for s in [
            "<b>Head of Product</b>",
            "**Staff Engineer**",
            "2025 to 2026",
            "Room １０１",
        ] {
            assert_eq!(sensitive_value(s), None, "{s}");
        }
    }

    #[test]
    fn quotes_refuse_only_credentials() {
        assert_eq!(
            credential_in_quote("Card number: 4000 0566 5566 5556"),
            Some("card number")
        );
        assert_eq!(
            credential_in_quote("Mortgage account: NCU-4471-0093-218"),
            Some("account or identifier")
        );
        assert_eq!(
            credential_in_quote("Gross salary (monthly): £7,450.00"),
            None
        );
        assert_eq!(
            credential_in_quote("The go-no-go review is on 2026-10-14"),
            None
        );
    }

    #[test]
    fn date_values() {
        for s in [
            "2026-11-03",
            "Friday 30 October",
            "end of Q3",
            "13:00",
            "tomorrow",
        ] {
            assert!(is_date_value(s), "{s}");
        }
        for s in ["4000 0566 5566 5556", "the launch", ""] {
            assert!(!is_date_value(s), "{s}");
        }
    }

    #[test]
    fn titles_have_the_shape_of_a_job_title() {
        let names = vec!["tessellate systems".to_owned(), "mara okafor".to_owned()];
        for t in [
            "Head of Product",
            "Staff Engineer, Platform",
            "VP Engineering",
            "nurse",
            "HR Business Partner",
        ] {
            assert_eq!(title_problem(t, &names), None, "{t}");
        }
        for t in [
            "NCU-4471-0093-218",
            "£7,450.00",
            "ferrous sulfate 200 mg",
            "25 November",
            "Tessellate Systems",
            "Mara Okafor",
            "backfill green, dual-write starts next sprint",
            "joined Corvid Labs as Head of Product",
            "senior staff principal lead engineer of platform",
        ] {
            assert!(title_problem(t, &names).is_some(), "{t}");
        }
    }

    #[test]
    fn health_and_finance_items_and_terms() {
        assert!(health_or_finance_item(
            "Health/2026-05-12-gp-visit.md",
            None,
            ""
        ));
        assert!(health_or_finance_item("0:Finance/x.md", None, ""));
        assert!(health_or_finance_item(
            "Notes/x.md",
            None,
            "Prescription: ferrous sulfate 200 mg"
        ));
        assert!(health_or_finance_item("x.md", Some("Payslip April"), ""));
        assert!(health_or_finance_item(
            "x.md",
            None,
            "Your balance and the statement for June"
        ));
        assert!(!health_or_finance_item(
            "Notes/Journal/2026-03-15.md",
            None,
            "Climbing with Sam, good work-life balance."
        ));
        assert!(!health_or_finance_item(
            "Mail/Inbox/1.eml",
            Some("Intro"),
            "I'm Gus Ferreira, your account manager at Tessellate Systems"
        ));
        assert_eq!(
            health_or_finance_term("iron-deficiency anaemia"),
            Some("anaemia")
        );
        assert_eq!(health_or_finance_term("ferrous sulfate 200 mg"), Some("mg"));
        assert_eq!(health_or_finance_term("window seats"), None);
    }

    #[test]
    fn hypothetical_quotes() {
        for q in [
            "I support her case for promotion to Principal Engineer",
            "She would make a great Head of Platform",
            "If Dana joins Corvid Labs",
            "I asked her in March to lead Harbor",
            "Proposed level: Principal Engineer.",
        ] {
            assert!(hypothetical(q).is_some(), "{q}");
        }
        for q in [
            "From Monday I'm Head of Product at Corvid Labs",
            "Employee: Mara Okafor, Staff Engineer, Brightwell Analytics",
            "Launch moved to 3 December",
        ] {
            assert_eq!(hypothetical(q), None, "{q}");
        }
    }

    #[test]
    fn field_labels() {
        for n in [
            "Mortgage account",
            "Gross salary (monthly",
            "Prescription",
            "Balance at 30 June 2026",
        ] {
            assert!(field_label(n).is_some(), "{n}");
        }
        for n in ["Mara Okafor", "Lena Brandt", "Gus Ferreira", "R. Chen"] {
            assert_eq!(field_label(n), None, "{n}");
        }
    }
}
