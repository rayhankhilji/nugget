//! Retrieval evaluation on a labelled fixture corpus.
//!
//! An [`EvalSet`] is a small corpus of documents plus queries labelled with their relevant
//! document ids (binary relevance). [`index`] loads it into a store (one item per document, one
//! chunk per paragraph, under source [`EVAL_SOURCE`]); [`run`] searches every query in four
//! modes and reports recall@1/5/10, MRR@10 and nDCG@10 per mode:
//!
//! | mode | strategy | MMR |
//! |---|---|---|
//! | `text_only` | full-text (bm25) | off |
//! | `vector_only` | vector kNN | off |
//! | `hybrid` | RRF of both | off |
//! | `hybrid_mmr` | RRF of both | on |
//!
//! All modes group chunks by item first, so metrics are per document. Vector modes need an
//! embedding provider on the retriever; without one they are reported as not run.
//!
//! [`HashEmbedder`] is a deterministic, dependency-free embedding provider with a little semantic
//! signal (hashed bag of words with synonym folding), for tests and benchmarks without a model.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use nugget_core::capability::Locality;
use nugget_core::ids::{ItemKey, ItemRef, ProviderId, SourceId};
use nugget_core::{Error, Result};
use nugget_infer::{ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::Retriever;
use crate::types::{SearchOptions, Strategy};

/// Source id the eval corpus is indexed under.
pub const EVAL_SOURCE: &str = "eval";
/// Results considered per query.
pub const EVAL_DEPTH: usize = 10;

/// One corpus document. Paragraphs (blank-line separated) become chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalDoc {
    pub id: String,
    pub title: String,
    pub text: String,
    #[serde(default)]
    pub url: Option<String>,
    /// RFC 3339 modification time.
    #[serde(default)]
    pub modified: Option<String>,
}

/// A query with the ids of its relevant documents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalQuery {
    pub id: String,
    pub query: String,
    pub relevant: Vec<String>,
}

/// A labelled evaluation set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalSet {
    pub name: String,
    pub docs: Vec<EvalDoc>,
    pub queries: Vec<EvalQuery>,
    /// Synonym groups for [`HashEmbedder`] (the fake embedder's "semantics").
    #[serde(default)]
    pub synonyms: Vec<Vec<String>>,
}

impl EvalSet {
    /// The fixture set shipped with this crate (`tests/fixtures/eval/`).
    pub fn builtin() -> Result<Self> {
        Self::from_parts(
            "nugget-retrieve fixtures",
            include_str!("../tests/fixtures/eval/docs.json"),
            include_str!("../tests/fixtures/eval/queries.json"),
            include_str!("../tests/fixtures/eval/synonyms.json"),
        )
    }

    /// Load `docs.json`, `queries.json` and (optional) `synonyms.json` from a directory.
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let read = |name: &str| std::fs::read_to_string(dir.join(name));
        let synonyms = match read("synonyms.json") {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "[]".into(),
            Err(e) => return Err(e.into()),
        };
        Self::from_parts(
            &dir.display().to_string(),
            &read("docs.json")?,
            &read("queries.json")?,
            &synonyms,
        )
    }

    /// Parse and validate the three JSON documents.
    pub fn from_parts(name: &str, docs: &str, queries: &str, synonyms: &str) -> Result<Self> {
        let set = Self {
            name: name.to_owned(),
            docs: serde_json::from_str(docs)?,
            queries: serde_json::from_str(queries)?,
            synonyms: serde_json::from_str(synonyms)?,
        };
        set.validate()?;
        Ok(set)
    }

    /// Unique, non-empty doc and query ids; every relevant id names a document.
    pub fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        for d in &self.docs {
            if d.id.is_empty() || !ids.insert(d.id.as_str()) {
                return Err(Error::Invalid(format!(
                    "empty or duplicate doc id {:?}",
                    d.id
                )));
            }
            if paragraphs(&d.text).is_empty() {
                return Err(Error::Invalid(format!("doc {} has no text", d.id)));
            }
        }
        let mut qids = HashSet::new();
        for q in &self.queries {
            if q.id.is_empty() || !qids.insert(q.id.as_str()) {
                return Err(Error::Invalid(format!(
                    "empty or duplicate query id {:?}",
                    q.id
                )));
            }
            if q.relevant.is_empty() {
                return Err(Error::Invalid(format!(
                    "query {} has no relevant docs",
                    q.id
                )));
            }
            if let Some(r) = q.relevant.iter().find(|r| !ids.contains(r.as_str())) {
                return Err(Error::Invalid(format!(
                    "query {} names unknown doc {r}",
                    q.id
                )));
            }
        }
        Ok(())
    }

    fn item_ref(id: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new(EVAL_SOURCE),
            key: ItemKey::new(id),
        }
    }
}

