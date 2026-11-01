//! Vector search: exact cosine kNN over `chunk_vec`, and for large vector sets an approximate
//! path (sign-bit hamming scan in `ann_bits`, exact rescoring from `ann_vectors`).

use std::sync::{Arc, PoisonError, Weak};

use nugget_core::{Error, Result};
use rusqlite::{CachedStatement, Connection, OptionalExtension, TransactionBehavior, params};

use crate::Store;
use crate::error::{ConnExt, DbExt};
use crate::items::{embedding_dim, f32_blob, f32_from_blob, item_ref, validate_embedding};
use crate::search::{MAX_KNN, excerpt};
use crate::store::Db;
use crate::types::{ChunkId, Hit};

/// Least vector count for [`Store::search_vector`] to switch from exact to approximate search.
pub const ANN_MIN_VECTORS: usize = 20_000;
/// Stored vector bytes (`count × dim × 4`) from which [`Store::search_vector`] goes approximate:
/// the size of a connection's page cache (`PRAGMA cache_size`, 32 MiB).
pub const ANN_MIN_VECTOR_BYTES: usize = 32 << 20;
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

/// Bit-vector width for embeddings of `dim` dimensions: `vec0` bit vectors need a multiple of 8, so
/// the tail is padded with zero bits (equal in every vector, so hamming distances are unchanged).
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

/// Create the approximate-search tables and their delete trigger if missing.
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
        if !ann_tables_exist(conn)? {
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

/// Mean of the vectors at `positions` (see [`vector_positions`]), one sequential read per `vec0`
/// chunk. Zeros when there are none.
fn mean_vector(
    conn: &Connection,
    positions: &[(i64, i64, ChunkId)],
    dim: usize,
) -> Result<Vec<f32>> {
    let stride = dim * 4;
    let mut sum = vec![0f64; dim];
    let mut n = 0usize;
    for group in positions.chunk_by(|a, b| a.0 == b.0) {
        let Some(blob) = vector_chunk_blob(conn, group[0].0)? else {
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

pub(crate) fn ann_ready(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM meta WHERE key = ?1)",
        [META_ANN_READY],
        |r| r.get(0),
    )
    .db("read approximate index flag")
}

/// Whether the approximate-search tables exist (complete or not).
fn ann_tables_exist(conn: &Connection) -> Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ann_vectors')",
    )
    .db("prepare index lookup")?
    .query_row([], |r| r.get(0))
    .db("look up approximate vector index")
}

/// Vector positions in `vec0`'s storage: (vec0 chunk, offset in it, rowid = chunk id), sorted in
/// storage order.
fn vector_positions(conn: &Connection) -> Result<Vec<(i64, i64, ChunkId)>> {
    let mut stmt = conn
        .prepare(
            "SELECT chunk_id, chunk_offset, rowid FROM chunk_vec_rowids
             WHERE chunk_id IS NOT NULL",
        )
        .db("prepare vector position scan")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .db("vector position scan")?;
    let mut positions: Vec<_> = rows
        .collect::<rusqlite::Result<_>>()
        .db("vector position scan")?;
    positions.sort_unstable();
    Ok(positions)
}

/// The vector blob of `vec0` chunk `chunk` (one sequential read instead of one overflow-chain
/// walk per vector). vec0 opens these blobs by the table's real rowid (its `rowid` column is
/// not an alias of it), hence `_rowid_`.
fn vector_chunk_blob(conn: &Connection, chunk: i64) -> Result<Option<Vec<u8>>> {
    conn.prepare_cached("SELECT vectors FROM chunk_vec_vector_chunks00 WHERE _rowid_ = ?1")
        .db("prepare vector chunk read")?
        .query_row([chunk], |r| r.get(0))
        .optional()
        .db("read vector chunk")
}

/// Most vectors copied into the index per writer transaction while building it.
const ANN_BUILD_BATCH: usize = 128;
/// A build re-reads the vector positions at most this many times to cover vectors it missed.
const ANN_BUILD_PASSES: usize = 3;

