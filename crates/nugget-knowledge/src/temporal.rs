//! Temporal cues in a claim's supporting quote: does the quote say the fact **ended** ("my last
//! day at Cindergate", "moved from Cindergate", "former CTO") or **started** ("joined
//! Quillfeather", "now Head of Data", "moved to Quillfeather")?
//!
//! The detector is small, deterministic and conservative. It only looks at the words just before
//! an occurrence of the claim's object in the quote (the object governs which value a cue is
//! about: in "moved from X to Y", X ended and Y started), within one clause: a sentence end, a
//! comma, colon or parenthesis stops the scan, and so does any word between the cue and the
//! object that is not a function word ([`BRIDGE`]: "at", "from", "of", "the", "her", ...,
//! "works", "job"), so in "Alice left Globex and works at Acme" the cue is about Globex only.
//!
//! - **Ended** cues, at most [`MAX_GAP`] words before the object: `last day at` / `last day
//!   with` / `last day as`, `left` (not `left on/at/onto/side/wing/turn`), `leaving`, `quit`,
//!   `departed`, `resigned from`, `retired from`, `moved from` / `moving from` / `switched
//!   from` / `went from`, `former`, `formerly`, `previously` (not `as previously`, nor
//!   `previously agreed/discussed/mentioned/noted/stated/planned`), `used to`, `no longer`,
//!   `until recently`, and `ex-` right before the object (any hyphen, U+2010–U+2015 included).
//! - **Started** cues, at most [`MAX_GAP`] words before the object: `joined`, `joining`, `now`,
//!   `new role`, `new job`, `new position`, `moved to` / `moving to`, `started at` / `started
//!   as` / `starting at` / `starts at`, `promoted to`; and the second half of a transition, a
//!   `to` or `for` right before the object when an ended transition cue (`moved from`, `left`,
//!   ...) precedes it in the clause ("moved from X to Y", "left X for Y").
//! - A cue negated within the two words before it (`not`, `never`, `n't`) is no cue.
//!
//! The nearest cue before an occurrence wins. When the object occurs several times, a started
//! occurrence makes the quote started; it is ended only when every occurrence is ended ("Priya
//! (CTO) replaces our former CTO" says nothing about Priya's title ending). A quote that does not
//! name the object (or the first two or more words of a longer name) has no cue. Quotes are
//! untrusted text: the detector only reads them, and its answer only orders values the quote
//! gate already accepted.

/// What a quote says about when its fact holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Cue {
    /// Nothing temporal: the fact held when the item was written.
    Neutral,
    /// The fact started around the item's date ("joined", "now").
    Started,
    /// The fact ended by the item's date ("last day at", "moved from", "former").
    Ended,
}

/// Words between a cue and the object at most.
pub(crate) const MAX_GAP: usize = 4;
/// Words looked at before an occurrence of the object at most (transition cues).
const WINDOW: usize = 12;

const ENDED: &[&[&str]] = &[
    &["last", "day", "at"],
    &["last", "day", "with"],
    &["last", "day", "as"],
    &["resigned", "from"],
    &["retired", "from"],
    &["moved", "from"],
    &["moving", "from"],
    &["switched", "from"],
    &["went", "from"],
    &["no", "longer"],
    &["until", "recently"],
    &["used", "to"],
    &["left"],
    &["leaving"],
    &["quit"],
    &["departed"],
    &["former"],
    &["formerly"],
    &["previously"],
];

/// Ended cues that start a transition ("moved from X to Y", "left X for Y").
const TRANSITION: &[&[&str]] = &[
    &["moved", "from"],
    &["moving", "from"],
    &["switched", "from"],
    &["went", "from"],
    &["left"],
    &["leaving"],
    &["quit"],
];

const STARTED: &[&[&str]] = &[
    &["new", "role"],
    &["new", "job"],
    &["new", "position"],
    &["moved", "to"],
    &["moving", "to"],
    &["started", "at"],
    &["started", "as"],
    &["starting", "at"],
    &["starts", "at"],
    &["promoted", "to"],
    &["joined"],
    &["joining"],
    &["now"],
];

/// Words that may stand between a cue and the object it is about.
pub(crate) const BRIDGE: &[&str] = &[
    "at", "from", "of", "the", "a", "an", "her", "his", "their", "my", "our", "your", "its", "as",
    "in", "with", "for", "work", "works", "worked", "working", "job", "role", "position",
];

/// `left` followed by one of these is a direction or a place, not a departure.
const NOT_AFTER_LEFT: &[&str] = &["on", "at", "onto", "side", "wing", "turn"];
/// `previously` followed by one of these (or preceded by `as`) refers to a conversation.
const NOT_AFTER_PREVIOUSLY: &[&str] = &[
    "agreed",
    "discussed",
    "mentioned",
    "noted",
    "stated",
    "planned",
];

