//! Ask, don't guess: spot a goal that names a target the screen shows more than once.
//!
//! [`ambiguity`] matches the words of the user's goal against the items a tool saw on screen
//! ([`ToolResult::targets`](crate::ToolResult)). When the best-matching items are two or more
//! different things ("Reply to Sam's email" with Sam Lee and Sam Ortiz in the inbox), the loop
//! asks the user which one before it acts ([`AgentConfig::clarify`](crate::AgentConfig)). Pure
//! and deterministic: no model call. Target labels are untrusted screen text; they only ever
//! reach the user, cleaned and cut, never the model.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::tool::truncate;

/// Most candidates offered to the user.
pub const MAX_CANDIDATES: usize = 6;

/// Longest candidate label, in characters.
pub const CANDIDATE_MAX: usize = 80;

/// A goal word (or words) that fits more than one item on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ambiguity {
    /// The matched goal word(s), as the user wrote them (e.g. `Sam`, `Q3 report`).
    pub term: String,
    /// The distinct items it fits, in screen order, cleaned and cut ([`MAX_CANDIDATES`] at most).
    pub candidates: Vec<String>,
}

impl Ambiguity {
    /// The question put to the user: fixed text around the term and the candidates.
    pub fn question(&self) -> String {
        format!(
            "Your request says “{}”, and I see more than one: {}. Which one did you mean?",
            clean(&self.term, CANDIDATE_MAX),
            self.candidates.join(", ")
        )
    }
}

/// Words that say nothing about which item is meant: function words and the generic nouns and
/// verbs of everyday tasks.
#[rustfmt::skip]
const STOP_WORDS: &[&str] = &[
    // Function words.
    "the", "an", "my", "me", "you", "your", "our", "we", "us", "to", "and", "or", "of", "for",
    "in", "on", "at", "by", "from", "with", "about", "into", "onto", "it", "its", "this", "that",
    "is", "are", "be", "was", "were", "please", "can", "could", "would", "will", "just", "now",
    "then", "up", "out", "off", "so", "if", "as", "do", "does", "did", "him", "her", "them",
    "his", "hers", "their", "he", "she", "they", "yes", "no", "ok", "okay", "thanks", "thank",
    "hi", "hello", "one", "again", "too", "also", "new", "old", "some", "there", "here", "what",
    "which", "who", "when", "where", "how", "say", "saying", "want", "need", "get",
    // Generic nouns.
    "email", "mail", "message", "msg", "text", "chat", "thread", "conversation", "inbox", "file",
    "document", "doc", "folder", "directory", "meeting", "event", "calendar", "invite",
    "invitation", "appointment", "contact", "person", "someone", "item", "entry", "row", "list",
    "note", "page", "tab", "window", "app",
    // Generic verbs.
    "reply", "respond", "answer", "send", "forward", "delete", "remove", "trash", "archive",
    "open", "close", "tell", "ask", "let", "know", "write", "draft", "rename", "move", "copy",
    "cancel", "reschedule", "schedule", "book", "set", "change", "update", "edit", "add",
    "create", "make", "find", "show", "read", "mark", "star", "flag", "pin", "call", "accept",
    "decline", "share", "save", "print", "check", "look", "go", "put", "give", "back",
];

/// Words that ask for several items at once: nothing to disambiguate.
#[rustfmt::skip]
const MANY_WORDS: &[&str] = &[
    "all", "every", "each", "both", "any", "these", "those", "everything", "everyone",
    "everybody", "multiple", "several",
];

