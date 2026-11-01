//! User search text → safe FTS5 `MATCH` expression.
//!
//! A deliberately small query language, parsed by a total function (it never fails or panics):
//!
//! - **plain words**: runs of alphanumeric characters; everything else separates words. Plain
//!   words are OR-ed, so bm25 ranks chunks matching more of them higher.
//! - **`"quoted phrases"`**: the words between a pair of quotes (`"`, `“` or `”`) must appear
//!   in order. A phrase is one more OR-ed term; being rarer than its words, a matching phrase
//!   ranks high on its own. A quote without a partner is ignored (its text is plain words).
//! - **`word*` prefix terms**: a word of at least [`MIN_PREFIX_CHARS`] characters directly
//!   followed by `*` matches every token starting with it (`nug*` → nugget, nuggets). Shorter
//!   prefixes are plain words (`a*` would match nearly everything).
//! - **`-word`, `-"phrase"`, `-word*` exclusions**: a `-` at the start of the query or after
//!   whitespace, directly followed by a letter or a quote, drops chunks containing that term.
//!   Exclusions only apply alongside at least one positive term (FTS5 `NOT` is binary); a query
//!   of exclusions alone matches nothing. `-5`, `well-known` and `a - b` are not exclusions.
//!
//! Everything else (`AND`, `OR`, `NOT`, `NEAR`, parentheses, `column:` filters, `^`, `+`, stray
//! `*`) is inert: every token emitted is a double-quoted FTS5 string (inner `"` doubled), so
//! nothing a user types can become FTS5 syntax. At most [`MAX_QUERY_TERMS`] words are used,
//! counting each word of a phrase.
//!
//! **CJK and Thai.** `unicode61` does not split scripts written without spaces (Han, kana,
//! Thai) or with particles attached (Hangul), so a whole sentence would be one token. Both the
//! indexed text (through the `fts_segment` SQL function, called by the FTS triggers) and the
//! query (here) go through [`segment`], which rewrites each run of such characters as its
//! overlapping bigrams (`星商事` → `星商 商事`). A query word becomes the phrase of its bigrams, so
//! character order matters; a lone character matches as a bigram prefix. Other text is unchanged.
//!
//! **Spelling variants.** Before segmenting, indexed text and query words are [`fold`]ed: NFKC
//! (`ＬＥＤ` → `LED`, `ｷｬﾝﾍﾟｰﾝ` → `キャンペーン`), Arabic light normalisation and ß → ss. The index
//! (through `fts_normalize`, see [`index_form`]) also holds [`variants`] of some words after the
//! text: Arabic words without their proclitics (`والكراسي` → `الكراسي`, `كراسي`) and German words
//! with ä/ö/ü spelled ae/oe/ue, so `Königstraße`, `Konigstrasse` and `Koenigstrasse` all match.
//!
//! **Word parts.** When a query finds little, text search retries its words as parts of longer
//! words ([`fallback_exprs`]): as prefixes in the main index (`sofreír` → `sofreírlo`) and as
//! suffixes in `chunks_rev`, which holds each longer word spelled backwards ([`reversed_form`],
//! through `fts_reversed`), so `Ausschuss` finds `Lenkungsausschuss` and `गाड़ी` `रेलगाड़ी`.

use std::borrow::Cow;

use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Value, ValueRef};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};

/// Most words used from a query (each word of a phrase counts).
pub(crate) const MAX_QUERY_TERMS: usize = 32;
/// Shortest word that may carry a `*` prefix marker.
pub(crate) const MIN_PREFIX_CHARS: usize = 2;

/// One searchable unit of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    /// One word, or a phrase of several.
    Words(Vec<String>),
    /// A word matched as a token prefix.
    Prefix(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    atom: Atom,
    exclude: bool,
}

/// Quote characters that delimit a phrase (straight and typographic double quotes).
fn is_quote(c: char) -> bool {
    matches!(c, '"' | '“' | '”')
}

