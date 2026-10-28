//! Vector search: exact cosine kNN over `chunk_vec`, and an approximate path for large vector
//! sets through a binary-quantized index.
//!
//! `sqlite-vec`'s `vec0` kNN is a brute-force scan, linear in the number of vectors (each one a
//! `4 × dim`-byte float array that SQLCipher must decrypt), which dominates query latency from
//! roughly a hundred thousand chunks. From [`ANN_MIN_VECTORS`] vectors on, search instead:
//!
//! 1. scans `ann_bits`, a `vec0 (bits bit[dim])` table holding the **sign bits** of each
//!    embedding (32× smaller than the floats; hamming distance is a popcount), for the
//!    `k × ANN_OVERSAMPLE` nearest candidates by hamming distance, then
//! 2. rescores those candidates by exact cosine distance and keeps the best `k`.
//!
//! Sign quantization approximates the angle between vectors (random-hyperplane LSH with the
//! coordinate axes as hyperplanes); rescoring keeps the returned scores exact, only recall is
//! approximate. Bits are taken **relative to a centre**, the mean of the vectors stored when the
//! index was built (kept in `meta`, then frozen): embeddings that share a large common component,
//! or sparse ones whose zero coordinates would all quantize alike, otherwise collapse onto a few
//! bit patterns.
//!
//! Sign bits only carry information for dense vectors. A sparse query (most coordinates zero,
//! as from hashed bag-of-words embedders) quantizes to noise on every zero coordinate: on the
//! bench's 256-dim hash embeddings at 20k vectors, recall@10 against exact search was 0.03 with
//! plain sign bits and 0.06 with centring. Such queries ([`quantizable`]) always use the exact
//! scan; neural embedding models produce dense vectors and take the approximate path.
//!
//! Rescoring reads the candidates' floats from `ann_vectors`, a plain rowid table holding a copy
//! of each embedding. Reading them from `chunk_vec` instead would cost more than the scan it
//! replaces: `vec0` keeps 1024 vectors per (1.5 MB at 384 dimensions) blob, and a point lookup
//! walks that blob's overflow-page chain up to the vector, measured at ~0.2 ms in memory and
//! ~2 ms on an encrypted file per lookup. The copy doubles vector storage, so the index is only
//! built once a store needs it: the first approximate search creates both tables and fills them
//! from `chunk_vec` (in batches, idempotently; see [`ensure_ann_ready`]), and from then on
//! `items::write_chunks` writes them in the same transaction as `chunk_vec`, and a delete trigger
//! on `chunks` cleans them.

use nugget_core::{Error, Result};
use rusqlite::{CachedStatement, Connection, OptionalExtension, TransactionBehavior, params};

use crate::Store;
use crate::error::DbExt;
use crate::items::{embedding_dim, f32_blob, f32_from_blob, item_ref, validate_embedding};
use crate::search::{MAX_KNN, excerpt};
use crate::types::{ChunkId, Hit};

/// Vector count from which [`Store::search_vector`] switches from exact to approximate search.
///
/// Below it the exact scan is fast enough (tens of milliseconds on an encrypted file) and the
/// approximate path would cost recall and the index's storage for little gain.
pub const ANN_MIN_VECTORS: usize = 20_000;
/// Approximate search rescores this many hamming candidates per requested hit...
pub const ANN_OVERSAMPLE: usize = 10;
/// ...but at least this many.
const ANN_MIN_CANDIDATES: usize = 100;
/// `meta` flag: the approximate-search index holds every vector of `chunk_vec`.
const META_ANN_READY: &str = "ann_index_ready";
/// `meta` key: the quantization centre (hex of little-endian `f32`s), written with the tables.
const META_ANN_CENTER: &str = "ann_center";

/// Least share of non-zero coordinates for a query to take the approximate path.
const ANN_MIN_DENSITY: f32 = 0.5;

/// Whether `v` is dense enough for its sign bits to rank neighbours (see the module docs).
fn quantizable(v: &[f32]) -> bool {
    let nonzero = v.iter().filter(|x| x.abs() > f32::EPSILON).count();
    nonzero as f32 >= v.len() as f32 * ANN_MIN_DENSITY
}

/// Bit-vector width for embeddings of `dim` dimensions: `vec0` bit vectors need a multiple of 8,
/// so the tail is padded with zero bits (equal in every vector, so hamming distances are
/// unchanged).
fn bits_dim(dim: usize) -> usize {
    dim.div_ceil(8) * 8
}

