//! Per-message text features: cleaning, greeting and sign-off extraction, formality, emoji.

use std::collections::HashMap;

/// Placeholder for the recipient's first name in normalised greetings.
pub const FIRST_NAME: &str = "{first_name}";
/// Placeholder for a surname after an honorific (`Dear Ms. {last_name},`).
pub const LAST_NAME: &str = "{last_name}";
/// Placeholder for the user's own name in normalised sign-offs.
pub const MY_NAME: &str = "{my_name}";

const GREETINGS: &[&str] = &[
    "good morning",
    "good afternoon",
    "good evening",
    "hi",
    "hey",
    "heya",
    "hiya",
    "hello",
    "hallo",
    "dear",
    "yo",
    "greetings",
    "howdy",
    "morning",
    "afternoon",
    "evening",
    "hola",
    "bonjour",
    "salut",
    "ciao",
    "hej",
];

/// Words that address a group rather than a person: kept as written.
const GROUP_WORDS: &[&str] = &[
    "all",
    "team",
    "everyone",
    "everybody",
    "folks",
    "guys",
    "there",
    "y'all",
    "yall",
    "both",
    "friends",
    "again",
    "you",
    "sir",
    "madam",
    "colleagues",
    "people",
    "gang",
    "crew",
];

const HONORIFICS: &[&str] = &[
    "mr",
    "mrs",
    "ms",
    "miss",
    "dr",
    "prof",
    "professor",
    "sir",
    "madam",
    "mx",
];

const CLOSINGS: &[&str] = &[
    "thanks",
    "thank you",
    "thx",
    "ty",
    "tks",
    "many thanks",
    "best",
    "best regards",
    "best wishes",
    "kind regards",
    "warm regards",
    "warmest regards",
    "regards",
    "rgds",
    "br",
    "cheers",
    "all the best",
    "talk soon",
    "speak soon",
    "talk later",
    "sincerely",
    "yours",
    "yours sincerely",
    "yours truly",
    "love",
    "lots of love",
    "xo",
    "xx",
    "ttyl",
    "take care",
    "much appreciated",
    "appreciate it",
    "warmly",
    "respectfully",
    "see you",
    "see ya",
    "later",
    "ta",
    "have a good",
    "have a great",
    "gruß",
    "grüße",
    "viele grüße",
    "merci",
    "saludos",
    "bises",
    "a+",
];

const SLANG: &[&str] = &[
    "lol", "lmao", "haha", "hahaha", "hehe", "gonna", "wanna", "gotta", "yeah", "yep", "yup",
    "nope", "nah", "btw", "cool", "awesome", "thx", "pls", "plz", "ya", "kinda", "sorta", "omg",
    "tbh", "imo", "imho", "lmk", "np", "ok", "okay", "k", "yo", "dude", "gotcha", "ty", "u", "ur",
    "cuz", "coz", "sup", "ttyl", "fyi", "asap", "ugh", "wow", "hey", "stuff", "super", "totally",
];

const FORMAL: &[&str] = &[
    "dear",
    "sincerely",
    "regards",
    "kindly",
    "furthermore",
    "however",
    "therefore",
    "moreover",
    "regarding",
    "accordingly",
    "pursuant",
    "hereby",
    "appreciate",
    "appreciated",
    "please",
    "respectfully",
    "attached",
    "enclosed",
    "mr",
    "mrs",
    "ms",
    "dr",
    "would",
    "shall",
    "request",
    "inform",
    "assistance",
    "convenience",
    "consideration",
    "apologies",
];

const CONTRACTION_SUFFIXES: &[&str] = &["n't", "'re", "'ll", "'ve", "'d", "'m"];
const CONTRACTION_WORDS: &[&str] = &[
    "it's", "that's", "what's", "there's", "here's", "let's", "he's", "she's", "who's", "how's",
    "where's",
];

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "your", "with", "this", "that", "have", "has",
    "had", "was", "were", "will", "would", "can", "could", "should", "from", "they", "them",
    "their", "there", "what", "when", "where", "which", "who", "why", "how", "about", "just",
    "into", "than", "then", "also", "been", "being", "our", "out", "all", "any", "some", "more",
    "most", "very", "too", "its", "it's", "i'm", "we're", "you're", "don't", "can't", "won't",
    "did", "does", "doing", "get", "got", "let", "know", "think", "thanks", "thank", "hi", "hey",
    "hello", "dear", "best", "regards", "cheers", "please", "wrote", "sent", "one", "yes", "okay",
    "sure", "like", "here", "over", "only", "such", "these", "those", "she", "him", "her", "his",
    "hers", "ours", "mine", "may", "might", "must", "shall", "each",
];