#[derive(Debug, Clone, PartialEq)]
struct Tok {
    word: String,
    /// A sentence boundary lies between this word and the previous one.
    brk: bool,
    /// A clause boundary (comma, colon, parenthesis, quotation mark) lies between this word and
    /// the previous one.
    soft: bool,
}

fn tokens(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut brk, mut soft) = (false, false);
    let flush = |cur: &mut String, out: &mut Vec<Tok>, brk: &mut bool, soft: &mut bool| {
        if !cur.is_empty() {
            out.push(Tok {
                word: std::mem::take(cur),
                brk: *brk,
                soft: *soft,
            });
            *brk = false;
            *soft = false;
        }
    };
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            cur.extend(ch.to_lowercase());
        } else {
            flush(&mut cur, &mut out, &mut brk, &mut soft);
            if matches!(ch, '.' | ';' | '!' | '?' | '\n' | '|') {
                brk = true;
            } else if matches!(ch, ',' | ':' | '(' | ')' | '"' | '\u{201c}' | '\u{201d}') {
                soft = true;
            }
        }
    }
    flush(&mut cur, &mut out, &mut brk, &mut soft);
    out
}

/// Whether `phrase` ends at word `end` (inclusive) of `toks`, all within one clause.
fn ends_at(toks: &[Tok], end: usize, phrase: &[&str]) -> bool {
    let n = phrase.len();
    if end + 1 < n {
        return false;
    }
    let start = end + 1 - n;
    toks[start..=end]
        .iter()
        .zip(phrase)
        .all(|(t, p)| t.word == *p)
        && toks[start + 1..=end].iter().all(|t| !t.brk && !t.soft)
}

/// Positions where `name` (as words) occurs in `toks`.
fn occurrences(toks: &[Tok], name: &[Tok]) -> Vec<usize> {
    if name.is_empty() || name.len() > toks.len() {
        return Vec::new();
    }
    (0..=toks.len() - name.len())
        .filter(|&i| {
            toks[i..i + name.len()]
                .iter()
                .zip(name)
                .all(|(a, b)| a.word == b.word)
        })
        .collect()
}

/// The words before `start` (at most two, within the sentence) negate what follows.
fn negated(toks: &[Tok], start: usize) -> bool {
    let mut k = start;
    for _ in 0..2 {
        if k == 0 || toks[k].brk {
            return false;
        }
        k -= 1;
        let w = toks[k].word.as_str();
        // "n't" splits into "...n" and "t" ("didn't", "won't", "isn't").
        let nt = w == "t" && k > 0 && toks[k - 1].word.ends_with('n');
        if matches!(w, "not" | "never") || nt {
            return true;
        }
    }
    false
}

/// A cue phrase ending at word `end`, unless an exception or a negation applies.
fn phrase_cue(toks: &[Tok], end: usize) -> Option<Cue> {
    let mut best: Option<(&[&str], Cue)> = None;
    for (list, cue) in [(ENDED, Cue::Ended), (STARTED, Cue::Started)] {
        for p in list {
            if ends_at(toks, end, p) && best.is_none_or(|(b, _)| p.len() > b.len()) {
                best = Some((p, cue));
            }
        }
    }
    let (phrase, cue) = best?;
    let start = end + 1 - phrase.len();
    let next = toks
        .get(end + 1)
        .filter(|t| !t.brk && !t.soft)
        .map(|t| t.word.as_str());
    let prev =
        (start > 0 && !toks[start].brk && !toks[start].soft).then(|| toks[start - 1].word.as_str());
    let excepted = match phrase {
        ["left"] => next.is_some_and(|n| NOT_AFTER_LEFT.contains(&n)),
        ["previously"] => {
            prev == Some("as") || next.is_some_and(|n| NOT_AFTER_PREVIOUSLY.contains(&n))
        }
        _ => false,
    };
    if excepted || negated(toks, start) {
        return None;
    }
    Some(cue)
}

