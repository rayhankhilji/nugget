//! Pure ranking steps: per-item grouping, score normalisation, recency, similarity and MMR.
//!
//! Everything here is deterministic: ties break on input order, which callers make
//! deterministic (the store breaks score ties on chunk id).

use std::collections::{BTreeSet, HashMap};

use nugget_core::ids::ItemRef;
use nugget_store::{ChunkId, Hit};
use serde::{Deserialize, Serialize};

use crate::config::RecencyBoost;

/// Another chunk of the same item that matched the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkMatch {
    pub chunk_id: ChunkId,
    /// Score from the underlying search (comparable within one call only).
    pub score: f32,
}

/// One item with its best-matching chunk, before diversification.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub item: ItemRef,
    pub title: Option<String>,
    /// Best chunk.
    pub chunk_id: ChunkId,
    pub snippet: String,
    /// The best chunk's score from the store.
    pub raw_score: f32,
    /// Other matching chunks, best first.
    pub matches: Vec<ChunkMatch>,
    /// Relevance in `[0, 1]` (after normalisation and any recency boost).
    pub rel: f32,
}

/// Group chunk hits by item, keeping the best chunk per item and recording up to `max_matches`
/// other matching chunks. Items come out in the order of their best hit (best first); hits are
/// re-sorted by score (ties: chunk id) first so the result does not depend on input order.
pub fn group_by_item(mut hits: Vec<Hit>, max_matches: usize) -> Vec<Candidate> {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk_id.cmp(&b.chunk_id))
    });
    let mut index: HashMap<ItemRef, usize> = HashMap::new();
    let mut out: Vec<Candidate> = Vec::new();
    for hit in hits {
        match index.get(&hit.item) {
            Some(&i) => {
                let c = &mut out[i];
                if c.matches.len() < max_matches {
                    c.matches.push(ChunkMatch {
                        chunk_id: hit.chunk_id,
                        score: hit.score,
                    });
                }
            }
            None => {
                index.insert(hit.item.clone(), out.len());
                out.push(Candidate {
                    item: hit.item,
                    title: hit.title,
                    chunk_id: hit.chunk_id,
                    snippet: hit.snippet,
                    raw_score: hit.score,
                    matches: Vec::new(),
                    rel: 0.0,
                });
            }
        }
    }
    normalise(&mut out);
    out
}

/// Scale relevance into `[0, 1]` relative to the best candidate. Scores of different search
/// modes have different ranges (bm25, cosine, RRF); dividing by the maximum keeps their shape.
/// If no score is positive (e.g. all cosine similarities negative), fall back to `1 / (1 + rank)`.
pub fn normalise(cands: &mut [Candidate]) {
    let max = cands
        .iter()
        .map(|c| c.raw_score)
        .filter(|s| s.is_finite())
        .fold(f32::NEG_INFINITY, f32::max);
    if max > 0.0 {
        for c in cands.iter_mut() {
            let s = if c.raw_score.is_finite() {
                c.raw_score
            } else {
                0.0
            };
            c.rel = (s / max).clamp(0.0, 1.0);
        }
    } else {
        for (rank, c) in cands.iter_mut().enumerate() {
            c.rel = 1.0 / (1.0 + rank as f32);
        }
    }
}

/// Recency multiplier for an item `age_days` old: `1 + weight * 0.5^(age / half_life)`.
/// Future timestamps count as age 0; a non-positive half-life disables the boost.
pub fn recency_factor(age_days: f64, boost: &RecencyBoost) -> f32 {
    let half_life_ok = boost.half_life_days.is_finite() && boost.half_life_days > 0.0;
    if !half_life_ok || !boost.weight.is_finite() || !age_days.is_finite() {
        return 1.0;
    }
    let age = age_days.max(0.0);
    let decay = 0.5f64.powf(age / f64::from(boost.half_life_days));
    (1.0 + f64::from(boost.weight.max(0.0)) * decay) as f32
}

/// English function words: no topical signal for lexical similarity, and in an OR-ed full-text
/// query they match nearly every chunk (FTS5 has no stopword list).
pub const STOPWORDS: &[&str] = &[
    "a", "about", "after", "all", "am", "an", "and", "any", "are", "as", "at", "be", "been", "but",
    "by", "can", "could", "did", "do", "does", "for", "from", "had", "has", "have", "he", "her",
    "his", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "of", "on", "or", "our",
    "she", "so", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this",
    "to", "us", "was", "we", "were", "what", "when", "where", "which", "who", "why", "will",
    "with", "would", "you", "your",
];

fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token.to_lowercase().as_str())
}

/// The query for the full-text leg: alphanumeric words minus [`STOPWORDS`]. If nothing would be
/// left (a query of only function words), the words are kept as they are.
pub fn text_query(query: &str) -> String {
    let words: Vec<&str> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let content: Vec<&str> = words.iter().copied().filter(|t| !is_stopword(t)).collect();
    if content.is_empty() {
        words.join(" ")
    } else {
        content.join(" ")
    }
}

/// Lowercased alphanumeric tokens of `text` (stopwords and 1-character tokens dropped).
pub fn token_set(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() > 1)
        .filter(|t| !is_stopword(t))
        .map(str::to_lowercase)
        .collect()
}

/// Jaccard similarity of two token sets (`0` if both are empty).
pub fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    inter as f32 / union as f32
}

/// Cosine similarity (`0` for mismatched lengths or zero vectors).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