/// Characters [`segment`] splits into bigrams: Han, kana, Hangul and Thai letters.
fn is_segmented(c: char) -> bool {
    matches!(c,
        '\u{0E01}'..='\u{0E30}' | '\u{0E32}'..='\u{0E33}' | '\u{0E40}'..='\u{0E46}' // Thai
        | '\u{1100}'..='\u{11FF}' // Hangul Jamo
        | '\u{3005}'..='\u{3007}' // 々 〆 〇
        | '\u{3041}'..='\u{3096}' | '\u{309D}'..='\u{309F}' // Hiragana
        | '\u{30A1}'..='\u{30FA}' | '\u{30FC}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' // Katakana
        | '\u{3130}'..='\u{318F}' | '\u{A960}'..='\u{A97F}' // Hangul compatibility jamo
        | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' // Han
        | '\u{AC00}'..='\u{D7FF}' // Hangul syllables, Jamo Extended-B
        | '\u{FF66}'..='\u{FF9F}' | '\u{FFA0}'..='\u{FFDC}' // halfwidth kana and Hangul
        | '\u{20000}'..='\u{3134F}' // Han extensions B-G
    )
}

/// Combining marks inside a segmented run (Thai vowel and tone marks, kana voicing marks).
/// `unicode61` treats them as separators, so [`segment`] drops them to keep a word in one run.
fn is_run_mark(c: char) -> bool {
    matches!(c,
        '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}' | '\u{3099}' | '\u{309A}'
    )
}

/// Whether `text` holds characters that [`segment`] rewrites.
pub(crate) fn has_segmented(text: &str) -> bool {
    text.chars().any(is_segmented)
}

