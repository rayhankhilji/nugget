//! Behaviour of `Retriever` against an in-memory store.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use nugget_extract::estimate_tokens;
use nugget_infer::{ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, Router};
use nugget_retrieve::eval::HashEmbedder;
use nugget_retrieve::{
    RecencyBoost, RetrievalMode, RetrieveConfig, Retriever, SearchOptions, Similarity, Strategy,
    TextOnlyReason,
};
use nugget_store::{ChunkId, NewChunk, NewItem, Store, content_hash};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn at(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap()
}

const DIM: usize = 64;

fn embedder() -> HashEmbedder {
    HashEmbedder::new(
        DIM,
        &[
            vec!["holiday".into(), "vacation".into(), "trip".into()],
            vec!["portugal".into(), "lisbon".into()],
        ],
    )
}

struct Doc<'a> {
    key: &'a str,
    title: &'a str,
    paras: &'a [&'a str],
    url: Option<&'a str>,
    modified: Option<OffsetDateTime>,
}

impl<'a> Doc<'a> {
    fn new(key: &'a str, title: &'a str, paras: &'a [&'a str]) -> Self {
        Self {
            key,
            title,
            paras,
            url: None,
            modified: None,
        }
    }
    fn url(mut self, url: &'a str) -> Self {
        self.url = Some(url);
        self
    }
    fn modified(mut self, at: OffsetDateTime) -> Self {
        self.modified = Some(at);
        self
    }
}

/// Index a doc (one chunk per paragraph), embedding with `emb` when given. Returns chunk ids.
fn add(store: &Store, d: Doc<'_>, emb: Option<&HashEmbedder>) -> Vec<ChunkId> {
    let mut item =
        NewItem::new("notes", d.key, content_hash(d.paras.join("\n"))).with_title(d.title);
    if let Some(u) = d.url {
        item = item.with_metadata(serde_json::json!({ "url": u }));
    }
    if let Some(m) = d.modified {
        item = item.with_modified_at(m);
    }
    store.upsert_item(&item).unwrap();
    let chunks: Vec<NewChunk> = d
        .paras
        .iter()
        .map(|p| match emb {
            Some(e) => NewChunk::embedded(*p, e.embed_text(p)),
            None => NewChunk::text(*p),
        })
        .collect();
    store.replace_chunks(&item.item, &chunks).unwrap()
}

fn router(providers: Vec<Arc<dyn Provider>>, probed: bool) -> Arc<Router> {
    let ids: Vec<ProviderId> = providers.iter().map(|p| p.id().clone()).collect();
    let tiers = HashMap::from([(Tier::Embedding, ids.clone())]);
    let r = Router::new(providers, tiers);
    if probed {
        for id in &ids {
            r.set_capabilities(
                id,
                Capabilities {
                    embeddings: true,
                    ..Default::default()
                },
            );
        }
    }
    Arc::new(r)
}

fn keys(items: &[nugget_retrieve::RetrievedItem]) -> Vec<&str> {
    items.iter().map(|r| r.item.key.as_str()).collect()
}

/// A small mixed corpus, embedded.
fn corpus(emb: Option<&HashEmbedder>) -> Store {
    let store = Store::open_in_memory().unwrap();
    add(
        &store,
        Doc::new(
            "lisbon",
            "Lisbon weekend",
            &[
                "Friday in Lisbon: tram 28 to Graça and dinner near the cathedral.",
                "Saturday: Belém tower and pastries, sunset at the miradouro.",
            ],
        )
        .url("file:///notes/lisbon.md"),
        emb,
    );
    add(
        &store,
        Doc::new(
            "boiler",
            "Boiler repair",
            &[
                "The boiler showed fault F28 and stopped heating the water.",
                "The engineer replaced the gas valve and the ignition electrode.",
                "A power flush is recommended next autumn for the radiators.",
            ],
        ),
        emb,
    );
    add(
        &store,
        Doc::new(
            "garden",
            "Garden plan",
            &[
                "Tomatoes and basil in the first raised bed.",
                "Prune the apple tree in winter while dormant.",
            ],
        ),
        emb,
    );
    store
}

// ---- fallback and locality ----