/// Sign bits of `v - center`, packed little-endian within each byte (bit `i` is
/// `v[i] > center[i]`; a missing centre coordinate counts as 0).
fn sign_bits(v: &[f32], center: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len().div_ceil(8)];
    for (i, x) in v.iter().enumerate() {
        if *x > center.get(i).copied().unwrap_or(0.0) {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// Create the approximate-search tables and their delete trigger if missing. Idempotent; does
/// **not** mark the index complete (see [`META_ANN_READY`]).
fn create_ann_tables(conn: &Connection, dim: usize) -> Result<()> {
    let bits = bits_dim(dim);
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS ann_bits USING vec0 (bits bit[{bits}]);
         CREATE TABLE IF NOT EXISTS ann_vectors (
             id        INTEGER PRIMARY KEY,
             embedding BLOB NOT NULL
         );
         CREATE TRIGGER IF NOT EXISTS chunks_ann_ad AFTER DELETE ON chunks BEGIN
             DELETE FROM ann_bits WHERE rowid = old.id;
             DELETE FROM ann_vectors WHERE id = old.id;
         END;"
    ))
    .db("create approximate vector index")
}

/// Writes a vector into the approximate-search index (both tables), inside the caller's
/// transaction.
pub(crate) struct AnnWriter<'c> {
    bits: CachedStatement<'c>,
    vectors: CachedStatement<'c>,
    center: Vec<f32>,
}

impl<'c> AnnWriter<'c> {
    /// `None` while the index has not been built (nothing to maintain).
    pub(crate) fn prepare(conn: &'c Connection) -> Result<Option<Self>> {
        let exists: bool = conn
            .prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master
                                WHERE type = 'table' AND name = 'ann_vectors')",
            )
            .db("prepare index lookup")?
            .query_row([], |r| r.get(0))
            .db("look up approximate vector index")?;
        if !exists {
            return Ok(None);
        }
        Ok(Some(Self {
            center: ann_center(conn)?,
            bits: conn
                .prepare_cached("INSERT INTO ann_bits (rowid, bits) VALUES (?1, vec_bit(?2))")
                .db("prepare vector bits insert")?,
            vectors: conn
                .prepare_cached("INSERT INTO ann_vectors (id, embedding) VALUES (?1, ?2)")
                .db("prepare vector copy insert")?,
        }))
    }

    pub(crate) fn insert(&mut self, id: ChunkId, embedding: &[f32]) -> Result<()> {
        self.bits
            .execute(params![id, sign_bits(embedding, &self.center)])
            .db("insert vector bits")?;
        self.vectors
            .execute(params![id, f32_blob(embedding)])
            .db("insert vector copy")?;
        Ok(())
    }
}

/// The quantization centre ([`META_ANN_CENTER`]); empty (no centring) if absent or malformed.
fn ann_center(conn: &Connection) -> Result<Vec<f32>> {
    let hex: Option<String> = conn
        .prepare_cached("SELECT value FROM meta WHERE key = ?1")
        .db("prepare index centre read")?
        .query_row([META_ANN_CENTER], |r| r.get(0))
        .optional()
        .db("read index centre")?;
    Ok(hex
        .and_then(|h| hex::decode(h).ok())
        .and_then(|b| f32_from_blob(&b))
        .unwrap_or_default())
}

/// Mean of the vectors at `positions` (see [`ensure_ann_ready`]), one sequential read per `vec0`
/// chunk. Zeros when there are none.
fn mean_vector(
    conn: &Connection,
    positions: &[(i64, i64, ChunkId)],
    dim: usize,
) -> Result<Vec<f32>> {
    let stride = dim * 4;
    let mut sum = vec![0f64; dim];
    let mut n = 0usize;
    let mut read = conn
        .prepare_cached("SELECT vectors FROM chunk_vec_vector_chunks00 WHERE _rowid_ = ?1")
        .db("prepare vector chunk read")?;
    for group in positions.chunk_by(|a, b| a.0 == b.0) {
        let blob: Option<Vec<u8>> = read
            .query_row([group[0].0], |r| r.get(0))
            .optional()
            .db("read vector chunk")?;
        let Some(blob) = blob else {
            continue;
        };
        for &(_, offset, _) in group {
            let v = usize::try_from(offset)
                .ok()
                .and_then(|o| blob.get(o * stride..(o + 1) * stride))
                .and_then(f32_from_blob);
            if let Some(v) = v {
                for (s, x) in sum.iter_mut().zip(&v) {
                    *s += f64::from(*x);
                }
                n += 1;
            }
        }
    }
    let n = n.max(1) as f64;
    Ok(sum.into_iter().map(|s| (s / n) as f32).collect())
}