/// Greedy maximal marginal relevance: repeatedly pick the candidate maximising
/// `λ·rel(i) − (1−λ)·max_{j picked} sim(i, j)`. Returns up to `k` indices in pick order.
/// Ties go to the lower index, so the first pick is always the most relevant candidate.
pub fn mmr_select(
    rel: &[f32],
    k: usize,
    lambda: f32,
    sim: &dyn Fn(usize, usize) -> f32,
) -> Vec<usize> {
    let lambda = if lambda.is_finite() {
        lambda.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let n = rel.len();
    let k = k.min(n);
    let mut picked: Vec<usize> = Vec::with_capacity(k);
    let mut taken = vec![false; n];
    // Highest similarity of each candidate to anything picked so far.
    let mut max_sim = vec![0.0f32; n];
    while picked.len() < k {
        let mut best: Option<(usize, f32)> = None;
        for i in (0..n).filter(|&i| !taken[i]) {
            let penalty = if picked.is_empty() { 0.0 } else { max_sim[i] };
            let score = lambda * rel[i] - (1.0 - lambda) * penalty;
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((i, score));
            }
        }
        let Some((i, _)) = best else { break };
        taken[i] = true;
        picked.push(i);
        for j in (0..n).filter(|&j| !taken[j]) {
            let s = sim(i, j);
            if s.is_finite() && s > max_sim[j] {
                max_sim[j] = s;
            }
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{ItemKey, SourceId};

    fn hit(key: &str, chunk: ChunkId, score: f32) -> Hit {
        Hit {
            item: ItemRef {
                source: SourceId::new("s"),
                key: ItemKey::new(key),
            },
            chunk_id: chunk,
            title: Some(key.to_uppercase()),
            snippet: format!("chunk {chunk}"),
            score,
        }
    }

    #[test]
    fn grouping_keeps_best_chunk_and_records_others() {
        let hits = vec![
            hit("a", 1, 0.5),
            hit("b", 2, 0.9),
            hit("a", 3, 0.8),
            hit("c", 4, 0.1),
            hit("b", 5, 0.2),
        ];
        let g = group_by_item(hits, 8);
        let keys: Vec<_> = g.iter().map(|c| c.item.key.as_str()).collect();
        assert_eq!(keys, ["b", "a", "c"]);
        assert_eq!(g[0].chunk_id, 2);
        assert_eq!(
            g[0].matches,
            vec![ChunkMatch {
                chunk_id: 5,
                score: 0.2
            }]
        );
        assert_eq!(g[1].chunk_id, 3);
        assert_eq!(g[1].matches[0].chunk_id, 1);
        assert!((g[0].rel - 1.0).abs() < 1e-6);
        assert!((g[2].rel - 0.1 / 0.9).abs() < 1e-6);
    }

    #[test]
    fn grouping_is_order_independent_and_caps_matches() {
        let a = vec![hit("a", 1, 0.5), hit("a", 2, 0.5), hit("a", 3, 0.4)];
        let mut b = a.clone();
        b.reverse();
        let (ga, gb) = (group_by_item(a, 1), group_by_item(b, 1));
        assert_eq!(ga, gb);
        assert_eq!(ga[0].chunk_id, 1, "score tie breaks on chunk id");
        assert_eq!(ga[0].matches.len(), 1);
    }

    #[test]
    fn non_positive_scores_normalise_by_rank() {
        let g = group_by_item(vec![hit("a", 1, -0.1), hit("b", 2, -0.3)], 4);
        assert_eq!(g[0].rel, 1.0);
        assert_eq!(g[1].rel, 0.5);
    }

    #[test]
    fn mmr_prefers_novel_candidates() {
        // 0 and 1 are near-duplicates; 2 is a little less relevant but different.
        let rel = [1.0, 0.95, 0.8];
        let sim = |i: usize, j: usize| {
            if (i, j) == (0, 1) || (i, j) == (1, 0) {
                0.95
            } else {
                0.05
            }
        };
        assert_eq!(mmr_select(&rel, 3, 0.7, &sim), vec![0, 2, 1]);
        // λ = 1 is pure relevance.
        assert_eq!(mmr_select(&rel, 3, 1.0, &sim), vec![0, 1, 2]);
        assert_eq!(mmr_select(&rel, 2, 0.5, &sim), vec![0, 2]);
        assert!(mmr_select(&[], 3, 0.5, &sim).is_empty());
    }

    #[test]
    fn similarity_measures() {
        let a = token_set("The quick brown fox, the QUICK dog");
        let b = token_set("quick fox jumps");
        assert_eq!(
            a.iter().map(String::as_str).collect::<Vec<_>>(),
            ["brown", "dog", "fox", "quick"]
        );
        assert!((jaccard(&a, &b) - 2.0 / 5.0).abs() < 1e-6);
        assert_eq!(jaccard(&BTreeSet::new(), &BTreeSet::new()), 0.0);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn text_query_drops_function_words() {
        assert_eq!(text_query("present for my Mother?"), "present Mother");
        assert_eq!(text_query("The Who"), "The Who");
        assert_eq!(text_query("what is it"), "what is it");
        assert_eq!(text_query("  "), "");
        assert_eq!(text_query("F28 (ignition)"), "F28 ignition");
    }

    #[test]
    fn recency_decays_with_age() {
        let b = RecencyBoost {
            half_life_days: 10.0,
            weight: 0.5,
        };
        assert!((recency_factor(0.0, &b) - 1.5).abs() < 1e-6);
        assert!((recency_factor(10.0, &b) - 1.25).abs() < 1e-6);
        assert!((recency_factor(-5.0, &b) - 1.5).abs() < 1e-6);
        assert!(recency_factor(1000.0, &b) < 1.0001);
        let off = RecencyBoost {
            half_life_days: 0.0,
            weight: 1.0,
        };
        assert_eq!(recency_factor(0.0, &off), 1.0);
    }
}
