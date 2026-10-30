//! Temporal cues in a claim's supporting quote: does the quote say the fact **ended** ("my last
//! day at Cindergate", "moved from Cindergate", "former CTO") or **started** ("joined
//! Quillfeather", "now Head of Data", "moved to Quillfeather")?
//!
//! The detector is small, deterministic and conservative. It only looks at the words just before
//! an occurrence of the claim's object in the quote (the object governs which value a cue is
//! about: in "moved from X to Y", X ended and Y started), within one sentence:
//!
//! - **Ended** cues, at most [`MAX_GAP`] words before the object: `last day at` / `last day
//!   with` / `last day`, `left`, `leaving`, `quit`, `departed`, `resigned from`, `retired from`,
//!   `moved from` / `moving from` / `switched from` / `went from`, `former`, `formerly`,
//!   `previously`, `used to`, `no longer`, `until recently`, and `ex-` right before the object.
//! - **Started** cues, at most [`MAX_GAP`] words before the object: `joined`, `joining`, `now`,
//!   `new role`, `new job`, `new position`, `moved to` / `moving to`, `started at` / `started
//!   as` / `starting at` / `starts at`, `promoted to`; and the second half of a transition, a
//!   `to` or `for` right before the object when an ended transition cue (`moved from`, `left`,
//!   ...) precedes it in the sentence ("moved from X to Y", "left X for Y").
//!
//! The nearest cue before an occurrence wins; when the object occurs several times, a started
//! occurrence wins over an ended one. A quote that does not name the object (or a prefix of its
//! name, "Quillfeather" for "Quillfeather Systems") has no cue. Quotes are untrusted text: the
//! detector only reads them, and its answer only orders values the quote gate already accepted.

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
    &["last", "day"],
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

#[derive(Debug, Clone, PartialEq)]
struct Tok {
    word: String,
    /// A sentence boundary lies between this word and the previous one.
    brk: bool,
}

fn tokens(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut brk = false;
    let flush = |cur: &mut String, out: &mut Vec<Tok>, brk: &mut bool| {
        if !cur.is_empty() {
            out.push(Tok {
                word: std::mem::take(cur),
                brk: *brk,
            });
            *brk = false;
        }
    };
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            cur.extend(ch.to_lowercase());
        } else {
            flush(&mut cur, &mut out, &mut brk);
            if matches!(ch, '.' | ';' | '!' | '?' | '\n' | '|') {
                brk = true;
            }
        }
    }
    flush(&mut cur, &mut out, &mut brk);
    out
}

/// Whether `phrase` ends at word `end` (inclusive) of `toks`, all within one sentence.
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
        && toks[start + 1..=end].iter().all(|t| !t.brk)
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

/// The cue governing the occurrence of the object at word `at`.
fn cue_at(toks: &[Tok], at: usize) -> Cue {
    // `ex-` (or "ex") right before the object.
    if at > 0 && toks[at - 1].word == "ex" && !toks[at].brk {
        return Cue::Ended;
    }
    let lo = at.saturating_sub(WINDOW);
    let mut j = at;
    while j > lo {
        // The words j..at are between the candidate cue (ending at j - 1) and the object.
        if toks[j].brk {
            break; // a sentence boundary before word j
        }
        let end = j - 1;
        let gap = at - j;
        if gap <= MAX_GAP {
            // Longest phrases first (e.g. "used to" before a bare "to").
            if let Some(c) = phrase_cue(toks, end) {
                return c;
            }
            if gap == 0 && matches!(toks[end].word.as_str(), "to" | "for") {
                // "from X to Y", "left X for Y": the second half of a transition.
                let mut k = end;
                while k > lo {
                    if toks[k].brk {
                        break;
                    }
                    k -= 1;
                    if TRANSITION.iter().any(|p| ends_at(toks, k, p)) {
                        return Cue::Started;
                    }
                }
            }
        }
        j -= 1;
    }
    Cue::Neutral
}

fn phrase_cue(toks: &[Tok], end: usize) -> Option<Cue> {
    let mut best: Option<(usize, Cue)> = None;
    for (list, cue) in [(ENDED, Cue::Ended), (STARTED, Cue::Started)] {
        for p in list {
            if ends_at(toks, end, p) && best.is_none_or(|(n, _)| p.len() > n) {
                best = Some((p.len(), cue));
            }
        }
    }
    best.map(|(_, c)| c)
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
        let nt = tokens(n);
        let occ = occurrences(&toks, &nt);
        found_full |= !occ.is_empty();
        at.extend(occ);
    }
    if !found_full {
        // "Quillfeather" for "Quillfeather Systems": the longest prefix of a name that occurs.
        for n in names {
            let nt = tokens(n);
            for len in (1..nt.len()).rev() {
                if nt[0].word.chars().count() < 3 {
                    break;
                }
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
    let mut out = Cue::Neutral;
    for i in at {
        match cue_at(&toks, i) {
            Cue::Started => return Cue::Started,
            Cue::Ended => out = Cue::Ended,
            Cue::Neutral => {}
        }
    }
    out
}

/// Text every quote with a cue contains (lower case): most quotes have none, and are answered
/// without tokenising them.
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
    let lower = quote.to_lowercase();
    STEMS.iter().any(|s| lower.contains(s))
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
            (
                "I've joined Quillfeather Systems as Head of Data",
                "Head of Data",
            ),
            ("Now Head of Data at Quillfeather", "Head of Data"),
            // A prefix of the name.
            ("Now Head of Data at Quillfeather", "Quillfeather Systems"),
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