/// The ambiguous target in `goal` given what the screen shows, if any.
///
/// The goal is split into lowercase words (letters and digits; a possessive `'s` is dropped),
/// minus [`STOP_WORDS`] and words shorter than two characters. Each target is matched word by
/// word: a target word matches a goal word when they are equal or the target word is its plural
/// (`report` → `reports`). Targets are grouped by the set of goal words they match; the groups
/// matching the most goal words are the best fits, and a best group with two or more distinct
/// items (by label, ignoring case and spacing) is ambiguous. Nothing is ambiguous when the goal
/// asks for several items ([`MANY_WORDS`], or a plural goal word whose singular is on screen).
pub fn ambiguity(goal: &str, targets: &[String]) -> Option<Ambiguity> {
    let all = words(goal);
    if all
        .iter()
        .any(|(_, lower)| MANY_WORDS.contains(&lower.as_str()))
    {
        return None;
    }
    // Distinct content words of the goal, in order, with their spelling in the goal.
    let mut seen = HashSet::new();
    let goal_words: Vec<(String, String)> = all
        .into_iter()
        .filter(|(_, lower)| lower.chars().count() >= 2 && !STOP_WORDS.contains(&lower.as_str()))
        .filter(|(_, lower)| seen.insert(lower.clone()))
        .collect();
    if goal_words.is_empty() || targets.len() < 2 {
        return None;
    }

    // Per target: the goal words it matches, and which of those it matched only as the singular
    // of a plural goal word.
    let mut matched: Vec<(BTreeSet<usize>, BTreeSet<usize>)> = Vec::with_capacity(targets.len());
    for target in targets {
        let target_words: Vec<String> = words(target).into_iter().map(|(_, l)| l).collect();
        let mut set = BTreeSet::new();
        let mut plural = BTreeSet::new();
        for (i, (_, g)) in goal_words.iter().enumerate() {
            if target_words.iter().any(|w| w == g || is_plural_of(w, g)) {
                set.insert(i);
            } else if target_words.iter().any(|w| is_plural_of(g, w)) {
                set.insert(i);
                plural.insert(i);
            }
        }
        matched.push((set, plural));
    }
    let best = matched.iter().map(|(s, _)| s.len()).max()?;
    if best == 0 {
        return None;
    }

    // The best-fitting sets, in the order the screen first shows them.
    let mut sets: Vec<&BTreeSet<usize>> = Vec::new();
    for (set, _) in &matched {
        if set.len() == best && !sets.contains(&set) {
            sets.push(set);
        }
    }
    for set in sets {
        // A goal word in its plural form means the user wants all the matching items.
        if matched
            .iter()
            .any(|(s, plural)| s == set && !plural.is_disjoint(set))
        {
            continue;
        }
        let mut keys = HashSet::new();
        let mut candidates = Vec::new();
        for (target, (s, _)) in targets.iter().zip(&matched) {
            if s != set {
                continue;
            }
            let key = normalize(target);
            if key.is_empty() || !keys.insert(key) {
                continue;
            }
            candidates.push(clean(target, CANDIDATE_MAX));
        }
        if candidates.len() < 2 {
            continue;
        }
        candidates.truncate(MAX_CANDIDATES);
        let term = set
            .iter()
            .map(|&i| goal_words[i].0.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(Ambiguity { term, candidates });
    }
    None
}

/// `plural` is `singular` with an `s` or `es` ending.
fn is_plural_of(plural: &str, singular: &str) -> bool {
    plural
        .strip_prefix(singular)
        .is_some_and(|rest| rest == "s" || rest == "es")
}

/// Words of `text` as (spelling, lowercase): runs of letters and digits, with apostrophes inside
/// a word dropped and a possessive `'s` (or a trailing `'`) removed.
fn words(text: &str) -> Vec<(String, String)> {
    let text = text.replace('’', "'");
    let mut out = Vec::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '\'')) {
        let raw = raw.trim_matches('\'');
        let raw = raw
            .strip_suffix("'s")
            .or_else(|| raw.strip_suffix("'S"))
            .unwrap_or(raw);
        let word: String = raw.chars().filter(|c| *c != '\'').collect();
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        out.push((word, lower));
    }
    out
}