fn ann_ready(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM meta WHERE key = ?1)",
        [META_ANN_READY],
        |r| r.get(0),
    )
    .db("read approximate index flag")
}

/// Make sure the approximate-search index holds every vector, building or completing it from
/// `chunk_vec` if not.
///
/// The tables are created (and committed) first, so writes from then on maintain them. The
/// existing vectors are then copied one `vec0` chunk (up to 1024 vectors) per transaction,
/// reading each chunk's vector blob once from `vec0`'s storage (`chunk_vec_rowids` maps a rowid
/// to its chunk and offset, `chunk_vec_vector_chunks00` holds the blobs): a sequential read
/// instead of one overflow-chain walk per vector. Vectors already indexed, or deleted or moved
/// since the scan began, are skipped, so an interrupted build resumes where it stopped and
/// repeating it is harmless.
fn ensure_ann_ready(conn: &mut Connection, dim: usize) -> Result<()> {
    if ann_ready(conn)? {
        return Ok(());
    }
    // (vec0 chunk, offset in it, rowid = chunk id), in storage order.
    let mut positions: Vec<(i64, i64, ChunkId)> = {
        let mut stmt = conn
            .prepare(
                "SELECT chunk_id, chunk_offset, rowid FROM chunk_vec_rowids
                 WHERE chunk_id IS NOT NULL",
            )
            .db("prepare vector position scan")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .db("vector position scan")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("vector position scan")?
    };
    positions.sort_unstable();
    // The centre is fixed once, with the tables, so every bit vector uses the same one; a build
    // resumed after an interruption keeps it.
    if ann_center(conn)?.len() != dim {
        let center = mean_vector(conn, &positions, dim)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .db("begin approximate index build")?;
        tx.execute_batch(
            "DROP TABLE IF EXISTS ann_bits;
             DROP TABLE IF EXISTS ann_vectors;
             DROP TRIGGER IF EXISTS chunks_ann_ad;",
        )
        .db("reset approximate vector index")?;
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![META_ANN_CENTER, hex::encode(f32_blob(&center))],
        )
        .db("store index centre")?;
        create_ann_tables(&tx, dim)?;
        tx.commit().db("commit approximate index tables")?;
    } else {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .db("begin approximate index build")?;
        create_ann_tables(&tx, dim)?;
        tx.commit().db("commit approximate index tables")?;
    }
    let stride = dim * 4;
    let mut added = 0usize;
    for group in positions.chunk_by(|a, b| a.0 == b.0) {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .db("begin approximate index batch")?;
        {
            // vec0 opens these blobs by the table's real rowid (its `rowid` column is not an
            // alias of it), hence `_rowid_`.
            let blob: Option<Vec<u8>> = tx
                .prepare_cached("SELECT vectors FROM chunk_vec_vector_chunks00 WHERE _rowid_ = ?1")
                .db("prepare vector chunk read")?
                .query_row([group[0].0], |r| r.get(0))
                .optional()
                .db("read vector chunk")?;
            let Some(blob) = blob else {
                continue;
            };
            let mut position = tx
                .prepare_cached(
                    "SELECT chunk_id, chunk_offset FROM chunk_vec_rowids WHERE rowid = ?1",
                )
                .db("prepare vector position check")?;
            let mut indexed = tx
                .prepare_cached("SELECT 1 FROM ann_vectors WHERE id = ?1")
                .db("prepare index check")?;
            let mut writer = AnnWriter::prepare(&tx)?.ok_or_else(|| {
                Error::Storage("approximate vector index disappeared during its build".into())
            })?;
            for &(chunk, offset, id) in group {
                let now: Option<(i64, i64)> = position
                    .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()
                    .db("check vector position")?;
                if now != Some((chunk, offset)) {
                    continue;
                }
                if indexed
                    .query_row([id], |_| Ok(()))
                    .optional()
                    .db("check index")?
                    .is_some()
                {
                    continue;
                }
                let v = usize::try_from(offset)
                    .ok()
                    .and_then(|o| blob.get(o * stride..(o + 1) * stride))
                    .and_then(f32_from_blob)
                    .ok_or_else(|| {
                        Error::Storage(format!("malformed stored vector for chunk {id}"))
                    })?;
                writer.insert(id, &v)?;
                added += 1;
            }
        }
        tx.commit().db("commit approximate index batch")?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')",
        [META_ANN_READY],
    )
    .db("mark approximate index ready")?;
    tracing::info!(vectors = added, "built the approximate vector index");
    Ok(())
}