/// The cue governing the occurrence of the object at word `at`.
fn cue_at(toks: &[Tok], at: usize) -> Cue {
    // `ex-` (or "ex") right before the object.
    if at > 0 && toks[at - 1].word == "ex" && !toks[at].brk && !toks[at].soft {
        return Cue::Ended;
    }
    let lo = at.saturating_sub(WINDOW);
    let mut j = at;
    while j > lo && at - j <= MAX_GAP {
        // The words j..at stand between the candidate cue (ending at j - 1) and the object.
        if toks[j].brk || toks[j].soft {
            break;
        }
        let end = j - 1;
        if let Some(c) = phrase_cue(toks, end) {
            return c;
        }
        if j == at && matches!(toks[end].word.as_str(), "to" | "for") {
            // "from X to Y", "left X for Y": the second half of a transition.
            let mut k = end;
            while k > lo {
                if toks[k].brk || toks[k].soft || matches!(toks[k].word.as_str(), "and" | "but") {
                    break;
                }
                k -= 1;
                if TRANSITION.iter().any(|p| ends_at(toks, k, p)) && !negated(toks, k) {
                    return Cue::Started;
                }
            }
        }
        if !BRIDGE.contains(&toks[end].word.as_str()) {
            break;
        }
        j -= 1;
    }
    Cue::Neutral
}

/// Text every quote with a cue contains (lower case, hyphens normalised): most quotes have
/// none, and are answered without tokenising them.
const STEMS: &[&str] = &[
    "last day",
    "resigned",
    "retired",
    "moved",
    "moving",
    "switched",
    "went from",
    "no longer",
    "until recently",
    "used to",
    "left",
    "leaving",
    "quit",
    "departed",
    "former",
    "previously",
    "ex-",
    "ex ",
    "new ",
    "start",
    "promoted",
    "join",
    "now",
];

fn may_have_cue(quote: &str) -> bool {
    let lower: String = quote
        .chars()
        .map(|c| {
            if ('\u{2010}'..='\u{2015}').contains(&c) {
                '-'
            } else {
                c
            }
        })
        .collect::<String>()
        .to_lowercase();
    STEMS.iter().any(|s| lower.contains(s))
}

/// The temporal cue `quote` carries for a claim whose object is called `names` (its name and
/// aliases, or the literal). See the module docs.
pub(crate) fn cue(quote: &str, names: &[&str]) -> Cue {
    if !may_have_cue(quote) {
        return Cue::Neutral;
    }
    let toks = tokens(quote);
    if toks.is_empty() {
        return Cue::Neutral;
    }
    let mut at: Vec<usize> = Vec::new();
    let mut found_full = false;
    for n in names {
        let occ = occurrences(&toks, &tokens(n));
        found_full |= !occ.is_empty();
        at.extend(occ);
    }
    if !found_full {
        // "Glimmerfen Labs" for "Glimmerfen Labs Ltd": the longest prefix of two words or more.
        for n in names {
            let nt = tokens(n);
            for len in (2..nt.len()).rev() {
                let occ = occurrences(&toks, &nt[..len]);
                if !occ.is_empty() {
                    at.extend(occ);
                    break;
                }
            }
        }
    }
    at.sort_unstable();
    at.dedup();
    if at.is_empty() {
        return Cue::Neutral;
    }
    let cues: Vec<Cue> = at.into_iter().map(|i| cue_at(&toks, i)).collect();
    if cues.contains(&Cue::Started) {
        Cue::Started
    } else if cues.iter().all(|c| *c == Cue::Ended) {
        Cue::Ended
    } else {
        Cue::Neutral
    }
}

/// Where the object first occurs in `quote`, in words (`None` when it does not occur). Used to
/// order two values cited by one quote ("from X to Y": Y comes later).
pub(crate) fn object_position(quote: &str, names: &[&str]) -> Option<usize> {
    let toks = tokens(quote);
    names
        .iter()
        .filter_map(|n| occurrences(&toks, &tokens(n)).into_iter().next())
        .min()
}

#[cfg(test)]
mod tests {
    use super::{Cue, cue, object_position};

    #[test]
    fn ended_cues() {
        for (q, o) in [
            (
                "Friday is my last day at Cindergate Analytics",
                "Cindergate Analytics",
            ),
            (
                "Moved from [[Cindergate Analytics]] to [[Quillfeather Systems]] in October 2026",
                "Cindergate Analytics",
            ),
            ("She left Cindergate in the spring", "Cindergate"),
            ("Priya no longer works at Cindergate", "Cindergate"),
            ("Priya used to work at Cindergate", "Cindergate"),
            ("our former Senior Data Scientist", "Senior Data Scientist"),
            ("Priya, previously at Cindergate", "Cindergate"),
            (
                "Priya, formerly of Cindergate Analytics",
                "Cindergate Analytics",
            ),
            ("an ex-Cindergate data scientist", "Cindergate"),
            ("an ex\u{2011}Cindergate data scientist", "Cindergate"),
            ("an ex\u{2013}Cindergate data scientist", "Cindergate"),
            ("She quit her job at Cindergate", "Cindergate"),
        ] {
            assert_eq!(cue(q, &[o]), Cue::Ended, "{q}");
        }
    }