/// `text` with each run of [`is_segmented`] characters replaced by its overlapping bigrams (a
/// one-character run stays as is), set off by spaces. Text without such characters is returned
/// unchanged.
pub(crate) fn segment(text: &str) -> Cow<'_, str> {
    if !has_segmented(text) {
        return Cow::Borrowed(text);
    }
    fn flush(run: &mut Vec<char>, out: &mut String) {
        match run.as_slice() {
            [] => return,
            [c] => {
                out.push(' ');
                out.push(*c);
            }
            _ => {
                for w in run.windows(2) {
                    out.push(' ');
                    out.push(w[0]);
                    out.push(w[1]);
                }
            }
        }
        out.push(' ');
        run.clear();
    }
    let mut out = String::with_capacity(text.len() * 3);
    let mut run = Vec::new();
    for c in text.chars() {
        if is_segmented(c) {
            run.push(c);
        } else if !(is_run_mark(c) && !run.is_empty()) {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    Cow::Owned(out)
}

/// Arabic marks [`fold`] drops: tashkeel (fathatan to sukun, superscript alef) and tatweel.
fn is_arabic_mark(c: char) -> bool {
    matches!(c, '\u{064B}'..='\u{0652}' | '\u{0670}' | '\u{0640}')
}

/// `text` in NFKC (full-width Latin, half-width kana, ligatures and the like become their usual
/// forms), with Arabic light normalisation (tashkeel and tatweel dropped, أ إ آ → ا, ة → ه,
/// ى → ي) and ß → ss. Both the index and query words go through it.
pub(crate) fn fold(text: &str) -> Cow<'_, str> {
    let nfkc = if is_nfkc_quick(text.chars()) == IsNormalized::Yes {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.nfkc().collect())
    };
    let changes =
        |c: char| is_arabic_mark(c) || matches!(c, 'أ' | 'إ' | 'آ' | 'ة' | 'ى' | 'ß' | 'ẞ');
    if !nfkc.chars().any(changes) {
        return nfkc;
    }
    let mut out = String::with_capacity(nfkc.len() + 8);
    for c in nfkc.chars() {
        match c {
            c if is_arabic_mark(c) => {}
            'أ' | 'إ' | 'آ' => out.push('ا'),
            'ة' => out.push('ه'),
            'ى' => out.push('ي'),
            'ß' => out.push_str("ss"),
            'ẞ' => out.push_str("SS"),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Extra index forms of the words of [`fold`]ed `text`, space-separated (empty for most text):
/// Arabic words without their proclitics, German words with ä/ö/ü spelled ae/oe/ue (`Königstrasse`
/// → `Koenigstrasse`; `unicode61` already matches `Konigstrasse`).
fn variants(text: &str) -> String {
    let mut out = String::new();
    for word in text.split(|c: char| !is_word_char(c)) {
        if word.contains(['ä', 'ö', 'ü', 'Ä', 'Ö', 'Ü']) {
            out.push(' ');
            for c in word.chars() {
                match c {
                    'ä' => out.push_str("ae"),
                    'ö' => out.push_str("oe"),
                    'ü' => out.push_str("ue"),
                    'Ä' => out.push_str("Ae"),
                    'Ö' => out.push_str("Oe"),
                    'Ü' => out.push_str("Ue"),
                    c => out.push(c),
                }
            }
        } else if word.starts_with(|c| matches!(c, '\u{0621}'..='\u{064A}')) {
            arabic_stems(word, &mut out);
        }
    }
    out
}

/// Append the forms of an Arabic `word` without its proclitics: a conjunction (و ف), a
/// preposition before the article (ب ك ل), the article (ال, or ل + ال written لل). `والكراسي` →
/// `الكراسي كراسي`. Like light stemmers, it cannot tell a clitic from a word's own first letter
/// (`فاتوره` also gives `اتوره`); such extra forms only cost index space.
fn arabic_stems(word: &str, out: &mut String) {
    let mut push = |s: &str| {
        out.push(' ');
        out.push_str(s);
    };
    let mut rest = word;
    if let Some(r) = rest.strip_prefix(['و', 'ف'])
        && r.chars().count() >= 3
    {
        push(r);
        rest = r;
    }
    if let Some(r) = rest.strip_prefix(['ب', 'ك', 'ل'])
        && r.starts_with("ال")
        && r.chars().count() >= 4
    {
        push(r);
        rest = r;
    }
    if let Some(r) = rest.strip_prefix("ال").or_else(|| rest.strip_prefix("لل"))
        && r.chars().count() >= 2
    {
        push(r);
    }
}

/// What the FTS tables index for `text` (the `fts_normalize` SQL function): [`fold`]ed and
/// [`segment`]ed, then the [`variants`] of its words on a line of their own (after the text, so
/// phrases still match). Text needing none of this is returned unchanged.
pub(crate) fn index_form(text: &str) -> Cow<'_, str> {
    let folded = fold(text);
    let extra = variants(&folded);
    let seg = segment(&folded);
    if !extra.is_empty() {
        return Cow::Owned(format!("{seg}\n{}", extra.trim_start()));
    }
    if matches!((&folded, &seg), (Cow::Borrowed(_), Cow::Borrowed(_))) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(seg.into_owned())
}

/// Whether `text` is indexed as something other than itself (so FTS5 token positions do not
/// line up with it).
pub(crate) fn is_rewritten(text: &str) -> bool {
    index_form(text) != text
}

/// Shortest word [`reversed_form`] indexes.
const MIN_SUFFIX_WORD_CHARS: usize = 6;

/// What `chunks_rev` indexes for `text` (the `fts_reversed` SQL function): each [`fold`]ed word
/// of at least [`MIN_SUFFIX_WORD_CHARS`] characters outside segmented runs, spelled backwards,
/// so that a prefix query there finds words by their ending.
pub(crate) fn reversed_form(text: &str) -> String {
    let folded = fold(text);
    let mut out = String::new();
    for word in folded.split(|c: char| !is_word_char(c) || is_segmented(c)) {
        if word.chars().count() >= MIN_SUFFIX_WORD_CHARS {
            if !out.is_empty() {
                out.push(' ');
            }
            out.extend(word.chars().rev());
        }
    }
    out
}

/// Register `name(text)` as `f` on `conn`; non-text values pass through.
fn register_text_fn(conn: &Connection, name: &str, f: fn(&str) -> String) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        name,
        1,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        move |ctx| {
            Ok(match ctx.get_raw(0) {
                ValueRef::Text(b) => Value::Text(f(&String::from_utf8_lossy(b))),
                raw => Value::try_from(raw)?,
            })
        },
    )
}

