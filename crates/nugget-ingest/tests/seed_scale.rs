//! Seeds a daemon home with a synthetic store for profiling `nuggetd` at scale (startup, idle
//! CPU, knowledge runs). Ignored: run it by hand, then start `nuggetd --home <dir>`.
//!
//! ```text
//! NUGGET_KEYCHAIN=off NUGGET_SEED_HOME=/tmp/home NUGGET_SEED_ITEMS=100000 \
//!   cargo test -p nugget-ingest --release --test seed_scale -- --ignored --nocapture
//! ```
//!
//! Items are written directly with `Store::upsert_item_with_chunks` under source `seed` (not in
//! `sources.toml`, so no connector reconciles them away), each with two chunks carrying a
//! 384-dimension vector, and text mentioning a few recurring people, organisations and emails
//! so deterministic extraction has something to find.

use std::path::PathBuf;
use std::time::Instant;

use nugget_core::paths::Paths;
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};

const DIM: usize = 384;
const FIRST: &[&str] = &[
    "Alice", "Bruno", "Chiara", "Deniz", "Elif", "Farid", "Greta", "Hugo",
];
const LAST: &[&str] = &[
    "Ashgrove",
    "Brightwater",
    "Coldbrook",
    "Dunmore",
    "Elmstead",
];
const ORGS: &[&str] = &[
    "Glimmerfen",
    "Heronworks",
    "Ironvale",
    "Juniper Labs",
    "Kestrel Co",
];

/// A cheap deterministic pseudo-random unit vector.
fn vector(seed: u64) -> Vec<f32> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v: Vec<f32> = (0..DIM)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 2001) as f32 / 1000.0 - 1.0
        })
        .collect();
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt().max(1e-6);
    v.iter_mut().for_each(|a| *a /= n);
    v
}

#[test]
#[ignore = "profiling helper: seeds NUGGET_SEED_HOME"]
fn seed_scale_home() {
    let Some(home) = std::env::var_os("NUGGET_SEED_HOME").map(PathBuf::from) else {
        panic!("set NUGGET_SEED_HOME");
    };
    let n: usize = std::env::var("NUGGET_SEED_ITEMS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);
    let paths = Paths::rooted(home);
    std::fs::create_dir_all(&paths.data).expect("create home");
    let key = StoreKey::load_or_create(&paths).expect("store key");
    let store = Store::open(paths.db_file(), &key).expect("open store");
    let started = Instant::now();
    for i in 0..n {
        let who = |k: usize| {
            format!(
                "{} {}",
                FIRST[(i + k) % FIRST.len()],
                LAST[(i / 7 + k) % LAST.len()]
            )
        };
        let org = ORGS[i % ORGS.len()];
        let (a, b) = (who(0), who(3));
        let body = format!(
            "Meeting notes {i}. {a} met {b} from {org} to discuss the quarterly plan. \
             Follow up with {b} at {}.{}@{}.test about item {i} next week.",
            FIRST[(i + 3) % FIRST.len()].to_lowercase(),
            LAST[(i / 7 + 3) % LAST.len()].to_lowercase(),
            org.split(' ').next().unwrap_or(org).to_lowercase(),
        );
        let second = format!("Action items for {org}: review budget {i}; send the summary to {a}.");
        let text = format!("{body}\n\n{second}");
        let item = NewItem::new(
            "seed",
            format!("notes/{:03}/note-{i}.md", i % 997),
            content_hash(&text),
        )
        .with_title(format!("Meeting notes {i}"))
        .with_mime("text/markdown")
        .with_version(format!("v1-{i}"))
        .with_text(text.clone());
        let chunks = vec![
            NewChunk::embedded(body, vector(i as u64 * 2)),
            NewChunk::embedded(second, vector(i as u64 * 2 + 1)),
        ];
        store
            .upsert_item_with_chunks(&item, &chunks)
            .expect("write item");
        if (i + 1) % 10_000 == 0 {
            eprintln!("{} items in {:.1}s", i + 1, started.elapsed().as_secs_f64());
        }
    }
    store.flush_egress();
    eprintln!(
        "seeded {n} items in {:.1}s",
        started.elapsed().as_secs_f64()
    );
}