/// Number of stored vectors.
fn vector_count(conn: &Connection) -> Result<usize> {
    // `vec0` keeps one row per vector in its `_rowids` shadow table; counting the virtual table
    // itself would read every vector.
    conn.query_row("SELECT count(*) FROM chunk_vec_rowids", [], |r| {
        r.get::<_, i64>(0)
    })
    .db("count vectors")
    .map(|n| usize::try_from(n).unwrap_or(0))
}

/// Which kNN strategy a vector search uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Exact below [`ANN_MIN_VECTORS`], approximate from there on.
    Auto,
    Exact,
    Approx,
}

/// Exact `k` nearest neighbours by cosine distance (`vec0` brute force).
fn exact_neighbours(c: &Connection, embedding: &[f32], k: usize) -> Result<Vec<(ChunkId, f64)>> {
    let mut knn = c
        .prepare_cached(
            "SELECT rowid, distance FROM chunk_vec
             WHERE embedding MATCH ?1 AND k = ?2
             ORDER BY distance",
        )
        .db("prepare vector search")?;
    let rows = knn
        .query_map(params![f32_blob(embedding), k as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .db("vector search")?;
    rows.collect::<rusqlite::Result<_>>().db("vector search")
}

/// Approximate `k` nearest neighbours: hamming kNN on the sign bits for `k × ANN_OVERSAMPLE`
/// candidates, rescored by exact cosine distance. The index must be ready.
fn approx_neighbours(c: &Connection, embedding: &[f32], k: usize) -> Result<Vec<(ChunkId, f64)>> {
    let candidates = k
        .saturating_mul(ANN_OVERSAMPLE)
        .clamp(ANN_MIN_CANDIDATES, MAX_KNN);
    let center = ann_center(c)?;
    let ids: Vec<ChunkId> = {
        let mut knn = c
            .prepare_cached(
                "SELECT rowid FROM ann_bits
                 WHERE bits MATCH vec_bit(?1) AND k = ?2
                 ORDER BY distance",
            )
            .db("prepare binary vector search")?;
        let rows = knn
            .query_map(
                params![sign_bits(embedding, &center), candidates as i64],
                |r| r.get(0),
            )
            .db("binary vector search")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("binary vector search")?
    };
    // Same distance function as the exact scan, so scores do not depend on the path taken.
    let mut rescore = c
        .prepare_cached("SELECT vec_distance_cosine(embedding, ?2) FROM ann_vectors WHERE id = ?1")
        .db("prepare vector rescoring")?;
    let query = f32_blob(embedding);
    let mut scored = Vec::with_capacity(ids.len());
    for id in ids {
        let d: Option<f64> = rescore
            .query_row(params![id, query], |r| r.get(0))
            .optional()
            .db("vector rescoring")?;
        scored.extend(d.map(|d| (id, d)));
    }
    scored.sort_by(|(ia, da), (ib, db)| da.total_cmp(db).then(ia.cmp(ib)));
    scored.truncate(k);
    Ok(scored)
}

/// Chunk and item rows for kNN results, best first.
fn neighbour_hits(c: &Connection, neighbours: Vec<(ChunkId, f64)>) -> Result<Vec<Hit>> {
    let mut details = c
        .prepare_cached(
            "SELECT i.source, i.key, i.title, ch.text
             FROM chunks ch JOIN items i ON i.id = ch.item_id
             WHERE ch.id = ?1",
        )
        .db("prepare chunk lookup")?;
    let mut hits = Vec::with_capacity(neighbours.len());
    for (chunk_id, distance) in neighbours {
        let hit = details
            .query_row([chunk_id], |r| {
                let text: String = r.get(3)?;
                Ok(Hit {
                    chunk_id,
                    item: item_ref(r.get(0)?, r.get(1)?),
                    title: r.get(2)?,
                    snippet: excerpt(&text),
                    score: (1.0 - distance) as f32,
                })
            })
            .optional()
            .db("chunk lookup")?;
        // A vector without a chunk cannot happen (the delete triggers remove both), but
        // skipping it is the safe reading if it ever does.
        hits.extend(hit);
    }
    // Equal distances: order by chunk id for determinism.
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk_id.cmp(&b.chunk_id))
    });
    Ok(hits)
}