#[cfg(test)]
thread_local! {
    /// Test override of ([`ANN_MIN_VECTORS`], [`ANN_MIN_VECTOR_BYTES`]) for searches on this
    /// thread.
    static TEST_THRESHOLDS: std::cell::Cell<Option<(usize, usize)>> =
        const { std::cell::Cell::new(None) };
    /// Test hook run by a build on this thread between reading a batch and writing it, to
    /// interleave other writes.
    static TEST_BETWEEN_BATCHES: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// (least vector count, least vector bytes) for the approximate path.
fn thresholds() -> (usize, usize) {
    #[cfg(test)]
    if let Some(t) = TEST_THRESHOLDS.with(std::cell::Cell::get) {
        return t;
    }
    (ANN_MIN_VECTORS, ANN_MIN_VECTOR_BYTES)
}

/// Whether `n` vectors of `dim` dimensions are past both thresholds (approximate search pays).
fn past_threshold(n: usize, dim: usize, (min_n, min_bytes): (usize, usize)) -> bool {
    n >= min_n && n.saturating_mul(dim * 4) >= min_bytes
}

/// Whether a store has shrunk far enough below the thresholds (under half of either) for its index
/// to be dropped.
fn far_below_threshold(n: usize, dim: usize, (min_n, min_bytes): (usize, usize)) -> bool {
    n < min_n / 2 || n.saturating_mul(dim * 4) < min_bytes / 2
}

/// Build or complete the approximate-search index from `chunk_vec`, **without holding the writer
/// for the whole build**.
fn build_ann(db: &Weak<Db>, dim: usize) -> Result<bool> {
    let live = || {
        db.upgrade()
            .ok_or_else(|| Error::Storage("store closed during index build".into()))
    };
    let Ok(d) = live() else { return Ok(false) };
    if d.with_read_conn(ann_ready)? {
        return Ok(true);
    }
    let prepared = d.with_read_conn(|c| Ok(ann_center(c)?.len() == dim && ann_tables_exist(c)?))?;
    if !prepared {
        // The centre is fixed once, with the tables, so every bit vector uses the same one.
        let center = d.with_read_conn(|c| mean_vector(c, &vector_positions(c)?, dim))?;
        d.with_conn(|c| {
            let tx = c
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
            tx.commit().db("commit approximate index tables")
        })?;
    }
    drop(d);
    let stride = dim * 4;
    let mut added = 0usize;
    for _ in 0..ANN_BUILD_PASSES {
        // Read after the tables were committed: anything written later is indexed by its writer.
        let positions = match live() {
            Ok(d) => d.with_read_conn(vector_positions)?,
            Err(_) => return Ok(false),
        };
        for group in positions.chunk_by(|a, b| a.0 == b.0) {
            let Ok(d) = live() else { return Ok(false) };
            let Some(blob) = d.with_read_conn(|c| vector_chunk_blob(c, group[0].0))? else {
                continue;
            };
            for batch in group.chunks(ANN_BUILD_BATCH) {
                #[cfg(test)]
                TEST_BETWEEN_BATCHES.with(|h| {
                    if let Some(h) = h.borrow_mut().as_mut() {
                        h();
                    }
                });
                let n = d.with_conn(|c| copy_batch(c, batch, &blob, stride))?;
                let Some(n) = n else {
                    // Dropped meanwhile (the store shrank): nothing left to build.
                    return Ok(false);
                };
                added += n;
            }
        }
        let Ok(d) = live() else { return Ok(false) };
        let complete = d.with_conn(|c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin approximate index check")?;
            if !ann_tables_exist(&tx)? {
                return Ok(None);
            }
            // `ann_vectors` is written and deleted together with `ann_bits` (same statements,
            // same transaction), so counting the small rowid tables suffices; counting
            // `ann_vectors` itself would read every copied vector (~0.8 s at 100k).
            let (vectors, bits): (i64, i64) = tx
                .query_row(
                    "SELECT (SELECT count(*) FROM chunk_vec_rowids),
                            (SELECT count(*) FROM ann_bits_rowids)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .db("count indexed vectors")?;
            if vectors != bits {
                return Ok(Some(false));
            }
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')",
                [META_ANN_READY],
            )
            .db("mark approximate index ready")?;
            tx.commit().db("commit approximate index flag")?;
            Ok(Some(true))
        })?;
        match complete {
            None => return Ok(false),
            Some(true) => {
                tracing::info!(vectors = added, "built the approximate vector index");
                return Ok(true);
            }
            Some(false) => {}
        }
    }
    Err(Error::Storage(
        "approximate vector index still incomplete after rebuilding passes".into(),
    ))
}

