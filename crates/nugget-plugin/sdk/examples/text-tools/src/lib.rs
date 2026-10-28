//! Example Nugget plugin: read-only agent tools that need no capabilities at all.
//!
//! - `text_stats`: word, character, sentence and paragraph counts, plus an estimated reading time.
//! - `top_words`: the most frequent words of a text, ignoring case.
//!
//! Everything is computed inside the component, so the manifest declares no hosts, secrets or
//! key-value store. Note the `BTreeMap`: `HashMap::new` needs a random seed, which plugins do not
//! have.

use std::collections::BTreeMap;

use nugget_plugin_sdk::{Error, Result, Tool, Tools, Value, export_plugin, json};

/// Largest text accepted, in characters. The host also caps tool input at its own limit.
const MAX_CHARS: usize = 200_000;
/// Default reading speed, in words per minute.
const DEFAULT_WPM: u64 = 230;

struct TextTools;

fn text(input: &Value) -> Result<&str> {
    let text = input["text"]
        .as_str()
        .ok_or_else(|| Error::invalid("input.text is required"))?;
    if text.chars().count() > MAX_CHARS {
        return Err(Error::invalid(format!(
            "input.text is longer than {MAX_CHARS} characters"
        )));
    }
    Ok(text)
}

/// Words: runs of alphanumeric characters, with inner apostrophes and hyphens kept.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '’' || c == '-'))
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
}

/// Sentences: runs of text ending in `.`, `!` or `?` (or the end of the text) that contain at
/// least one alphanumeric character.
fn sentence_count(text: &str) -> usize {
    text.split(['.', '!', '?'])
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .count()
}

fn paragraph_count(text: &str) -> usize {
    let mut count = 0;
    let mut in_paragraph = false;
    for line in text.lines() {
        let blank = line.trim().is_empty();
        if !blank && !in_paragraph {
            count += 1;
        }
        in_paragraph = !blank;
    }
    count
}

fn text_stats(input: &Value) -> Result<Value> {
    let text = text(input)?;
    let wpm = input["words_per_minute"]
        .as_u64()
        .unwrap_or(DEFAULT_WPM)
        .clamp(50, 1000);
    let words = words(text).count() as u64;
    let seconds = (words * 60).div_ceil(wpm);
    Ok(json!({
        "words": words,
        "characters": text.chars().count(),
        "characters_no_spaces": text.chars().filter(|c| !c.is_whitespace()).count(),
        "sentences": sentence_count(text),
        "paragraphs": paragraph_count(text),
        "reading_time_seconds": seconds,
    }))
}

fn top_words(input: &Value) -> Result<Value> {
    let text = text(input)?;
    let limit = input["limit"].as_u64().unwrap_or(10).clamp(1, 100) as usize;
    let min_length = input["min_length"].as_u64().unwrap_or(1).clamp(1, 50) as usize;
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for w in words(text).filter(|w| w.chars().count() >= min_length) {
        *counts.entry(w.to_lowercase()).or_default() += 1;
    }
    let mut ranked: Vec<(String, u64)> = counts.into_iter().collect();
    // Most frequent first; ties in alphabetical order (the map is already sorted by word).
    ranked.sort_by_key(|w| std::cmp::Reverse(w.1));
    let words: Vec<Value> = ranked
        .into_iter()
        .take(limit)
        .map(|(word, count)| json!({ "word": word, "count": count }))
        .collect();
    Ok(json!({ "words": words }))
}

impl Tools for TextTools {
    fn tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "text_stats",
                "Count the words, characters, sentences and paragraphs of a text and estimate its reading time.",
                json!({
                    "type": "object",
                    "required": ["text"],
                    "properties": {
                        "text": { "type": "string" },
                        "words_per_minute": { "type": "integer", "minimum": 50, "maximum": 1000 }
                    }
                }),
            ),
            Tool::new(
                "top_words",
                "List the most frequent words of a text, ignoring case.",
                json!({
                    "type": "object",
                    "required": ["text"],
                    "properties": {
                        "text": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                        "min_length": { "type": "integer", "minimum": 1, "maximum": 50 }
                    }
                }),
            ),
        ]
    }

    fn call(name: &str, input: Value) -> Result<Value> {
        match name {
            "text_stats" => text_stats(&input),
            "top_words" => top_words(&input),
            _ => Err(Error::NotFound(format!("no tool `{name}`"))),
        }
    }
}

export_plugin!(tools: TextTools);
