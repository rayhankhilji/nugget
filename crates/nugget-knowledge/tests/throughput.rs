//! Extraction throughput on a synthetic corpus, against the real graph (`SqliteGraph` in an
//! encrypted on-disk store). Ignored by default; run with
//!
//! ```text
//! cargo test -p nugget-knowledge --test throughput -- --ignored --nocapture
//! ```
//!
//! Set `NUGGET_TP_ITEMS` to change the corpus size (default 1000).

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::*;
use nugget_graph::GraphStore;
use nugget_knowledge::{Knowledge, KnowledgeConfig, MeConfig, RunBudget};
use nugget_store::{NewChunk, Store, StoreKey};

/// Small deterministic PRNG (xorshift64*), so runs are comparable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const FIRST: [&str; 10] = [
    "Sam", "Alex", "Robin", "Kim", "Noor", "Jo", "Ari", "Lee", "Mika", "Dana",
];
const LAST: [&str; 10] = [
    "Smith",
    "Okafor",
    "Lindqvist",
    "Tanaka",
    "Moreau",
    "Silva",
    "Novak",
    "Haddad",
    "Kaur",
    "Berg",
];

fn person(i: usize) -> (String, String) {
    let name = format!("{} {}{}", FIRST[i % 10], LAST[(i / 10) % 10], i / 100);
    let domain = format!("org{}.example", i % 25);
    let email = format!("{}.{}@{}", FIRST[i % 10], i, domain).to_lowercase();
    (name, email)
}

fn mail(rng: &mut Rng, n: usize, edit: u32) -> (nugget_store::NewItem, String) {
    let (from_name, from_email) = person(rng.below(300));
    let mut to = vec!["Tanuj <tanuj@trylumi.co>".to_owned()];
    for _ in 0..rng.below(3) {
        let (name, email) = person(rng.below(300));
        to.push(format!("{name} <{email}>"));
    }
    let from = format!("{from_name} <{from_email}>");
    let to_refs: Vec<&str> = to.iter().map(String::as_str).collect();
    let body = format!(
        "Hi, following up on thread {n} (rev {edit}). Let's sync about project {} next week.",
        rng.below(40)
    );
    let date = format!(
        "2026-{:02}-{:02}T10:00:00Z",
        1 + rng.below(9),
        1 + rng.below(28)
    );
    let key = format!("INBOX/{n}");
    let item = Mail {
        key: &key,
        from: &from,
        to: &to_refs,
        date: &date,
        subject: "Sync",
        body: &body,
        ..Default::default()
    }
    .item();
    (item, body)
}

fn cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            name: Some("Tanuj".into()),
            emails: vec!["tanuj@trylumi.co".into()],
            infer_from_sent: true,
        },
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "synthetic throughput run; use --ignored --nocapture"]
async fn extraction_throughput() {
    let n: usize = std::env::var("NUGGET_TP_ITEMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("nugget.db"), &StoreKey::from_bytes([7; 32])).unwrap();
    let graph = Arc::new(store.graph());
    let mut rng = Rng(0x5eed);
    for i in 0..n {
        let (item, body) = mail(&mut rng, i, 0);
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text(body)])
            .unwrap();
    }
    let k = Knowledge::new(store.clone(), graph.clone(), None, cfg());

    let t = Instant::now();
    let first = k.run_once(RunBudget::unlimited()).await.unwrap();
    let initial = t.elapsed();
    assert_eq!(first.items_processed as usize, n);
    assert_eq!(first.items_failed, 0);

    // Nothing changed: the planning cost alone.
    let t = Instant::now();
    let idle = k.run_once(RunBudget::unlimited()).await.unwrap();
    let noop = t.elapsed();
    assert_eq!(idle.items_processed, 0);

    // 10% edited, 5% deleted.
    for i in (0..n).step_by(10) {
        let (item, body) = mail(&mut rng, i, 1);
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text(body)])
            .unwrap();
    }
    for i in (5..n).step_by(20) {
        store
            .delete_item(&item_ref("imap:me@corp.example", &format!("INBOX/{i}")))
            .unwrap();
    }
    let t = Instant::now();
    let inc = k.run_once(RunBudget::unlimited()).await.unwrap();
    let incremental = t.elapsed();

    let counts = graph.counts().unwrap();
    println!(
        "items {n}: initial {:.2} s ({:.0} items/s), idle re-run {:.0} ms, incremental \
         ({} changed, {} deleted) {:.2} s",
        initial.as_secs_f64(),
        n as f64 / initial.as_secs_f64(),
        noop.as_secs_f64() * 1e3,
        inc.items_processed,
        inc.items_deleted,
        incremental.as_secs_f64(),
    );
    println!(
        "graph: {} entities, {} active claims, {} mentions",
        counts.entities, counts.claims, counts.mentions
    );
}
