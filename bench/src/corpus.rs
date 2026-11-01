//! Deterministic synthetic corpus and query generation.
//!
//! Bodies mix Zipf background words, topic words and rare words to mimic a notes or mail corpus;
//! each known-item query has a single relevant document. Seeded, so identical on every OS.

/// SplitMix64: tiny, fast, deterministic and portable. Not cryptographic.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform float in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in `[0, n)`. `n` must be non-zero.
    pub fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next_u64() % n as u64) as usize
    }

    /// Exponentially distributed sample with the given mean.
    pub fn exp(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.unit()).ln()
    }
}

const SYLLABLES: [&str; 16] = [
    "ka", "lo", "mi", "ru", "ta", "ne", "so", "vi", "de", "pa", "zu", "re", "fo", "gi", "ba", "ly",
];

/// The `i`-th vocabulary word. Distinct `i` always give distinct words.
pub fn word(i: usize) -> String {
    let mut n = i;
    let mut s = String::with_capacity(8);
    let mut len = 0;
    loop {
        s.push_str(SYLLABLES[n % 16]);
        n /= 16;
        len += 1;
        if n == 0 && len >= 2 {
            return s;
        }
    }
}

/// Corpus shape. Defaults suit a quick local run.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusConfig {
    pub docs: usize,
    pub topics: usize,
    pub vocab: usize,
    pub words_per_topic: usize,
    /// Body length in words: exactly this many when `length_sigma` is 0 (the default), else
    /// the median of a log-normal length distribution.
    pub words_per_doc: usize,
    pub seed: u64,
    /// σ of `ln(words)` for body length; 0 gives exactly `words_per_doc` words.
    pub length_sigma: f64,
}

/// σ of `ln(words)` for a mailbox-like size mix (with a median of 80 words: about 10% of items
/// under 22 words, 10% over 290, 1% over 1,000; capped at [`MAX_WORDS_PER_DOC`]).
pub const MAIL_LENGTH_SIGMA: f64 = 1.0;
/// Longest body a log-normal draw may produce.
pub const MAX_WORDS_PER_DOC: usize = 4_000;
/// Shortest body a log-normal draw may produce.
pub const MIN_WORDS_PER_DOC: usize = 8;

impl Default for CorpusConfig {
    fn default() -> Self {
        Self {
            docs: 1_000,
            topics: 40,
            vocab: 20_000,
            words_per_topic: 60,
            words_per_doc: 120,
            seed: 42,
            length_sigma: 0.0,
        }
    }
}

/// A generated document. `words` holds the vocabulary ids of the body (no marker), which is
/// what queries are sampled from.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub id: usize,
    pub topic: usize,
    pub title: String,
    pub body: String,
    pub words: Vec<u32>,
}

/// A known-item query: exactly one relevant document.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub text: String,
    pub relevant: usize,
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub config: CorpusConfig,
    pub vocab: Vec<String>,
    pub topic_words: Vec<Vec<u32>>,
    pub docs: Vec<Doc>,
}

impl Corpus {
    pub fn generate(config: CorpusConfig) -> Self {
        assert!(config.vocab >= 16, "vocabulary too small");
        assert!(config.topics > 0 && config.words_per_topic > 0 && config.words_per_doc > 0);
        let mut rng = Rng::new(config.seed);
        let vocab: Vec<String> = (0..config.vocab).map(word).collect();
        let topic_words: Vec<Vec<u32>> = (0..config.topics)
            .map(|_| {
                (0..config.words_per_topic)
                    .map(|_| rng.below(config.vocab) as u32)
                    .collect()
            })
            .collect();
        let mut corpus = Self {
            config,
            vocab,
            topic_words,
            docs: Vec::new(),
        };
        let n = corpus.config.docs;
        corpus.docs.reserve(n);
        for id in 0..n {
            let topic = rng.below(corpus.config.topics);
            let doc = corpus.make_doc(&mut rng, id, topic);
            corpus.docs.push(doc);
        }
        corpus
    }

