//! Stylometry: the habits of one message or a set of messages, and a distance between them.
//!
//! Profiles use the [`nugget_voice::StyleStats`] contract type, so the voice-match metric can
//! switch to `nugget_voice::style_distance` once M3-D lands it (see [`distance`]). Until then
//! the features and weights here are the suite's own definition, documented in
//! `bench/README.md`.

use std::collections::BTreeMap;

use nugget_voice::StyleStats;

const GREETINGS: &[&str] = &[
    "hi",
    "hey",
    "hello",
    "dear",
    "hiya",
    "yo",
    "greetings",
    "morning",
];
const SIGNOFFS: &[&str] = &[
    "best",
    "thanks",
    "thank",
    "cheers",
    "regards",
    "kind",
    "warm",
    "warmly",
    "sincerely",
    "talk",
    "speak",
    "many",
    "all",
];
const FORMAL: &[&str] = &[
    "dear",
    "regards",
    "kindly",
    "please",
    "would",
    "grateful",
    "appreciate",
    "ensure",
    "sincerely",
    "accordingly",
    "should",
    "therefore",
];
const CASUAL: &[&str] = &[
    "hey", "yeah", "yep", "nope", "gonna", "lol", "haha", "omg", "awesome", "super", "cool", "btw",
    "pls", "thx", "ok", "sooo", "gooo",
];
const EMOTICONS: &[&str] = &[":)", ":-)", ":D", ";)", ":(", ":P"];

/// One message split into greeting, body and sign-off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// Greeting pattern with the addressee replaced by `{name}`, e.g. `Hi {name},`.
    pub greeting: Option<String>,
    pub body: String,
    /// Sign-off block as written, lines joined with `\n`, e.g. `Best,\nMaya`.
    pub signoff: Option<String>,
}

