//! Temporal cues in a claim's supporting quote.

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
    &["before", "that"],
    &["before", "this"],
    &["prior", "to", "that"],
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
    "in", "with", "for", "work", "works", "worked", "working", "job", "role", "position", "she",
    "he", "they", "i", "we", "was", "were", "had", "been",
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

/// Text every quote with a cue contains (lower case, hyphens normalised).
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
    "before th",
    "prior to",
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

/// Where the object first occurs in `quote`, in words (`None` when it does not occur).
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
    fn cues() {
        for (q, o, want) in [
            (
                "Friday is my last day at Cindergate Analytics",
                "Cindergate Analytics",
                Cue::Ended,
            ),
            (
                "Moved from [[Cindergate]] to [[Quillfeather]] in October",
                "Cindergate",
                Cue::Ended,
            ),
            (
                "Moved from [[Cindergate]] to [[Quillfeather]] in October",
                "Quillfeather",
                Cue::Started,
            ),
            (
                "Priya no longer works at Cindergate",
                "Cindergate",
                Cue::Ended,
            ),
            (
                "our former Senior Data Scientist",
                "Senior Data Scientist",
                Cue::Ended,
            ),
            (
                "an ex\u{2011}Cindergate data scientist",
                "Cindergate",
                Cue::Ended,
            ),
            // QA GG5-15: the 8B model kept Dana "Design Lead" from this.
            ("Before that she was Design Lead", "Design Lead", Cue::Ended),
            (
                "I've joined Quillfeather Systems as Head of Data",
                "Quillfeather Systems",
                Cue::Started,
            ),
            (
                "Now Head of Data at Quillfeather",
                "Head of Data",
                Cue::Started,
            ),
            ("Sam was promoted to Director", "Director", Cue::Started),
            ("Alice works at Acme", "Acme", Cue::Neutral),
            // A cue too far from the object, or in another sentence.
            (
                "As previously discussed with the whole team last week, Alice works at Acme",
                "Acme",
                Cue::Neutral,
            ),
            ("I left early. Alice works at Acme", "Acme", Cue::Neutral),
            ("Friday is my last day", "Acme", Cue::Neutral),
            ("Priya never left Cindergate", "Cindergate", Cue::Neutral),
            (
                "Priya won't be leaving Cindergate",
                "Cindergate",
                Cue::Neutral,
            ),
            ("Turn left at Cindergate", "Cindergate", Cue::Neutral),
            (
                "Friday is the last day Cindergate is open",
                "Cindergate",
                Cue::Neutral,
            ),
            // The cue governs Globex: a conjunction and a verb stand before Acme.
            ("Alice left Globex and works at Acme", "Acme", Cue::Neutral),
            ("Alice left Globex and works at Acme", "Globex", Cue::Ended),
            ("Sam is leaving, Acme is hiring", "Acme", Cue::Neutral),
            (
                "Priya (CTO) replaces our former CTO Sam",
                "CTO",
                Cue::Neutral,
            ),
            ("our former CTO, formerly CTO of Acme", "CTO", Cue::Ended),
            (
                "Now Head of Data at Quillfeather",
                "Quillfeather Systems",
                Cue::Neutral,
            ),
            (
                "my last day at Glimmerfen",
                "Glimmerfen Labs Ltd",
                Cue::Neutral,
            ),
            (
                "She left Acme in 2020 and has now joined Acme again",
                "Acme",
                Cue::Started,
            ),
        ] {
            assert_eq!(cue(q, &[o]), want, "{q} / {o}");
        }
        assert_eq!(
            cue("my last day at CA", &["Cindergate Analytics", "CA"]),
            Cue::Ended
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
