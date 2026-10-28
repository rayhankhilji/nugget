//! Knowledge-graph performance at personal scale: 10k entities, 50k claims, 100k mentions in an
//! encrypted store. Ignored by default (it loads ~160k rows); run with
//!
//! ```text
//! cargo test -p nugget-store --release --test graph_perf -- --ignored --nocapture
//! ```
//!
//! The bounds asserted are deliberately loose (CI machines vary); the printed timings are the
//! numbers to look at.

use std::time::{Duration, Instant};

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, EntityQuery, Evidence, GraphStore, Mention, NewClaim,
    NewEntity,
};
use nugget_store::{Store, StoreKey};

const ENTITIES: usize = 10_000;
const CLAIMS: usize = 50_000;
const MENTIONS: usize = 100_000;
const ITEMS: usize = 5_000;
/// Claims attached to entity 0, to measure a hub (e.g. the user themself).
const HUB_CLAIMS: usize = 2_000;

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

fn item(i: usize) -> ItemRef {
    ItemRef {
        source: SourceId::new("mail"),
        key: ItemKey::new(format!("msg-{i}")),
    }
}

fn name(i: usize) -> String {
    const FIRST: [&str; 8] = ["Sam", "Alex", "Robin", "Kim", "Noor", "Jo", "Ari", "Lee"];
    format!("{} Person{i}", FIRST[i % FIRST.len()])
}

/// Mean and max of `n` runs of `f`.
fn time<T>(n: usize, mut f: impl FnMut(usize) -> T) -> (Duration, Duration) {
    let mut total = Duration::ZERO;
    let mut max = Duration::ZERO;
    for i in 0..n {
        let t = Instant::now();
        std::hint::black_box(f(i));
        let d = t.elapsed();
        total += d;
        max = max.max(d);
    }
    (total / n as u32, max)
}

