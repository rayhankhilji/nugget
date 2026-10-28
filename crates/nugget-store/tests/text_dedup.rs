//! Item text is stored once (schema v6): when chunks cover an item's text, `items.text` is NULL
//! and the text is rebuilt from a layout over the chunks. These tests check that every API keeps
//! returning byte-identical text and unchanged search results, across all write paths.

use std::path::{Path, PathBuf};

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{Error, NewChunk, NewItem, Store, StoreKey, UpsertOutcome, content_hash};

const KEY: [u8; 32] = [3u8; 32];

fn open(dir: &tempfile::TempDir, name: &str) -> (Store, PathBuf) {
    let path = dir.path().join(name);
    (
        Store::open(&path, &StoreKey::from_bytes(KEY)).unwrap(),
        path,
    )
}

/// Plain connection to the encrypted file, for inspecting how text is stored.
fn raw(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    let hex: String = KEY.iter().map(|b| format!("{b:02x}")).collect();
    conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
        .unwrap();
    conn
}

fn count(path: &Path, sql: &str) -> i64 {
    raw(path).query_row(sql, [], |r| r.get(0)).unwrap()
}

fn iref(key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new("files:docs"),
        key: ItemKey::new(key),
    }
}

fn new_item(key: &str, text: &str) -> NewItem {
    NewItem::new("files:docs", key, content_hash(text))
        .with_title(format!("Doc {key}"))
        .with_text(text)
}

fn text_of(s: &Store, key: &str) -> Option<String> {
    s.get_item(&iref(key)).unwrap().unwrap().text
}

// ---------------------------------------------------------------- generated corpus

/// xorshift64*: deterministic, dependency-free.
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
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
}

const WORDS: &[&str] = &[
    "the",
    "boiler",
    "service",
    "invoice",
    "meeting",
    "garden",
    "naïve",
    "café",
    "東京",
    "会議",
    "Ελλάδα",
    "über",
    "e\u{301}clair",
    "🎉",
    "résumé",
    "data",
    "Sam",
    "Acme",
    "valve",
    "plan",
    "😀👍",
    "한국어",
    "x",
    "longerwordwithoutanybreaksatallforhardsplits",
    "end.",
    "why?",
];

const GAPS: &[&str] = &[
    "\n\n",
    "\n \n",
    "\n\t\n\n",
    "\r\n\r\n",
    "\n\n\n  ",
    "\n\u{3000}\n",
];

/// A document: paragraphs of unicode words, varied whitespace-only gaps, repeated passages.
fn gen_doc(rng: &mut Rng) -> String {
    let mut paras: Vec<String> = Vec::new();
    for _ in 0..1 + rng.below(25) {
        if !paras.is_empty() && rng.chance(15) {
            let again = paras[rng.below(paras.len())].clone();
            paras.push(again);
            continue;
        }
        let mut p = String::new();
        for w in 0..1 + rng.below(60) {
            if w > 0 {
                p.push_str(if rng.chance(8) { "\n" } else { " " });
            }
            p.push_str(WORDS[rng.below(WORDS.len())]);
        }
        paras.push(p);
    }
    let mut doc = String::new();
    if rng.chance(30) {
        doc.push_str("  \n");
    }
    for (i, p) in paras.iter().enumerate() {
        if i > 0 {
            doc.push_str(GAPS[rng.below(GAPS.len())]);
        }
        doc.push_str(p);
    }
    if rng.chance(30) {
        doc.push_str("\n\n ");
    }
    doc
}

/// Trimmed, non-empty spans of the text separated by blank lines.
fn para_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    let mut start: Option<usize> = None;
    let mut last_end = 0;
    for line in text.split_inclusive('\n') {
        let ls = pos;
        pos += line.len();
        if line.trim().is_empty() {
            if let Some(st) = start.take() {
                out.push(trim(text, st, last_end));
            }
        } else {
            start.get_or_insert(ls);
            last_end = pos;
        }
    }
    if let Some(st) = start {
        out.push(trim(text, st, last_end));
    }
    out
}

fn trim(text: &str, a: usize, b: usize) -> (usize, usize) {
    let s = &text[a..b];
    (
        a + (s.len() - s.trim_start().len()),
        b - (s.len() - s.trim_end().len()),
    )
}