/// Register Nugget's FTS functions on `conn`: `fts_normalize` ([`index_form`]) and
/// `fts_reversed` ([`reversed_form`]), which the FTS triggers call, so every connection that
/// writes items or chunks needs them; and `fts_segment` ([`segment`] alone), the index form
/// before v12, which migrations use.
pub(crate) fn register_functions(conn: &Connection) -> rusqlite::Result<()> {
    register_text_fn(conn, "fts_segment", |t| segment(t).into_owned())?;
    register_text_fn(conn, "fts_normalize", |t| index_form(t).into_owned())?;
    register_text_fn(conn, "fts_reversed", reversed_form)
}

/// Characters that continue a word: alphanumerics and combining marks (Devanagari vowel signs
/// and nukta, Thai and kana marks inside a segmented run).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || is_run_mark(c) || is_combining_mark(c)
}

/// Parse `query` into terms (see the module docs).
fn parse(query: &str) -> Vec<Term> {
    let cs: Vec<char> = query.chars().collect();
    let n = cs.len();
    let mut budget = MAX_QUERY_TERMS;
    let mut terms = Vec::new();
    let mut i = 0;
    while i < n && budget > 0 {
        let at_boundary = i == 0 || cs[i - 1].is_whitespace();
        let exclude = cs[i] == '-'
            && at_boundary
            && cs
                .get(i + 1)
                .is_some_and(|&c| is_quote(c) || c.is_alphabetic());
        let start = if exclude { i + 1 } else { i };
        let c = cs[start];
        if is_quote(c) {
            let close = cs[start + 1..].iter().position(|&q| is_quote(q));
            match close {
                Some(len) => {
                    let end = start + 1 + len;
                    let words: Vec<String> = cs[start + 1..end]
                        .split(|&c| !is_word_char(c))
                        .filter(|w| !w.is_empty())
                        .take(budget)
                        .map(|w| w.iter().collect())
                        .collect();
                    budget -= words.len();
                    if !words.is_empty() {
                        terms.push(Term {
                            atom: Atom::Words(words),
                            exclude,
                        });
                    }
                    i = end + 1;
                }
                // Unpaired quote: ignore it and read on as plain text.
                None => i = start + 1,
            }
            continue;
        }
        if c.is_alphanumeric() {
            let mut end = start;
            while end < n && is_word_char(cs[end]) {
                end += 1;
            }
            let word: String = cs[start..end].iter().collect();
            let prefix = cs.get(end) == Some(&'*') && end - start >= MIN_PREFIX_CHARS;
            terms.push(Term {
                atom: if prefix {
                    Atom::Prefix(word)
                } else {
                    Atom::Words(vec![word])
                },
                exclude,
            });
            budget -= 1;
            i = if prefix { end + 1 } else { end };
            continue;
        }
        i += 1;
    }
    terms
}

/// A segmented word of at most this many bigrams (five characters) is matched as a phrase...
const MAX_PHRASE_BIGRAMS: usize = 4;
/// ...a longer one by any of its first this many distinct bigrams.
const MAX_RUN_BIGRAMS: usize = 64;

/// `s` as an FTS5 string literal.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn render(atom: &Atom) -> String {
    match atom {
        Atom::Words(words) => {
            let text = words
                .iter()
                .flat_map(|w| {
                    segment(&fold(w))
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
                .join(" ");
            // A lone segmented character is only indexed inside bigrams: match their prefix.
            let mut cs = text.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) if is_segmented(c) => format!("{} *", quoted(&text)),
                // A long unspaced run (a CJK sentence, as in a thread a draft replies to) as one
                // phrase would only match itself verbatim: any of its bigrams, so bm25 ranks
                // chunks by how many they share, as it does for spaced words.
                _ if words.len() == 1 && text.split(' ').count() > MAX_PHRASE_BIGRAMS => {
                    let mut seen = std::collections::HashSet::new();
                    text.split(' ')
                        .filter(|b| seen.insert(*b))
                        .take(MAX_RUN_BIGRAMS)
                        .map(quoted)
                        .collect::<Vec<_>>()
                        .join(" OR ")
                }
                _ => quoted(&text),
            }
        }
        Atom::Prefix(word) => {
            let text = segment(&fold(word))
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            format!("{} *", quoted(&text))
        }
    }
}