/// Remove quoted replies, attribution lines, forwarded originals, `-- ` signatures and mobile
/// signatures that survived extraction. `signature` (from extraction metadata) is removed when
/// the text ends with it.
pub(crate) fn clean_body(text: &str, signature: Option<&str>) -> String {
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with('>') {
            continue;
        }
        let lower = t.to_lowercase();
        let has_content = kept.iter().any(|l| !l.trim().is_empty());
        if has_content
            && ((t.ends_with("wrote:") || t.ends_with("writes:")) && t.len() < 400
                || (t.starts_with("--") && lower.contains("original message"))
                || (t.starts_with("--") && lower.contains("forwarded message"))
                || (t.len() >= 20
                    && t.chars().all(|c| c == '_')
                    && lines
                        .get(i + 1)
                        .is_some_and(|n| n.trim_start().starts_with("From:"))))
        {
            break;
        }
        if *line == "-- " || *line == "--" {
            break;
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|l| {
        let t = l.trim();
        t.is_empty() || is_mobile_signature(t)
    }) {
        kept.pop();
    }
    let mut out = collapse_blank_lines(&kept);
    if let Some(sig) = signature.map(str::trim).filter(|s| !s.is_empty())
        && let Some(head) = out.strip_suffix(sig)
    {
        out = head.trim_end().to_owned();
    }
    out
}

fn is_mobile_signature(t: &str) -> bool {
    t.len() < 80
        && (t.starts_with("Sent from my ")
            || t.starts_with("Get Outlook for ")
            || t.starts_with("Sent from Mail for ")
            || t.starts_with("Sent via "))
}

fn collapse_blank_lines(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut blank = false;
    for l in lines {
        let l = l.trim_end();
        if l.trim().is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push_str("\n\n");
        } else if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(l);
        blank = false;
    }
    out
}

fn non_empty_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

fn starts_with_word(lower: &str, phrase: &str) -> bool {
    lower.strip_prefix(phrase).is_some_and(|rest| {
        rest.chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '\'')
    })
}

fn is_nameish(word: &str) -> bool {
    let core = word.trim_matches(|c: char| !c.is_alphabetic());
    !core.is_empty()
        && core
            .chars()
            .all(|c| c.is_alphabetic() || c == '-' || c == '\'' || c == '’')
}

/// The greeting of a message (first line), normalised: names become [`FIRST_NAME`] (or
/// [`LAST_NAME`] after an honorific). `recipient_names` are lowercase first names that are
/// replaced even when written in lowercase. `None` when the message has no greeting.
pub fn normalize_greeting(text: &str, recipient_names: &[String]) -> Option<String> {
    let first = *non_empty_lines(text).first()?;
    let lower = first.to_lowercase();
    let greeting = GREETINGS.iter().find(|g| starts_with_word(&lower, g))?;
    // The greeting segment: up to the first `,`, `!`, `:`, `.` or dash, when that comes within
    // the first few words; otherwise the greeting word plus following names.
    let cut = first
        .char_indices()
        .find(|&(i, c)| {
            matches!(c, ',' | '!' | ':' | '—' | '–')
                || (c == '.'
                    && !is_abbreviation(
                        first[..i].rsplit(char::is_whitespace).next().unwrap_or(""),
                    ))
        })
        .map(|(i, c)| i + c.len_utf8());
    let segment = match cut {
        Some(end) if first[..end].split_whitespace().count() <= 6 => &first[..end],
        _ => {
            // Greeting word(s), then capitalised / known names.
            let words: Vec<&str> = first.split_whitespace().collect();
            let gw = greeting.split_whitespace().count();
            let mut n = gw.min(words.len());
            while n < words.len() && n < gw + 3 {
                let w = words[n];
                let wl = w.to_lowercase();
                let known = recipient_names.contains(&wl);
                let cap = w.chars().next().is_some_and(char::is_uppercase);
                if is_nameish(w) && (known || cap) && !GROUP_WORDS.contains(&wl.as_str()) {
                    n += 1;
                } else {
                    break;
                }
            }
            let idx = word_end(first, n);
            &first[..idx]
        }
    };
    Some(replace_names(segment, greeting, recipient_names))
}