/// Chunks in the shapes the extract chunker produces (trimmed spans, word-aligned overlap with
/// the previous chunk, optional heading-path prefix line, hard splits inside long paragraphs),
/// plus, rarely, a whitespace-normalised chunk that does not occur in the text.
fn gen_chunks(rng: &mut Rng, text: &str) -> Vec<NewChunk> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (a, b) in para_spans(text) {
        // Sometimes split a paragraph at an arbitrary char boundary.
        if rng.chance(25) {
            let cuts: Vec<usize> = text[a..b].char_indices().map(|(i, _)| a + i).collect();
            let cut = cuts[rng.below(cuts.len())];
            if cut > a {
                spans.push((a, cut));
                spans.push((cut, b));
                continue;
            }
        }
        spans.push((a, b));
    }
    let mut out = Vec::new();
    let mut i = 0;
    let mut prev: Option<(usize, usize)> = None;
    while i < spans.len() {
        let take = 1 + rng.below(3).min(spans.len() - i - 1);
        let (mut cs, ce) = (spans[i].0, spans[i + take - 1].1);
        i += take;
        if let Some((ps, pe)) = prev
            && rng.chance(60)
        {
            // Overlap: start at a word start inside the previous chunk.
            let starts: Vec<usize> = text[ps..pe]
                .char_indices()
                .filter(|&(j, c)| {
                    !c.is_whitespace()
                        && j > 0
                        && text[..ps + j]
                            .chars()
                            .next_back()
                            .is_some_and(char::is_whitespace)
                })
                .map(|(j, _)| ps + j)
                .collect();
            if !starts.is_empty() {
                cs = starts[rng.below(starts.len())];
            }
        }
        let body = &text[cs..ce];
        let chunk = if rng.chance(3) {
            body.split_whitespace().collect::<Vec<_>>().join(" ")
        } else if rng.chance(30) {
            format!("Guide > Section {}\n{body}", rng.below(9))
        } else {
            body.to_owned()
        };
        out.push(NewChunk::text(chunk));
        prev = Some((cs, ce));
    }
    out
}

fn corpus(seed: u64, n: usize) -> Vec<(String, Vec<NewChunk>)> {
    let mut rng = Rng(seed);
    (0..n)
        .map(|_| {
            let doc = gen_doc(&mut rng);
            let chunks = gen_chunks(&mut rng, &doc);
            (doc, chunks)
        })
        .collect()
}

// ---------------------------------------------------------------- tests

#[test]
fn generated_documents_round_trip_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let (s, path) = open(&dir, "rt.db");
    let docs = corpus(0x5eed_1234, 600);
    for (i, (doc, chunks)) in docs.iter().enumerate() {
        let out = s
            .upsert_item_with_chunks(&new_item(&i.to_string(), doc), chunks)
            .unwrap();
        assert_eq!(out, UpsertOutcome::Inserted);
    }
    for (i, (doc, chunks)) in docs.iter().enumerate() {
        assert_eq!(
            text_of(&s, &i.to_string()).as_deref(),
            Some(doc.as_str()),
            "doc {i}"
        );
        // Same content again is still recognised as unchanged.
        let again = s
            .upsert_item_with_chunks(&new_item(&i.to_string(), doc), chunks)
            .unwrap();
        assert_eq!(again, UpsertOutcome::Unchanged, "doc {i}");
    }
    let compact = count(&path, "SELECT count(*) FROM items WHERE text IS NULL");
    let layouts = count(&path, "SELECT count(*) FROM item_text");
    assert_eq!(compact, layouts);
    assert!(
        compact > 400,
        "most items stored without duplicate text: {compact}/600"
    );

    // Re-chunking every item (as re-embedding or a chunker change does) keeps the text.
    let mut rng = Rng(77);
    for (i, (doc, _)) in docs.iter().enumerate() {
        let chunks = if i % 5 == 0 {
            Vec::new()
        } else {
            gen_chunks(&mut rng, doc)
        };
        s.replace_chunks(&iref(&i.to_string()), &chunks).unwrap();
        assert_eq!(
            text_of(&s, &i.to_string()).as_deref(),
            Some(doc.as_str()),
            "doc {i}"
        );
    }
}

