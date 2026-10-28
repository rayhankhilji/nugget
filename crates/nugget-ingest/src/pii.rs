//! Deterministic PII and secret scan.
//!
//! Finds payment card numbers (13–19 digits, optional single space/dash separators, Luhn-valid,
//! plausible issuer prefix), US Social Security numbers (`AAA-GG-SSSS` with the SSA's invalid
//! ranges excluded), IBANs (ISO 13616 mod-97 check) and API-key-like secrets (well-known token
//! prefixes, PEM private keys, and `key = <long random value>` assignments).
//!
//! Only counts are reported; matched values are never returned, logged or stored. Any finding
//! marks the item sensitive, which keeps it away from cloud triage and cloud embeddings.

use serde::{Deserialize, Serialize};

/// Number of matches per category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiiFindings {
    pub card: u32,
    pub ssn: u32,
    pub iban: u32,
    pub secret: u32,
}

impl PiiFindings {
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn total(&self) -> u32 {
        self.card + self.ssn + self.iban + self.secret
    }

    fn add(&mut self, o: PiiFindings) {
        self.card += o.card;
        self.ssn += o.ssn;
        self.iban += o.iban;
        self.secret += o.secret;
    }
}

/// Scan every given text (e.g. title and body) and sum the findings.
pub fn scan_all<'a>(texts: impl IntoIterator<Item = &'a str>) -> PiiFindings {
    let mut f = PiiFindings::default();
    for t in texts {
        f.add(scan(t));
    }
    f
}

/// Scan one text.
pub fn scan(text: &str) -> PiiFindings {
    let b = text.as_bytes();
    PiiFindings {
        card: count_cards(b),
        ssn: count_ssns(b),
        iban: count_ibans(b),
        secret: count_secrets(text),
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

// ---- payment cards ----

fn count_cards(b: &[u8]) -> u32 {
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && (is_word(b[i - 1]) || b[i - 1] == b'.')) {
            i += 1;
            continue;
        }
        // Collect digits with single separators.
        let mut digits: Vec<u8> = Vec::with_capacity(19);
        let mut j = i;
        let mut sep: Option<u8> = None;
        while j < b.len() {
            let c = b[j];
            if c.is_ascii_digit() {
                digits.push(c - b'0');
                j += 1;
                if digits.len() > 19 {
                    break;
                }
            } else if (c == b' ' || c == b'-')
                && j + 1 < b.len()
                && b[j + 1].is_ascii_digit()
                && sep.is_none_or(|s| s == c)
            {
                sep = Some(c);
                j += 1;
            } else {
                break;
            }
        }
        let end_ok = j >= b.len()
            || !(is_word(b[j]) || (b[j] == b'.' && b.get(j + 1).is_some_and(u8::is_ascii_digit)));
        if (13..=19).contains(&digits.len())
            && end_ok
            && (2..=6).contains(&digits[0])
            && !digits.iter().all(|d| *d == digits[0])
            && luhn(&digits)
        {
            n += 1;
        }
        i = j.max(i + 1);
    }
    n
}

