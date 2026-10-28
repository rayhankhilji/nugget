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
                        .split(|c| !c.is_alphanumeric())
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
            while end < n && cs[end].is_alphanumeric() {
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

/// `s` as an FTS5 string literal.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn render(atom: &Atom) -> String {
    match atom {
        Atom::Words(words) => quoted(&words.join(" ")),
        Atom::Prefix(word) => format!("{} *", quoted(word)),
    }
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