#[test]
fn search_results_match_a_store_with_duplicated_text() {
    let dir = tempfile::tempdir().unwrap();
    let (compact, _) = open(&dir, "compact.db");
    let (legacy, legacy_path) = open(&dir, "legacy.db");
    let docs = corpus(42, 150);
    for (i, (doc, chunks)) in docs.iter().enumerate() {
        let it = new_item(&i.to_string(), doc);
        compact.upsert_item_with_chunks(&it, chunks).unwrap();
        // Item first, chunks separately: text stays in `items.text`, as before schema v6.
        legacy.upsert_item(&it).unwrap();
        legacy.replace_chunks(&it.item, chunks).unwrap();
    }
    assert_eq!(count(&legacy_path, "SELECT count(*) FROM item_text"), 0);
    for q in [
        "boiler",
        "東京",
        "naïve café",
        "Ελλάδα",
        "invoice meeting",
        "eclair",
        "guide",
    ] {
        let a = compact.search_text(q, 20).unwrap();
        let b = legacy.search_text(q, 20).unwrap();
        let view = |h: &[nugget_store::Hit]| -> Vec<(String, String)> {
            h.iter()
                .map(|h| (h.item.key.as_str().to_owned(), h.snippet.clone()))
                .collect()
        };
        assert_eq!(view(&a), view(&b), "query {q:?}");
    }
    for i in 0..docs.len() {
        assert_eq!(
            text_of(&compact, &i.to_string()),
            text_of(&legacy, &i.to_string())
        );
    }
}