#[tokio::test]
async fn text_only_without_an_embedder() {
    let store = corpus(None);
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let res = r.search("boiler fault", 5).await.unwrap();
    assert_eq!(
        res.mode,
        RetrievalMode::TextOnly {
            reason: TextOnlyReason::NoEmbedder
        }
    );
    assert_eq!(keys(&res.items), ["boiler"]);
    // A single result: nothing to diversify.
    assert_eq!(res.similarity, Similarity::None);
}

#[tokio::test]
async fn no_vectors_indexed_means_the_query_is_not_sent() {
    let store = corpus(None);
    let emb = Arc::new(embedder());
    let r = Retriever::new(
        store,
        Some(router(vec![emb.clone()], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("boiler", 5).await.unwrap();
    assert_eq!(
        res.mode,
        RetrievalMode::TextOnly {
            reason: TextOnlyReason::NoVectorsIndexed
        }
    );
    assert_eq!(emb.calls(), 0);
}

#[tokio::test]
async fn hybrid_with_a_local_embedder_finds_synonyms() {
    let local = embedder();
    let store = corpus(Some(&local));
    let emb = Arc::new(embedder());
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![emb.clone()], true)),
        RetrieveConfig::default(),
    );
    // No word of the query occurs in the Lisbon note; only the embedding connects them.
    let res = r.search("Portugal vacation", 3).await.unwrap();
    assert_eq!(
        res.mode,
        RetrievalMode::Hybrid {
            provider: ProviderId::new("hash-embedder")
        }
    );
    assert_eq!(res.items[0].item.key.as_str(), "lisbon");
    assert!(emb.calls() >= 1);
    let text = Retriever::new(store, None, RetrieveConfig::default())
        .search("vacation Portugal", 3)
        .await
        .unwrap();
    assert!(text.items.is_empty(), "full-text alone cannot find it");
}

#[tokio::test]
async fn unprobed_embedding_tier_member_is_tried() {
    let local = embedder();
    let store = corpus(Some(&local));
    let emb = Arc::new(embedder());
    let r = Retriever::new(
        store,
        Some(router(vec![emb.clone()], false)),
        RetrieveConfig::default(),
    );
    let res = r.search("Portugal", 3).await.unwrap();
    assert!(matches!(res.mode, RetrievalMode::Hybrid { .. }));
}

#[tokio::test]
async fn provider_probed_without_embeddings_is_skipped() {
    let local = embedder();
    let store = corpus(Some(&local));
    let emb = Arc::new(embedder());
    let rt = router(vec![emb.clone()], false);
    rt.set_capabilities(emb.id(), Capabilities::default());
    let r = Retriever::new(store, Some(rt), RetrieveConfig::default());
    let res = r.search("boiler", 3).await.unwrap();
    assert_eq!(
        res.mode,
        RetrievalMode::TextOnly {
            reason: TextOnlyReason::NoEmbedder
        }
    );
    assert_eq!(emb.calls(), 0);
}

#[tokio::test]
async fn cloud_embedder_is_not_used_unless_allowed() {
    let local = embedder();
    let store = corpus(Some(&local));
    let cloud = Arc::new(
        embedder()
            .with_id("cloud-embed")
            .with_locality(Locality::Cloud),
    );

    // Not allowed (the default): text-only, and the query never reaches the provider.
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![cloud.clone()], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("boiler heating", 3).await.unwrap();
    assert_eq!(
        res.mode,
        RetrievalMode::TextOnly {
            reason: TextOnlyReason::CloudNotAllowed
        }
    );
    let ctx = r.pack_context("boiler heating", 500).await.unwrap();
    assert!(ctx.mode.is_text_only());
    assert_eq!(cloud.calls(), 0, "no query or passage sent to the cloud");

    // A local provider later in the tier is used instead of the cloud one before it.
    let local_p = Arc::new(embedder().with_id("local-embed"));
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![cloud.clone(), local_p.clone()], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("boiler heating", 3).await.unwrap();
    assert_eq!(
        res.mode.provider().map(ProviderId::as_str),
        Some("local-embed")
    );
    assert_eq!(cloud.calls(), 0);
    assert!(local_p.calls() >= 1);

    // Allowed explicitly: the cloud provider serves the query.
    let cfg = RetrieveConfig {
        allow_cloud_embedding: true,
        ..Default::default()
    };
    let r = Retriever::new(store, Some(router(vec![cloud.clone()], true)), cfg);
    let res = r.search("boiler heating", 3).await.unwrap();
    assert_eq!(
        res.mode.provider().map(ProviderId::as_str),
        Some("cloud-embed")
    );
    assert!(cloud.calls() >= 1);
}