/// Byte offset just after the `n`-th whitespace-separated word of `s` (`s.len()` if fewer).
fn word_end(s: &str, n: usize) -> usize {
    let mut count = 0;
    let mut in_word = false;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if in_word {
                count += 1;
                if count == n {
                    return i;
                }
            }
            in_word = false;
        } else {
            in_word = true;
        }
    }
    s.len()
}

fn replace_names(segment: &str, greeting: &str, recipient_names: &[String]) -> String {
    let greeting_words: Vec<&str> = greeting.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    let mut prev_honorific = false;
    let mut prev_placeholder = false;
    for (i, raw) in segment.split_whitespace().enumerate() {
        // Separate trailing punctuation.
        let core_end = raw
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_alphanumeric())
            .map_or(0, |(i, c)| i + c.len_utf8());
        let (core, tail) = raw.split_at(core_end);
        let lower = core.to_lowercase();
        let is_greeting_word = i < greeting_words.len();
        let honorific = HONORIFICS.contains(&lower.trim_end_matches('.'));
        let known = recipient_names.contains(&lower);
        let cap = core.chars().next().is_some_and(char::is_uppercase);
        let name = !is_greeting_word
            && !honorific
            && is_nameish(core)
            && core != "I"
            && (known || cap)
            && !GROUP_WORDS.contains(&lower.as_str());
        if name {
            let ph = if prev_honorific {
                LAST_NAME
            } else {
                FIRST_NAME
            };
            if prev_placeholder {
                // "Sam Lee" is one name.
                if let Some(last) = out.last_mut() {
                    *last = format!("{ph}{tail}");
                }
            } else {
                out.push(format!("{ph}{tail}"));
            }
            prev_placeholder = tail.is_empty();
            prev_honorific = false;
        } else {
            out.push(raw.to_owned());
            prev_placeholder = false;
            prev_honorific = honorific;
        }
    }
    out.join(" ")
}

fn is_closing(line: &str) -> bool {
    let lower = line.to_lowercase();
    let lower = lower.trim_start_matches(['-', '–', '—', '~', ' ']);
    line.split_whitespace().count() <= 6
        && line.chars().count() <= 50
        && CLOSINGS.iter().any(|c| starts_with_word(lower, c))
}

fn name_line(line: &str, my_names: &[String]) -> Option<String> {
    let prefix: String = line
        .chars()
        .take_while(|c| matches!(c, '-' | '–' | '—' | '~' | ' '))
        .collect();
    let rest = line[prefix.len()..].trim();
    let core = rest.trim_end_matches(['.', '!', ',']).trim();
    if core.is_empty() || core.chars().count() > 30 {
        return None;
    }
    let lower = core.to_lowercase();
    if my_names.contains(&lower) {
        return Some(format!("{prefix}{MY_NAME}"));
    }
    None
}

/// The sign-off of a message (its last line or two), normalised: the user's name becomes
/// [`MY_NAME`]. `my_names` are lowercase name variants of the user (full, first, initials);
/// when empty, a single capitalised word after a closing line counts as the name.
pub fn normalize_signoff(text: &str, my_names: &[String]) -> Option<String> {
    let lines = non_empty_lines(text);
    if lines.len() < 2 {
        // A one-line message has no separate sign-off.
        return None;
    }
    let last = *lines.last()?;
    let prev = lines[lines.len() - 2];
    let name = name_line(last, my_names).or_else(|| {
        // Unknown user: "Cheers,\nAlex" (a capitalised single word after a closing).
        let w: Vec<&str> = last.split_whitespace().collect();
        (w.len() == 1
            && is_closing(prev)
            && last.chars().next().is_some_and(char::is_uppercase)
            && is_nameish(w[0])
            && last.chars().count() <= 15
            && !is_closing(last))
        .then(|| MY_NAME.to_owned())
    });
    match name {
        Some(n) if is_closing(prev) && lines.len() >= 3 => Some(format!("{prev}\n{n}")),
        Some(n) if is_closing(prev) => Some(format!("{prev}\n{n}")),
        Some(n) => Some(n),
        None if is_closing(last) => Some(last.to_owned()),
        None => None,
    }
}