fn luhn(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    for (k, d) in digits.iter().rev().enumerate() {
        let mut v = u32::from(*d);
        if k % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum.is_multiple_of(10)
}

// ---- US SSN ----

fn count_ssns(b: &[u8]) -> u32 {
    let mut n = 0;
    if b.len() < 11 {
        return 0;
    }
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    let num = |s: &[u8]| s.iter().fold(0u32, |a, d| a * 10 + u32::from(d - b'0'));
    let mut i = 0;
    while i + 11 <= b.len() {
        let w = &b[i..i + 11];
        let before_ok = i == 0 || !(is_word(b[i - 1]) || b[i - 1] == b'-');
        let after_ok = i + 11 == b.len() || !(is_word(b[i + 11]) || b[i + 11] == b'-');
        if before_ok
            && after_ok
            && w[3] == b'-'
            && w[6] == b'-'
            && digits(&w[0..3])
            && digits(&w[4..6])
            && digits(&w[7..11])
        {
            let area = num(&w[0..3]);
            let group = num(&w[4..6]);
            let serial = num(&w[7..11]);
            if area != 0 && area != 666 && area < 900 && group != 0 && serial != 0 {
                n += 1;
                i += 11;
                continue;
            }
        }
        i += 1;
    }
    n
}

// ---- IBAN ----

fn count_ibans(b: &[u8]) -> u32 {
    let mut n = 0;
    let mut i = 0;
    while i + 4 <= b.len() {
        let start_ok = i == 0 || !is_word(b[i - 1]);
        if !(start_ok
            && b[i].is_ascii_uppercase()
            && b[i + 1].is_ascii_uppercase()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit())
        {
            i += 1;
            continue;
        }
        // Groups of [A-Z0-9] separated by single spaces.
        let mut groups: Vec<(usize, usize)> = Vec::new();
        let mut j = i;
        let mut total = 0;
        loop {
            let s = j;
            while j < b.len() && (b[j].is_ascii_uppercase() || b[j].is_ascii_digit()) {
                j += 1;
            }
            if j == s {
                break;
            }
            total += j - s;
            groups.push((s, j));
            if total > 34 || j >= b.len() || b[j] != b' ' {
                break;
            }
            if j + 1 < b.len() && (b[j + 1].is_ascii_uppercase() || b[j + 1].is_ascii_digit()) {
                j += 1;
            } else {
                break;
            }
        }
        // Try the longest prefix of whole groups that is a valid IBAN.
        let mut matched_end = None;
        for k in (0..groups.len()).rev() {
            let end = groups[k].1;
            if end < b.len() && is_word(b[end]) {
                continue;
            }
            let compact: Vec<u8> = groups[..=k]
                .iter()
                .flat_map(|(s, e)| b[*s..*e].iter().copied())
                .collect();
            if (15..=34).contains(&compact.len()) && iban_valid(&compact) {
                matched_end = Some(end);
                break;
            }
        }
        match matched_end {
            Some(end) => {
                n += 1;
                i = end;
            }
            None => i += 1,
        }
    }
    n
}

fn iban_valid(s: &[u8]) -> bool {
    // Move the first four characters to the end, letters → 10..35, then mod 97 == 1.
    let mut rem: u32 = 0;
    for &c in s[4..].iter().chain(&s[..4]) {
        let v = match c {
            b'0'..=b'9' => u32::from(c - b'0'),
            b'A'..=b'Z' => u32::from(c - b'A') + 10,
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

// ---- secrets ----

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn count_secrets(text: &str) -> u32 {
    let mut n = 0;
    // PEM private keys: a `-----BEGIN … PRIVATE KEY-----` line.
    n += text
        .lines()
        .filter(|l| l.contains("-----BEGIN") && l.contains("PRIVATE KEY-----"))
        .count() as u32;
    // Well-known token shapes.
    let mut prefixed_hits = 0;
    for tok in text.split(|c: char| !is_token_char(c)) {
        if known_token(tok) {
            prefixed_hits += 1;
        }
    }
    n += prefixed_hits;
    // `api_key = "…"`-style assignments with a long, mixed value (not already counted).
    n += assignments(text);
    n
}

fn known_token(t: &str) -> bool {
    let len = t.len();
    let alnum_tail = |p: &str, min: usize| {
        t.strip_prefix(p)
            .is_some_and(|rest| rest.len() >= min && rest.chars().all(is_token_char) && mixed(rest))
    };
    // AWS access key ids.
    if len == 20
        && (t.starts_with("AKIA") || t.starts_with("ASIA"))
        && t.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return true;
    }
    // Google API keys.
    if len == 39 && t.starts_with("AIza") {
        return true;
    }
    for (p, min) in [
        ("sk-ant-", 20),
        ("sk-proj-", 20),
        ("sk-", 20),
        ("ghp_", 30),
        ("gho_", 30),
        ("ghu_", 30),
        ("ghs_", 30),
        ("ghr_", 30),
        ("github_pat_", 22),
        ("glpat-", 20),
        ("xoxb-", 10),
        ("xoxp-", 10),
        ("xoxa-", 10),
        ("xoxr-", 10),
        ("xoxs-", 10),
        ("sk_live_", 16),
        ("rk_live_", 16),
        ("hf_", 30),
    ] {
        if alnum_tail(p, min) {
            return true;
        }
    }
    false
}

/// Has both letters and digits (cheap randomness heuristic).
fn mixed(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_alphabetic()) && s.chars().any(|c| c.is_ascii_digit())
}

const SECRET_KEYWORDS: &[&str] = &[
    "api_key",
    "api-key",
    "apikey",
    "secret",
    "token",
    "password",
    "passwd",
    "client_secret",
    "access_key",
    "private_key",
];

fn assignments(text: &str) -> u32 {
    let lower = text.to_ascii_lowercase();
    let lb = lower.as_bytes();
    let tb = text.as_bytes();
    // Value start offsets already counted (`client_secret` also contains `secret`).
    let mut counted = std::collections::HashSet::new();
    for kw in SECRET_KEYWORDS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(kw) {
            let at = from + pos;
            from = at + kw.len();
            // Skip an optional closing quote and blanks, then require `:` or `=`.
            let mut j = at + kw.len();
            while j < lb.len() && matches!(lb[j], b'"' | b'\'' | b' ' | b'\t') {
                j += 1;
            }
            if j >= lb.len() || !matches!(lb[j], b':' | b'=') {
                continue;
            }
            j += 1;
            while j < lb.len() && matches!(lb[j], b'"' | b'\'' | b' ' | b'\t') {
                j += 1;
            }
            let start = j;
            while j < tb.len()
                && (tb[j].is_ascii_alphanumeric()
                    || matches!(tb[j], b'_' | b'-' | b'/' | b'+' | b'.' | b'='))
            {
                j += 1;
            }
            let value = &text[start..j];
            if value.len() >= 16 && mixed(value) && !known_token_in(value) {
                counted.insert(start);
            }
        }
    }
    counted.len() as u32
}

fn known_token_in(v: &str) -> bool {
    v.split(|c: char| !is_token_char(c)).any(known_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards() {
        assert_eq!(scan("visa 4111 1111 1111 1111 exp").card, 1);
        assert_eq!(scan("4111-1111-1111-1111").card, 1);
        assert_eq!(scan("amex 378282246310005.").card, 1);
        assert_eq!(scan("mc 5555555555554444, visa 4012888888881881").card, 2);
        // Luhn failure, wrong length, mixed separators, part of a longer token.
        assert_eq!(scan("4111 1111 1111 1112").card, 0);
        assert_eq!(scan("411111111111").card, 0);
        assert_eq!(scan("4111-1111 1111-1111").card, 0);
        assert_eq!(scan("id4111111111111111").card, 0);
        assert_eq!(scan("order 1234567890123452 shipped").card, 0, "prefix 1");
        assert_eq!(scan("v1.4111111111111111").card, 0);
    }

    #[test]
    fn ssns() {
        assert_eq!(scan("SSN: 123-45-6789").ssn, 1);
        assert_eq!(scan("123-45-6789 and 234-56-7890").ssn, 2);
        for bad in [
            "000-12-3456",
            "666-12-3456",
            "900-12-3456",
            "123-00-4567",
            "123-45-0000",
            "1123-45-6789",
            "123-45-67890",
            "phone 555-123-4567",
        ] {
            assert_eq!(scan(bad).ssn, 0, "{bad}");
        }
    }

    #[test]
    fn ibans() {
        assert_eq!(scan("IBAN DE89 3704 0044 0532 0130 00 please").iban, 1);
        assert_eq!(scan("GB82WEST12345698765432").iban, 1);
        assert_eq!(scan("pay to GB82 WEST 1234 5698 7654 32 TODAY").iban, 1);
        assert_eq!(scan("GB82WEST12345698765433").iban, 0, "bad checksum");
        assert_eq!(scan("DE89").iban, 0);
    }

    #[test]
    fn secrets() {
        assert_eq!(
            scan("key sk-ant-api03-abcDEF1234567890abcdef1234").secret,
            1
        );
        assert_eq!(scan("AKIAIOSFODNN7EXAMPLE").secret, 1);
        assert_eq!(scan("ghp_1234567890abcdefghijklmnopqrstuvwxyzAB").secret, 1);
        assert_eq!(scan("AIzaSyA1234567890abcdefghijklmnopqrstuv").secret, 1);
        assert_eq!(
            scan("-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n-----END RSA PRIVATE KEY-----").secret,
            1
        );
        assert_eq!(scan("api_key = \"q8Zr2LmN0pX4vB7tK1wY\"").secret, 1);
        assert_eq!(
            scan("\"client_secret\": \"a1b2c3d4e5f6g7h8i9j0\"").secret,
            1
        );
        // Not secrets.
        assert_eq!(scan("the token is in the drawer").secret, 0);
        assert_eq!(scan("password: hunter2").secret, 0);
        assert_eq!(scan("ask-me-anything sk-short").secret, 0);
        assert_eq!(
            scan("secret = averyveryverylongwordwithoutdigits").secret,
            0
        );
    }

    #[test]
    fn clean_text_and_hostile_input() {
        let f = scan("Meeting notes: ship v2 on 2024-05-01. Call 555-0100.");
        assert!(f.is_empty(), "{f:?}");
        // Arbitrary bytes and long digit runs never panic.
        let junk: String = (0..5000u32)
            .map(|i| char::from_u32(i % 0x2FF + 1).unwrap())
            .collect();
        let _ = scan(&junk);
        let _ = scan(&"9".repeat(100_000));
        let _ = scan(&"DE89 ".repeat(10_000));
        let _ = scan("é4111111111111111ü api_key=ñ");
    }
}