/// An embedding provider that always fails, or returns vectors of the wrong size.
struct Broken {
    id: ProviderId,
    dim: Option<usize>,
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for Broken {
    fn id(&self) -> &ProviderId {
        &self.id
    }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "broken".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        Err(Error::Unsupported("no chat".into()))
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.dim {
            Some(d) => Ok(texts.iter().map(|_| vec![0.5; d]).collect()),
            None => Err(Error::Network("connection refused".into())),
        }
    }
}

#[tokio::test]
async fn embedding_failures_fall_back_to_text() {
    let local = embedder();
    let store = corpus(Some(&local));
    let down = Arc::new(Broken {
        id: ProviderId::new("down"),
        dim: None,
        calls: AtomicUsize::new(0),
    });
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![down.clone()], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("boiler", 3).await.unwrap();
    match &res.mode {
        RetrievalMode::TextOnly {
            reason: TextOnlyReason::EmbeddingFailed(msg),
        } => assert!(msg.contains("connection refused")),
        other => panic!("unexpected mode {other:?}"),
    }
    assert_eq!(keys(&res.items), ["boiler"]);
    assert_eq!(down.calls.load(Ordering::SeqCst), 1);

    // A failing provider falls through to the next one.
    let ok = Arc::new(embedder().with_id("ok"));
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![down.clone(), ok.clone()], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("boiler", 3).await.unwrap();
    assert_eq!(res.mode.provider().map(ProviderId::as_str), Some("ok"));

    let wrong = Arc::new(Broken {
        id: ProviderId::new("wrong-dim"),
        dim: Some(DIM / 2),
        calls: AtomicUsize::new(0),
    });
    let r = Retriever::new(
        store,
        Some(router(vec![wrong], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("boiler", 3).await.unwrap();
    assert!(matches!(
        res.mode,
        RetrievalMode::TextOnly {
            reason: TextOnlyReason::DimensionMismatch(_)
        }
    ));
    assert_eq!(keys(&res.items), ["boiler"]);
}

#[tokio::test]
async fn strategies() {
    let local = embedder();
    let store = corpus(Some(&local));
    let emb = Arc::new(embedder());
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![emb.clone()], true)),
        RetrieveConfig::default(),
    );
    let text = SearchOptions {
        strategy: Strategy::TextOnly,
        mmr: None,
    };
    let res = r.search_with("boiler", 3, text).await.unwrap();
    assert_eq!(
        res.mode,
        RetrievalMode::TextOnly {
            reason: TextOnlyReason::Requested
        }
    );
    assert_eq!(emb.calls(), 0);
    let vector = SearchOptions {
        strategy: Strategy::VectorOnly,
        mmr: Some(false),
    };
    let res = r.search_with("Portugal", 1, vector).await.unwrap();
    assert!(matches!(res.mode, RetrievalMode::VectorOnly { .. }));
    assert_eq!(keys(&res.items), ["lisbon"]);
    let none = Retriever::new(store, None, RetrieveConfig::default());
    assert!(matches!(
        none.search_with("Portugal", 1, vector).await,
        Err(Error::Unsupported(_))
    ));
    // Degenerate input.
    assert!(none.search("", 5).await.unwrap().items.is_empty());
    assert!(none.search("?!", 5).await.unwrap().items.is_empty());
    assert!(none.search("boiler", 0).await.unwrap().items.is_empty());
}

// ---- grouping, MMR, recency, mapping ----