    #[test]
    fn started_cues() {
        for (q, o) in [
            (
                "I've joined Quillfeather Systems as Head of Data",
                "Quillfeather Systems",
            ),
            ("Now Head of Data at Quillfeather", "Head of Data"),
            (
                "Moved from [[Cindergate Analytics]] to [[Quillfeather Systems]] in October 2026",
                "Quillfeather Systems",
            ),
            ("She left Cindergate for Quillfeather", "Quillfeather"),
            ("Her new role as Head of Data starts Monday", "Head of Data"),
            ("Priya started at Quillfeather this week", "Quillfeather"),
            ("Sam was promoted to Director", "Director"),
        ] {
            assert_eq!(cue(q, &[o]), Cue::Started, "{q} / {o}");
        }
    }

    #[test]
    fn neutral_quotes() {
        for (q, o) in [
            (
                "Priya Castellane, our Senior Data Scientist, will join from the Cindergate side",
                "Senior Data Scientist",
            ),
            (
                "Priya Castellane, our Senior Data Scientist, will join from the Cindergate side",
                "Cindergate Analytics",
            ),
            ("Alice works at Acme", "Acme"),
            // A cue too far from the object, or in another sentence.
            (
                "As previously discussed with the whole team last week, Alice works at Acme",
                "Acme",
            ),
            ("I left early. Alice works at Acme", "Acme"),
            // The object is not in the quote.
            ("Friday is my last day", "Acme"),
            ("", "Acme"),
        ] {
            assert_eq!(cue(q, &[o]), Cue::Neutral, "{q} / {o}");
        }
    }

    #[test]
    fn a_cue_is_about_the_object_right_after_it() {
        // The cue governs Globex: a conjunction and a verb stand before Acme.
        assert_eq!(
            cue("Alice left Globex and works at Acme", &["Acme"]),
            Cue::Neutral
        );
        assert_eq!(
            cue("Alice left Globex and works at Acme", &["Globex"]),
            Cue::Ended
        );
        // A comma ends the clause.
        assert_eq!(
            cue("Sam is leaving, Acme is hiring", &["Acme"]),
            Cue::Neutral
        );
    }

    #[test]
    fn one_ended_occurrence_does_not_end_the_value() {
        assert_eq!(
            cue("Priya (CTO) replaces our former CTO Sam", &["CTO"]),
            Cue::Neutral
        );
        assert_eq!(
            cue("our former CTO, formerly CTO of Acme", &["CTO"]),
            Cue::Ended
        );
    }

    #[test]
    fn a_one_word_prefix_of_a_name_is_not_the_name() {
        assert_eq!(
            cue(
                "Now Head of Data at Quillfeather",
                &["Quillfeather Systems"]
            ),
            Cue::Neutral
        );
        assert_eq!(
            cue("my last day at Glimmerfen Labs", &["Glimmerfen Labs Ltd"]),
            Cue::Ended
        );
        assert_eq!(
            cue("my last day at Glimmerfen", &["Glimmerfen Labs Ltd"]),
            Cue::Neutral
        );
    }

    #[test]
    fn realistic_non_cues() {
        for (q, o) in [
            ("Priya never left Cindergate", "Cindergate"),
            ("Priya is not leaving Cindergate", "Cindergate"),
            ("Priya won't be leaving Cindergate", "Cindergate"),
            ("Priya didn't quit Cindergate", "Cindergate"),
            ("Turn left at Cindergate", "Cindergate"),
            ("the office on the left side of Cindergate", "Cindergate"),
            ("Friday is the last day Cindergate is open", "Cindergate"),
            ("as previously agreed with Cindergate", "Cindergate"),
            ("previously discussed at Cindergate", "Cindergate"),
        ] {
            assert_eq!(cue(q, &[o]), Cue::Neutral, "{q}");
        }
    }

    #[test]
    fn aliases_count_and_a_started_occurrence_wins() {
        assert_eq!(
            cue("my last day at CA", &["Cindergate Analytics", "CA"]),
            Cue::Ended
        );
        assert_eq!(
            cue(
                "She left Acme in 2020 and has now joined Acme again",
                &["Acme"]
            ),
            Cue::Started
        );
    }

    #[test]
    fn positions() {
        let q = "Moved from [[Cindergate Analytics]] to [[Quillfeather Systems]]";
        assert!(
            object_position(q, &["Quillfeather Systems"])
                > object_position(q, &["Cindergate Analytics"])
        );
        assert_eq!(object_position(q, &["Acme"]), None);
    }
}