/// The positive words of `query`, lowercased: what a hit's text should contain.
pub(crate) fn needles(query: &str) -> Vec<String> {
    parse(query)
        .into_iter()
        .filter(|t| !t.exclude)
        .flat_map(|t| match t.atom {
            Atom::Words(words) => words,
            Atom::Prefix(word) => vec![word],
        })
        .map(|w| w.to_lowercase())
        .collect()
}

/// The FTS5 `MATCH` expression for user text, or `None` if it has no positive term.
pub(crate) fn match_expr(query: &str) -> Option<String> {
    let terms = parse(query);
    let (neg, pos): (Vec<&Term>, Vec<&Term>) = terms.iter().partition(|t| t.exclude);
    if pos.is_empty() {
        return None;
    }
    let any = |ts: &[&Term]| {
        ts.iter()
            .map(|t| render(&t.atom))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let positive = any(&pos);
    Some(if neg.is_empty() {
        positive
    } else {
        format!("({positive}) NOT ({})", any(&neg))
    })
}

/// Shortest query word [`fallback_exprs`] looks up as part of longer words.
const MIN_PART_CHARS: usize = 4;

/// `MATCH` expressions of the word-part fallback of text search (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fallback {
    /// For `chunks_fts`: words starting with a query word, minus the query's exclusions.
    pub prefix: String,
    /// For `chunks_rev`: words ending with a query word.
    pub suffix: String,
    /// For `chunks_fts`: the query's exclusions, which `chunks_rev` cannot apply.
    pub exclude: Option<String>,
}