impl Store {
    /// k-nearest-neighbour search by cosine distance (best first; `score` is cosine similarity).
    ///
    /// Exact while the store holds fewer than [`ANN_MIN_VECTORS`] vectors, for sparse queries (or when
    /// `limit × ANN_OVERSAMPLE` exceeds 4096); approximate from there on, through the
    /// binary-quantized index described in this module: scores stay exact, but a true
    /// neighbour may occasionally be missed. The first approximate search builds that index
    /// (measured at 3.8 s for 100k vectors on an encrypted file); if building it fails, search
    /// stays exact.
    ///
    /// Returns `Error::Invalid` if the query's dimension differs from the store's. An empty result
    /// is returned while no embeddings have been stored.
    pub fn search_vector(&self, embedding: &[f32], limit: usize) -> Result<Vec<Hit>> {
        self.search_vector_mode(embedding, limit, Mode::Auto)
    }

    /// [`Store::search_vector`] always using the exact scan, whatever the store's size (for
    /// recall measurements and benchmarks).
    pub fn search_vector_exact(&self, embedding: &[f32], limit: usize) -> Result<Vec<Hit>> {
        self.search_vector_mode(embedding, limit, Mode::Exact)
    }

    /// [`Store::search_vector`] always using the approximate path (building its index if
    /// needed), whatever the store's size; still exact when `limit × ANN_OVERSAMPLE` exceeds
    /// 4096. For tests and benchmarks.
    #[doc(hidden)]
    pub fn search_vector_approx(&self, embedding: &[f32], limit: usize) -> Result<Vec<Hit>> {
        self.search_vector_mode(embedding, limit, Mode::Approx)
    }

