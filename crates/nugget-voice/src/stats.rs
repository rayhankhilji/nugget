//! Stylometric statistics and the style distance used to score drafts.
//!
//! # Definitions (per message, then aggregated over a set of the user's messages)
//! - **`samples`**: number of messages (deduplicated by `Message-ID`).
//! - **`median_words`**: median count of whitespace-separated words containing a letter or
//!   digit, after quoted replies and signatures are removed.
//! - **`greetings`**: the first line's greeting (`Hi`, `Hey`, `Dear`, `Good morning`, ...)
//!   up to its punctuation, with names replaced by `{first_name}` (or `{last_name}` after an
//!   honorific: `Dear Dr. {last_name},`). Each entry is `(greeting, share of messages)`, most
//!   common first; `1 - Σ share` is the share of messages without a greeting.
//! - **`signoffs`**: a closing line (`Thanks`, `Cheers`, `Best`, ...) and/or the user's name
//!   as the last line, with the name replaced by `{my_name}` (`Cheers,\n{my_name}`). Shares as
//!   for greetings.
//! - **`formality`**: mean of a per-message heuristic in `[0, 1]`: starts at 0.5; contractions
//!   (up to −0.25), slang (−0.08 each, up to −0.25), formal vocabulary and honorifics (+0.06
//!   each, up to +0.3), mean sentence length (`(len − 10) / 30`, in `[−0.15, 0.2]`), a
//!   lowercase start (−0.1), any emoji (−0.1), any exclamation (−0.05).
//! - **`emoji_per_message`**: mean count of emoji (Unicode pictographs and common emoticons).
//! - **`exclamations_per_message`**: mean count of `!` runs (`!!!` counts once).
//! - **`lowercase_start_rate`**: share of messages whose first letter is lowercase.
//! - **`languages`**: share of messages per detected language (ISO 639-3, e.g. `eng`), among
//!   messages long enough to detect.

use std::collections::HashMap;

use crate::StyleStats;
use crate::text::{Features, canonical};

/// Most greetings, sign-offs and languages kept per [`StyleStats`].
pub const MAX_LISTED: usize = 8;

/// Aggregate per-message features into [`StyleStats`].
pub(crate) fn aggregate<'a>(features: impl IntoIterator<Item = &'a Features>) -> StyleStats {
    let fs: Vec<&Features> = features.into_iter().collect();
    if fs.is_empty() {
        return StyleStats::default();
    }
    let n = fs.len() as f32;
    let mut words: Vec<u32> = fs.iter().map(|f| f.words).collect();
    words.sort_unstable();
    let median_words = if words.len() % 2 == 1 {
        words[words.len() / 2] as f32
    } else {
        (words[words.len() / 2 - 1] + words[words.len() / 2]) as f32 / 2.0
    };
    let detected = fs.iter().filter(|f| f.language.is_some()).count() as f32;
    StyleStats {
        samples: fs.len() as u32,
        median_words,
        greetings: shares(fs.iter().filter_map(|f| f.greeting.as_deref()), n),
        signoffs: shares(fs.iter().filter_map(|f| f.signoff.as_deref()), n),
        formality: fs.iter().map(|f| f.formality).sum::<f32>() / n,
        emoji_per_message: fs.iter().map(|f| f.emoji as f32).sum::<f32>() / n,
        exclamations_per_message: fs.iter().map(|f| f.exclamations as f32).sum::<f32>() / n,
        lowercase_start_rate: fs.iter().filter(|f| f.lowercase_start).count() as f32 / n,
        languages: if detected > 0.0 {
            shares(fs.iter().filter_map(|f| f.language.map(|l| l.0)), detected)
        } else {
            Vec::new()
        },
    }
}