fn report(what: &str, (mean, max): (Duration, Duration)) {
    println!(
        "{what:<44} mean {:>9.3} ms   max {:>9.3} ms",
        mean.as_secs_f64() * 1e3,
        max.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore = "loads 160k rows; run in release with --ignored"]
fn graph_at_personal_scale() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("perf.db");
    // `NUGGET_GRAPH_PERF_MEMORY=1` runs against an unencrypted in-memory store instead, to
    // separate query cost from file I/O and decryption.
    let in_memory = std::env::var_os("NUGGET_GRAPH_PERF_MEMORY").is_some();
    let store = if in_memory {
        Store::open_in_memory().unwrap()
    } else {
        Store::open(&path, &StoreKey::from_bytes([4u8; 32])).unwrap()
    };
    println!(
        "store: {}",
        if in_memory {
            "in-memory, unencrypted"
        } else {
            "encrypted file (SQLCipher, WAL)"
        }
    );
    let g = store.graph();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);

    // ---- load
    let t = Instant::now();
    let kinds = [
        EntityKind::Person,
        EntityKind::Person,
        EntityKind::Organization,
        EntityKind::Project,
        EntityKind::Topic,
    ];
    let ids: Vec<EntityId> = (0..ENTITIES)
        .map(|i| {
            let mut e = NewEntity::new(kinds[i % kinds.len()], name(i));
            e.aliases = vec![format!("P{i}")];
            g.create_entity(&e).unwrap()
        })
        .collect();
    let load_entities = t.elapsed();

    // Claims and mentions are written item by item, as extraction does: each item yields
    // CLAIMS / ITEMS claims (half also cite a second, random item) and MENTIONS / ITEMS mentions.
    let preds = ["knows", "works_at", "attended", "owes", "mentions"];
    let (mut load_claims, mut load_mentions) = (Duration::ZERO, Duration::ZERO);
    let mut c = 0usize;
    for it in 0..ITEMS {
        let t = Instant::now();
        for _ in 0..CLAIMS / ITEMS {
            let subject = if c.is_multiple_of(CLAIMS / HUB_CLAIMS) {
                0
            } else {
                rng.below(ENTITIES)
            };
            let object = if c % 5 < 3 {
                ClaimObject::Entity(ids[rng.below(ENTITIES)].clone())
            } else {
                ClaimObject::Literal(format!("value {c}"))
            };
            let mut evidence = vec![Evidence {
                item: item(it),
                chunk_id: Some(rng.below(50) as i64),
                quote: Some(format!("quote {c}")),
            }];
            if c % 2 == 1 {
                evidence.push(Evidence {
                    item: item(rng.below(ITEMS)),
                    chunk_id: Some(rng.below(50) as i64),
                    quote: Some(format!("quote {c}")),
                });
            }
            g.add_claim(&NewClaim {
                subject: ids[subject].clone(),
                predicate: preds[c % preds.len()].into(),
                object,
                text: format!("claim {c}"),
                confidence: 0.8,
                status: ClaimStatus::Active,
                evidence,
                valid_from: None,
                valid_to: None,
            })
            .unwrap();
            c += 1;
        }
        load_claims += t.elapsed();
        let t = Instant::now();
        for _ in 0..MENTIONS / ITEMS {
            let e = rng.below(ENTITIES);
            g.add_mention(&Mention {
                entity: ids[e].clone(),
                item: item(it),
                chunk_id: Some(rng.below(50) as i64),
                surface: format!("P{e}"),
                confidence: 0.9,
            })
            .unwrap();
        }
        load_mentions += t.elapsed();
    }
    let counts = g.counts().unwrap();
    println!(
        "loaded {} entities in {:.1}s, {} claims in {:.1}s, {} mentions in {:.1}s ({} on disk)",
        counts.entities,
        load_entities.as_secs_f64(),
        counts.claims,
        load_claims.as_secs_f64(),
        counts.mentions,
        load_mentions.as_secs_f64(),
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
            + std::fs::metadata(dir.path().join("perf.db-wal"))
                .map(|m| m.len())
                .unwrap_or(0),
    );
    assert_eq!(counts.entities as usize, ENTITIES);
    assert!(
        counts.claims as usize > CLAIMS * 9 / 10,
        "few triples dedupe"
    );

    // ---- queries
    let find = time(1000, |i| {
        let n = rng.below(ENTITIES);
        let q = if i % 2 == 0 { name(n) } else { format!("p{n}") };
        let hits = g.find_by_name(&q, None).unwrap();
        assert_eq!(hits, vec![ids[n].clone()]);
    });
    report("find_by_name (name or alias)", find);

    let claims = time(1000, |_| {
        g.claims_for(&ids[1 + rng.below(ENTITIES - 1)], false)
            .unwrap()
    });
    report("claims_for (typical entity)", claims);
    let hub = time(20, |_| g.claims_for(&ids[0], true).unwrap());
    report(&format!("claims_for (hub, ~{HUB_CLAIMS} claims)"), hub);

    let nb = time(200, |_| {
        g.neighbourhood(&ids[1 + rng.below(ENTITIES - 1)], 2, 500)
            .unwrap()
    });
    report("neighbourhood depth 2, limit 500", nb);
    let nb_hub = time(10, |_| g.neighbourhood(&ids[0], 2, 500).unwrap());
    report("neighbourhood depth 2 from hub, limit 500", nb_hub);
    let nb_wide = time(10, |_| g.neighbourhood(&ids[0], 2, 10_000).unwrap());
    report("neighbourhood depth 2 from hub, limit 10k", nb_wide);

    let mentions = time(1000, |_| {
        g.mentions_for(&ids[rng.below(ENTITIES)], 50).unwrap()
    });
    report("mentions_for (limit 50)", mentions);

    let list = time(200, |i| {
        g.list_entities(&EntityQuery {
            text: Some(format!("person{}", 100 + i)),
            kind: None,
            limit: 20,
            offset: 0,
        })
        .unwrap()
    });
    report("list_entities text search (limit 20)", list);
    let page = time(200, |i| {
        g.list_entities(&EntityQuery {
            text: None,
            kind: Some(EntityKind::Person),
            limit: 50,
            offset: i * 10,
        })
        .unwrap()
    });
    report("list_entities by kind (page of 50)", page);

    let (_, start) = g.changes_since(0, 1).unwrap();
    let feed = time(100, |i| {
        g.changes_since(start + i as u64 * 100, 500).unwrap()
    });
    report("changes_since (page of 500)", feed);

    let retract = time(200, |i| g.retract_item(&item(i)).unwrap());
    report("retract_item (~15 evidence, 20 mentions)", retract);
    let delete = time(100, |i| store.delete_item(&item(1000 + i)).unwrap());
    report("Store::delete_item with graph cleanup", delete);

    let merge = time(100, |i| {
        g.merge(&ids[2000 + 2 * i], &ids[2001 + 2 * i]).unwrap()
    });
    report("merge (typical entities)", merge);

    // Generous ceilings: every hot-path read is expected in single-digit milliseconds.
    for (what, (mean, _)) in [
        ("find_by_name", find),
        ("claims_for", claims),
        ("neighbourhood", nb),
        ("retract_item", retract),
    ] {
        assert!(
            mean < Duration::from_millis(50),
            "{what} too slow: {mean:?}"
        );
    }

    // ---- query plans: the hot paths must use indexes, never scan a graph table.
    if in_memory {
        return;
    }
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", "04".repeat(32)))
        .unwrap();
    for (what, sql) in [
        (
            "find_by_name",
            "SELECT DISTINCT a.entity_id FROM entity_aliases a JOIN entities e ON e.id = a.entity_id
             WHERE a.alias_norm = 'x' AND e.merged_into IS NULL AND (NULL IS NULL OR e.kind = NULL)",
        ),
        (
            "claim by triple",
            "SELECT id FROM claims WHERE subject = 'a' AND predicate = 'p'
             AND object_entity IS 'b' AND object_literal IS NULL
             AND status IN ('active', 'user_asserted') ORDER BY id LIMIT 1",
        ),
        (
            "claims_for",
            "SELECT c.id FROM claims c WHERE (c.subject = 'a' OR c.object_entity = 'a')
             AND c.status IN ('active', 'user_asserted') ORDER BY c.id",
        ),
        (
            "retract evidence",
            "SELECT DISTINCT claim_id FROM claim_evidence WHERE source = 's' AND key = 'k'",
        ),
        (
            "retract mentions",
            "DELETE FROM mentions WHERE source = 's' AND key = 'k'",
        ),
        (
            "mentions_for",
            "SELECT id FROM mentions WHERE entity_id = 'a' ORDER BY id LIMIT 5",
        ),
        (
            "changes_since",
            "SELECT entity_id, seq FROM graph_changes WHERE seq > 5 ORDER BY seq LIMIT 5",
        ),
    ] {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        println!("plan {what}: {}", plan.join(" | "));
        assert!(
            !plan
                .iter()
                .any(|p| p.starts_with("SCAN ") && !p.contains("USING")),
            "{what} scans a table: {plan:?}"
        );
    }
}