    fn search_vector_mode(&self, embedding: &[f32], limit: usize, mode: Mode) -> Result<Vec<Hit>> {
        validate_embedding(embedding).map_err(Error::Invalid)?;
        // Searches run on a reader connection; only the one-off index build takes the writer.
        let Some((dim, k)) = self.with_read_conn(|c| {
            let Some(dim) = embedding_dim(c)? else {
                return Ok(None);
            };
            if dim != embedding.len() {
                return Err(Error::Invalid(format!(
                    "query embedding dimension {} does not match this store's dimension {dim}",
                    embedding.len()
                )));
            }
            Ok(Some((dim, limit.min(MAX_KNN))))
        })?
        else {
            return Ok(Vec::new());
        };
        if k == 0 {
            return Ok(Vec::new());
        }
        let wanted = k.saturating_mul(ANN_OVERSAMPLE) <= MAX_KNN
            && match mode {
                Mode::Exact => false,
                Mode::Approx => true,
                Mode::Auto => {
                    quantizable(embedding) && self.with_read_conn(vector_count)? >= ANN_MIN_VECTORS
                }
            };
        let approx = wanted
            && (self.with_read_conn(ann_ready)?
                || match self.with_conn(|c| ensure_ann_ready(c, dim)) {
                    Ok(()) => true,
                    Err(e) if matches!(mode, Mode::Approx) => return Err(e),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "cannot build the approximate vector index; searching exhaustively"
                        );
                        false
                    }
                });
        self.with_read_conn(|c| {
            // vec0 kNN queries must stand alone (no joins or extra ORDER BY), so fetch the
            // neighbours first, then their chunk and item rows.
            let neighbours = if approx {
                approx_neighbours(c, embedding, k)?
            } else {
                exact_neighbours(c, embedding, k)?
            };
            neighbour_hits(c, neighbours)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NewChunk, NewItem};

    fn count(s: &Store, table: &str) -> i64 {
        s.with_conn(|c| {
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .db("count")
        })
        .unwrap()
    }

    fn unit(seed: u32, dim: usize) -> Vec<f32> {
        let v: Vec<f32> = (0..dim)
            .map(|i| ((seed as f32 + 1.0) * (i as f32 + 0.5)).sin())
            .collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }

    fn put(s: &Store, key: &str, seeds: &[u32], dim: usize) {
        let chunks: Vec<NewChunk> = seeds
            .iter()
            .map(|&i| NewChunk::embedded(format!("chunk {i}"), unit(i, dim)))
            .collect();
        let item = NewItem::new("t", key, format!("{key}-{seeds:?}"));
        s.upsert_item_with_chunks(&item, &chunks).unwrap();
    }

    fn index_sizes(s: &Store) -> (i64, i64, i64) {
        (
            count(s, "chunk_vec_rowids"),
            count(s, "ann_bits_rowids"),
            count(s, "ann_vectors"),
        )
    }

    #[test]
    fn sparse_queries_are_not_quantized() {
        assert!(quantizable(&[0.1, -0.2, 0.3, 0.0]));
        assert!(!quantizable(&[0.0, 0.0, 0.0, 0.5]));
    }

    #[test]
    fn sign_bits_pack_and_pad() {
        assert_eq!(sign_bits(&[1.0, -1.0, 0.0, 2.0], &[]), vec![0b1001]);
        assert_eq!(sign_bits(&[1.0; 9], &[]), vec![0xff, 0x01]);
        assert_eq!(
            sign_bits(&[1.0, -1.0, 0.0, 2.0], &[1.5, -2.0, -0.5, 1.0]),
            vec![0b1110]
        );
        assert_eq!(bits_dim(384), 384);
        assert_eq!(bits_dim(10), 16);
    }

    #[test]
    fn index_is_built_on_demand_then_follows_writes_and_deletes() {
        // 12 dimensions: not a multiple of 8, so the padded bit width is exercised.
        let s = Store::open_in_memory().unwrap();
        put(&s, "a", &[1, 2, 3], 12);
        put(&s, "b", &[4, 5], 12);
        s.replace_chunks(
            &item_ref("t".into(), "b".into()),
            &[NewChunk::text("text only")],
        )
        .unwrap();
        put(&s, "c", &[7, 8], 12);
        // Exact search does not build the index.
        s.search_vector(&unit(1, 12), 3).unwrap();
        assert_eq!(count(&s, "sqlite_master WHERE name = 'ann_vectors'"), 0);

        let hits = s.search_vector_approx(&unit(8, 12), 1).unwrap();
        assert_eq!(hits[0].snippet, "chunk 8");
        assert_eq!(index_sizes(&s), (5, 5, 5));

        put(&s, "a", &[6], 12); // content change: chunks replaced
        put(&s, "d", &[9, 10], 12);
        assert_eq!(index_sizes(&s), (5, 5, 5));
        s.delete_item(&item_ref("t".into(), "c".into())).unwrap();
        assert_eq!(index_sizes(&s), (3, 3, 3));
        let hits = s.search_vector_approx(&unit(6, 12), 3).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].snippet, "chunk 6");
        let exact = s.search_vector_exact(&unit(6, 12), 3).unwrap();
        assert_eq!(hits[0].score, exact[0].score);
    }

    #[test]
    fn index_build_spans_vec0_chunks_and_resumes() {
        let s = Store::open_in_memory().unwrap();
        // More than one vec0 chunk (1024 vectors each), with holes from deletions.
        for i in 0..1_500u32 {
            put(&s, &format!("k{i}"), &[i], 16);
        }
        for i in (0..1_500u32).step_by(7) {
            s.delete_item(&item_ref("t".into(), format!("k{i}")))
                .unwrap();
        }
        let live = count(&s, "chunk_vec_rowids");
        assert_eq!(live, 1_500 - 215);
        let q = unit(1_000, 16);
        let exact = s.search_vector_exact(&q, 1).unwrap();
        let approx = s.search_vector_approx(&q, 1).unwrap();
        assert_eq!(approx[0].chunk_id, exact[0].chunk_id);
        assert_eq!(index_sizes(&s), (live, live, live));
        // Every copied vector matches its original.
        s.with_conn(|c| {
            let differing: i64 = c
                .query_row(
                    "SELECT count(*) FROM ann_vectors a
                     WHERE a.embedding != (SELECT embedding FROM chunk_vec v WHERE v.rowid = a.id)",
                    [],
                    |r| r.get(0),
                )
                .db("compare")?;
            assert_eq!(differing, 0);
            Ok(())
        })
        .unwrap();

        // An interrupted build (part of the index, flag unset) completes without duplicates.
        s.with_conn(|c| {
            c.execute_batch(
                "DELETE FROM meta WHERE key = 'ann_index_ready';
                 DELETE FROM ann_bits WHERE rowid IN (SELECT id FROM ann_vectors WHERE id % 3 = 0);
                 DELETE FROM ann_vectors WHERE id % 3 = 0;",
            )
            .db("interrupt")
        })
        .unwrap();
        assert!(index_sizes(&s).2 < live);
        s.search_vector_approx(&q, 1).unwrap();
        assert_eq!(index_sizes(&s), (live, live, live));
    }
}