/// The word-part fallback of `query`: its positive single words of at least [`MIN_PART_CHARS`]
/// characters outside segmented scripts (whose bigrams already match inside words) and without
/// digits, or `None` if it has none.
pub(crate) fn fallback_exprs(query: &str) -> Option<Fallback> {
    let terms = parse(query);
    let words: Vec<String> = terms
        .iter()
        .filter_map(|t| match &t.atom {
            Atom::Words(w) if !t.exclude && w.len() == 1 => Some(fold(&w[0]).into_owned()),
            _ => None,
        })
        // Words with digits are identifiers (`INV-2041`, `crashtok12`): a prefix of one names
        // other identifiers, not a part of a compound.
        .filter(|w| {
            !has_segmented(w)
                && w.chars().count() >= MIN_PART_CHARS
                && !w.chars().any(char::is_numeric)
        })
        .collect();
    if words.is_empty() {
        return None;
    }
    let prefixes = |rev: bool| {
        words
            .iter()
            .map(|w| {
                let w: String = if rev {
                    w.chars().rev().collect()
                } else {
                    w.clone()
                };
                format!("{} *", quoted(&w))
            })
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let neg: Vec<String> = terms
        .iter()
        .filter(|t| t.exclude)
        .map(|t| render(&t.atom))
        .collect();
    let exclude = (!neg.is_empty()).then(|| neg.join(" OR "));
    let prefix = match &exclude {
        Some(n) => format!("({}) NOT ({n})", prefixes(false)),
        None => prefixes(false),
    };
    Some(Fallback {
        prefix,
        suffix: prefixes(true),
        exclude,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn q(s: &str) -> Option<String> {
        match_expr(s)
    }

    #[test]
    fn plain_words_are_quoted_and_or_ed() {
        assert_eq!(q(""), None);
        assert_eq!(q("  \t\n"), None);
        assert_eq!(q("rust tokio"), Some(r#""rust" OR "tokio""#.into()));
        assert_eq!(q("café über"), Some(r#""café" OR "über""#.into()));
        assert_eq!(q("F28 (ignition)"), Some(r#""F28" OR "ignition""#.into()));
    }

    #[test]
    fn segment_splits_cjk_and_thai_runs_into_bigrams() {
        assert!(matches!(segment("café über 120"), Cow::Borrowed(_)));
        assert_eq!(segment("星商事ロゴ"), " 星商 商事 事ロ ロゴ ");
        assert_eq!(segment("ABC星商(金)"), "ABC 星商 ( 金 )");
        assert_eq!(segment("다음 주"), " 다음   주 ");
        // Thai vowel and tone marks are dropped inside a run.
        assert_eq!(segment("ประชุม"), " ปร ระ ะช ชม ");
    }

    #[test]
    fn cjk_query_words_become_bigram_phrases() {
        assert_eq!(q("星商事"), Some(r#""星商 商事""#.into()));
        assert_eq!(q("Hoshi 見積り"), Some(r#""Hoshi" OR "見積 積り""#.into()));
        assert_eq!(q("金"), Some(r#""金" *"#.into()));
        assert_eq!(q("お見積*"), Some(r#""お見 見積" *"#.into()));
        assert_eq!(q(r#""星商事 ロゴ""#), Some(r#""星商 商事 ロゴ""#.into()));
        assert_eq!(q("x -上海"), Some(r#"("x") NOT ("上海")"#.into()));
        // Thai marks do not split a query word.
        assert_eq!(q("ประชุม"), Some(r#""ปร ระ ะช ชม""#.into()));
        assert_eq!(needles("Hoshi -x 見積り"), ["hoshi", "見積り"]);
    }

    #[test]
    fn fold_unifies_width_arabic_and_sharp_s() {
        assert!(matches!(fold("café über 120"), Cow::Borrowed(_)));
        assert_eq!(fold("ＬＥＤ照明 ｷｬﾝﾍﾟｰﾝ"), "LED照明 キャンペーン");
        assert_eq!(fold("فاتورة أحمد إلى مستشفى"), "فاتوره احمد الي مستشفي");
        assert_eq!(fold("مُحَمَّد ـتـ"), "محمد ت");
        assert_eq!(fold("Königstraße"), "Königstrasse");
    }

    #[test]
    fn index_form_appends_variants_after_the_text() {
        assert!(matches!(index_form("plain text"), Cow::Borrowed(_)));
        assert_eq!(
            index_form("Königstraße 14"),
            "Königstrasse 14\nKoenigstrasse"
        );
        assert_eq!(
            index_form("والكراسي للبيت"),
            "والكراسي للبيت\nالكراسي كراسي بيت"
        );
        assert_eq!(index_form("بالكتاب"), "بالكتاب\nالكتاب كتاب");
        assert_eq!(index_form("ＬＥＤ照明"), "LED 照明 ");
        assert!(is_rewritten("Büro"));
        assert!(!is_rewritten("Buro"));
    }

    #[test]
    fn reversed_form_keeps_longer_words_backwards() {
        assert_eq!(reversed_form("Der Lenkungsausschuss"), "ssuhcssuasgnukneL");
        assert_eq!(reversed_form("short 見積書 words"), "");
        assert_eq!(reversed_form("Straße"), "essartS");
    }

    #[test]
    fn fallback_uses_long_single_words() {
        assert_eq!(fallback_exprs("aus 見積書 \"two words\""), None);
        assert_eq!(
            fallback_exprs("Ausschuss sofreír -Bericht"),
            Some(Fallback {
                prefix: r#"("Ausschuss" * OR "sofreír" *) NOT ("Bericht")"#.into(),
                suffix: r#""ssuhcssuA" * OR "ríerfos" *"#.into(),
                exclude: Some(r#""Bericht""#.into()),
            })
        );
        assert_eq!(fallback_exprs("Straße").unwrap().suffix, r#""essartS" *"#);
    }

    #[test]
    fn query_words_are_folded() {
        assert_eq!(q("ＬＥＤ照明"), Some(r#""LED 照明""#.into()));
        assert_eq!(q("Königstraße"), Some(r#""Königstrasse""#.into()));
        assert_eq!(q("فاتورة"), Some(r#""فاتوره""#.into()));
        // A decomposed nukta does not split a word.
        assert_eq!(q("ग\u{0921}\u{093C}"), Some("\"ग\u{0921}\u{093C}\"".into()));
    }

    #[test]
    fn phrases() {
        assert_eq!(
            q(r#"find "the quick fox" now"#),
            Some(r#""find" OR "the quick fox" OR "now""#.into())
        );
        // Punctuation inside a phrase separates its words, as the tokenizer does.
        assert_eq!(q(r#""don't stop-me""#), Some(r#""don t stop me""#.into()));
        // Typographic quotes work too.
        assert_eq!(q("“exact match”"), Some(r#""exact match""#.into()));
        // Empty and punctuation-only phrases vanish.
        assert_eq!(q(r#""" "  ?! " word"#), Some(r#""word""#.into()));
        // An unpaired quote is ignored.
        assert_eq!(q(r#"a "b c"#), Some(r#""a" OR "b" OR "c""#.into()));
        assert_eq!(
            q(r#""a b" "c"#),
            Some(r#""a b" OR "c""#.into()),
            "the third quote has no partner"
        );
    }

    #[test]
    fn prefixes() {
        assert_eq!(q("nug*"), Some(r#""nug" *"#.into()));
        assert_eq!(q("ab* x*"), Some(r#""ab" * OR "x""#.into()), "too short");
        assert_eq!(q("* ** *x"), Some(r#""x""#.into()), "stray stars are inert");
        assert_eq!(q("foo-bar*"), Some(r#""foo" OR "bar" *"#.into()));
        assert_eq!(q("ab**"), Some(r#""ab" *"#.into()));
        // A star after a phrase is inert.
        assert_eq!(q(r#""a b"*"#), Some(r#""a b""#.into()));
    }

    #[test]
    fn exclusions() {
        assert_eq!(q("rust -tokio"), Some(r#"("rust") NOT ("tokio")"#.into()));
        assert_eq!(
            q(r#"-"async std" rust -tok* go"#),
            Some(r#"("rust" OR "go") NOT ("async std" OR "tok" *)"#.into())
        );
        // Exclusions alone match nothing.
        assert_eq!(q("-tokio"), None);
        assert_eq!(q(r#"-"a b""#), None);
        // Not exclusions: inside words, before digits or spaces, doubled.
        assert_eq!(q("well-known"), Some(r#""well" OR "known""#.into()));
        assert_eq!(q("-5 degrees"), Some(r#""5" OR "degrees""#.into()));
        assert_eq!(q("a - b"), Some(r#""a" OR "b""#.into()));
        assert_eq!(q("--x y"), Some(r#""x" OR "y""#.into()));
    }

    #[test]
    fn syntax_is_inert() {
        assert_eq!(
            q("rust AND tokio NEAR(x, 2) OR NOT y"),
            Some(
                r#""rust" OR "AND" OR "tokio" OR "NEAR" OR "x" OR "2" OR "OR" OR "NOT" OR "y""#
                    .into()
            )
        );
        assert_eq!(
            q("title:secret ^start +plus"),
            Some(r#""title" OR "secret" OR "start" OR "plus""#.into())
        );
        assert_eq!(q("\"(*)-^"), None);
        assert_eq!(q(r#""AND" "NOT x""#), Some(r#""AND" OR "NOT x""#.into()));
    }

    #[test]
    fn term_budget_counts_phrase_words() {
        let many = (0..100)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let expr = q(&many).unwrap();
        assert_eq!(expr.matches(" OR ").count(), MAX_QUERY_TERMS - 1);
        let long_phrase = format!("\"{many}\" tail");
        let expr = q(&long_phrase).unwrap();
        assert_eq!(
            expr.split(' ').count(),
            MAX_QUERY_TERMS,
            "phrase truncated, tail dropped"
        );
        assert!(!expr.contains("tail"));
    }

    /// Small deterministic PRNG (xorshift64*), so the fuzz loop needs no extra crate.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn fts_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5 (
                 text, tokenize = 'porter unicode61 remove_diacritics 2'
             );
             INSERT INTO t (text) VALUES
                 ('the quick brown fox'), ('nuggets of wisdom'), ('café über naïve'),
                 ('AND OR NOT NEAR'), ('x² ½ ٣ 中文 テキスト 🙂');",
        )
        .unwrap();
        c
    }

    fn assert_accepted(c: &Connection, input: &str) {
        let Some(expr) = match_expr(input) else {
            return;
        };
        let mut stmt = c
            .prepare_cached("SELECT rowid, bm25(t) FROM t WHERE t MATCH ?1 ORDER BY bm25(t)")
            .unwrap();
        let res: rusqlite::Result<Vec<(i64, f64)>> = stmt
            .query_map([&expr], |r| Ok((r.get(0)?, r.get(1)?)))
            .and_then(|rows| rows.collect());
        if let Err(e) = res {
            panic!("input {input:?} → {expr:?} rejected by FTS5: {e}");
        }
    }

    #[test]
    fn adversarial_inputs_are_accepted_by_fts5() {
        let c = fts_conn();
        for input in [
            "\"",
            "\"\"",
            "\"\"\"",
            "*",
            "-",
            "-\"",
            "-*",
            "a*",
            "ab*",
            "-ab*",
            "(",
            ")",
            "^",
            ":",
            "NEAR(",
            "AND",
            "OR",
            "NOT",
            "a NOT",
            "NOT a",
            "x:y",
            "{a b}:c",
            "a + b",
            "\u{0}",
            "\u{301}",
            "e\u{301}*",
            "\u{200b}",
            "ʰ",
            "ʰ*",
            "ﬁ*",
            "²*",
            "-ʰ x",
            "x -ʰ",
            "“",
            "”“",
            "“a”b”",
            "-“a” b",
            "\"a\"\"b\"",
            "a\"\"b",
            "x²*",
            "-\u{301}",
            "🙂*",
        ] {
            assert_accepted(&c, input);
        }
    }

    #[test]
    fn random_inputs_are_accepted_by_fts5() {
        let c = fts_conn();
        const PIECES: &[&str] = &[
            "\"", "“", "”", "*", "-", "(", ")", ":", "^", "+", " ", "  ", "\t", "\n", "NEAR",
            "AND", "OR", "NOT", "a", "ab", "fox", "nug", "café", "x²", "½", "ʰ", "\u{301}",
            "\u{0}", "\u{200b}", "中", "🙂", "'", ",", "{", "}", "\\", "e\u{301}", "ﬁ", "٣",
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..4000 {
            let input = if round % 2 == 0 {
                // Random bytes, lossily decoded.
                let len = rng.below(24);
                let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                String::from_utf8_lossy(&bytes).into_owned()
            } else {
                // Random sequences of syntax-heavy pieces.
                let len = rng.below(16);
                (0..len).map(|_| PIECES[rng.below(PIECES.len())]).collect()
            };
            assert_accepted(&c, &input);
        }
    }

    #[test]
    fn phrases_prefixes_and_exclusions_match_as_documented() {
        let c = fts_conn();
        let ids = |input: &str| -> Vec<i64> {
            let expr = match_expr(input).unwrap();
            let mut stmt = c
                .prepare("SELECT rowid FROM t WHERE t MATCH ?1 ORDER BY rowid")
                .unwrap();
            stmt.query_map([&expr], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(ids("\"quick brown\""), [1]);
        assert!(ids("\"brown quick\"").is_empty());
        assert_eq!(ids("nugg*"), [2], "prefix, stemmed like the index");
        assert_eq!(ids("wisdom fox"), [1, 2]);
        assert_eq!(ids("wisdom fox -quick"), [2]);
        assert_eq!(ids("cafe NOT"), [3, 4], "operators are plain words");
    }
}