/// A canonical key for comparing greetings and sign-offs: lowercase, without punctuation and
/// extra whitespace.
pub(crate) fn canonical(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| c.is_whitespace())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '{' && c != '}' && c != '_'))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn words(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect()
}

pub(crate) fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF   // emoticons, symbols, pictographs, flags, ...
        | 0x2600..=0x27BF   // misc symbols, dingbats
        | 0x2B50 | 0x2B55 | 0x203C | 0x2049)
}

const EMOTICONS: &[&str] = &[
    ":)", ":-)", ":(", ":-(", ":D", ":-D", ";)", ";-)", ":P", ":-P", ":p", "<3", "xD", "XD", ":/",
];

pub(crate) fn emoji_count(text: &str) -> u32 {
    let unicode = text.chars().filter(|c| is_emoji(*c)).count();
    let emoticons = text
        .split_whitespace()
        .filter(|w| {
            let w = w.trim_end_matches([',', '.']);
            EMOTICONS.contains(&w)
        })
        .count();
    (unicode + emoticons) as u32
}

/// Runs of `!` (so `!!!` counts once).
pub(crate) fn exclamation_count(text: &str) -> u32 {
    let mut n = 0;
    let mut prev = false;
    for c in text.chars() {
        let bang = c == '!' || c == '！';
        if bang && !prev {
            n += 1;
        }
        prev = bang;
    }
    n
}

/// Whether the first letter of the message is lowercase.
pub(crate) fn lowercase_start(text: &str) -> bool {
    text.chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_lowercase)
}

const ABBREVIATIONS: &[&str] = &["e.g", "i.e", "etc", "vs", "approx", "no", "st", "jr", "sr"];

fn is_abbreviation(word: &str) -> bool {
    let w = word
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches('.')
        .to_lowercase();
    HONORIFICS.contains(&w.as_str()) || ABBREVIATIONS.contains(&w.as_str())
}

/// Words per sentence. Sentences end at `.`, `!`, `?` (not after an abbreviation such as
/// `Dr.`) and at line breaks.
fn sentence_lengths(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut n = 0;
        for w in line.split_whitespace() {
            if w.chars().any(char::is_alphanumeric) {
                n += 1;
            }
            let ends = w.ends_with(['!', '?']) || (w.ends_with('.') && !is_abbreviation(w));
            if ends && n > 0 {
                out.push(n);
                n = 0;
            }
        }
        if n > 0 {
            out.push(n);
        }
    }
    out
}

/// Heuristic formality in `[0, 1]` (0 casual, 1 formal): contractions, slang, emoji,
/// exclamations and a lowercase start lower it; honorifics, formal vocabulary and longer
/// sentences raise it.
pub(crate) fn formality(text: &str) -> f32 {
    let ws = words(text);
    if ws.is_empty() {
        return 0.5;
    }
    let n = ws.len() as f32;
    let mut contractions = 0usize;
    let mut slang = 0usize;
    let mut formal = 0usize;
    for w in &ws {
        let lower = w
            .to_lowercase()
            .replace('’', "'")
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
            .to_owned();
        if CONTRACTION_SUFFIXES.iter().any(|s| lower.ends_with(s))
            || CONTRACTION_WORDS.contains(&lower.as_str())
        {
            contractions += 1;
        }
        if SLANG.contains(&lower.as_str()) {
            slang += 1;
        }
        if FORMAL.contains(&lower.trim_end_matches('.')) {
            formal += 1;
        }
    }
    let sentences = sentence_lengths(text);
    let avg_sentence = if sentences.is_empty() {
        n
    } else {
        sentences.iter().sum::<usize>() as f32 / sentences.len() as f32
    };
    let mut score = 0.5;
    score -= (contractions as f32 / n * 4.0).min(0.25);
    score -= (slang as f32 * 0.08).min(0.25);
    score += (formal as f32 * 0.06).min(0.3);
    score += ((avg_sentence - 10.0) / 30.0).clamp(-0.15, 0.2);
    if lowercase_start(text) {
        score -= 0.1;
    }
    if emoji_count(text) > 0 {
        score -= 0.1;
    }
    if exclamation_count(text) > 0 {
        score -= 0.05;
    }
    score.clamp(0.0, 1.0)
}

