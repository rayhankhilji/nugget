//! Store read paths and open cost at 100k items in an encrypted store: batched item loads
//! (`get_items`, `item_dates`) against per-item lookups, `source_counts`, and `Store::open` on a
//! large database.
//! Ignored by default (it writes 100k items); run with
//!
//! ```text
//! cargo test -p nugget-store --release --test read_paths_perf -- --ignored --nocapture
//! ```
//!
//! The bounds asserted are loose (CI machines vary); the printed timings are the numbers.

use std::time::{Duration, Instant};

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{NewChunk, NewItem, Store, StoreKey};

const ITEMS: usize = 100_000;
/// Candidates a search loads (a large rerank / date-filter pool).
const CANDIDATES: usize = 300;
const ROUNDS: u32 = 20;

fn iref(i: usize) -> ItemRef {
    ItemRef {
        source: SourceId::new("files:bench"),
        key: ItemKey::new(format!("doc{i:06}.md")),
    }
}

fn time<T>(rounds: u32, mut f: impl FnMut() -> T) -> Duration {
    let start = Instant::now();
    for _ in 0..rounds {
        std::hint::black_box(f());
    }
    start.elapsed() / rounds
}

#[test]
#[ignore = "perf: writes 100k items; run with --release -- --ignored --nocapture"]
fn read_paths_at_100k_items() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nugget.db");
    let key = StoreKey::from_bytes([1u8; 32]);
    {
        let s = Store::open(&path, &key).unwrap();
        let start = Instant::now();
        for i in 0..ITEMS {
            let a = format!("Document {i} talks about topic {} and more. ", i % 97);
            let b = format!("A second paragraph for document {i} with filler words.");
            let it = NewItem::new("files:bench", format!("doc{i:06}.md"), format!("h{i}"))
                .with_title(format!("doc {i}"))
                .with_text(format!("{a}{b}"));
            s.upsert_item_with_chunks(&it, &[NewChunk::text(a.trim_end()), NewChunk::text(b)])
                .unwrap();
        }
        println!("populate {ITEMS} items: {:?}", start.elapsed());
    }

    // Open cost on the large database (no migrations pending), cold-ish and warm.
    let mut opens = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let s = Store::open(&path, &key).unwrap();
        opens.push(start.elapsed());
        drop(s);
    }
    println!("Store::open at {ITEMS} items: {opens:?}");

    let s = Store::open(&path, &key).unwrap();
    // Candidates spread over the table, as search results are.
    let refs: Vec<ItemRef> = (0..CANDIDATES).map(|i| iref(i * 331 % ITEMS)).collect();

    let per_item = time(ROUNDS, || {
        refs.iter()
            .map(|r| s.get_item(r).unwrap())
            .collect::<Vec<_>>()
    });
    let batched = time(ROUNDS, || s.get_items(&refs).unwrap());
    assert_eq!(
        s.get_items(&refs).unwrap(),
        refs.iter()
            .map(|r| s.get_item(r).unwrap())
            .collect::<Vec<_>>()
    );
    println!("load {CANDIDATES} items: per-item get_item {per_item:?}, get_items {batched:?}");

    let dates_per_item = time(ROUNDS, || {
        refs.iter()
            .map(|r| s.item_dates(std::slice::from_ref(r)).unwrap())
            .collect::<Vec<_>>()
    });
    let dates_batched = time(ROUNDS, || s.item_dates(&refs).unwrap());
    println!(
        "dates of {CANDIDATES} items: one call per item {dates_per_item:?}, one call {dates_batched:?}"
    );

    let counts = time(ROUNDS, || s.source_counts().unwrap());
    let got = s.source_counts().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].items, got[0].chunks),
        (ITEMS as u64, 2 * ITEMS as u64)
    );
    println!("source_counts: {counts:?}");

    assert!(batched < Duration::from_secs(1));
    assert!(dates_batched < Duration::from_secs(1));
}