/// Blank-line separated paragraphs, trimmed.
fn paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Index `set` into `store`: one item per document (title, `url` metadata, modification time),
/// one chunk per paragraph, embedded with `embedder` when given.
pub async fn index(store: &Store, set: &EvalSet, embedder: Option<&dyn Provider>) -> Result<()> {
    for d in &set.docs {
        let mut item = NewItem::new(EVAL_SOURCE, d.id.clone(), content_hash(&d.text))
            .with_title(d.title.clone())
            .with_mime("text/plain");
        if let Some(url) = &d.url {
            item = item.with_metadata(serde_json::json!({ "url": url }));
        }
        if let Some(m) = &d.modified {
            let at = OffsetDateTime::parse(m, &Rfc3339)
                .map_err(|e| Error::Invalid(format!("doc {}: bad modified time: {e}", d.id)))?;
            item = item.with_modified_at(at);
        }
        store.upsert_item(&item)?;
        let paras = paragraphs(&d.text);
        let chunks: Vec<NewChunk> = match embedder {
            Some(p) => {
                let vectors = p.embed(&paras).await?;
                if vectors.len() != paras.len() {
                    return Err(Error::Provider("embedder returned the wrong count".into()));
                }
                paras
                    .into_iter()
                    .zip(vectors)
                    .map(|(t, v)| NewChunk::embedded(t, v))
                    .collect()
            }
            None => paras.into_iter().map(NewChunk::text).collect(),
        };
        store.replace_chunks(&item.item, &chunks)?;
    }
    Ok(())
}

/// The four evaluated configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalMode {
    TextOnly,
    VectorOnly,
    Hybrid,
    HybridMmr,
}

impl EvalMode {
    pub const ALL: [EvalMode; 4] = [
        EvalMode::TextOnly,
        EvalMode::VectorOnly,
        EvalMode::Hybrid,
        EvalMode::HybridMmr,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EvalMode::TextOnly => "text_only",
            EvalMode::VectorOnly => "vector_only",
            EvalMode::Hybrid => "hybrid",
            EvalMode::HybridMmr => "hybrid_mmr",
        }
    }

    fn options(self) -> SearchOptions {
        let (strategy, mmr) = match self {
            EvalMode::TextOnly => (Strategy::TextOnly, false),
            EvalMode::VectorOnly => (Strategy::VectorOnly, false),
            EvalMode::Hybrid => (Strategy::Auto, false),
            EvalMode::HybridMmr => (Strategy::Auto, true),
        };
        SearchOptions {
            strategy,
            mmr: Some(mmr),
        }
    }
}

/// Mean metrics over all queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr_at_10: f64,
    pub ndcg_at_10: f64,
}

/// Ranked document ids for one query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryOutcome {
    pub id: String,
    pub ranked: Vec<String>,
    /// 1-based rank of the first relevant document within the depth, if any.
    pub first_relevant: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeReport {
    pub mode: EvalMode,
    /// False when the mode could not run (no embedder); metrics are then zero.
    pub ran: bool,
    /// Why it did not run, or how it degraded (e.g. hybrid answered text-only).
    pub note: Option<String>,
    pub metrics: Metrics,
    pub queries: Vec<QueryOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub set: String,
    pub docs: usize,
    pub queries: usize,
    pub modes: Vec<ModeReport>,
}

impl EvalReport {
    pub fn mode(&self, mode: EvalMode) -> Option<&ModeReport> {
        self.modes.iter().find(|m| m.mode == mode)
    }

