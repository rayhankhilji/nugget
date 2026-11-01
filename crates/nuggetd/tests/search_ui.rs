//! `search.faceted`, `search.timeline` and `search.preview` through IPC, against a real daemon
//! whose store is seeded directly with mail, calendar and file items, entities (one merged),
//! mentions and claims.

use std::sync::Arc;
use std::time::Duration;

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_core::paths::Paths;
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, Evidence, GraphStore, Mention, NewClaim, NewEntity,
};
use nugget_ingest::{MemorySecrets, NoPowerPolicy};
use nugget_ipc::{Client, IpcError, code};
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const CALL: Duration = Duration::from_secs(30);

// Extraction off, so nothing but the seeded graph is there.
const CONFIG: &str = "[runtime]\nenabled = false\n\n[knowledge]\nenabled = false\n\n[vault]\nenabled = false\n\n[mcp]\nenabled = false\n";

fn at(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap()
}

fn iref(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    }
}

struct Harness {
    _tmp: tempfile::TempDir,
    client: Client,
    /// To add items in a test.
    store: Store,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

/// What the seed created.
struct Seed {
    sam: EntityId,
    samuel: EntityId,
    acme: EntityId,
    topic: EntityId,
    works_at: i64,
    lives_in: i64,
    rejected: i64,
    m1_chunk: i64,
    f1_chunk: i64,
}

impl Harness {
    async fn start() -> (Self, Seed) {
        let tmp = tempfile::Builder::new()
            .prefix("nugget-ii2-")
            .tempdir()
            .unwrap();
        let paths = Paths::rooted(tmp.path().join("home"));
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), CONFIG).unwrap();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes([7; 32]));
        opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
        opts.power = Some(Arc::new(NoPowerPolicy));
        opts.home = Some(tmp.path().join("home"));
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let store = daemon.store();
        let seed = seed(&store);
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&paths).await.unwrap();
        (
            Self {
                _tmp: tmp,
                client,
                store,
                stop: Some(tx),
                server: Some(server),
            },
            seed,
        )
    }

    async fn call(&self, m: &str, params: Value) -> Value {
        tokio::time::timeout(CALL, self.client.call::<Value, Value>(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} {params} failed: {e}"))
    }

    async fn call_err(&self, m: &str, params: Value) -> (i64, String) {
        match self.client.call_value(m, params).await {
            Err(IpcError::Rpc(e)) => (e.code, e.message),
            other => panic!("{m}: expected an RPC error, got {other:?}"),
        }
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(CALL, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }
}

fn put(store: &Store, item: NewItem, body: &str) -> i64 {
    let item = item.with_text(body);
    let r = item.item.clone();
    store
        .upsert_item_with_chunks(&item, &[NewChunk::text(body)])
        .unwrap();
    store.get_chunks_by_ord(&r, &[0]).unwrap()[0].id
}

fn mention(e: &EntityId, item: &ItemRef, surface: &str) -> Mention {
    Mention {
        entity: e.clone(),
        item: item.clone(),
        chunk_id: None,
        surface: surface.into(),
        confidence: 0.9,
    }
}

fn claim(
    subject: &EntityId,
    predicate: &str,
    lit: &str,
    text: &str,
    evidence: &[(&ItemRef, &str)],
) -> NewClaim {
    NewClaim {
        subject: subject.clone(),
        predicate: predicate.into(),
        object: ClaimObject::Literal(lit.into()),
        text: text.into(),
        confidence: 0.9,
        status: ClaimStatus::Active,
        evidence: evidence
            .iter()
            .map(|(i, q)| Evidence {
                item: (*i).clone(),
                chunk_id: None,
                quote: Some((*q).into()),
            })
            .collect(),
        valid_from: None,
        valid_to: None,
    }
}

/// mail/m1 (2024-03-02, Sam + Acme), mail/m2 (2024-04-10, Samuel → Sam), cal/e1
/// (2024-03-31 23:30 UTC: April at +01:00, Sam), files/f1 (modified 2023-11-05, sensitive,
/// Acme), files/f2 (no "budget"; Samuel). Samuel is merged into Sam.
fn seed(store: &Store) -> Seed {
    let m1_chunk = put(
        store,
        NewItem::new("mail", "m1", content_hash("m1"))
            .with_title("Budget review")
            .with_mime("message/rfc822")
            .with_metadata(json!({"sensitive": false, "url": "https://mail.example/m1",
                                  "extracted": {"date": "2024-03-02T10:00:00Z"}})),
        "Budget review with Sam from Acme: the budget is approved.",
    );
    put(
        store,
        NewItem::new("mail", "m2", content_hash("m2"))
            .with_title("Budget follow-up")
            .with_metadata(json!({"sensitive": false, "kind": "email",
                                  "extracted": {"date": "2024-04-10T09:00:00Z"}})),
        "Following up on the budget with Samuel.",
    );
    put(
        store,
        NewItem::new("cal", "e1", content_hash("e1"))
            .with_title("Budget planning")
            .with_mime("text/calendar")
            .with_metadata(json!({"sensitive": false,
                                  "extracted": {"start": "2024-03-31T23:30:00Z"}})),
        "Budget planning meeting with Sam.",
    );
    let f1_chunk = put(
        store,
        NewItem::new("files", "f1", content_hash("f1"))
            .with_title("Budget spreadsheet notes")
            .with_modified_at(at("2023-11-05T08:00:00Z"))
            .with_metadata(json!({"sensitive": true})),
        "Acme budget spreadsheet notes.",
    );
    put(
        store,
        NewItem::new("files", "f2", content_hash("f2"))
            .with_title("Holiday plans")
            .with_modified_at(at("2022-07-01T08:00:00Z"))
            .with_metadata(json!({"sensitive": false})),
        "Holiday plans: Samuel moves to Berlin.",
    );

    let g = store.graph();
    let sam = g
        .create_entity(&NewEntity::new(EntityKind::Person, "Sam Lee"))
        .unwrap();
    let samuel = g
        .create_entity(&NewEntity::new(EntityKind::Person, "Samuel"))
        .unwrap();
    let acme = g
        .create_entity(&NewEntity::new(EntityKind::Organization, "Acme"))
        .unwrap();
    let (m1, m2, e1, f1, f2) = (
        iref("mail", "m1"),
        iref("mail", "m2"),
        iref("cal", "e1"),
        iref("files", "f1"),
        iref("files", "f2"),
    );
    // A topic: not in the people facet nor on hits, but selectable.
    let topic = g
        .create_entity(&NewEntity::new(EntityKind::Topic, "Budgeting"))
        .unwrap();
    g.add_mention(&mention(&topic, &m1, "budget")).unwrap();
    g.add_mention(&mention(&topic, &m2, "budget")).unwrap();
    g.add_mention(&mention(&sam, &m1, "Sam")).unwrap();
    g.add_mention(&mention(&acme, &m1, "Acme")).unwrap();
    g.add_mention(&mention(&samuel, &m2, "Samuel")).unwrap();
    g.add_mention(&mention(&sam, &e1, "Sam")).unwrap();
    g.add_mention(&mention(&acme, &f1, "Acme")).unwrap();
    let works_at = g
        .add_claim(&claim(
            &sam,
            "works_at",
            "Acme",
            "Sam works at Acme",
            &[(&m1, "Sam from Acme")],
        ))
        .unwrap();
    // Evidence only (no mention) in f2; dated by valid_from.
    let mut lives = claim(
        &samuel,
        "lives_in",
        "Berlin",
        "Sam lives in Berlin",
        &[(&f2, "moves to Berlin")],
    );
    lives.valid_from = Some(at("2022-08-01T00:00:00Z"));
    let lives_in = g.add_claim(&lives).unwrap();
    let rejected = g
        .add_claim(&claim(
            &sam,
            "works_at",
            "Globex",
            "Sam works at Globex",
            &[(&m1, "Globex")],
        ))
        .unwrap();
    g.set_claim_status(rejected, ClaimStatus::Rejected).unwrap();
    g.merge(&sam, &samuel).unwrap();
    Seed {
        sam,
        samuel,
        acme,
        topic,
        works_at,
        lives_in,
        rejected,
        m1_chunk,
        f1_chunk,
    }
}

fn values(facet: &Value) -> Vec<(String, u64)> {
    facet
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["value"].as_str().unwrap().to_owned(),
                c["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn pairs(v: &[(&str, u64)]) -> Vec<(String, u64)> {
    v.iter().map(|(a, b)| ((*a).to_owned(), *b)).collect()
}

fn keys(r: &Value) -> Vec<String> {
    let mut k: Vec<String> = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["item"]["key"].as_str().unwrap().to_owned())
        .collect();
    k.sort();
    k
}

#[tokio::test]
async fn faceted_search_filters_and_counts_disjunctively() {
    let (h, s) = Harness::start().await;
    let r = h.call("search.faceted", json!({"query": "budget"})).await;
    assert_eq!(r["query"], "budget");
    assert_eq!(r["pool"], 4, "{r}");
    assert_eq!(r["total"], 4);
    assert_eq!(r["pool_limited"], false);
    assert_eq!(keys(&r), ["e1", "f1", "m1", "m2"]);
    let f = &r["facets"];
    assert_eq!(
        values(&f["sources"]),
        pairs(&[("mail", 2), ("cal", 1), ("files", 1)])
    );
    assert_eq!(
        values(&f["kinds"]),
        pairs(&[("mail", 2), ("event", 1), ("file", 1)])
    );
    assert_eq!(
        values(&f["months"]),
        pairs(&[("2023-11", 1), ("2024-03", 2), ("2024-04", 1)])
    );
    let people = f["people"].as_array().unwrap();
    assert_eq!(people[0]["value"], s.sam.as_str());
    assert_eq!(people[0]["name"], "Sam Lee");
    assert_eq!(people[0]["kind"], "person");
    assert_eq!(people[0]["count"], 3, "m1, m2 (merged Samuel) and e1");
    assert_eq!(people[1]["value"], s.acme.as_str());
    assert_eq!(people[1]["kind"], "organization");
    assert_eq!(people[1]["count"], 2);
    assert_eq!(people.len(), 2, "merged ids never appear on their own");

    // Each hit carries its own date, kind, sensitivity and people.
    let hit = |key: &str| -> Value {
        r["hits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["item"]["key"] == key)
            .unwrap()
            .clone()
    };
    let m1 = hit("m1");
    assert_eq!(m1["date"], "2024-03-02T10:00:00Z");
    assert_eq!(m1["date_source"], "extracted");
    assert_eq!(m1["kind"], "mail");
    assert_eq!(m1["sensitive"], false);
    assert_eq!(m1["url"], "https://mail.example/m1");
    assert_eq!(m1["title"], "Budget review");
    assert!(m1["chunk_id"].is_i64());
    assert!(m1["score"].is_number());
    assert!(m1["snippet"].is_string());
    let names: Vec<&str> = m1["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(
        names.contains(&"Sam Lee") && names.contains(&"Acme"),
        "{names:?}"
    );
    // Kind from a metadata synonym.
    let m2 = hit("m2");
    assert_eq!(m2["date"], "2024-04-10T09:00:00Z");
    assert_eq!(m2["kind"], "mail");
    assert_eq!(m2["people"][0]["id"], s.sam.as_str());
    let e1 = hit("e1");
    assert_eq!(e1["kind"], "event");
    assert_eq!(
        e1["date"], "2024-03-31T23:30:00Z",
        "dated by the event start"
    );
    assert_eq!(e1["date_source"], "extracted");
    let f1 = hit("f1");
    assert_eq!(f1["kind"], "file");
    assert_eq!(f1["sensitive"], true);
    assert_eq!(f1["date_source"], "modified");
    assert_eq!(f1["date"], "2023-11-05T08:00:00Z");

    // Kind filter: the kind facet ignores it, the others apply it.
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "filters": {"kinds": ["mail", "chat"]}}),
        )
        .await;
    assert_eq!(keys(&r), ["m1", "m2"]);
    assert_eq!(r["total"], 2);
    let f = &r["facets"];
    assert_eq!(
        values(&f["kinds"]),
        pairs(&[("mail", 2), ("event", 1), ("file", 1), ("chat", 0)]),
        "selected values appear with count 0"
    );
    assert_eq!(values(&f["sources"]), pairs(&[("mail", 2)]));
    assert_eq!(
        values(&f["months"]),
        pairs(&[("2024-03", 1), ("2024-04", 1)])
    );

    // Source filter.
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "filters": {"sources": ["files", "cal"]}}),
        )
        .await;
    assert_eq!(keys(&r), ["e1", "f1"]);
    assert_eq!(
        values(&r["facets"]["sources"]),
        pairs(&[("mail", 2), ("cal", 1), ("files", 1)])
    );

    // People filter by a merged-away id: resolves to Sam, shown under the given id.
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "filters": {"people": [s.samuel.as_str()]}}),
        )
        .await;
    assert_eq!(keys(&r), ["e1", "m1", "m2"]);
    let people = r["facets"]["people"].as_array().unwrap();
    assert_eq!(people[0]["value"], s.samuel.as_str());
    assert_eq!(people[0]["name"], "Sam Lee");
    assert_eq!(people[0]["count"], 3);
    assert_eq!(
        values(&r["facets"]["kinds"]),
        pairs(&[("mail", 2), ("event", 1)])
    );

    // An entity of another kind filters and shows when selected.
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "filters": {"people": [s.topic.as_str()]}}),
        )
        .await;
    assert_eq!(keys(&r), ["m1", "m2"]);
    let topic = r["facets"]["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["value"] == s.topic.as_str())
        .unwrap()
        .clone();
    assert_eq!(topic["count"], 2);
    assert_eq!(topic["kind"], "topic");
    assert_eq!(topic["name"], "Budgeting");

    // An unknown person matches nothing but stays visible.
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "filters": {"people": ["ent_nobody"]}}),
        )
        .await;
    assert_eq!(r["total"], 0);
    assert!(r["hits"].as_array().unwrap().is_empty());
    let people = r["facets"]["people"].as_array().unwrap();
    let nobody = people.iter().find(|p| p["value"] == "ent_nobody").unwrap();
    assert_eq!(nobody["count"], 0);
    // The people facet ignores the people filter.
    assert!(people.iter().any(|p| p["value"] == s.sam.as_str()));

    // A date range in local days: at +01:00, e1 (23:30 UTC on 31 March) is on 1 April.
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "utc_offset_minutes": 60,
                   "filters": {"from": "2024-03-01", "to": "2024-03-31"}}),
        )
        .await;
    assert_eq!(keys(&r), ["m1"]);
    assert_eq!(
        values(&r["facets"]["months"]),
        pairs(&[("2023-11", 1), ("2024-03", 1), ("2024-04", 2)]),
        "months ignore the date filter and bucket at the offset"
    );
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "utc_offset_minutes": 0,
                   "filters": {"from": "2024-03-01", "to": "2024-03-31"}}),
        )
        .await;
    assert_eq!(keys(&r), ["e1", "m1"]);
    // JJ2: with a time zone, each date at its own offset. A London client in winter sends +0,
    // but 23:30 UTC on 31 March is already summer time there (00:30 on 1 April).
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "utc_offset_minutes": 0, "time_zone": "Europe/London",
                   "filters": {"from": "2024-03-01", "to": "2024-03-31"}}),
        )
        .await;
    assert_eq!(keys(&r), ["m1"]);
    assert_eq!(
        values(&r["facets"]["months"]),
        pairs(&[("2023-11", 1), ("2024-03", 1), ("2024-04", 2)]),
        "months bucket at each date's own offset"
    );
    // A zone the bundled database doesn't know falls back to the offset.
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "utc_offset_minutes": 0, "time_zone": "Mars/Olympus",
                   "filters": {"from": "2024-03-01", "to": "2024-03-31"}}),
        )
        .await;
    assert_eq!(keys(&r), ["e1", "m1"]);
    let r = h
        .call(
            "search.faceted",
            json!({"query": "budget", "filters": {"from": "2024-04-01", "to": null}}),
        )
        .await;
    assert_eq!(keys(&r), ["m2"]);

    // Paging over the filtered hits, in retriever order.
    let all = h.call("search.faceted", json!({"query": "budget"})).await;
    let page = h
        .call(
            "search.faceted",
            json!({"query": "budget", "limit": 2, "offset": 1}),
        )
        .await;
    assert_eq!(page["total"], 4);
    assert_eq!(page["hits"].as_array().unwrap().len(), 2);
    assert_eq!(page["hits"][0]["item"], all["hits"][1]["item"]);
    assert_eq!(page["hits"][1]["item"], all["hits"][2]["item"]);

    // Empty query: empty result, not an error.
    for q in [json!({"query": "   "}), json!({}), Value::Null] {
        let r = h.call("search.faceted", q).await;
        assert_eq!(r["total"], 0);
        assert_eq!(r["pool"], 0);
        assert!(r["hits"].as_array().unwrap().is_empty());
        assert!(r["facets"]["people"].as_array().unwrap().is_empty());
        assert!(r["facets"]["months"].as_array().unwrap().is_empty());
    }

    // Bad input names the field.
    for (params, field) in [
        (
            json!({"query": "budget", "filters": {"from": "March"}}),
            "filters.from",
        ),
        (
            json!({"query": "budget", "filters": {"to": "2024-02-30"}}),
            "filters.to",
        ),
        (
            json!({"query": "budget", "filters": {"kinds": ["fax"]}}),
            "filters.kinds",
        ),
        (
            json!({"query": "budget", "filters": {"sources": vec!["s"; 51]}}),
            "filters.sources",
        ),
        (
            json!({"query": "budget", "utc_offset_minutes": 5000}),
            "utc_offset_minutes",
        ),
    ] {
        let (c, msg) = h.call_err("search.faceted", params).await;
        assert_eq!(c, code::INVALID_PARAMS, "{msg}");
        assert!(msg.contains(field), "{field}: {msg}");
    }
    h.stop().await;
}

