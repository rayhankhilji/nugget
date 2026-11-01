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

use std::borrow::Cow;

use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Value, ValueRef};

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

/// Register `fts_segment(text)` ([`segment`] as SQL; non-text values pass through) on `conn`.
/// The FTS triggers call it, so every connection that writes items or chunks needs it.
pub(crate) fn register_functions(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "fts_segment",
        1,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| {
            Ok(match ctx.get_raw(0) {
                ValueRef::Text(b) => Value::Text(segment(&String::from_utf8_lossy(b)).into_owned()),
                raw => Value::try_from(raw)?,
            })
        },
    )
}

/// Characters that continue a word: alphanumerics, and marks inside a segmented run.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || is_run_mark(c)
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
                    segment(w)
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
            let text = segment(word)
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

/// A query's rendered FTS5 terms: the OR-ed positive ones and the excluded ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchParts {
    /// Each a self-contained FTS5 expression (a string, a prefix, or an OR of bigrams).
    pub positive: Vec<String>,
    pub negative: Vec<String>,
}

impl MatchParts {
    /// `terms` OR-ed, minus the excluded terms: the expression for these positive terms.
    pub(crate) fn expr_of(&self, terms: &[&str]) -> String {
        self.with_exclusions(terms.join(" OR "))
    }

    /// `expr` minus the excluded terms.
    pub(crate) fn with_exclusions(&self, expr: String) -> String {
        if self.negative.is_empty() {
            expr
        } else {
            format!("({expr}) NOT ({})", self.negative.join(" OR "))
        }
    }

    /// The whole query's expression.
    pub(crate) fn expr(&self) -> String {
        let all: Vec<&str> = self.positive.iter().map(String::as_str).collect();
        self.expr_of(&all)
    }
}

/// [`match_expr`]'s parts, or `None` if the query has no positive term.
pub(crate) fn match_parts(query: &str) -> Option<MatchParts> {
    let terms = parse(query);
    let (neg, pos): (Vec<&Term>, Vec<&Term>) = terms.iter().partition(|t| t.exclude);
    if pos.is_empty() {
        return None;
    }
    let render_all = |ts: &[&Term]| ts.iter().map(|t| render(&t.atom)).collect::<Vec<_>>();
    Some(MatchParts {
        positive: render_all(&pos),
        negative: render_all(&neg),
    })
}

/// The FTS5 `MATCH` expression for user text, or `None` if it has no positive term.
pub(crate) fn match_expr(query: &str) -> Option<String> {
    match_parts(query).map(|p| p.expr())
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