/// ISO 639-3 code of the text's language (`eng`, `deu`, ...), when detection is confident.
pub(crate) fn language(text: &str) -> Option<String> {
    if words(text).len() < 4 {
        return None;
    }
    let info = whatlang::detect(text)?;
    (info.is_reliable() || info.confidence() >= 0.5).then(|| info.lang().code().to_owned())
}

/// Lowercase content terms (no stopwords, at least 3 characters) with their counts.
pub(crate) fn terms(text: &str) -> HashMap<String, f32> {
    let mut out: HashMap<String, f32> = HashMap::new();
    for t in text.split(|c: char| !c.is_alphanumeric()) {
        if t.chars().count() < 3 {
            continue;
        }
        let t = t.to_lowercase();
        if STOPWORDS.contains(&t.as_str()) || t.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        *out.entry(t).or_default() += 1.0;
    }
    out
}

pub(crate) fn cosine(a: &HashMap<String, f32>, b: &HashMap<String, f32>) -> f32 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let dot: f32 = small
        .iter()
        .filter_map(|(k, v)| large.get(k).map(|w| v * w))
        .sum();
    let na: f32 = a.values().map(|v| v * v).sum::<f32>().sqrt();
    let nb: f32 = b.values().map(|v| v * v).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Up to `max` distinctive terms of `text` for a full-text query, favouring the end of the
/// text (the most recent messages of a thread) and repeated terms.
pub(crate) fn query_terms(text: &str, max: usize) -> Vec<String> {
    let tail: String = {
        let chars: Vec<char> = text.chars().collect();
        let start = chars.len().saturating_sub(4000);
        chars[start..].iter().collect()
    };
    let counts = terms(&tail);
    let mut ranked: Vec<(String, f32)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.0.len().cmp(&a.0.len()))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.into_iter().take(max).map(|(t, _)| t).collect()
}

/// Cut `text` to at most `max` characters, at a sentence or word boundary, with `…`.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut = text
        .char_indices()
        .nth(max.saturating_sub(1))
        .map_or(text.len(), |(i, _)| i);
    let head = &text[..cut];
    let sentence_end = head
        .rfind(['.', '!', '?', '\n'])
        .filter(|&i| i > cut / 2)
        .map(|i| i + 1);
    match sentence_end {
        Some(end) => format!("{} …", head[..end].trim_end()),
        None => {
            let h = head
                .rsplit_once(char::is_whitespace)
                .map_or(head, |(h, _)| h);
            format!("{}…", h.trim_end())
        }
    }
}

/// All per-message features.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Features {
    pub words: u32,
    pub greeting: Option<String>,
    pub signoff: Option<String>,
    pub formality: f32,
    pub emoji: u32,
    pub exclamations: u32,
    pub lowercase_start: bool,
    pub language: Option<String>,
}