#[tokio::test]
async fn results_are_grouped_by_item_with_all_matching_chunks() {
    let store = Store::open_in_memory().unwrap();
    let ids = add(
        &store,
        Doc::new(
            "kestrel",
            "Kestrel",
            &[
                "Kestrel backfill is running.",
                "Unrelated paragraph about lunch.",
                "Kestrel cutover next week.",
                "Kestrel rollback owner is Omar.",
            ],
        ),
        None,
    );
    add(
        &store,
        Doc::new("other", "Other", &["Kestrel is also a bird."]),
        None,
    );
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let res = r.search("kestrel", 10).await.unwrap();
    assert_eq!(res.items.len(), 2, "one result per item");
    let k = res
        .items
        .iter()
        .find(|i| i.item.key.as_str() == "kestrel")
        .unwrap();
    let mut got = k.chunk_ids();
    got.sort_unstable();
    assert_eq!(got, vec![ids[0], ids[2], ids[3]]);
    assert_eq!(k.matches.len(), 2);
    let hits = res.hits();
    assert!(hits.iter().all(|h| h.chunk_id.is_some()));
    assert!(res.items.iter().all(|i| (0.0..=1.0).contains(&i.score)));
}

fn mmr_corpus(emb: Option<&HashEmbedder>) -> Store {
    let store = Store::open_in_memory().unwrap();
    let dup = "Quarterly sales forecast for the northern region: widgets up, gizmos flat, northern stores steady.";
    add(&store, Doc::new("dup-a", "Northern report A", &[dup]), emb);
    add(&store, Doc::new("dup-b", "Northern report B", &[dup]), emb);
    add(
        &store,
        Doc::new(
            "distinct",
            "Supplier delay",
            &[
                "Quarterly sales forecast revised after the supplier delay hit gadget deliveries across the southern warehouses and the online shop during the busy autumn season.",
            ],
        ),
        emb,
    );
    store
}

#[tokio::test]
async fn mmr_demotes_near_duplicates_lexically() {
    let store = mmr_corpus(None);
    let plain = Retriever::new(
        store.clone(),
        None,
        RetrieveConfig {
            mmr: false,
            ..Default::default()
        },
    );
    let res = plain.search("quarterly sales forecast", 3).await.unwrap();
    assert_eq!(keys(&res.items), ["dup-a", "dup-b", "distinct"]);
    assert_eq!(res.similarity, Similarity::None);

    let mmr = Retriever::new(store, None, RetrieveConfig::default());
    let res = mmr.search("quarterly sales forecast", 3).await.unwrap();
    assert_eq!(keys(&res.items), ["dup-a", "distinct", "dup-b"]);
    assert_eq!(res.similarity, Similarity::Lexical);
    // With λ = 1 MMR is pure relevance.
    let pure = Retriever::new(
        mmr.store().clone(),
        None,
        RetrieveConfig {
            mmr_lambda: 1.0,
            ..Default::default()
        },
    );
    let res = pure.search("quarterly sales forecast", 3).await.unwrap();
    assert_eq!(keys(&res.items), ["dup-a", "dup-b", "distinct"]);
}

#[tokio::test]
async fn mmr_uses_vectors_when_the_query_was_embedded() {
    let local = embedder();
    let store = mmr_corpus(Some(&local));
    let emb = Arc::new(embedder());
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![emb.clone()], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("quarterly sales forecast", 3).await.unwrap();
    assert_eq!(res.similarity, Similarity::Vector);
    assert_eq!(keys(&res.items)[2], "dup-b", "duplicate pushed last");
    assert_eq!(
        emb.calls(),
        1,
        "only the query is embedded; the pool's vectors come from the store"
    );

    // Vector MMR can be turned off: lexical similarity, one embedding call.
    let emb2 = Arc::new(embedder());
    let r = Retriever::new(
        store,
        Some(router(vec![emb2.clone()], true)),
        RetrieveConfig {
            mmr_vectors: false,
            ..Default::default()
        },
    );
    let res = r.search("quarterly sales forecast", 3).await.unwrap();
    assert_eq!(res.similarity, Similarity::Lexical);
    assert_eq!(emb2.calls(), 1);
}

/// Wraps the hash embedder and records each call as (`"query"` or `"document"`, number of texts).
struct Recording {
    inner: HashEmbedder,
    batches: std::sync::Mutex<Vec<(&'static str, usize)>>,
}

#[async_trait]
impl Provider for Recording {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }
    fn meta(&self) -> ProviderMeta {
        self.inner.meta()
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        self.inner.chat(req).await
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.batches.lock().unwrap().push(("document", texts.len()));
        self.inner.embed(texts).await
    }
    async fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.batches.lock().unwrap().push(("query", texts.len()));
        self.inner.embed_query(texts).await
    }
}