/// A label's identity: lowercase with whitespace collapsed.
fn normalize(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Screen text made safe to show in one line: control characters become spaces, whitespace is
/// collapsed, and the result is cut to `max` characters.
fn clean(label: &str, max: usize) -> String {
    let plain: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    truncate(&plain.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|s| s.to_string()).collect()
    }

    const INBOX: &[&str] = &[
        "Sam Lee — Lunch on Friday?",
        "Priya Shah — Q3 numbers",
        "Sam Ortiz — Re: contract draft",
        "Newsletter: Weekly digest",
        "Newsletter: Product update",
    ];

    #[test]
    fn two_sams_are_ambiguous() {
        let a = ambiguity("Reply to Sam's email and say yes", &t(INBOX)).unwrap();
        assert_eq!(a.term, "Sam");
        assert_eq!(
            a.candidates,
            vec![
                "Sam Lee — Lunch on Friday?",
                "Sam Ortiz — Re: contract draft"
            ]
        );
        assert_eq!(
            a.question(),
            "Your request says “Sam”, and I see more than one: Sam Lee — Lunch on Friday?, \
             Sam Ortiz — Re: contract draft. Which one did you mean?"
        );
        // A curly apostrophe and other casing read the same.
        let a = ambiguity("reply to SAM’s message", &t(INBOX)).unwrap();
        assert_eq!(a.term, "SAM");
        assert_eq!(a.candidates.len(), 2);
    }

    #[test]
    fn a_full_name_or_a_single_match_is_not_ambiguous() {
        assert_eq!(ambiguity("Reply to Sam Lee's email", &t(INBOX)), None);
        assert_eq!(
            ambiguity("Tell Priya the numbers look fine", &t(INBOX)),
            None
        );
        // More of the goal fits one Sam than the other.
        assert_eq!(
            ambiguity("Reply to Sam about the contract", &t(INBOX)),
            None
        );
    }

    #[test]
    fn asking_for_several_is_not_ambiguous() {
        assert_eq!(ambiguity("Delete all newsletters", &t(INBOX)), None);
        assert_eq!(ambiguity("Archive both newsletter emails", &t(INBOX)), None);
        assert_eq!(ambiguity("Delete the newsletters", &t(INBOX)), None);
        assert_eq!(ambiguity("Forward every Sam email to me", &t(INBOX)), None);
        // The singular is a single item, so it is ambiguous.
        let a = ambiguity("Delete the newsletter", &t(INBOX)).unwrap();
        assert_eq!(a.term, "newsletter");
    }

    #[test]
    fn files_with_the_same_name() {
        let files = t(&[
            "Q3 report.docx — Documents",
            "Q3 report.docx — Desktop",
            "Q4 plan.xlsx — Documents",
        ]);
        let a = ambiguity("rename the Q3 report to Q3 final", &files).unwrap();
        assert_eq!(a.term, "Q3 report");
        assert_eq!(a.candidates.len(), 2);

        let files = t(&["Q3 report.docx", "Q3 report final.docx"]);
        assert!(ambiguity("rename the Q3 report", &files).is_some());
        assert_eq!(ambiguity("rename Q3 report final", &files), None);
        // A singular goal word matches a plural on screen.
        let files = t(&["Reports 2024", "Reports 2023"]);
        assert_eq!(ambiguity("open the report", &files).unwrap().term, "report");
    }

    #[test]
    fn calendar_entries_with_the_same_title() {
        let events = t(&[
            "Team sync, Mon 10:00",
            "Dentist, Tue 15:30",
            "Team sync, Thu 10:00",
        ]);
        let a = ambiguity("Move the team sync meeting to 11am", &events).unwrap();
        assert_eq!(a.term, "team sync");
        assert_eq!(
            a.candidates,
            vec!["Team sync, Mon 10:00", "Team sync, Thu 10:00"]
        );
        assert_eq!(ambiguity("Cancel the dentist", &events), None);
    }

    #[test]
    fn duplicate_labels_and_empty_screens() {
        let same = t(&["Sam Lee", "sam  lee", "Priya Shah"]);
        assert_eq!(ambiguity("Reply to Sam", &same), None);
        assert_eq!(ambiguity("Reply to Sam", &[]), None);
        assert_eq!(ambiguity("Reply to Sam", &t(&["Sam Lee"])), None);
        assert_eq!(ambiguity("", &t(INBOX)), None);
        // Only stop words left: nothing names a target.
        assert_eq!(ambiguity("reply to the email", &t(INBOX)), None);
        assert_eq!(ambiguity("Reply to Alex", &t(INBOX)), None);
    }

    #[test]
    fn candidates_are_cleaned_capped_and_cut() {
        let long = format!("Sam {}", "x".repeat(200));
        let mut many = vec![long.clone(), "Sam\tLee\n<b>hi</b>".to_string()];
        for i in 0..8 {
            many.push(format!("Sam {i}"));
        }
        let a = ambiguity("call Sam", &many).unwrap();
        assert_eq!(a.candidates.len(), MAX_CANDIDATES);
        assert_eq!(a.candidates[0].chars().count(), CANDIDATE_MAX + 1);
        assert!(a.candidates[0].ends_with('…'));
        assert_eq!(a.candidates[1], "Sam Lee <b>hi</b>");
        assert!(!a.question().contains('\n'));
    }

    #[test]
    fn words_strip_possessives_and_apostrophes() {
        let w: Vec<String> = words("Sam's  Q3-report, O'Brien’s don't")
            .into_iter()
            .map(|(w, _)| w)
            .collect();
        assert_eq!(w, vec!["Sam", "Q3", "report", "OBrien", "dont"]);
    }
}