#[tokio::test]
async fn timeline_orders_truncates_and_resolves_merged_ids() {
    let (h, s) = Harness::start().await;
    let t = h
        .call("search.timeline", json!({"entity": s.samuel.as_str()}))
        .await;
    assert_eq!(t["entity"]["id"], s.sam.as_str(), "merged ids resolve");
    assert_eq!(t["entity"]["name"], "Sam Lee");
    assert_eq!(t["entity"]["kind"], "person");
    assert_eq!(t["total"], 6, "{t:#}");
    assert_eq!(t["truncated"], false);
    let events = t["events"].as_array().unwrap();
    let summary: Vec<String> = events
        .iter()
        .map(|e| match e["type"].as_str().unwrap() {
            "item" => format!("item {}", e["item"]["key"].as_str().unwrap()),
            _ => format!("fact {}", e["claim"]["id"]),
        })
        .collect();
    assert_eq!(
        summary,
        [
            "item f2".to_owned(),
            format!("fact {}", s.lives_in),
            "item m1".to_owned(),
            format!("fact {}", s.works_at),
            "item e1".to_owned(),
            "item m2".to_owned(),
        ],
        "ascending; the fact dated by its evidence comes after its item; the rejected fact \
         ({}) is left out",
        s.rejected
    );
    let f2 = &events[0];
    assert_eq!(f2["at"], "2022-07-01T08:00:00Z");
    assert_eq!(f2["date_source"], "modified");
    assert_eq!(f2["title"], "Holiday plans");
    assert_eq!(f2["snippet"], "Holiday plans: Samuel moves to Berlin.");
    assert_eq!(f2["kind"], "file");
    assert_eq!(f2["sensitive"], false);
    let lives = &events[1];
    assert_eq!(lives["at"], "2022-08-01T00:00:00Z");
    assert_eq!(lives["date_source"], "valid_from");
    assert_eq!(lives["claim"]["predicate"], "lives_in");
    assert_eq!(lives["claim"]["status"], "active");
    assert_eq!(lives["claim"]["text"], "Sam lives in Berlin");
    assert_eq!(lives["claim"]["evidence"][0]["item"]["key"], "f2");
    assert_eq!(lives["claim"]["evidence"][0]["title"], "Holiday plans");
    assert_eq!(lives["claim"]["evidence"][0]["quote"], "moves to Berlin");
    let works = &events[3];
    assert_eq!(works["at"], "2024-03-02T10:00:00Z");
    assert_eq!(works["date_source"], "evidence");
    assert!((works["claim"]["confidence"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(events[2]["url"], "https://mail.example/m1");
    assert_eq!(events[4]["kind"], "event");
    assert_eq!(
        values(&t["months"]),
        pairs(&[
            ("2022-07", 1),
            ("2022-08", 1),
            ("2024-03", 3),
            ("2024-04", 1)
        ])
    );

    // The newest `limit` are kept; months still cover everything.
    let cut = h
        .call(
            "search.timeline",
            json!({"entity": s.sam.as_str(), "limit": 2}),
        )
        .await;
    assert_eq!(cut["truncated"], true);
    assert_eq!(cut["total"], 6);
    let keys: Vec<&str> = cut["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["item"]["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["e1", "m2"]);
    assert_eq!(cut["months"], t["months"]);

    // from/to narrow before the limit, in local days at the offset.
    let back = h
        .call(
            "search.timeline",
            json!({"entity": s.sam.as_str(), "to": "2024-03-31", "limit": 2,
                   "utc_offset_minutes": 60}),
        )
        .await;
    assert_eq!(back["total"], 4, "e1 is on 1 April at +01:00");
    let last = back["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["claim"]["id"], s.works_at);
    let window = h
        .call(
            "search.timeline",
            json!({"entity": s.sam.as_str(), "from": "2024-03-31", "to": "2024-03-31"}),
        )
        .await;
    assert_eq!(window["total"], 1);
    assert_eq!(window["events"][0]["item"]["key"], "e1");

    // Another entity: Acme has mentions only (the claims are about Sam, with Acme a literal).
    let acme = h
        .call("search.timeline", json!({"entity": s.acme.as_str()}))
        .await;
    assert_eq!(acme["total"], 2);
    assert_eq!(acme["events"][0]["item"]["key"], "f1");
    assert_eq!(acme["events"][0]["sensitive"], true);

    let (c, msg) = h
        .call_err("search.timeline", json!({"entity": "ent_nobody"}))
        .await;
    assert_eq!(c, code::NOT_FOUND, "{msg}");
    let (c, msg) = h
        .call_err(
            "search.timeline",
            json!({"entity": s.sam.as_str(), "from": "yesterday"}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS);
    assert!(msg.contains("from"), "{msg}");
    let (c, _) = h.call_err("search.timeline", json!({})).await;
    assert_eq!(c, code::INVALID_PARAMS);
    h.stop().await;
}

#[tokio::test]
async fn preview_shows_text_passage_people_and_facts() {
    let (h, s) = Harness::start().await;
    let p = h
        .call(
            "search.preview",
            json!({"item": {"source": "mail", "key": "m1"}, "chunk_id": s.m1_chunk}),
        )
        .await;
    assert_eq!(p["item"]["key"], "m1");
    assert_eq!(p["title"], "Budget review");
    assert_eq!(p["kind"], "mail");
    assert_eq!(p["mime"], "message/rfc822");
    assert_eq!(p["url"], "https://mail.example/m1");
    assert_eq!(p["date"], "2024-03-02T10:00:00Z");
    assert_eq!(p["date_source"], "extracted");
    assert_eq!(p["sensitive"], false);
    assert_eq!(
        p["text"],
        "Budget review with Sam from Acme: the budget is approved."
    );
    assert_eq!(p["truncated"], false);
    assert_eq!(
        p["passage"],
        "Budget review with Sam from Acme: the budget is approved."
    );
    let people: Vec<&str> = p["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    // The preview lists every kind of entity the item mentions.
    assert_eq!(people.len(), 3, "{people:?}");
    assert!(people.contains(&"Sam Lee") && people.contains(&"Acme"));
    assert!(people.contains(&"Budgeting"));
    let facts = p["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 1, "the rejected fact is left out: {facts:?}");
    assert_eq!(facts[0]["id"], s.works_at);
    assert_eq!(facts[0]["subject"]["id"], s.sam.as_str());
    assert_eq!(facts[0]["subject"]["name"], "Sam Lee");
    assert_eq!(facts[0]["subject"]["kind"], "person");
    assert_eq!(facts[0]["predicate"], "works_at");
    assert_eq!(facts[0]["status"], "active");
    assert_eq!(facts[0]["quote"], "Sam from Acme");

    // A chunk of another item is not this item's passage; no chunk, no passage.
    let p = h
        .call(
            "search.preview",
            json!({"item": {"source": "mail", "key": "m1"}, "chunk_id": s.f1_chunk}),
        )
        .await;
    assert_eq!(p["passage"], Value::Null);
    let f1 = h
        .call(
            "search.preview",
            json!({"item": {"source": "files", "key": "f1"}}),
        )
        .await;
    assert_eq!(f1["passage"], Value::Null);
    assert_eq!(f1["sensitive"], true);
    assert_eq!(f1["kind"], "file");
    assert_eq!(f1["date_source"], "modified");
    assert_eq!(f1["people"][0]["id"], s.acme.as_str());
    assert!(f1["facts"].as_array().unwrap().is_empty());
    // Facts about a merged entity show the survivor as subject.
    let f2 = h
        .call(
            "search.preview",
            json!({"item": {"source": "files", "key": "f2"}}),
        )
        .await;
    assert_eq!(f2["facts"][0]["id"], s.lives_in);
    assert_eq!(f2["facts"][0]["subject"]["id"], s.sam.as_str());

    let (c, msg) = h
        .call_err(
            "search.preview",
            json!({"item": {"source": "mail", "key": "nope"}}),
        )
        .await;
    assert_eq!(c, code::NOT_FOUND, "{msg}");
    let (c, _) = h.call_err("search.preview", json!({})).await;
    assert_eq!(c, code::INVALID_PARAMS);
    h.stop().await;
}

/// QA JJ1-10 and JJ1-11: dates in the user's zone, whatever today's offset.
#[tokio::test]
async fn zoned_events_and_dst_dates_land_on_the_users_day() {
    let (h, _) = Harness::start().await;
    // JJ1-10: DTSTART;TZID=America/New_York:20260415T200000 is 00:00 UTC on 16 April.
    put(
        &h.store,
        NewItem::new("cal", "ny", content_hash("ny"))
            .with_title("Quarzite review")
            .with_mime("text/calendar")
            .with_metadata(json!({"sensitive": false,
                                  "extracted": {"start": "2026-04-15T20:00:00",
                                                "timezone": "America/New_York"}})),
        "Quarzite review call.",
    );
    // JJ1-11: a Berlin mail sent at 23:30 CET on 15 January.
    put(
        &h.store,
        NewItem::new("mail", "berlin", content_hash("berlin"))
            .with_title("Quarzite invoice")
            .with_metadata(json!({"sensitive": false,
                                  "extracted": {"date": "2026-01-15T22:30:00Z"}})),
        "Quarzite invoice attached.",
    );
    let day = |d: &str| json!({"from": d, "to": d});
    let call = |filters: Value, offset: i32| {
        json!({"query": "quarzite", "utc_offset_minutes": offset, "time_zone": "Europe/Berlin",
               "filters": filters})
    };
    // A Berlin user in summer (+120) filtering 16 April finds the New York event, dated at its
    // instant everywhere.
    let r = h.call("search.faceted", call(day("2026-04-16"), 120)).await;
    assert_eq!(keys(&r), ["ny"], "{r}");
    assert_eq!(r["hits"][0]["date"], "2026-04-16T00:00:00Z");
    let p = h
        .call(
            "search.preview",
            json!({"item": {"source": "cal", "key": "ny"}, "time_zone": "Europe/Berlin"}),
        )
        .await;
    assert_eq!(p["date"], "2026-04-16T00:00:00Z");
    // The same user (sending today's +120) filtering 15 January still finds the winter mail.
    let r = h.call("search.faceted", call(day("2026-01-15"), 120)).await;
    assert_eq!(keys(&r), ["berlin"], "{r}");
    let r = h.call("search.faceted", call(day("2026-01-16"), 120)).await;
    assert!(keys(&r).is_empty(), "{r}");
    // Months too: January and April.
    let r = h.call("search.faceted", call(json!({}), 120)).await;
    assert_eq!(
        values(&r["facets"]["months"]),
        pairs(&[("2026-01", 1), ("2026-04", 1)])
    );
    h.stop().await;
}
