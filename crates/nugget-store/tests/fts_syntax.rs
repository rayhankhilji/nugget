//! End-to-end full-text query syntax through [`Store::search_text`] and
//! [`Store::search_hybrid`]: phrases, prefixes, exclusions, and inert FTS5 operators.

use nugget_store::{Hit, NewChunk, NewItem, Store, content_hash};

fn store_with(docs: &[(&str, &str, &str)]) -> Store {
    let s = Store::open_in_memory().expect("open in-memory store");
    for (key, title, body) in docs {
        let it = NewItem::new("files:test", *key, content_hash(body))
            .with_title(*title)
            .with_text(*body);
        s.upsert_item(&it).unwrap();
        s.replace_chunks(&it.item, &[NewChunk::text(*body)])
            .unwrap();
    }
    s
}

fn keys(hits: &[Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.item.key.as_str()).collect()
}

fn corpus() -> Store {
    store_with(&[
        (
            "phrase",
            "Notes",
            "the boiler pressure valve needs replacing soon",
        ),
        (
            "scattered",
            "Misc",
            "pressure at work; the valve on the bike; a boiler",
        ),
        (
            "nuggets",
            "Prospecting",
            "we found three gold nuggets in the creek",
        ),
        ("other", "Groceries", "milk, eggs and bread"),
    ])
}

#[test]
fn quoted_phrase_requires_adjacent_words() {
    let s = corpus();
    assert_eq!(
        keys(&s.search_text("\"pressure valve\"", 10).unwrap()),
        ["phrase"]
    );
    // Word order matters inside a phrase.
    assert!(s.search_text("\"valve pressure\"", 10).unwrap().is_empty());
    // Without quotes both documents match.
    let loose_hits = s.search_text("pressure valve", 10).unwrap();
    let mut loose = keys(&loose_hits);
    loose.sort_unstable();
    assert_eq!(loose, ["phrase", "scattered"]);
}

#[test]
fn phrase_ors_with_plain_words_and_ranks_first() {
    let s = corpus();
    // Both documents contain every word; the one with the phrase ranks first.
    let hits = s.search_text("\"pressure valve\" boiler", 10).unwrap();
    assert_eq!(keys(&hits), ["phrase", "scattered"]);
    // A phrase is one more OR-ed term: other words still find their own documents.
    let hits = s.search_text("\"pressure valve\" milk", 10).unwrap();
    let mut k = keys(&hits);
    k.sort_unstable();
    assert_eq!(k, ["other", "phrase"]);
    // Stopwords inside a phrase still have to line up.
    assert_eq!(
        keys(&s.search_text("\"found three gold\"", 10).unwrap()),
        ["nuggets"]
    );
    // Typographic quotes behave the same.
    assert_eq!(
        keys(&s.search_text("“pressure valve”", 10).unwrap()),
        ["phrase"]
    );
}

#[test]
fn prefix_terms_match_word_starts() {
    let s = corpus();
    assert_eq!(keys(&s.search_text("nug*", 10).unwrap()), ["nuggets"]);
    assert_eq!(
        keys(&s.search_text("prospect*", 10).unwrap()),
        ["nuggets"],
        "title prefix"
    );
    // Without the star, "nug" is a whole word that appears nowhere.
    assert!(s.search_text("nug", 10).unwrap().is_empty());
    // One-character prefixes are plain words, not match-everything.
    assert!(s.search_text("b*", 10).unwrap().is_empty());
}

#[test]
fn exclusions_filter_positive_matches() {
    let s = corpus();
    assert_eq!(
        keys(&s.search_text("pressure -bike", 10).unwrap()),
        ["phrase"]
    );
    assert_eq!(
        keys(&s.search_text("valve -\"on the bike\"", 10).unwrap()),
        ["phrase"]
    );
    // An exclusion alone matches nothing; a hyphen inside a word is not an exclusion.
    assert!(s.search_text("-bike", 10).unwrap().is_empty());
    assert_eq!(keys(&s.search_text("three-gold", 10).unwrap()), ["nuggets"]);
}

#[test]
fn operators_stay_inert_end_to_end() {
    let s = corpus();
    for q in [
        "milk AND NOT",
        "NEAR(milk eggs",
        "text:milk",
        "^milk",
        "milk\"",
        "\"milk",
        "milk) OR (",
        "{text}: milk",
        "milk + * -",
    ] {
        assert_eq!(
            keys(&s.search_text(q, 10).unwrap()),
            ["other"],
            "query {q:?}"
        );
    }
    for q in ["", "\"\"", "*", "-", "\"(*)-^", "-\"a b\""] {
        assert!(s.search_text(q, 10).unwrap().is_empty(), "query {q:?}");
    }
}

#[test]
fn hybrid_search_uses_the_same_syntax() {
    let s = corpus();
    assert_eq!(
        keys(&s.search_hybrid("\"pressure valve\"", None, 10).unwrap()),
        ["phrase"]
    );
    assert_eq!(
        keys(&s.search_hybrid("nugg*", None, 10).unwrap()),
        ["nuggets"]
    );
}