#[tokio::test]
async fn vector_mmr_embeds_only_passages_stored_without_a_vector() {
    // dup-a and distinct are stored with vectors, dup-b (same text as dup-a) text-only.
    let local = embedder();
    let store = Store::open_in_memory().unwrap();
    let dup = "Quarterly sales forecast for the northern region: widgets up, gizmos flat, northern stores steady.";
    add(
        &store,
        Doc::new("dup-a", "Northern report A", &[dup]),
        Some(&local),
    );
    add(&store, Doc::new("dup-b", "Northern report B", &[dup]), None);
    add(
        &store,
        Doc::new(
            "distinct",
            "Supplier delay",
            &["Quarterly sales forecast revised after the supplier delay hit gadget deliveries."],
        ),
        Some(&local),
    );
    let rec = Arc::new(Recording {
        inner: embedder(),
        batches: Default::default(),
    });
    let r = Retriever::new(
        store,
        Some(router(vec![rec.clone()], true)),
        RetrieveConfig::default(),
    );
    let res = r.search("quarterly sales forecast", 3).await.unwrap();
    assert_eq!(res.similarity, Similarity::Vector);
    assert_eq!(keys(&res.items).len(), 3);
    assert_eq!(keys(&res.items)[2], "dup-b", "{:?}", keys(&res.items));
    assert_eq!(
        *rec.batches.lock().unwrap(),
        vec![("query", 1), ("document", 1)],
        "the query (as a query), then only the one passage without a stored vector (as a document)"
    );
}

#[tokio::test]
async fn fusion_depth_keeps_strong_single_list_matches() {
    // The target is the only full-text match for "zephyr" but far away in vector space. 80
    // fillers match "common" (more often = better full-text rank) and sit near the query vector
    // in the *opposite* order, so in deep lists every filler is in both lists and fuses above
    // the target. 100 unrelated notes, orthogonal to the query, sit between the fillers and the
    // target in vector space. With the default depth (40 per list for 5 results) no filler is in
    // both lists.
    let emb = embedder();
    let query = "zephyr common";
    let q = emb.embed_text(query);
    let mut unused = (0..q.len()).filter(|&i| q[i] == 0.0);
    let (free, other) = (unused.next().unwrap(), unused.next().unwrap());
    let far: Vec<f32> = q.iter().map(|x| -x).collect();
    let mut orthogonal = vec![0.0; q.len()];
    orthogonal[other] = 1.0;
    let store = Store::open_in_memory().unwrap();
    let put = |key: &str, text: &str, v: Vec<f32>| {
        let it = NewItem::new("notes", key, content_hash(text)).with_title(key);
        store
            .upsert_item_with_chunks(&it, &[NewChunk::embedded(text, v)])
            .unwrap();
    };
    put("target", "the zephyr signal at dawn", far.clone());
    for j in 0..80usize {
        let text = format!("{}{}", "common ".repeat(j + 1), "pad ".repeat(100 - j));
        let mut v = q.clone();
        v[free] = j as f32 / 20.0;
        put(&format!("filler-{j:02}"), &text, v);
    }
    for j in 0..100usize {
        put(
            &format!("other-{j:03}"),
            &format!("unrelated note {j}"),
            orthogonal.clone(),
        );
    }
    let opts = SearchOptions {
        strategy: Strategy::Auto,
        mmr: Some(false),
    };
    let r = Retriever::new(
        store.clone(),
        Some(router(vec![Arc::new(embedder())], true)),
        RetrieveConfig::default(),
    );
    let res = r.search_with(query, 5, opts).await.unwrap();
    assert!(matches!(res.mode, RetrievalMode::Hybrid { .. }));
    assert!(
        keys(&res.items).contains(&"target"),
        "{:?}",
        keys(&res.items)
    );

    let deep = Retriever::new(
        store,
        Some(router(vec![Arc::new(embedder())], true)),
        RetrieveConfig {
            min_fusion_depth: 200,
            ..Default::default()
        },
    );
    let res = deep.search_with(query, 5, opts).await.unwrap();
    assert!(
        !keys(&res.items).contains(&"target"),
        "{:?}",
        keys(&res.items)
    );
}