#[test]
fn every_write_path_keeps_text_and_change_feed_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let (s, path) = open(&dir, "paths.db");
    let layouts = |p: &Path| count(p, "SELECT count(*) FROM item_text");
    let body = "Sam from Acme serviced the boiler on Tuesday. ".repeat(20);
    let text = format!("{body}\n\n  Invoice 42 is due next week, says naïve café owner.\n");
    let paras: Vec<&str> = text.split("\n\n").map(str::trim).collect();
    let chunks = vec![NewChunk::text(paras[0]), NewChunk::text(paras[1])];
    let it = new_item("a", &text);

    assert_eq!(
        s.upsert_item_with_chunks(&it, &chunks).unwrap(),
        UpsertOutcome::Inserted
    );
    assert_eq!(layouts(&path), 1);
    assert_eq!(
        count(&path, "SELECT count(*) FROM items WHERE text IS NULL"),
        1
    );
    let stored = s.get_item(&it.item).unwrap().unwrap();
    assert_eq!(stored.text.as_deref(), Some(text.as_str()));
    let head = s.item_changes_head().unwrap();

    // Plain re-upsert of identical fields: unchanged, nothing written.
    assert_eq!(s.upsert_item(&it).unwrap(), UpsertOutcome::Unchanged);
    // Re-embedding the same chunks: the layout survives and the item row is untouched.
    let embedded: Vec<NewChunk> = paras
        .iter()
        .map(|p| NewChunk::embedded(*p, vec![1.0, 0.5]))
        .collect();
    s.replace_chunks(&it.item, &embedded).unwrap();
    assert_eq!(
        s.item_changes_head().unwrap(),
        head,
        "re-chunking is not an item change"
    );
    let after = s.get_item(&it.item).unwrap().unwrap();
    assert_eq!(after, stored, "text and updated_at unchanged");
    assert_eq!(layouts(&path), 1);

    // Chunks that no longer cover the text, then none at all: text kept verbatim.
    s.replace_chunks(&it.item, &[NewChunk::text("unrelated words")])
        .unwrap();
    assert_eq!(text_of(&s, "a").as_deref(), Some(text.as_str()));
    s.replace_chunks(&it.item, &[]).unwrap();
    assert_eq!(text_of(&s, "a").as_deref(), Some(text.as_str()));
    // Late chunks for an item without any (interrupted earlier write): compacted again.
    assert_eq!(
        s.upsert_item_with_chunks(&it, &chunks).unwrap(),
        UpsertOutcome::Updated
    );
    assert_eq!(s.get_item(&it.item).unwrap().unwrap(), stored);
    assert_eq!(s.item_changes_head().unwrap(), head);

    // Field-only update (same hash and text) through `upsert_item`: chunks and layout kept.
    let retitled = it.clone().with_title("Renamed");
    assert_eq!(s.upsert_item(&retitled).unwrap(), UpsertOutcome::Updated);
    assert_eq!(text_of(&s, "a").as_deref(), Some(text.as_str()));
    assert_eq!(layouts(&path), 1);
    assert!(s.item_changes_head().unwrap() > head);

    // Same text but the caller drops it: text becomes None, layout removed.
    let mut no_text = retitled.clone();
    no_text.text = None;
    assert_eq!(s.upsert_item(&no_text).unwrap(), UpsertOutcome::Updated);
    assert_eq!(text_of(&s, "a"), None);
    assert_eq!(layouts(&path), 0);
    assert_eq!(
        s.upsert_item_with_chunks(&retitled, &chunks).unwrap(),
        UpsertOutcome::Updated
    );
    assert_eq!(text_of(&s, "a").as_deref(), Some(text.as_str()));

    // New content via `upsert_item`: stale chunks and layout dropped, new text stored inline.
    let newer = new_item("a", "completely new body");
    assert_eq!(s.upsert_item(&newer).unwrap(), UpsertOutcome::Updated);
    assert_eq!(text_of(&s, "a").as_deref(), Some("completely new body"));
    assert_eq!(layouts(&path), 0);
    assert_eq!(s.counts().unwrap().chunks, 0);

    // Text-less items with chunks stay text-less.
    let mut bare = new_item("b", &text);
    bare.text = None;
    s.upsert_item_with_chunks(&bare, &chunks).unwrap();
    assert_eq!(text_of(&s, "b"), None);

    // Deletes take the layout with them (item and whole source).
    s.upsert_item_with_chunks(&new_item("c", &text), &chunks)
        .unwrap();
    s.upsert_item_with_chunks(&new_item("d", &text), &chunks)
        .unwrap();
    assert_eq!(layouts(&path), 2);
    assert!(s.delete_item(&iref("c")).unwrap());
    assert_eq!(layouts(&path), 1);
    s.delete_source(&SourceId::new("files:docs")).unwrap();
    assert_eq!(layouts(&path), 0);
    assert_eq!(
        count(&path, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
}

#[test]
fn a_corrupt_layout_is_an_integrity_error_not_wrong_text() {
    let dir = tempfile::tempdir().unwrap();
    let (s, path) = open(&dir, "corrupt.db");
    let text = "one two three four five six seven eight nine ten ".repeat(10);
    s.upsert_item_with_chunks(&new_item("a", &text), &[NewChunk::text(text.trim_end())])
        .unwrap();
    assert_eq!(text_of(&s, "a").as_deref(), Some(text.as_str()));
    raw(&path)
        .execute("UPDATE chunks SET text = 'short'", [])
        .unwrap();
    assert!(matches!(s.get_item(&iref("a")), Err(Error::Integrity(_))));
}

/// Database size for one corpus, stored the pre-v6 way (text in `items.text` and in chunks)
/// and the v6 way. Prints the numbers; asserts a clear saving.
#[test]
fn text_storage_size_before_and_after() {
    let dir = tempfile::tempdir().unwrap();
    let (compact, compact_path) = open(&dir, "size_compact.db");
    let (legacy, legacy_path) = open(&dir, "size_legacy.db");
    // Long documents, mostly unprefixed chunks: the common ingest shape.
    let mut rng = Rng(9);
    let mut text_bytes = 0;
    let mut chunk_bytes = 0;
    for i in 0..300 {
        let doc = (0..6)
            .map(|_| gen_doc(&mut rng))
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = gen_chunks(&mut rng, &doc);
        text_bytes += doc.len();
        chunk_bytes += chunks.iter().map(|c| c.text.len()).sum::<usize>();
        let it = new_item(&i.to_string(), &doc);
        compact.upsert_item_with_chunks(&it, &chunks).unwrap();
        legacy.upsert_item(&it).unwrap();
        legacy.replace_chunks(&it.item, &chunks).unwrap();
    }
    drop((compact, legacy));
    let used = |p: &Path| -> (i64, i64) {
        let c = raw(p);
        c.execute_batch("VACUUM;").unwrap();
        let pages: i64 = c
            .query_row("SELECT page_count FROM pragma_page_count", [], |r| r.get(0))
            .unwrap();
        drop(c);
        let size = std::fs::metadata(p).unwrap().len() as i64;
        (pages, size)
    };
    let (lp, lb) = used(&legacy_path);
    let (cp, cb) = used(&compact_path);
    let layout_bytes = count(
        &compact_path,
        "SELECT coalesce(sum(length(layout)), 0) FROM item_text",
    );
    let inline = count(
        &compact_path,
        "SELECT count(*) FROM items WHERE text IS NOT NULL",
    );
    println!(
        "corpus: 300 items, {text_bytes} text bytes, {chunk_bytes} chunk bytes; \
         before: {lp} pages ({lb} B); after: {cp} pages ({cb} B), {:.1}% smaller; \
         layouts {layout_bytes} B, {inline} items still inline",
        100.0 * (lb - cb) as f64 / lb as f64
    );
    assert!(cb < lb, "compact store is smaller");
    assert!(
        (lb - cb) as f64 > 0.8 * text_bytes as f64,
        "saves most of the item text: {} of {text_bytes}",
        lb - cb
    );
}