/// Copy one batch of pre-existing vectors (positions from [`vector_positions`], all in the `vec0`
/// chunk whose bytes are `blob`) into the index, in one writer transaction.
fn copy_batch(
    conn: &mut Connection,
    batch: &[(i64, i64, ChunkId)],
    blob: &[u8],
    stride: usize,
) -> Result<Option<usize>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .db("begin approximate index batch")?;
    let mut added = 0;
    {
        let Some(mut writer) = AnnWriter::prepare(&tx)? else {
            return Ok(None);
        };
        let mut position = tx
            .prepare_cached("SELECT chunk_id, chunk_offset FROM chunk_vec_rowids WHERE rowid = ?1")
            .db("prepare vector position check")?;
        let mut indexed = tx
            .prepare_cached("SELECT 1 FROM ann_vectors WHERE id = ?1")
            .db("prepare index check")?;
        for &(chunk, offset, id) in batch {
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
                .ok_or_else(|| Error::Storage(format!("malformed stored vector for chunk {id}")))?;
            writer.insert(id, &v)?;
            added += 1;
        }
    }
    tx.commit().db("commit approximate index batch")?;
    Ok(Some(added))
}

/// Drop the approximate-search index (tables, trigger, `meta` keys), so writes stop paying for it.
fn drop_ann(db: &Db) -> Result<bool> {
    let Ok(_build) = db.ann_build.try_lock() else {
        return Ok(false);
    };
    db.with_conn(|c| {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .db("begin approximate index drop")?;
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS chunks_ann_ad;
             DROP TABLE IF EXISTS ann_bits;
             DROP TABLE IF EXISTS ann_vectors;",
        )
        .db("drop approximate vector index")?;
        tx.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            [META_ANN_READY, META_ANN_CENTER],
        )
        .db("forget approximate vector index")?;
        tx.commit().db("commit approximate index drop")
    })?;
    tracing::info!("dropped the approximate vector index: the store shrank below its threshold");
    Ok(true)
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
    /// Exact below [`ANN_MIN_VECTORS`] and [`ANN_MIN_VECTOR_BYTES`], approximate from there on.
    Auto,
    Exact,
    Approx,
}

/// Exact `k` nearest neighbours by cosine distance (`vec0` brute force).
fn exact_neighbours(c: &Connection, embedding: &[f32], k: usize) -> Result<Vec<(ChunkId, f64)>> {
    c.rows(
        "SELECT rowid, distance FROM chunk_vec
             WHERE embedding MATCH ?1 AND k = ?2
             ORDER BY distance",
        params![f32_blob(embedding), k as i64],
        |r| Ok((r.get(0)?, r.get(1)?)),
        "vector search",
    )
}