/// `(value, count / total)`, most frequent first (ties alphabetical), at most [`MAX_LISTED`].
/// Values that differ only in case or punctuation are merged under the most common spelling.
fn shares<'a>(values: impl Iterator<Item = &'a str>, total: f32) -> Vec<(String, f32)> {
    let mut by_key: HashMap<String, HashMap<&'a str, u32>> = HashMap::new();
    for v in values {
        *by_key
            .entry(canonical(v))
            .or_default()
            .entry(v)
            .or_default() += 1;
    }
    let mut out: Vec<(String, u32)> = by_key
        .into_values()
        .map(|spellings| {
            let total: u32 = spellings.values().sum();
            let best = spellings
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(s, _)| s.to_owned())
                .unwrap_or_default();
            (best, total)
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(MAX_LISTED);
    out.into_iter()
        .map(|(v, c)| (v, c as f32 / total.max(1.0)))
        .collect()
}

/// Share of messages in `list` matching `value` (`None` = no greeting / sign-off). An exact
/// canonical match counts fully; one with the same first word counts half.
fn share_of(list: &[(String, f32)], value: Option<&str>) -> f32 {
    let Some(v) = value else {
        return (1.0 - list.iter().map(|(_, s)| s).sum::<f32>()).max(0.0);
    };
    let key = canonical(v);
    if let Some((_, s)) = list.iter().find(|(g, _)| canonical(g) == key) {
        return *s;
    }
    let first = key.split_whitespace().next().unwrap_or_default();
    list.iter()
        .find(|(g, _)| canonical(g).split_whitespace().next() == Some(first))
        .map_or(0.0, |(_, s)| s * 0.5)
}

/// How likely a value is relative to the most common choice, as a distance in `[0, 1]`.
fn choice_distance(list: &[(String, f32)], value: Option<&str>) -> f32 {
    let none = (1.0 - list.iter().map(|(_, s)| s).sum::<f32>()).max(0.0);
    let best = list.iter().map(|(_, s)| *s).fold(none, f32::max);
    if best <= 0.0 {
        return 0.0;
    }
    (1.0 - share_of(list, value) / best).clamp(0.0, 1.0)
}

/// Feature weights in [`style_distance`].
const W_LENGTH: f32 = 1.0;
const W_FORMALITY: f32 = 1.5;
const W_GREETING: f32 = 1.0;
const W_SIGNOFF: f32 = 1.0;
const W_EMOJI: f32 = 0.5;
const W_EXCLAMATION: f32 = 0.5;
const W_LOWERCASE: f32 = 0.75;
const W_LANGUAGE: f32 = 0.5;

/// Distance between a text and a style, in `[0, 1]` (lower is closer). A weighted mean of:
/// - length: `|ln(words + 1) − ln(median + 1)| / ln 4` (capped at 1; 4× longer or shorter is 1),
/// - formality: absolute difference,
/// - greeting and sign-off: `1 − share(text's choice) / share(most common choice)`, where "no
///   greeting" is a choice too,
/// - emoji and exclamations: difference of counts (capped at 3) / 3,
/// - lowercase start: `|start − rate|`,
/// - language: `1 − share` of the text's language (0 when either is unknown).
///
/// Names in the text are recognised heuristically (capitalised words after a greeting, a
/// capitalised word after a closing line). Returns 1 for empty stats.
pub fn style_distance(stats: &StyleStats, text: &str) -> f32 {
    distance_with(stats, text, &[], &[])
}

pub(crate) fn distance_with(
    stats: &StyleStats,
    text: &str,
    recipient_names: &[String],
    my_names: &[String],
) -> f32 {
    if stats.samples == 0 {
        return 1.0;
    }
    let f = Features::of(text.trim(), recipient_names, my_names);
    let length = ((f.words as f32 + 1.0).ln() - (stats.median_words + 1.0).ln()).abs() / 4f32.ln();
    let count = |x: u32, mean: f32| ((x.min(3) as f32) - mean.min(3.0)).abs() / 3.0;
    let language = match &f.language {
        Some(l) if !stats.languages.is_empty() => {
            1.0 - stats
                .languages
                .iter()
                .find(|(x, _)| x == l.0)
                .map_or(0.0, |(_, s)| *s)
        }
        _ => 0.0,
    };
    let parts = [
        (W_LENGTH, length.min(1.0)),
        (W_FORMALITY, (f.formality - stats.formality).abs()),
        (
            W_GREETING,
            choice_distance(&stats.greetings, f.greeting.as_deref()),
        ),
        (
            W_SIGNOFF,
            choice_distance(&stats.signoffs, f.signoff.as_deref()),
        ),
        (W_EMOJI, count(f.emoji, stats.emoji_per_message)),
        (
            W_EXCLAMATION,
            count(f.exclamations, stats.exclamations_per_message),
        ),
        (
            W_LOWERCASE,
            (f32::from(u8::from(f.lowercase_start)) - stats.lowercase_start_rate).abs(),
        ),
        (W_LANGUAGE, language),
    ];
    let total: f32 = parts.iter().map(|(w, _)| w).sum();
    (parts.iter().map(|(w, d)| w * d).sum::<f32>() / total).clamp(0.0, 1.0)
}

/// Voice similarity of `text` to the user's overall style, in `[0, 1]` (higher is closer):
/// `1 − style_distance`. See [`crate::VoiceProfile::similarity_for`] for a channel- and
/// recipient-specific score.
pub fn similarity(profile: &crate::VoiceProfile, text: &str) -> f32 {
    profile.similarity(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feats(texts: &[&str]) -> Vec<Features> {
        texts.iter().map(|t| Features::of(t, &[], &[])).collect()
    }

    #[test]
    fn aggregates_shares_and_median() {
        let f = feats(&[
            "Hi Sam,\nSounds good.\nCheers,\nAlex",
            "Hi Jo,\nWorks for me, see you then.\nCheers,\nAlex",
            "hi Kim, ok\nCheers,\nAlex",
            "Quick note: the deck is attached.",
        ]);
        let s = aggregate(&f);
        assert_eq!(s.samples, 4);
        assert_eq!(s.greetings[0].0, "Hi {first_name},");
        assert!((s.greetings[0].1 - 0.75).abs() < 1e-6, "{:?}", s.greetings);
        assert_eq!(s.signoffs[0], ("Cheers,\n{my_name}".to_string(), 0.75));
        assert!((s.lowercase_start_rate - 0.25).abs() < 1e-6);
    }

    #[test]
    fn empty_stats_are_far() {
        assert_eq!(style_distance(&StyleStats::default(), "hello"), 1.0);
    }

    #[test]
    fn matching_text_is_closer() {
        let casual = feats(&[
            "hey sam! yeah works for me 🙂\nthx",
            "hey jo! sounds great, lmk 🙂\nthx",
            "hey kim! gonna be late lol\nthx",
        ]);
        let s = aggregate(&casual);
        let near = style_distance(&s, "hey pat! yep that's fine 🙂\nthx");
        let far = style_distance(
            &s,
            "Dear Dr. Okafor,\nI would appreciate your consideration regarding the attached \
             proposal at your earliest convenience.\nKind regards,\nAlex Morgan",
        );
        assert!(near < far, "near {near} far {far}");
        assert!(near < 0.25, "{near}");
    }
}