    /// Flat `retrieval.<mode>.<metric>` pairs for benchmark reports (modes that ran only).
    pub fn metric_pairs(&self) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for m in self.modes.iter().filter(|m| m.ran) {
            let mode = m.mode.as_str();
            let x = m.metrics;
            for (name, v) in [
                ("recall@1", x.recall_at_1),
                ("recall@5", x.recall_at_5),
                ("recall@10", x.recall_at_10),
                ("mrr@10", x.mrr_at_10),
                ("ndcg@10", x.ndcg_at_10),
            ] {
                out.push((format!("retrieval.{mode}.{name}"), v));
            }
        }
        out
    }
}

impl fmt::Display for EvalReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} ({} docs, {} queries)",
            self.set, self.docs, self.queries
        )?;
        writeln!(
            f,
            "{:<12} {:>6} {:>6} {:>6} {:>7} {:>8}",
            "mode", "R@1", "R@5", "R@10", "MRR@10", "nDCG@10"
        )?;
        for m in &self.modes {
            if m.ran {
                let x = m.metrics;
                write!(
                    f,
                    "{:<12} {:>6.3} {:>6.3} {:>6.3} {:>7.3} {:>8.3}",
                    m.mode.as_str(),
                    x.recall_at_1,
                    x.recall_at_5,
                    x.recall_at_10,
                    x.mrr_at_10,
                    x.ndcg_at_10
                )?;
            } else {
                write!(f, "{:<12} not run", m.mode.as_str())?;
            }
            match &m.note {
                Some(n) => writeln!(f, "  ({n})")?,
                None => writeln!(f)?,
            }
        }
        Ok(())
    }
}

/// Fraction of `relevant` found in the top `k` of `ranked`.
pub fn recall_at(ranked: &[String], relevant: &HashSet<&str>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let found = ranked
        .iter()
        .take(k)
        .filter(|id| relevant.contains(id.as_str()))
        .count();
    found as f64 / relevant.len() as f64
}

/// Reciprocal rank of the first relevant result within the top `k` (0 if none).
pub fn mrr_at(ranked: &[String], relevant: &HashSet<&str>, k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .position(|id| relevant.contains(id.as_str()))
        .map_or(0.0, |p| 1.0 / (p as f64 + 1.0))
}

/// Binary-relevance nDCG over the top `k`.
pub fn ndcg_at(ranked: &[String], relevant: &HashSet<&str>, k: usize) -> f64 {
    let gain = |i: usize| 1.0 / ((i as f64) + 2.0).log2();
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, id)| relevant.contains(id.as_str()))
        .map(|(i, _)| gain(i))
        .sum();
    let ideal: f64 = (0..relevant.len().min(k)).map(gain).sum();
    if ideal == 0.0 { 0.0 } else { dcg / ideal }
}

/// Run every query of `set` through `retriever` in each [`EvalMode`]. `store` must hold the set
/// (see [`index`]); `Error::Invalid` otherwise.
pub async fn run(store: &Store, retriever: &Retriever, set: &EvalSet) -> Result<EvalReport> {
    set.validate()?;
    for d in &set.docs {
        if store.get_item(&EvalSet::item_ref(&d.id))?.is_none() {
            return Err(Error::Invalid(format!(
                "eval doc {} is not indexed; call eval::index first",
                d.id
            )));
        }
    }
    let mut modes = Vec::new();
    for mode in EvalMode::ALL {
        modes.push(run_mode(retriever, set, mode).await?);
    }
    Ok(EvalReport {
        set: set.name.clone(),
        docs: set.docs.len(),
        queries: set.queries.len(),
        modes,
    })
}