/// Approximate `k` nearest neighbours: hamming kNN on the sign bits for `k × ANN_OVERSAMPLE`
/// candidates, rescored by exact cosine distance. The index must be ready.
fn approx_neighbours(c: &Connection, embedding: &[f32], k: usize) -> Result<Vec<(ChunkId, f64)>> {
    let candidates = k
        .saturating_mul(ANN_OVERSAMPLE)
        .clamp(ANN_MIN_CANDIDATES, MAX_KNN);
    let center = ann_center(c)?;
    let ids: Vec<ChunkId> = {
        c.rows(
            "SELECT rowid FROM ann_bits
                 WHERE bits MATCH vec_bit(?1) AND k = ?2
                 ORDER BY distance",
            params![sign_bits(embedding, &center), candidates as i64],
            |r| r.get(0),
            "binary vector search",
        )?
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
    pub fn search_vector(&self, embedding: &[f32], limit: usize) -> Result<Vec<Hit>> {
        self.search_vector_mode(embedding, limit, Mode::Auto)
    }

    /// [`Store::search_vector`] always using the exact scan, whatever the store's size (for
    /// recall measurements and benchmarks).
    pub fn search_vector_exact(&self, embedding: &[f32], limit: usize) -> Result<Vec<Hit>> {
        self.search_vector_mode(embedding, limit, Mode::Exact)
    }

    /// [`Store::search_vector`] always using the approximate path (building its index on this
    /// thread if needed, and never dropping it), whatever the store's size; still exact when `limit × ANN_OVERSAMPLE` exceeds
    /// 4096. For tests and benchmarks.
    #[doc(hidden)]
    pub fn search_vector_approx(&self, embedding: &[f32], limit: usize) -> Result<Vec<Hit>> {
        self.search_vector_mode(embedding, limit, Mode::Approx)
    }

    fn search_vector_mode(&self, embedding: &[f32], limit: usize, mode: Mode) -> Result<Vec<Hit>> {
        validate_embedding(embedding).map_err(Error::Invalid)?;
        // Searches run on a reader connection; the index build and drop take the writer only for
        // short transactions.
        let Some((dim, k, indexed, ready)) = self.with_read_conn(|c| {
            let Some(dim) = embedding_dim(c)? else {
                return Ok(None);
            };
            if dim != embedding.len() {
                return Err(Error::Invalid(format!(
                    "query embedding dimension {} does not match this store's dimension {dim}",
                    embedding.len()
                )));
            }
            let indexed = ann_tables_exist(c)?;
            let ready = indexed && ann_ready(c)?;
            Ok(Some((dim, limit.min(MAX_KNN), indexed, ready)))
        })?
        else {
            return Ok(Vec::new());
        };
        if k == 0 {
            return Ok(Vec::new());
        }
        let usable = k.saturating_mul(ANN_OVERSAMPLE) <= MAX_KNN;
        let approx = match mode {
            Mode::Exact => false,
            Mode::Approx => usable && (ready || self.build_ann_now(dim)?),
            Mode::Auto => {
                let dense = quantizable(embedding);
                // The count is only needed (and paid for) by dense queries and by stores that
                // have an index to keep or drop.
                if dense || indexed {
                    let n = self.with_read_conn(vector_count)?;
                    let limits = thresholds();
                    if indexed && far_below_threshold(n, dim, limits) {
                        if let Err(e) = drop_ann(self.db()) {
                            tracing::warn!(error = %e, "cannot drop the approximate vector index");
                        }
                        false
                    } else if usable && dense && past_threshold(n, dim, limits) {
                        // Until the index is complete, search exactly while it builds.
                        ready || {
                            self.spawn_ann_build(dim);
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        };
        self.with_read_conn(|c| {
            // One read transaction, so the index cannot be dropped between the readiness check
            // and its use.
            let tx = c.unchecked_transaction().db("begin vector search")?;
            let approx = approx && ann_ready(&tx)?;
            // vec0 kNN queries must stand alone (no joins or extra ORDER BY), so fetch the
            // neighbours first, then their chunk and item rows.
            let neighbours = if approx {
                approx_neighbours(&tx, embedding, k)?
            } else {
                exact_neighbours(&tx, embedding, k)?
            };
            let hits = neighbour_hits(&tx, neighbours)?;
            tx.commit().db("end vector search")?;
            Ok(hits)
        })
    }

    /// Build the approximate index on this thread (waiting for a build in progress), in short
    /// writer transactions. Returns whether it is complete.
    fn build_ann_now(&self, dim: usize) -> Result<bool> {
        let lock = self.db().ann_build.clone();
        let _build = lock.lock().unwrap_or_else(PoisonError::into_inner);
        build_ann(&Arc::downgrade(self.db()), dim)
    }

    /// Start building the approximate index on a background thread, unless a build is already
    /// running.
    fn spawn_ann_build(&self, dim: usize) {
        let lock = self.db().ann_build.clone();
        if lock.try_lock().is_err() {
            return;
        }
        let db = Arc::downgrade(self.db());
        let spawned = std::thread::Builder::new()
            .name("nugget-ann-build".into())
            .spawn(move || {
                // Several searches may race to start a build; only one runs it.
                let Ok(_build) = lock.try_lock() else {
                    return;
                };
                if let Err(e) = build_ann(&db, dim) {
                    tracing::warn!(
                        error = %e,
                        "cannot build the approximate vector index; searching exhaustively"
                    );
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "cannot start the approximate vector index build");
        }
    }

    /// Whether the approximate-search index is complete (searches past the thresholds use it).
    #[doc(hidden)]
    pub fn ann_index_ready(&self) -> Result<bool> {
        self.with_read_conn(|c| Ok(ann_tables_exist(c)? && ann_ready(c)?))
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
        let s = Store::open_in_memory().unwrap();
        put(&s, "a", &[1, 2, 3], 12);
        put(&s, "b", &[4, 5], 12);
        s.replace_chunks(
            &item_ref("t".into(), "b".into()),
            &[NewChunk::text("text only")],
        )
        .unwrap();
        put(&s, "c", &[7, 8], 12);
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

    fn ann_exists(s: &Store) -> bool {
        count(
            s,
            "sqlite_master WHERE name IN ('ann_bits', 'ann_vectors', 'chunks_ann_ad')",
        ) > 0
    }

    fn ann_meta(s: &Store) -> i64 {
        count(s, "meta WHERE key IN ('ann_index_ready', 'ann_center')")
    }

    /// Wait (bounded) for the background build to complete the index.
    fn wait_ready(s: &Store) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !s.ann_index_ready().unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "background index build did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Every vector is indexed once, with its exact copy.
    fn assert_index_consistent(s: &Store) {
        let (v, b, c) = index_sizes(s);
        assert_eq!((b, c), (v, v));
        let differing = count(
            s,
            "chunk_vec_rowids r WHERE NOT EXISTS (
                 SELECT 1 FROM ann_vectors a WHERE a.id = r.rowid
                 AND a.embedding = (SELECT embedding FROM chunk_vec v WHERE v.rowid = r.rowid))",
        );
        assert_eq!(differing, 0);
    }

    #[test]
    fn index_builds_in_the_background_and_drops_when_the_store_shrinks() {
        TEST_THRESHOLDS.with(|t| t.set(Some((40, 0))));
        let s = Store::open_in_memory().unwrap();
        for i in 0..30u32 {
            put(&s, &format!("k{i}"), &[i], 16);
        }
        s.search_vector(&unit(3, 16), 3).unwrap();
        assert!(!ann_exists(&s));

        for i in 30..50u32 {
            put(&s, &format!("k{i}"), &[i], 16);
        }
        let first = s.search_vector(&unit(3, 16), 3).unwrap();
        assert_eq!(first[0].snippet, "chunk 3");
        wait_ready(&s);
        assert_index_consistent(&s);
        let hits = s.search_vector(&unit(7, 16), 3).unwrap();
        assert_eq!(hits[0].snippet, "chunk 7");

        for i in 0..25u32 {
            s.delete_item(&item_ref("t".into(), format!("k{i}")))
                .unwrap();
        }
        s.search_vector(&unit(30, 16), 3).unwrap();
        assert!(s.ann_index_ready().unwrap());
        assert_index_consistent(&s);

        for i in 25..32u32 {
            s.delete_item(&item_ref("t".into(), format!("k{i}")))
                .unwrap();
        }
        let mut sparse = vec![0.0; 16];
        sparse[0] = 1.0;
        s.search_vector(&sparse, 3).unwrap();
        assert!(!ann_exists(&s));
        assert_eq!(ann_meta(&s), 0);
        for i in 50..60u32 {
            put(&s, &format!("k{i}"), &[i], 16);
        }
        let hits = s.search_vector(&unit(55, 16), 3).unwrap();
        assert_eq!(hits[0].snippet, "chunk 55");
        assert!(!ann_exists(&s));

        for i in 60..75u32 {
            put(&s, &format!("k{i}"), &[i], 16);
        }
        s.search_vector(&unit(65, 16), 3).unwrap();
        wait_ready(&s);
        assert_index_consistent(&s);
        assert_eq!(ann_meta(&s), 2);
    }

    #[test]
    fn index_build_releases_the_writer_and_covers_concurrent_writes() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..1_500u32 {
            put(&s, &format!("k{i}"), &[i], 16);
        }
        let writer = s.clone();
        let batches = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let counter = batches.clone();
        TEST_BETWEEN_BATCHES.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                let del = |key: String| {
                    writer.delete_item(&item_ref("t".into(), key)).unwrap();
                };
                let batch = counter.get();
                counter.set(batch + 1);
                match batch {
                    0 => {
                        del("k1499".into());
                        put(&writer, "new0", &[5_000], 16);
                    }
                    1 => {
                        del("k1300".into());
                        put(&writer, "k1301", &[5_001, 5_002], 16);
                        put(&writer, "new1", &[5_003], 16);
                    }
                    2 => del("k3".into()),
                    _ => {}
                }
            }));
        });
        let q = unit(5_002, 16);
        let approx = s.search_vector_approx(&q, 1).unwrap();
        TEST_BETWEEN_BATCHES.with(|h| h.borrow_mut().take());
        let batches = batches.get();
        assert!(batches >= 6, "expected several batches, ran {batches}");
        assert_eq!(approx[0].snippet, "chunk 5002");
        assert!(s.ann_index_ready().unwrap());
        assert_index_consistent(&s);
        assert_eq!(index_sizes(&s).0, 1_500);
    }
}