#[tokio::test]
async fn neighbours_follow_positions_after_rechunking() {
    // Re-chunk an item after another item was indexed: neighbours still come from the same item
    // and adjacent positions.
    let store = Store::open_in_memory().unwrap();
    add(
        &store,
        Doc::new("story", "The Story", &["Old zero.", "Old one."]),
        None,
    );
    add(
        &store,
        Doc::new("other", "Other", &["Unrelated text about a quokka."]),
        None,
    );
    let ids = add(
        &store,
        Doc::new(
            "story",
            "The Story",
            &[
                "Intro paragraph.",
                "The zephyr arrives at dawn.",
                "Closing paragraph.",
            ],
        ),
        None,
    );
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let ctx = r.pack_context("zephyr", 1000).await.unwrap();
    assert_eq!(ctx.passages.len(), 1);
    assert_eq!(ctx.passages[0].chunk_ids, ids);
    assert!(!ctx.response.text.contains("quokka"));
    assert!(ctx.response.text.contains("Intro paragraph."));
    assert!(ctx.response.text.contains("Closing paragraph."));
}

#[tokio::test]
async fn title_only_match_is_retrieved() {
    let store = Store::open_in_memory().unwrap();
    add(
        &store,
        Doc::new(
            "pension",
            "Pension consolidation",
            &["Moved the two old workplace pots into one provider in May."],
        ),
        None,
    );
    add(
        &store,
        Doc::new("other", "Groceries", &["Eggs, milk and bread."]),
        None,
    );
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let res = r.search("pension", 5).await.unwrap();
    assert_eq!(keys(&res.items), ["pension"]);
}

#[tokio::test]
async fn recency_boost_is_off_by_default_and_prefers_recent_items() {
    let store = Store::open_in_memory().unwrap();
    let text = ["Notes on the quarterly budget review."];
    add(
        &store,
        Doc::new("old", "Old", &text).modified(at("2024-01-01T00:00:00Z")),
        None,
    );
    add(
        &store,
        Doc::new("new", "New", &text).modified(at("2026-10-01T00:00:00Z")),
        None,
    );
    let now = at("2026-10-28T00:00:00Z");
    let cfg = RetrieveConfig {
        mmr: false,
        ..Default::default()
    };
    let off = Retriever::new(store.clone(), None, cfg.clone()).with_reference_time(now);
    let res = off.search("budget review", 2).await.unwrap();
    assert_eq!(keys(&res.items), ["old", "new"], "tie broken by chunk id");

    let on = Retriever::new(
        store,
        None,
        RetrieveConfig {
            recency: Some(RecencyBoost::default()),
            ..cfg
        },
    )
    .with_reference_time(now);
    let res = on.search("budget review", 2).await.unwrap();
    assert_eq!(keys(&res.items), ["new", "old"]);
    assert_eq!(res.items[0].score, 1.0);
    assert!(res.items[1].score < 1.0);
}

#[tokio::test]
async fn hits_carry_url_from_item_metadata() {
    let store = corpus(None);
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let hits = r.search("tram cathedral", 5).await.unwrap().hits();
    assert_eq!(hits[0].url.as_deref(), Some("file:///notes/lisbon.md"));
    assert_eq!(hits[0].title.as_deref(), Some("Lisbon weekend"));
    let hits = r.search("boiler", 5).await.unwrap().hits();
    assert_eq!(hits[0].url, None);
}

// ---- context packing ----

fn long_doc_store() -> (Store, Vec<ChunkId>) {
    let store = Store::open_in_memory().unwrap();
    let ids = add(
        &store,
        Doc::new(
            "story",
            "The Story",
            &[
                "Paragraph zero sets the scene.",
                "Paragraph one introduces the lighthouse keeper.",
                "Paragraph two reveals the zephyr signal.",
                "Paragraph three describes the storm.",
                "Paragraph four ends it.",
            ],
        )
        .url("file:///notes/story.md"),
        None,
    );
    (store, ids)
}