async fn run_mode(retriever: &Retriever, set: &EvalSet, mode: EvalMode) -> Result<ModeReport> {
    let mut sum = Metrics::default();
    let mut outcomes = Vec::with_capacity(set.queries.len());
    let mut fell_back = 0usize;
    for q in &set.queries {
        let results = match retriever
            .search_with(&q.query, EVAL_DEPTH, mode.options())
            .await
        {
            Ok(r) => r,
            Err(Error::Unsupported(msg)) if mode == EvalMode::VectorOnly => {
                return Ok(ModeReport {
                    mode,
                    ran: false,
                    note: Some(msg),
                    metrics: Metrics::default(),
                    queries: Vec::new(),
                });
            }
            Err(e) => return Err(e),
        };
        if matches!(mode, EvalMode::Hybrid | EvalMode::HybridMmr) && results.mode.is_text_only() {
            fell_back += 1;
        }
        let ranked: Vec<String> = results
            .items
            .iter()
            .filter(|r| r.item.source.as_str() == EVAL_SOURCE)
            .map(|r| r.item.key.as_str().to_owned())
            .collect();
        let relevant: HashSet<&str> = q.relevant.iter().map(String::as_str).collect();
        sum.recall_at_1 += recall_at(&ranked, &relevant, 1);
        sum.recall_at_5 += recall_at(&ranked, &relevant, 5);
        sum.recall_at_10 += recall_at(&ranked, &relevant, 10);
        sum.mrr_at_10 += mrr_at(&ranked, &relevant, 10);
        sum.ndcg_at_10 += ndcg_at(&ranked, &relevant, 10);
        let first_relevant = ranked
            .iter()
            .take(EVAL_DEPTH)
            .position(|id| relevant.contains(id.as_str()))
            .map(|p| p + 1);
        outcomes.push(QueryOutcome {
            id: q.id.clone(),
            ranked,
            first_relevant,
        });
    }
    let n = set.queries.len().max(1) as f64;
    let metrics = Metrics {
        recall_at_1: sum.recall_at_1 / n,
        recall_at_5: sum.recall_at_5 / n,
        recall_at_10: sum.recall_at_10 / n,
        mrr_at_10: sum.mrr_at_10 / n,
        ndcg_at_10: sum.ndcg_at_10 / n,
    };
    Ok(ModeReport {
        mode,
        ran: true,
        note: (fell_back > 0).then(|| format!("{fell_back} queries answered text-only")),
        metrics,
        queries: outcomes,
    })
}

// ---- fake embedder ----

/// Function words the fake embedder ignores.
const EMBED_STOPWORDS: &[&str] = &[
    "a", "about", "after", "all", "an", "and", "any", "are", "as", "at", "be", "been", "but", "by",
    "can", "did", "do", "does", "for", "from", "had", "has", "have", "how", "i", "if", "in",
    "into", "is", "it", "its", "me", "my", "of", "on", "or", "our", "so", "than", "that", "the",
    "their", "them", "then", "there", "these", "they", "this", "to", "up", "us", "was", "we",
    "were", "what", "when", "where", "which", "who", "why", "will", "with", "you", "your",
];

/// A deterministic embedding "model": hashed bag of words (FNV-1a, signed buckets, sublinear
/// term frequency, L2-normalised) after lowercasing, light plural stemming and folding each
/// synonym group to one token. Words that are synonyms embed identically, which gives vector
/// search a little semantic reach that full-text search lacks.
pub struct HashEmbedder {
    id: ProviderId,
    dim: usize,
    fold: HashMap<String, String>,
    locality: Locality,
    calls: AtomicUsize,
}

impl fmt::Debug for HashEmbedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HashEmbedder")
            .field("id", &self.id)
            .field("dim", &self.dim)
            .field("locality", &self.locality)
            .finish_non_exhaustive()
    }
}

impl HashEmbedder {
    /// An on-device embedder with id `hash-embedder`. `dim` is clamped to at least 8.
    pub fn new(dim: usize, synonyms: &[Vec<String>]) -> Self {
        let mut fold = HashMap::new();
        for group in synonyms {
            let mut words = group.iter().map(|w| stem(&w.to_lowercase()));
            if let Some(head) = words.next() {
                for w in words {
                    fold.entry(w).or_insert_with(|| head.clone());
                }
            }
        }
        Self {
            id: ProviderId::new("hash-embedder"),
            dim: dim.max(8),
            fold,
            locality: Locality::OnDevice,
            calls: AtomicUsize::new(0),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = ProviderId::new(id);
        self
    }

    /// Pretend to run somewhere else (to test locality gates).
    pub fn with_locality(mut self, locality: Locality) -> Self {
        self.locality = locality;
        self
    }

    /// Number of `embed` calls so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Normalised tokens as the embedder sees them.
    pub fn tokens(&self, text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| stem(&t.to_lowercase()))
            .filter(|t| !EMBED_STOPWORDS.contains(&t.as_str()))
            .map(|t| self.fold.get(&t).cloned().unwrap_or(t))
            .collect()
    }