    /// Draw a vocabulary id from a Zipf-like (log-uniform rank) distribution.
    fn zipf(&self, rng: &mut Rng) -> u32 {
        let v = self.config.vocab as f64;
        let idx = v.powf(rng.unit()) as usize;
        (idx.saturating_sub(1)).min(self.config.vocab - 1) as u32
    }

    fn draw_word(&self, rng: &mut Rng, topic: usize) -> u32 {
        let r = rng.unit();
        if r < 0.55 {
            self.zipf(rng)
        } else if r < 0.85 {
            let tw = &self.topic_words[topic];
            tw[rng.below(tw.len())]
        } else {
            rng.below(self.config.vocab) as u32
        }
    }

    /// Body length of the next document: fixed, or a log-normal draw (Box–Muller) when
    /// `length_sigma` is set. A fixed length draws nothing, so fixed corpora are unchanged.
    fn doc_len(&self, rng: &mut Rng) -> usize {
        let sigma = self.config.length_sigma;
        if sigma <= 0.0 {
            return self.config.words_per_doc;
        }
        let u1 = rng.unit().max(f64::MIN_POSITIVE);
        let u2 = rng.unit();
        let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
        let len = (self.config.words_per_doc as f64 * (sigma * z).exp()).round() as usize;
        len.clamp(MIN_WORDS_PER_DOC, MAX_WORDS_PER_DOC)
    }

    /// Generate a fresh document (also used to produce edits of an existing one).
    pub fn make_doc(&self, rng: &mut Rng, id: usize, topic: usize) -> Doc {
        let len = self.doc_len(rng);
        let words: Vec<u32> = (0..len).map(|_| self.draw_word(rng, topic)).collect();
        let title_words: Vec<u32> = (0..3).map(|_| self.draw_word(rng, topic)).collect();
        Doc {
            id,
            topic,
            title: self.render(&title_words),
            body: self.render(&words),
            words,
        }
    }

    pub fn render(&self, ids: &[u32]) -> String {
        let mut s = String::with_capacity(ids.len() * 7);
        for (i, &w) in ids.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(&self.vocab[w as usize]);
        }
        s
    }

    /// Generate `n` known-item queries of `terms` words each. With probability `noise`, one
    /// query word is replaced by a random vocabulary word.
    pub fn queries(&self, n: usize, terms: usize, noise: f64, seed: u64) -> Vec<Query> {
        let mut rng = Rng::new(seed ^ 0x5157_4A3E_0000_0001);
        let mut out = Vec::with_capacity(n);
        if self.docs.is_empty() {
            return out;
        }
        for _ in 0..n {
            let doc = &self.docs[rng.below(self.docs.len())];
            let mut ids: Vec<u32> = Vec::with_capacity(terms);
            let mut attempts = 0;
            while ids.len() < terms && attempts < terms * 8 {
                attempts += 1;
                let w = doc.words[rng.below(doc.words.len())];
                if !ids.contains(&w) {
                    ids.push(w);
                }
            }
            if !ids.is_empty() && rng.unit() < noise {
                let slot = rng.below(ids.len());
                ids[slot] = rng.below(self.config.vocab) as u32;
            }
            out.push(Query {
                text: self.render(&ids),
                relevant: doc.id,
            });
        }
        out
    }
}

/// Unique marker token for version `version` of document `id`. Markers let the freshness
/// simulation check that an edit (or delete) really propagated.
pub fn marker(id: usize, version: u32) -> String {
    format!("mk{id}v{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        let cfg = CorpusConfig {
            docs: 50,
            ..CorpusConfig::default()
        };
        let a = Corpus::generate(cfg.clone());
        let b = Corpus::generate(cfg.clone());
        assert_eq!(a.docs, b.docs);
        assert_eq!(a.queries(20, 4, 0.3, 7), b.queries(20, 4, 0.3, 7));
        let c = Corpus::generate(CorpusConfig { seed: 43, ..cfg });
        assert_ne!(a.docs, c.docs);
    }
}