#[tokio::test]
async fn context_expands_neighbours_and_merges_adjacent_chunks() {
    let (store, ids) = long_doc_store();
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let ctx = r.pack_context("zephyr", 1000).await.unwrap();
    assert_eq!(
        ctx.response.text,
        "[1] The Story (file:///notes/story.md)\n\
         Paragraph one introduces the lighthouse keeper.\n\
         Paragraph two reveals the zephyr signal.\n\
         Paragraph three describes the storm."
    );
    assert_eq!(ctx.passages[0].chunk_ids, vec![ids[1], ids[2], ids[3]]);
    assert_eq!(ctx.response.citations.len(), 1);
    let c = &ctx.response.citations[0];
    assert_eq!((c.n, c.chunk_id), (1, ids[2]));
    assert_eq!(c.url.as_deref(), Some("file:///notes/story.md"));

    // A budget that only fits the matching passage: no expansion.
    let header_and_primary =
        "[1] The Story (file:///notes/story.md)\nParagraph two reveals the zephyr signal.";
    let tight = estimate_tokens(header_and_primary);
    let ctx = r.pack_context("zephyr", tight).await.unwrap();
    assert_eq!(ctx.response.text, header_and_primary);
    assert_eq!(ctx.passages[0].chunk_ids, vec![ids[2]]);
    assert!(!ctx.passages[0].truncated);

    // Expansion can be turned off.
    let r = Retriever::new(
        r.store().clone(),
        None,
        RetrieveConfig {
            expand_neighbours: false,
            ..Default::default()
        },
    );
    let ctx = r.pack_context("zephyr", 1000).await.unwrap();
    assert_eq!(ctx.passages[0].chunk_ids, vec![ids[2]]);
}

#[tokio::test]
async fn secondary_matches_render_with_gap_markers() {
    let (store, ids) = long_doc_store();
    let r = Retriever::new(
        store,
        None,
        RetrieveConfig {
            expand_neighbours: false,
            ..Default::default()
        },
    );
    // "zero" and "four" match the first and last paragraphs only.
    let ctx = r.pack_context("zero four", 1000).await.unwrap();
    let mut want = vec![ids[0], ids[4]];
    want.sort_unstable();
    assert_eq!(ctx.passages[0].chunk_ids, want);
    assert!(ctx.response.text.contains("scene.\n…\nParagraph four"));
}

/// xorshift64*: deterministic randomness without a dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[tokio::test]
async fn context_never_exceeds_the_budget() {
    let set = nugget_retrieve::eval::EvalSet::builtin().unwrap();
    let store = Store::open_in_memory().unwrap();
    nugget_retrieve::eval::index(&store, &set, None)
        .await
        .unwrap();
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let queries: Vec<&str> = set.queries.iter().map(|q| q.query.as_str()).collect();
    for _ in 0..300 {
        let budget = match rng.below(4) {
            0 => rng.below(40) as usize,
            1 => rng.below(200) as usize,
            _ => rng.below(2000) as usize,
        };
        let q = queries[rng.below(queries.len() as u64) as usize];
        let ctx = r.pack_context(q, budget).await.unwrap();
        let used = estimate_tokens(&ctx.response.text);
        assert!(used <= budget, "{used} > {budget} for {q:?}");
        assert_eq!(ctx.tokens, used);
        // Citations are 1..n, each with a header in the text, in order.
        let mut pos = 0;
        for (i, c) in ctx.response.citations.iter().enumerate() {
            assert_eq!(c.n as usize, i + 1);
            let marker = format!("[{}] ", c.n);
            let at = ctx.response.text[pos..]
                .find(&marker)
                .unwrap_or_else(|| panic!("missing {marker} in {:?}", ctx.response.text));
            pos += at + marker.len();
        }
        if ctx.response.citations.is_empty() {
            assert!(ctx.response.text.is_empty());
        }
    }
    let ctx = r.build_context("kestrel", 0).await.unwrap();
    assert!(ctx.text.is_empty() && ctx.citations.is_empty());
}