    /// Embed one text synchronously.
    pub fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut tf: HashMap<String, u32> = HashMap::new();
        for t in self.tokens(text) {
            *tf.entry(t).or_default() += 1;
        }
        let mut v = vec![0.0f32; self.dim];
        for (t, n) in tf {
            let h = fnv1a(t.as_bytes());
            let bucket = (h % self.dim as u64) as usize;
            let sign = if (h >> 63) == 0 { 1.0 } else { -1.0 };
            v[bucket] += sign * (1.0 + (n as f32).ln());
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        } else {
            v[0] = 1.0;
        }
        v
    }
}

#[async_trait]
impl Provider for HashEmbedder {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "hash-bow".into(),
            locality: self.locality,
            context_tokens: None,
        }
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        Err(Error::Unsupported("HashEmbedder only embeds".into()))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts.iter().map(|t| self.embed_text(t)).collect())
    }
}

/// Very light stemming: plural `-ies` → `-y`, trailing `-s` (not `-ss`/`-us`/`-is`) dropped.
fn stem(w: &str) -> String {
    if w.len() > 4 && w.ends_with("ies") {
        return format!("{}y", &w[..w.len() - 3]);
    }
    if w.len() > 3
        && w.ends_with('s')
        && !w.ends_with("ss")
        && !w.ends_with("us")
        && !w.ends_with("is")
    {
        return w[..w.len() - 1].to_owned();
    }
    w.to_owned()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn metrics_match_hand_computation() {
        let ranked = ids(&["x", "a", "y", "b"]);
        let rel: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        assert_eq!(recall_at(&ranked, &rel, 1), 0.0);
        assert!((recall_at(&ranked, &rel, 10) - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(mrr_at(&ranked, &rel, 10), 0.5);
        assert_eq!(mrr_at(&ranked, &rel, 1), 0.0);
        let dcg = 1.0 / 3f64.log2() + 1.0 / 5f64.log2();
        let idcg = 1.0 + 1.0 / 3f64.log2() + 1.0 / 4f64.log2();
        assert!((ndcg_at(&ranked, &rel, 10) - dcg / idcg).abs() < 1e-12);
        let perfect = ids(&["a", "b", "c"]);
        assert!((ndcg_at(&perfect, &rel, 10) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn hash_embedder_folds_synonyms_and_is_deterministic() {
        let e = HashEmbedder::new(64, &[vec!["car".into(), "automobile".into()]]);
        assert_eq!(e.tokens("The Automobiles"), vec!["car"]);
        assert_eq!(e.embed_text("my car"), e.embed_text("an automobile"));
        let v = e.embed_text("anything at all");
        let norm: f32 = v.iter().map(|x| x * x).sum();
        assert!((norm - 1.0).abs() < 1e-5);
        assert_eq!(e.embed_text("the of"), {
            let mut z = vec![0.0; 64];
            z[0] = 1.0;
            z
        });
        assert_eq!(stem("policies"), "policy");
        assert_eq!(stem("class"), "class");
        assert_eq!(stem("bus"), "bus");
    }

    #[test]
    fn builtin_set_is_valid() {
        let set = EvalSet::builtin().unwrap();
        assert!(
            (40..=80).contains(&set.docs.len()),
            "{} docs",
            set.docs.len()
        );
        assert!(set.queries.len() >= 30, "{} queries", set.queries.len());
    }

    #[test]
    fn validation_rejects_bad_labels() {
        let docs = r#"[{"id":"a","title":"A","text":"hello"}]"#;
        let bad = r#"[{"id":"q","query":"hello","relevant":["zzz"]}]"#;
        assert!(EvalSet::from_parts("t", docs, bad, "[]").is_err());
        let dup = r#"[{"id":"a","title":"A","text":"x"},{"id":"a","title":"B","text":"y"}]"#;
        let ok = r#"[{"id":"q","query":"x","relevant":["a"]}]"#;
        assert!(EvalSet::from_parts("t", dup, ok, "[]").is_err());
        assert!(EvalSet::from_parts("t", docs, ok, "[]").is_ok());
    }
}
