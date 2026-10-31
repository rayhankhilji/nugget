//! Ask, don't guess: spot a goal that names a target the screen shows more than once.
//!
//! [`ambiguity`] matches the words of the user's goal against the items a tool saw on screen
//! ([`ToolResult::targets`](crate::ToolResult)). When the best-matching items are two or more
//! different things ("Reply to Sam's email" with Sam Lee and Sam Ortiz in the inbox), the loop
//! asks the user which one before it acts ([`AgentConfig::clarify`](crate::AgentConfig)). Pure
//! and deterministic: no model call. Target labels are untrusted screen text; they only ever
//! reach the user, cleaned and cut, never the model.

use std::collections::{BTreeSet, HashSet};

use nugget_policy::RiskClass;
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
    "empty", "erase", "clear", "clean", "tidy", "buy", "pay", "purchase",
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
    // A whole name the goal spells out (`draft.txt`, `sam@ourco.example`) that one target shows
    // is that target.
    let names = dotted_names(goal);
    if names.iter().any(|n| {
        targets
            .iter()
            .filter(|t| t.to_lowercase().contains(n.as_str()))
            .count()
            == 1
    }) {
        return None;
    }
    // A file extension says what kind of item, not which one.
    let extensions: HashSet<String> = names
        .iter()
        .filter_map(|n| n.rsplit_once('.').map(|(_, ext)| ext.to_string()))
        .collect();
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
        .filter(|(_, lower)| is_content_word(lower))
        .filter(|(_, lower)| !extensions.contains(lower))
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

/// Words after which the goal names a place, not the object (`… in Documents`). Not "from",
/// which as often names a sender ("the spam email from Prize Center").
const PLACE_WORDS: &[&str] = &["in", "inside", "within", "under", "at"];

/// The words naming what the goal acts on, as written: its content words before a place phrase
/// ("annual report" in "Delete the annual report in Documents"). `None` when there are none or
/// the goal asks for several items.
pub fn goal_object(goal: &str) -> Option<String> {
    let all = words(goal);
    if all
        .iter()
        .any(|(_, lower)| MANY_WORDS.contains(&lower.as_str()))
    {
        return None;
    }
    let mut object = Vec::new();
    for (word, lower) in all {
        if PLACE_WORDS.contains(&lower.as_str()) {
            break;
        }
        if is_content_word(&lower) {
            object.push(word);
        }
    }
    (!object.is_empty()).then(|| object.join(" "))
}

/// Whether some target shows a word of the goal's object ([`goal_object`]), singular or plural.
pub fn shows_object(goal: &str, targets: &[String]) -> bool {
    let Some(object) = goal_object(goal) else {
        return true;
    };
    let wanted: Vec<String> = words(&object).into_iter().map(|(_, l)| l).collect();
    targets.iter().any(|t| {
        words(t).iter().any(|(_, w)| {
            wanted
                .iter()
                .any(|g| w == g || is_plural_of(w, g) || is_plural_of(g, w))
        })
    })
}

/// Labels of steps that destroy data for good.
const IRREVERSIBLE: &[&str] = &["empty", "erase", "purge", "wipe", "permanently", "shred"];

/// When `label` names an irreversible step (Empty Trash, Erase) that `goal` does not ask for:
/// the word. Asking for any irreversible step ("empty", "erase", "permanently") allows them.
pub fn unrequested_irreversible(goal: &str, label: &str) -> Option<&'static str> {
    let label_words: Vec<String> = words(label).into_iter().map(|(_, l)| l).collect();
    let word = IRREVERSIBLE
        .iter()
        .copied()
        .find(|w| label_words.iter().any(|l| l == w))?;
    let goal_words: Vec<String> = words(goal).into_iter().map(|(_, l)| l).collect();
    let asked = goal_words.iter().any(|g| {
        IRREVERSIBLE.iter().any(|w| {
            g == w
                || g.strip_prefix(w)
                    .is_some_and(|r| ["s", "ed", "ing"].contains(&r))
        })
    });
    (!asked).then_some(word)
}

/// Verbs that need a send, a deletion or a payment to have happened before the request is done.
const SEND_VERBS: &[&str] = &["send", "reply", "forward", "respond"];
const DESTROY_VERBS: &[&str] = &["delete", "trash", "remove", "erase"];
const SPEND_VERBS: &[&str] = &["buy", "pay", "purchase"];
/// Words that say the request stops short of the act ("draft a reply, don't send it").
const STOP_SHORT: &[&str] = &["dont", "not", "without", "draft", "never"];

/// The least risk class some action must have reached for `goal` to be done: [`RiskClass::Send`]
/// for "reply", "send", "forward" (or a goal that starts with "email"), [`RiskClass::Destroy`]
/// for "delete", [`RiskClass::Spend`] for "buy" or "pay". `None` when the goal names none of these
/// or says it stops short.
pub fn required_risk(goal: &str) -> Option<RiskClass> {
    let w: Vec<String> = words(goal).into_iter().map(|(_, l)| l).collect();
    if w.iter().any(|x| STOP_SHORT.contains(&x.as_str())) {
        return None;
    }
    let has = |verbs: &[&str]| w.iter().any(|x| verbs.contains(&x.as_str()));
    let first_is_mail = w.first().is_some_and(|x| x == "email" || x == "mail");
    if has(SEND_VERBS) || first_is_mail {
        Some(RiskClass::Send)
    } else if has(DESTROY_VERBS) {
        Some(RiskClass::Destroy)
    } else if has(SPEND_VERBS) {
        Some(RiskClass::Spend)
    } else {
        None
    }
}