impl Features {
    pub fn of(text: &str, recipient_names: &[String], my_names: &[String]) -> Self {
        Self {
            words: words(text).len() as u32,
            greeting: normalize_greeting(text, recipient_names),
            signoff: normalize_signoff(text, my_names),
            formality: formality(text),
            emoji: emoji_count(text),
            exclamations: exclamation_count(text),
            lowercase_start: lowercase_start(text),
            language: language(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greetings_are_normalised() {
        let none: Vec<String> = Vec::new();
        assert_eq!(
            normalize_greeting("Hi Sam,\n\nSounds good.", &none).as_deref(),
            Some("Hi {first_name},")
        );
        assert_eq!(
            normalize_greeting("hey sam - quick one", &["sam".into()]).as_deref(),
            Some("hey {first_name}")
        );
        assert_eq!(
            normalize_greeting("Hey Sam Lee, all good", &none).as_deref(),
            Some("Hey {first_name},")
        );
        assert_eq!(
            normalize_greeting("Dear Dr. Okafor,\nPlease find...", &none).as_deref(),
            Some("Dear Dr. {last_name},")
        );
        assert_eq!(
            normalize_greeting("Hi team,\nupdate below", &none).as_deref(),
            Some("Hi team,")
        );
        assert_eq!(
            normalize_greeting("Good morning Priya!", &none).as_deref(),
            Some("Good morning {first_name}!")
        );
        assert_eq!(normalize_greeting("Sounds good to me.", &none), None);
        assert_eq!(
            normalize_greeting("Hi Sam and Jo, see below", &none).as_deref(),
            Some("Hi {first_name} and {first_name},")
        );
    }

    #[test]
    fn signoffs_are_normalised() {
        let me = vec![
            "alex morgan".to_string(),
            "alex".into(),
            "am".into(),
            "a".into(),
        ];
        assert_eq!(
            normalize_signoff("Hi,\nSure thing.\n\nCheers,\nAlex", &me).as_deref(),
            Some("Cheers,\n{my_name}")
        );
        assert_eq!(
            normalize_signoff("Sure thing.\n- A", &me).as_deref(),
            Some("- {my_name}")
        );
        assert_eq!(
            normalize_signoff("Sure thing.\nThanks!", &me).as_deref(),
            Some("Thanks!")
        );
        assert_eq!(normalize_signoff("Sure thing.\nSee the doc.", &me), None);
        // Unknown user: a capitalised word after a closing is the name.
        assert_eq!(
            normalize_signoff("Ok.\nBest,\nJordan", &[]).as_deref(),
            Some("Best,\n{my_name}")
        );
    }

    #[test]
    fn cleans_leftover_quotes_and_signatures() {
        let body = "Works for me.\n\nOn Tue, Sam wrote:\n> can we move it?\n> thanks";
        assert_eq!(clean_body(body, None), "Works for me.");
        let body = "Works for me.\n-- \nAlex Morgan\nCEO";
        assert_eq!(clean_body(body, None), "Works for me.");
        let body = "Works for me.\n\nSent from my iPhone";
        assert_eq!(clean_body(body, None), "Works for me.");
        let body = "Works for me.\nAlex Morgan | CEO";
        assert_eq!(clean_body(body, Some("Alex Morgan | CEO")), "Works for me.");
    }

    #[test]
    fn formality_orders_registers() {
        let casual = "hey!! yeah that's gonna work lol, lmk 😀";
        let formal = "Dear Dr. Okafor, I would appreciate your consideration regarding the \
                      attached proposal. Kindly let me know at your convenience.";
        assert!(formality(casual) < 0.3, "{}", formality(casual));
        assert!(formality(formal) > 0.7, "{}", formality(formal));
    }

    #[test]
    fn counts_emoji_and_exclamations() {
        assert_eq!(emoji_count("great 🎉🎉 :)"), 3);
        assert_eq!(exclamation_count("wow!!! ok! fine"), 2);
        assert!(lowercase_start("  sure, works"));
        assert!(!lowercase_start("Sure"));
    }

    #[test]
    fn detects_language() {
        assert_eq!(
            language("Thanks for the update, I will review the document tomorrow morning.")
                .as_deref(),
            Some("eng")
        );
        assert_eq!(
            language("Vielen Dank für die Nachricht, ich melde mich morgen früh bei dir.")
                .as_deref(),
            Some("deu")
        );
        assert_eq!(language("ok"), None);
    }

    #[test]
    fn truncates_at_boundaries() {
        let t = "First sentence here. Second sentence is longer and goes on.";
        assert_eq!(truncate(t, 30), "First sentence here. …");
        assert_eq!(truncate("short", 30), "short");
    }
}