#[tokio::test]
async fn citation_numbering_is_stable_and_follows_rank() {
    let set = nugget_retrieve::eval::EvalSet::builtin().unwrap();
    let store = Store::open_in_memory().unwrap();
    nugget_retrieve::eval::index(&store, &set, None)
        .await
        .unwrap();
    let r = Retriever::new(store, None, RetrieveConfig::default());
    for q in ["Kestrel migration", "insurance renewal", "Biscuit"] {
        let a = r.build_context(q, 800).await.unwrap();
        let b = r.build_context(q, 800).await.unwrap();
        assert_eq!(a, b, "deterministic for {q:?}");
        assert!(!a.citations.is_empty());
        // Cited items appear in search order.
        let ranked: Vec<ItemRef> = r
            .search(q, r.config().context_max_items)
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|i| i.item)
            .collect();
        let mut last = 0;
        for c in &a.citations {
            let p = ranked.iter().position(|i| *i == c.item).unwrap();
            assert!(p >= last, "citation order follows rank");
            last = p;
        }
        // Headers carry title and source.
        let first = &a.citations[0];
        let header = format!(
            "[1] {} ({})",
            first.title.as_deref().unwrap(),
            first.url.as_deref().unwrap()
        );
        assert!(a.text.starts_with(&header), "{:?}", a.text);
        // A bigger budget only adds: what was cited stays cited, in the same order.
        let big = r.build_context(q, 20_000).await.unwrap();
        let mut it = big.citations.iter();
        for c in &a.citations {
            assert!(it.any(|b| b.item == c.item), "{q:?}: {:?} dropped", c.item);
        }
    }
}

#[tokio::test]
async fn oversized_passage_is_cut_with_a_marker() {
    let store = Store::open_in_memory().unwrap();
    let long = format!("zephyr {}", "lorem ipsum dolor ".repeat(200));
    add(&store, Doc::new("long", "Long", &[long.as_str()]), None);
    let r = Retriever::new(store, None, RetrieveConfig::default());
    let ctx = r.pack_context("zephyr", 100).await.unwrap();
    assert!(ctx.tokens <= 100);
    assert!(ctx.passages[0].truncated);
    assert!(ctx.response.text.ends_with('…'));
    assert!(
        ctx.response
            .text
            .starts_with("[1] Long (notes)\nzephyr lorem")
    );
}

/// Index a text-only item with explicit `sensitive` metadata (`None`: no flag at all).
fn add_flagged(store: &Store, key: &str, text: &str, sensitive: Option<bool>) {
    let meta = match sensitive {
        Some(s) => serde_json::json!({ "sensitive": s }),
        None => serde_json::json!({}),
    };
    let item = NewItem::new("notes", key, content_hash(text))
        .with_title(key)
        .with_metadata(meta);
    store.upsert_item(&item).unwrap();
    store
        .replace_chunks(&item.item, &[NewChunk::text(text)])
        .unwrap();
}

#[tokio::test]
async fn sensitive_items_can_be_left_out_of_the_context() {
    let store = Store::open_in_memory().unwrap();
    add_flagged(
        &store,
        "bank",
        "Heron account number 4111 for the heron savings.",
        Some(true),
    );
    add_flagged(
        &store,
        "walk",
        "We watched a heron by the river on Sunday.",
        Some(false),
    );
    add_flagged(
        &store,
        "legacy",
        "A heron note without a sensitivity flag.",
        None,
    );
    let r = Retriever::new(store, None, RetrieveConfig::default());

    // Default: everything relevant is packed, each citation flagged.
    let all = r.build_context("heron", 4000).await.unwrap();
    assert_eq!(all.citations.len(), 3);
    assert_eq!(all.excluded_sensitive, 0);
    let flag = |key: &str| {
        all.citations
            .iter()
            .find(|c| c.item.key.as_str() == key)
            .unwrap()
            .sensitive
    };
    assert!(flag("bank"));
    assert!(!flag("walk"));
    assert!(flag("legacy"), "a missing flag fails closed");
    assert!(all.text.contains("4111"));

    // For a cloud model: sensitive (and unflagged) items never reach the packed text.
    let opts = nugget_retrieve::ContextOptions {
        exclude_sensitive: true,
    };
    let cloud = r.build_context_with("heron", 4000, opts).await.unwrap();
    assert_eq!(cloud.excluded_sensitive, 2);
    let keys: Vec<&str> = cloud
        .citations
        .iter()
        .map(|c| c.item.key.as_str())
        .collect();
    assert_eq!(keys, vec!["walk"]);
    assert!(cloud.citations.iter().all(|c| !c.sensitive));
    assert!(!cloud.text.contains("4111"));
    assert!(!cloud.text.contains("sensitivity flag"));
    assert_eq!(
        cloud.citations[0].n, 1,
        "numbering restarts after exclusion"
    );
}