fn first_word(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

fn greeting_pattern(line: &str) -> Option<String> {
    let line = line.trim();
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.is_empty() || words.len() > 4 || !GREETINGS.contains(&first_word(line).as_str()) {
        return None;
    }
    let punct = line
        .chars()
        .last()
        .filter(|c| matches!(c, ',' | '!' | ':' | '.'))
        .map(String::from)
        .unwrap_or_default();
    let word = words[0].trim_end_matches([',', '!', ':', '.']);
    if words.len() == 1 {
        return Some(format!("{word}{punct}"));
    }
    Some(format!("{word} {{name}}{punct}"))
}

fn is_signoff_line(line: &str) -> bool {
    let line = line.trim();
    let n = line.split_whitespace().count();
    if n == 0 || n > 4 {
        return false;
    }
    SIGNOFFS.contains(&first_word(line).as_str()) || line.starts_with('-') || line.starts_with('–')
}

/// Split a message into greeting, body and sign-off.
pub fn split(text: &str) -> Parts {
    let mut lines: Vec<&str> = text.lines().map(str::trim).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut start = 0;
    while lines.get(start).is_some_and(|l| l.is_empty()) {
        start += 1;
    }
    let mut greeting = None;
    if lines.len() > start + 1
        && let Some(g) = greeting_pattern(lines[start])
    {
        greeting = Some(g);
        start += 1;
    }
    let mut end = lines.len();
    let mut signoff = None;
    if end > start + 1 {
        let last = lines[end - 1];
        let prev = lines[end - 2];
        let short = |l: &str| (1..=3).contains(&l.split_whitespace().count());
        if short(last) && is_signoff_line(prev) && prev.ends_with(',') {
            // "Best,\nMaya"
            signoff = Some(format!("{prev}\n{last}"));
            end -= 2;
        } else if is_signoff_line(last) {
            signoff = Some(last.to_owned());
            end -= 1;
        }
    }
    let body = lines[start.min(end)..end]
        .iter()
        .filter(|l| !l.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    Parts {
        greeting,
        body,
        signoff,
    }
}

/// Emoji and emoticons in `text`.
pub fn emoji_count(text: &str) -> usize {
    let pictographs = text
        .chars()
        .filter(|c| matches!(*c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F000..=0x1F2FF))
        .count();
    let emoticons: usize = EMOTICONS.iter().map(|e| text.matches(e).count()).sum();
    pictographs + emoticons
}

/// The emoji and emoticons in `text`, in order.
pub fn emoji_list(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .chars()
        .filter(|c| matches!(*c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF))
        .map(String::from)
        .collect();
    for e in EMOTICONS {
        for _ in 0..text.matches(e).count() {
            out.push((*e).to_owned());
        }
    }
    out
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// A 0 (casual) to 1 (formal) score from formal and casual markers, contractions, emoji,
/// exclamation marks and a lowercase start.
pub fn formality(parts: &Parts) -> f32 {
    let all = format!(
        "{} {} {}",
        parts.greeting.as_deref().unwrap_or(""),
        parts.body,
        parts.signoff.as_deref().unwrap_or("")
    );
    let ws = words(&all);
    let formal = ws.iter().filter(|w| FORMAL.contains(&w.as_str())).count();
    let casual = ws.iter().filter(|w| CASUAL.contains(&w.as_str())).count();
    let contractions = ws
        .iter()
        .filter(|w| {
            ["n't", "'ll", "'re", "'m", "'ve", "'d", "'s"]
                .iter()
                .any(|s| w.ends_with(s))
        })
        .count();
    let mut f = 0.5 + 0.3 * (formal.min(3) as f32 / 3.0)
        - 0.3 * ((casual + contractions).min(3) as f32 / 3.0);
    if emoji_count(&all) > 0 {
        f -= 0.1;
    }
    if all.contains('!') {
        f -= 0.1;
    }
    if lowercase_start(&parts.body) {
        f -= 0.1;
    }
    f.clamp(0.0, 1.0)
}

fn lowercase_start(body: &str) -> bool {
    body.chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_lowercase)
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Shares of each pattern, most common first (ties by text). Messages without one count in the
/// denominator, so shares need not sum to 1.
fn shares(patterns: impl Iterator<Item = Option<String>>, n: usize) -> Vec<(String, f32)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in patterns.flatten() {
        *counts.entry(p).or_default() += 1;
    }
    let mut v: Vec<(String, f32)> = counts
        .into_iter()
        .map(|(k, c)| (k, c as f32 / n.max(1) as f32))
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Measured habits of a set of messages (one message gives a single-sample profile).
pub fn stats<'a>(messages: impl IntoIterator<Item = &'a str>) -> StyleStats {
    let parts: Vec<Parts> = messages.into_iter().map(split).collect();
    let n = parts.len();
    if n == 0 {
        return StyleStats::default();
    }
    let mean = |f: &dyn Fn(&Parts) -> f32| parts.iter().map(f).sum::<f32>() / n as f32;
    StyleStats {
        samples: n as u32,
        median_words: median(
            parts
                .iter()
                .map(|p| p.body.split_whitespace().count() as f32)
                .collect(),
        ),
        greetings: shares(parts.iter().map(|p| p.greeting.clone()), n),
        signoffs: shares(parts.iter().map(|p| p.signoff.clone()), n),
        formality: mean(&formality),
        emoji_per_message: mean(&|p| emoji_count(&p.body) as f32),
        exclamations_per_message: mean(&|p| p.body.matches('!').count() as f32),
        lowercase_start_rate: mean(&|p| f32::from(u8::from(lowercase_start(&p.body)))),
        languages: vec![("en".to_owned(), 1.0)],
    }
}

/// The dominant pattern (share at least one half), if any.
pub fn dominant(patterns: &[(String, f32)]) -> Option<&str> {
    patterns
        .first()
        .filter(|(_, share)| *share >= 0.5)
        .map(|(p, _)| p.as_str())
}

/// Compare greetings and sign-offs by their first word (`hi`, `best`, `-`), or `none`.
fn pattern_key(p: Option<&str>) -> String {
    match p {
        None => "none".to_owned(),
        Some(p) if p.trim_start().starts_with(['-', '–']) => "-".to_owned(),
        Some(p) => first_word(p),
    }
}

/// Feature weights of [`distance`]; they sum to 1.
pub const WEIGHTS: [(&str, f32); 7] = [
    ("length", 0.20),
    ("greeting", 0.20),
    ("signoff", 0.20),
    ("formality", 0.15),
    ("emoji", 0.10),
    ("exclamations", 0.05),
    ("lowercase_start", 0.10),
];

/// Stylometric distance in `[0, 1]` between two profiles (0 = same habits). Greetings and
/// sign-offs compare the dominant pattern's first word; length compares median words on a log
/// scale (a 4× difference is the maximum); emoji compares presence; exclamations saturate at 3
/// per message.
///
/// When `nugget_voice::style_distance` lands (M3-D), the suite switches to it.
pub fn distance(a: &StyleStats, b: &StyleStats) -> f32 {
    let len = ((a.median_words + 1.0) / (b.median_words + 1.0)).ln().abs() / 4f32.ln();
    let greet = f32::from(u8::from(
        pattern_key(dominant(&a.greetings)) != pattern_key(dominant(&b.greetings)),
    ));
    let sign = f32::from(u8::from(
        pattern_key(dominant(&a.signoffs)) != pattern_key(dominant(&b.signoffs)),
    ));
    let form = (a.formality - b.formality).abs();
    let emoji = (a.emoji_per_message.min(1.0) - b.emoji_per_message.min(1.0)).abs();
    let excl = (a.exclamations_per_message - b.exclamations_per_message)
        .abs()
        .min(3.0)
        / 3.0;
    let lower = (a.lowercase_start_rate - b.lowercase_start_rate).abs();
    let d = [len.min(1.0), greet, sign, form, emoji, excl, lower];
    WEIGHTS
        .iter()
        .zip(d)
        .map(|((_, w), x)| w * x)
        .sum::<f32>()
        .clamp(0.0, 1.0)
}

/// `1 - distance`: how well `draft` matches the habits of `target`.
pub fn voice_match(draft: &str, target: &StyleStats) -> f32 {
    1.0 - distance(&stats([draft]), target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_greeting_body_and_signoff() {
        let p = split("Hi Priya,\n\nThe price is $48,000.\nThanks!\n\nBest,\nMaya");
        assert_eq!(p.greeting.as_deref(), Some("Hi {name},"));
        assert_eq!(p.signoff.as_deref(), Some("Best,\nMaya"));
        assert_eq!(p.body, "The price is $48,000. Thanks!");

        let chat = split("nov 18 now! 🚀");
        assert_eq!(chat.greeting, None);
        assert_eq!(chat.signoff, None);
        assert_eq!(chat.body, "nov 18 now! 🚀");

        let h = split("Hello Tom,\nNoted.\nRegards, Henrik");
        assert_eq!(h.greeting.as_deref(), Some("Hello {name},"));
        assert_eq!(h.signoff.as_deref(), Some("Regards, Henrik"));
        assert_eq!(h.body, "Noted.");
    }

    #[test]
    fn emoji_and_formality() {
        assert_eq!(emoji_count("great :) 🎉🙌"), 3);
        assert_eq!(emoji_list("ok 🎉 :)"), ["🎉", ":)"]);
        let formal = formality(&split(
            "Dear Lena,\n\nI would be grateful if you could please confirm.\n\nKind regards,\nHenrik",
        ));
        let casual = formality(&split("omg yes, can't wait!! 🎉"));
        assert!(formal > 0.75, "{formal}");
        assert!(casual < 0.2, "{casual}");
    }

    #[test]
    fn distance_is_zero_for_same_habits_and_grows_with_differences() {
        let a = stats(["Hi Tom,\n\nThe launch is on November 18.\n\nBest,\nMaya"]);
        let b = stats(["Hi Sam,\n\nThe offsite is in Porto.\n\nBest,\nMaya"]);
        assert!(distance(&a, &b) < 0.05);
        let c = stats(["porto! 🎉"]);
        assert!(distance(&a, &c) > 0.5);
        assert!((distance(&a, &c) - distance(&c, &a)).abs() < 1e-6);
        let total: f32 = WEIGHTS.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-6);
    }

    #[test]
    fn profile_shares_and_dominant_patterns() {
        let s = stats([
            "Hi A,\n\nOne.\n\nBest,\nMaya",
            "Hi B,\n\nTwo two.\n\nThanks,\nMaya",
            "Hi C,\n\nThree three three.\n\nBest,\nMaya",
            "sure",
        ]);
        assert_eq!(s.samples, 4);
        assert_eq!(s.greetings[0], ("Hi {name},".to_owned(), 0.75));
        assert_eq!(dominant(&s.greetings), Some("Hi {name},"));
        assert_eq!(dominant(&s.signoffs), Some("Best,\nMaya"));
        assert_eq!(s.median_words, 1.5);
        assert_eq!(s.lowercase_start_rate, 0.25);
        assert_eq!(stats(std::iter::empty::<&str>()), StyleStats::default());
    }
}
