//! A deterministic in-process answering "model" for CI (`ask` without `--local-models`).
//!
//! It reads the passages from the request's system prompt (lines starting `[n]` open a
//! passage; the lines after it, up to the next such line, are its body), splits them into
//! sentences, and answers with the one sentence sharing the most content words with the
//! question (its passage title counting half), followed by ` [n]`. With fewer than
//! [`MIN_OVERLAP`] shared words, or under [`MIN_SHARE`] of the question's content words, it
//! answers [`nuggetd::ask::NOT_FOUND_REPLY`]. No understanding, no injection handling: it is
//! enough to drive retrieval, the prompt, the streaming filter, the post-check and every metric
//! end to end. The answer streams in small fixed-size deltas, so markers split across deltas.

use std::collections::HashSet;

use async_trait::async_trait;
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Part, Provider, ProviderKind, ProviderMeta, Role,
    StopReason,
};

/// Shared content words a sentence needs to be used.
pub const MIN_OVERLAP: f64 = 2.0;
/// Share of the question's content words it needs.
pub const MIN_SHARE: f64 = 0.4;
/// Characters per streamed delta.
pub const DELTA_CHARS: usize = 5;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "our", "are", "was", "were", "this", "that", "these", "those",
    "you", "your", "what", "when", "which", "who", "how", "why", "where", "can", "could", "would",
    "should", "will", "did", "does", "has", "have", "had", "about", "from", "into", "any", "much",
    "many", "there", "their", "they", "them", "its", "his", "her", "she", "him", "but", "not",
    "all", "now", "new", "get", "got", "going", "been", "being", "than", "then", "also", "just",
    "some", "each", "per", "too", "very", "say", "said", "tell", "me", "my", "mine", "out", "off",
    "over", "under", "after", "before", "next", "last",
];

/// Content words: lowercase alphanumeric runs of three or more characters (or with a digit),
/// stopwords dropped, a trailing `s` removed.
pub fn words(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 || w.chars().any(|c| c.is_ascii_digit()))
        .filter(|w| !w.is_empty() && !STOPWORDS.contains(w))
        .map(|w| {
            if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
                w[..w.len() - 1].to_owned()
            } else {
                w.to_owned()
            }
        })
        .collect()
}

/// `(n, title line, body)` of each passage in a system prompt.
pub fn passages(system: &str) -> Vec<(u32, String, String)> {
    let mut out: Vec<(u32, String, String)> = Vec::new();
    for line in system.lines() {
        let t = line.trim_start();
        let header = t
            .strip_prefix('[')
            .and_then(|r| r.split_once(']'))
            .and_then(|(n, rest)| n.trim().parse::<u32>().ok().map(|n| (n, rest)));
        match header {
            Some((n, rest)) => out.push((n, rest.trim().to_owned(), String::new())),
            None => {
                if let Some(last) = out.last_mut() {
                    last.2.push_str(line);
                    last.2.push('\n');
                }
            }
        }
    }
    out
}

/// The fake's answer to `question` given `system`.
pub fn answer(system: &str, question: &str) -> String {
    let q = words(question);
    if q.is_empty() {
        return nuggetd::ask::NOT_FOUND_REPLY.to_owned();
    }
    let mut best: Option<(f64, String, u32)> = None;
    for (n, title, body) in passages(system) {
        let tw = words(&title);
        let title_score = 0.5 * q.intersection(&tw).count() as f64;
        for s in super::score::sentences(&body) {
            let s = super::score::strip_markers(&s).trim().to_owned();
            let overlap = q.intersection(&words(&s)).count() as f64;
            if overlap == 0.0 {
                continue;
            }
            let total = overlap + title_score;
            if best.as_ref().is_none_or(|(b, _, _)| total > *b) {
                best = Some((total, s, n));
            }
        }
    }
    match best {
        Some((score, s, n)) if score >= MIN_OVERLAP && score / q.len() as f64 >= MIN_SHARE => {
            let s = s.trim_end_matches(['.', '!', '?']);
            format!("{s} [{n}].")
        }
        _ => nuggetd::ask::NOT_FOUND_REPLY.to_owned(),
    }
}

/// The fake as a [`Provider`].
pub struct FakeAnswerer {
    id: ProviderId,
}

impl Default for FakeAnswerer {
    fn default() -> Self {
        Self {
            id: ProviderId::new("fake-ask"),
        }
    }
}

fn text_of(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Provider for FakeAnswerer {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "fake-ask-deterministic".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }

    async fn chat(&self, req: ChatRequest) -> nugget_core::Result<ChatStream> {
        let system = req.system.clone().unwrap_or_default();
        let question = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| text_of(&m.content))
            .unwrap_or_default();
        let text = answer(&system, &question);
        let prompt = format!("{system}\n{question}");
        let chars: Vec<char> = text.chars().collect();
        let mut events: Vec<nugget_core::Result<ChatEvent>> = chars
            .chunks(DELTA_CHARS)
            .map(|c| Ok(ChatEvent::TextDelta(c.iter().collect())))
            .collect();
        events.push(Ok(ChatEvent::Done {
            stop_reason: StopReason::EndTurn,
            input_tokens: Some(nugget_extract::estimate_tokens(&prompt) as u32),
            output_tokens: Some(nugget_extract::estimate_tokens(&text) as u32),
        }));
        Ok(Box::pin(futures::stream::iter(events)))
    }

    async fn embed(&self, _: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
        Err(nugget_core::Error::Unsupported("embed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM: &str = "Answer from the passages.\n\nPassages:\n[1] Car insurance renewal (eval)\n\
        The car insurance with Harbourline expires on 14 November. The renewal quote came in at \
        612 pounds.\n\n[2] Boiler (eval)\nThe boiler showed fault code F28.";

    #[test]
    fn answers_with_the_best_sentence_or_not_found() {
        assert_eq!(
            answer(SYSTEM, "How much was the renewal quote?"),
            "The renewal quote came in at 612 pounds [1]."
        );
        assert_eq!(
            answer(SYSTEM, "What fault code did the boiler show?"),
            "The boiler showed fault code F28 [2]."
        );
        assert_eq!(
            answer(SYSTEM, "What is my sister's phone number?"),
            nuggetd::ask::NOT_FOUND_REPLY
        );
        assert_eq!(
            answer("", "anything at all here"),
            nuggetd::ask::NOT_FOUND_REPLY
        );
    }

    #[test]
    fn streams_in_small_deltas() {
        use futures::StreamExt;
        let req = ChatRequest::new(vec![nugget_infer::Message::user_text(
            "What fault code did the boiler show?",
        )])
        .with_system(SYSTEM);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let events: Vec<_> = rt.block_on(async {
            FakeAnswerer::default()
                .chat(req)
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
        });
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                Ok(ChatEvent::TextDelta(d)) => Some(d.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "The boiler showed fault code F28 [2].");
        assert!(events.len() > 3);
        assert!(matches!(
            events.last(),
            Some(Ok(ChatEvent::Done {
                input_tokens: Some(_),
                ..
            }))
        ));
    }
}