/// A word that can say which item is meant: two characters or more, not a [`STOP_WORDS`] word,
/// and not a bare number of one or two digits ("2 files"; a year such as `2023` still counts,
/// and so does a clock time, which [`words`] keeps whole).
fn is_content_word(lower: &str) -> bool {
    let n = lower.chars().count();
    n >= 2 && !STOP_WORDS.contains(&lower) && !(n <= 2 && lower.chars().all(|c| c.is_ascii_digit()))
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
    // A clock time ("13:00") is one word: its parts alone would match every time on screen.
    let chars: Vec<char> = text.chars().collect();
    let keep = |i: usize| {
        let c = chars[i];
        c.is_alphanumeric()
            || c == '\''
            || (c == ':'
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
    };
    let text: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| if keep(i) { c } else { ' ' })
        .collect();
    let mut out = Vec::new();
    for raw in text.split(' ') {
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

/// Lowercase tokens of `text` with a dot (or `@`) between letters or digits: file names and
/// addresses, which name one item whole.
fn dotted_names(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '“' | '”' | ',' | '(' | ')'))
        .map(|w| w.trim_end_matches(['.', '!', '?', ':', ';']).to_lowercase())
        .filter(|w| {
            let b = w.as_bytes();
            (1..b.len().saturating_sub(1)).any(|i| {
                matches!(b[i], b'.' | b'@')
                    && b[i - 1].is_ascii_alphanumeric()
                    && b[i + 1].is_ascii_alphanumeric()
            })
        })
        .collect()
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

    #[test]
    fn a_whole_file_name_is_not_ambiguous() {
        let files = t(&["File draft.txt", "File notes.txt", "Folder Projects"]);
        assert_eq!(
            ambiguity("In Documents, rename draft.txt to final.txt.", &files),
            None
        );
        // The extension alone never makes a term.
        assert_eq!(ambiguity("Rename the txt file to final.txt", &files), None);
        let two = t(&["File q3 report.txt", "File q3 report final.txt"]);
        assert_eq!(
            ambiguity("Delete the q3 report", &two).map(|a| a.term),
            Some("q3 report".into())
        );
    }

    #[test]
    fn goal_objects_and_whether_the_screen_shows_them() {
        assert_eq!(
            goal_object("Delete the annual report in Documents").as_deref(),
            Some("annual report")
        );
        assert_eq!(goal_object("Delete all the newsletters"), None);
        assert_eq!(goal_object("Empty the trash"), None);
        assert_eq!(
            goal_object("Delete the spam email from Prize Center").as_deref(),
            Some("spam Prize Center")
        );
        let folders = t(&["Folder Documents", "Folder Downloads"]);
        assert!(!shows_object(
            "Delete the annual report in Documents",
            &folders
        ));
        let files = t(&["File annual-report-2023.pdf", "File notes.txt"]);
        assert!(shows_object(
            "Delete the annual report in Documents",
            &files
        ));
        assert!(shows_object("Empty the trash", &folders));
    }

    #[test]
    fn irreversible_steps_need_asking_for() {
        let goal = "Delete backup.zip from Downloads";
        assert_eq!(unrequested_irreversible(goal, "Empty Trash"), Some("empty"));
        assert_eq!(unrequested_irreversible(goal, "Move to Trash"), None);
        assert_eq!(
            unrequested_irreversible("Empty the trash", "Empty Trash"),
            None
        );
        assert_eq!(
            unrequested_irreversible("Erase this laptop before I sell it", "Erase all content"),
            None
        );
        assert_eq!(
            unrequested_irreversible("Permanently delete old.txt", "Empty Trash"),
            None
        );
    }

    #[test]
    fn required_risk_follows_the_verb() {
        assert_eq!(
            required_risk("Reply to Sam and tell him yes"),
            Some(RiskClass::Send)
        );
        assert_eq!(required_risk("Email Bob about Q3"), Some(RiskClass::Send));
        assert_eq!(
            required_risk("Delete the old newsletter email"),
            Some(RiskClass::Destroy)
        );
        assert_eq!(required_risk("Buy a USB-C cable"), Some(RiskClass::Spend));
        assert_eq!(
            required_risk("Draft a reply to Sam but don't send it"),
            None
        );
        assert_eq!(required_risk("Turn on dark mode"), None);
    }

    #[test]
    fn clock_times_do_not_make_a_calendar_ambiguous() {
        let week = t(&[
            "Monday 09:00 Team standup",
            "Tuesday 11:00 Design review",
            "Thursday 13:00 Customer call",
        ]);
        assert_eq!(
            ambiguity(
                "Block Wednesday 10:00 in my calendar as \"Focus time\".",
                &week
            ),
            None
        );
        // The time still picks out its own event.
        let thursday = t(&[
            "Thursday 13:00 Customer call",
            "Thursday 14:00 1:1 with Sam",
        ]);
        assert_eq!(ambiguity("Move my Thursday 13:00 meeting", &thursday), None);
        assert!(ambiguity("Cancel my Thursday meeting", &thursday).is_some());
        let reports = t(&["File report-2023.pdf", "File report-2024.pdf"]);
        assert_eq!(ambiguity("Delete the 2023 report", &reports), None);
    }
}
