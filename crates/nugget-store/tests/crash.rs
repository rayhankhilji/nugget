//! Crash safety of the store under hard kills.
//!
//! The parent test ([`store_survives_hard_kills`]) re-executes this test binary as a worker
//! ([`crash_worker`], selected by the [`WORKER_ENV`] variable) that runs a tight loop of writes
//! through the public store API on one encrypted database: items with chunks and vectors
//! (`upsert_item_with_chunks`), re-chunking (`replace_chunks`), re-embedding (an upsert of the
//! same content with vectors, as the ingest re-embed job writes it), deletes, knowledge-graph
//! writes, audit entries, egress records and two-phase egress rows left open. The parent kills it
//! at a random point (`Child::kill`: SIGKILL on Unix, `TerminateProcess` on Windows; no cleanup
//! runs either way), reopens the database and checks:
//!
//! - [`Store::check_consistency`]: `integrity_check`, foreign keys, the audit chain, the
//!   full-text and vector indexes against their tables, duplicates, dangling rows, the change
//!   feed and the per-source counts;
//! - **no loss of committed writes**: the worker appends `B <seq> <op>` to a journal before each
//!   store call and `C <seq>` (fsynced) after it returns, so every op journaled as completed must
//!   be visible with exactly its content (item hash, chunk texts and count, vectors, full-text
//!   hits); the one op in flight may be either applied or not, never half (the parent records
//!   which with `A <seq>` or `X <seq>` before the next worker starts);
//! - two-phase egress rows the dead worker left open are marked `interrupted` on reopen.
//!
//! What a process kill can and cannot lose: the store runs SQLite in WAL mode with
//! `synchronous = NORMAL`. A transaction is durable against a *process* crash as soon as its
//! commit returns (the WAL frames are in the OS page cache, which survives the process), which is
//! what this harness asserts. On *power loss* the last transactions before the most recent WAL
//! sync may be lost (never corrupted); that is not simulated here.
//!
//! The approximate vector index (built from [`nugget_store::ANN_MIN_VECTORS`] vectors) is not
//! reached by this harness's database sizes.
//!
//! Environment: `NUGGET_CRASH_ITERATIONS` (default 50), `NUGGET_CRASH_SEED` (default: the clock;
//! printed, so a failure can be replayed; the kill timing still varies with the machine).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, Evidence, GraphStore, NewClaim, NewEntity,
};
use nugget_store::{AuditEntry, EgressState, NewChunk, NewItem, Store, StoreKey, content_hash};
use serde_json::json;

/// Set (to the work directory) in the worker's environment.
const WORKER_ENV: &str = "NUGGET_CRASH_STORE_WORKER";
/// The worker's RNG seed.
const WORKER_SEED_ENV: &str = "NUGGET_CRASH_STORE_WORKER_SEED";
const KEY: [u8; 32] = [7; 32];
const SOURCE: &str = "crash";
const DIM: usize = 8;
/// A worker exits by itself after this long, so a dead parent never leaves it running.
const WORKER_LIFETIME: Duration = Duration::from_secs(60);
/// Old keys re-checked per iteration (besides every key the iteration touched).
const SAMPLE: usize = 40;

// ---- tiny deterministic RNG ----------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    /// xorshift64*.
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn seed() -> u64 {
    env_u64("NUGGET_CRASH_SEED").unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
    })
}

// ---- ops and the journal ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Op {
    /// `upsert_item_with_chunks`: content version `ver`, `chunks` chunks, vectors if `emb`.
    Upsert {
        key: u64,
        ver: u64,
        chunks: u32,
        emb: bool,
    },
    /// `replace_chunks` with new texts (version `ver`) and vectors.
    Rechunk {
        key: u64,
        ver: u64,
        chunks: u32,
    },
    /// The re-embed write: same content, metadata naming the embedder, the same chunk texts
    /// with vectors tagged `tag`.
    Reembed {
        key: u64,
        tag: u64,
    },
    Delete {
        key: u64,
    },
    /// An entity `E<n>` with an alias and a claim citing item `key`.
    Graph {
        n: u64,
        key: u64,
    },
    Audit {
        n: u64,
    },
    Egress {
        n: u64,
    },
    /// A two-phase egress row that is never finished.
    EgressOpen {
        n: u64,
    },
}

impl Op {
    fn encode(&self) -> String {
        match self {
            Op::Upsert {
                key,
                ver,
                chunks,
                emb,
            } => format!("up {key} {ver} {chunks} {}", u8::from(*emb)),
            Op::Rechunk { key, ver, chunks } => format!("re {key} {ver} {chunks}"),
            Op::Reembed { key, tag } => format!("em {key} {tag}"),
            Op::Delete { key } => format!("del {key}"),
            Op::Graph { n, key } => format!("gr {n} {key}"),
            Op::Audit { n } => format!("au {n}"),
            Op::Egress { n } => format!("eg {n}"),
            Op::EgressOpen { n } => format!("eo {n}"),
        }
    }

    fn decode(s: &str) -> Option<Op> {
        let p: Vec<&str> = s.split_whitespace().collect();
        let n = |i: usize| p.get(i).and_then(|v| v.parse::<u64>().ok());
        Some(match *p.first()? {
            "up" => Op::Upsert {
                key: n(1)?,
                ver: n(2)?,
                chunks: n(3)? as u32,
                emb: n(4)? == 1,
            },
            "re" => Op::Rechunk {
                key: n(1)?,
                ver: n(2)?,
                chunks: n(3)? as u32,
            },
            "em" => Op::Reembed {
                key: n(1)?,
                tag: n(2)?,
            },
            "del" => Op::Delete { key: n(1)? },
            "gr" => Op::Graph {
                n: n(1)?,
                key: n(2)?,
            },
            "au" => Op::Audit { n: n(1)? },
            "eg" => Op::Egress { n: n(1)? },
            "eo" => Op::EgressOpen { n: n(1)? },
            _ => return None,
        })
    }
}

/// What a live item should look like.
#[derive(Debug, Clone, PartialEq)]
struct ItemState {
    /// Content version (the item's hash and title token).
    ver: u64,
    /// Version of the chunk texts.
    chunk_ver: u64,
    chunks: u32,
    /// Tag of the vectors (their first component), if the chunks have vectors.
    emb: Option<u64>,
}

/// The state the journal says the store must be in.
#[derive(Debug, Default, Clone)]
struct Model {
    items: BTreeMap<u64, ItemState>,
    /// Entities `E<n>` written completely, with the item their claim cites.
    graph: BTreeMap<u64, u64>,
    audits: u64,
    egress: u64,
    egress_open: u64,
    next_seq: u64,
    /// The op that began but was never resolved (the worker was killed during it).
    pending: Option<(u64, Op)>,
}

impl Model {
    fn apply(&mut self, op: &Op) {
        match op {
            Op::Upsert {
                key,
                ver,
                chunks,
                emb,
            } => {
                self.items.insert(
                    *key,
                    ItemState {
                        ver: *ver,
                        chunk_ver: *ver,
                        chunks: *chunks,
                        emb: emb.then_some(*ver),
                    },
                );
            }
            Op::Rechunk { key, ver, chunks } => {
                if let Some(s) = self.items.get_mut(key) {
                    s.chunk_ver = *ver;
                    s.chunks = *chunks;
                    s.emb = Some(*ver);
                }
            }
            Op::Reembed { key, tag } => {
                if let Some(s) = self.items.get_mut(key) {
                    s.emb = Some(*tag);
                }
            }
            Op::Delete { key } => {
                self.items.remove(key);
                // Deleting an item retracts the claims it alone supported.
                self.graph.retain(|_, k| k != key);
            }
            Op::Graph { n, key } => {
                self.graph.insert(*n, *key);
            }
            Op::Audit { .. } => self.audits += 1,
            Op::Egress { .. } => self.egress += 1,
            Op::EgressOpen { .. } => self.egress_open += 1,
        }
    }

    /// Replay a journal.
    fn replay(text: &str) -> Model {
        let mut m = Model::default();
        for line in text.lines() {
            let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
            let (seq, op) = rest.split_once(' ').unwrap_or((rest, ""));
            let Ok(seq) = seq.trim().parse::<u64>() else {
                continue; // a torn last line
            };
            match tag {
                "B" => {
                    let Some(op) = Op::decode(op) else { continue };
                    assert!(m.pending.is_none(), "journal: two ops in flight at {seq}");
                    m.pending = Some((seq, op));
                    m.next_seq = m.next_seq.max(seq + 1);
                }
                "C" | "A" => {
                    let (s, op) = m.pending.take().expect("journal: completion without begin");
                    assert_eq!(s, seq, "journal: completion of another op");
                    m.apply(&op);
                }
                "X" => {
                    let (s, _) = m.pending.take().expect("journal: abort without begin");
                    assert_eq!(s, seq, "journal: abort of another op");
                }
                _ => {}
            }
        }
        m
    }
}

fn item_ref(key: u64) -> ItemRef {
    ItemRef {
        source: SourceId::new(SOURCE),
        key: ItemKey::new(format!("k{key}")),
    }
}

/// Unique full-text tokens: the item's content and its chunks'.
fn item_token(key: u64, ver: u64) -> String {
    format!("itk{key}x{ver}")
}

fn chunk_token(key: u64, ver: u64) -> String {
    format!("chk{key}x{ver}")
}

fn hash_of(key: u64, ver: u64) -> String {
    content_hash(format!("{key}/{ver}"))
}

fn chunk_text(key: u64, ver: u64, i: u32) -> String {
    format!(
        "{} part {i} of a note about crash safety, write ahead logs and durable commits",
        chunk_token(key, ver)
    )
}

fn vector(tag: u64, i: u32) -> Vec<f32> {
    let mut v = vec![0.25f32; DIM];
    v[0] = tag as f32;
    v[1] = i as f32;
    v
}

fn new_item(key: u64, ver: u64) -> NewItem {
    let tok = item_token(key, ver);
    NewItem::new(SOURCE, format!("k{key}"), hash_of(key, ver))
        .with_title(format!("Note {tok}"))
        .with_text(format!("{tok} body"))
        .with_version(format!("v{ver}"))
}

fn chunks(key: u64, ver: u64, n: u32, emb: Option<u64>) -> Vec<NewChunk> {
    (0..n)
        .map(|i| NewChunk {
            text: chunk_text(key, ver, i),
            embedding: emb.map(|t| vector(t, i)),
        })
        .collect()
}

// ---- the worker ------------------------------------------------------------------------------

/// Not a test of its own: the worker body, run only in a child process the parent started with
/// [`WORKER_ENV`] set. It never returns there (it is killed, or exits after
/// [`WORKER_LIFETIME`]).
#[test]
fn crash_worker() {
    let Some(dir) = std::env::var_os(WORKER_ENV).map(PathBuf::from) else {
        return;
    };
    let seed = env_u64(WORKER_SEED_ENV).unwrap_or(1);
    let journal_path = dir.join("journal");
    let mut model = Model::replay(&fs::read_to_string(&journal_path).unwrap_or_default());
    assert!(model.pending.is_none(), "the parent resolves the last op");
    let store = Store::open(dir.join("nugget.db"), &StoreKey::from_bytes(KEY)).unwrap();
    let graph = store.graph();
    let mut journal = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&journal_path)
        .unwrap();
    let mut rng = Rng::new(seed);
    let started = Instant::now();
    while started.elapsed() < WORKER_LIFETIME {
        let seq = model.next_seq;
        let op = pick_op(&mut rng, &model, seq);
        journal
            .write_all(format!("B {seq} {}\n", op.encode()).as_bytes())
            .unwrap();
        run_op(&store, &graph, &op);
        journal.write_all(format!("C {seq}\n").as_bytes()).unwrap();
        // Not needed against a process kill (the page cache survives it); fsynced anyway so the
        // journal is never the weak link.
        journal.sync_data().unwrap();
        model.next_seq = seq + 1;
        model.apply(&op);
    }
    std::process::exit(0);
}

fn pick_op(rng: &mut Rng, m: &Model, seq: u64) -> Op {
    let live: Vec<u64> = m.items.keys().copied().collect();
    let pick = |rng: &mut Rng| live[rng.below(live.len() as u64) as usize];
    let roll = rng.below(100);
    if live.len() < 8 || roll < 30 {
        return Op::Upsert {
            key: seq,
            ver: seq,
            chunks: rng.range(1, 6) as u32,
            emb: rng.below(4) != 0,
        };
    }
    match roll {
        30..=44 => Op::Upsert {
            key: pick(rng),
            ver: seq,
            chunks: rng.range(0, 6) as u32,
            emb: rng.below(4) != 0,
        },
        45..=56 => Op::Rechunk {
            key: pick(rng),
            ver: seq,
            chunks: rng.range(1, 5) as u32,
        },
        57..=66 => Op::Reembed {
            key: pick(rng),
            tag: seq,
        },
        67..=78 => Op::Delete { key: pick(rng) },
        79..=87 => Op::Graph {
            n: seq,
            key: pick(rng),
        },
        88..=93 => Op::Audit { n: seq },
        94..=97 => Op::Egress { n: seq },
        _ => Op::EgressOpen { n: seq },
    }
}

fn egress_event(n: u64, feature: &str) -> EgressEvent {
    EgressEvent {
        at: time::OffsetDateTime::now_utc(),
        host: format!("h{n}.example"),
        feature: feature.into(),
        bytes_sent: n,
        bytes_received: 1,
        items: Vec::new(),
    }
}

fn run_op(store: &Store, graph: &nugget_store::SqliteGraph, op: &Op) {
    match op {
        Op::Upsert {
            key,
            ver,
            chunks: n,
            emb,
        } => {
            store
                .upsert_item_with_chunks(
                    &new_item(*key, *ver),
                    &chunks(*key, *ver, *n, emb.then_some(*ver)),
                )
                .unwrap();
        }
        Op::Rechunk {
            key,
            ver,
            chunks: n,
        } => {
            store
                .replace_chunks(&item_ref(*key), &chunks(*key, *ver, *n, Some(*ver)))
                .unwrap();
        }
        Op::Reembed { key, tag } => {
            let item = store.get_item(&item_ref(*key)).unwrap().unwrap();
            let n = store
                .get_chunks_by_ord(&item_ref(*key), &(0..64).collect::<Vec<u32>>())
                .unwrap();
            let mut new = NewItem::new(SOURCE, format!("k{key}"), item.content_hash.clone())
                .with_metadata(json!({"embedding_provider": format!("p{tag}")}));
            new.title = item.title;
            new.text = item.text;
            new.version = item.version;
            let chunks: Vec<NewChunk> = n
                .iter()
                .enumerate()
                .map(|(i, c)| NewChunk {
                    text: c.text.clone(),
                    embedding: Some(vector(*tag, i as u32)),
                })
                .collect();
            if store.upsert_item_with_chunks(&new, &chunks).unwrap()
                == nugget_store::UpsertOutcome::Unchanged
            {
                store.replace_chunks(&item_ref(*key), &chunks).unwrap();
            }
        }
        Op::Delete { key } => {
            store.delete_item(&item_ref(*key)).unwrap();
        }
        Op::Graph { n, key } => {
            let id = graph
                .create_entity(&NewEntity::new(EntityKind::Person, format!("E{n}")))
                .unwrap();
            graph.add_alias(&id, &format!("Alias{n}")).unwrap();
            graph
                .add_claim(&NewClaim {
                    subject: id,
                    predicate: "works_on".into(),
                    object: ClaimObject::Literal(format!("project {n}")),
                    text: format!("E{n} works on project {n}"),
                    confidence: 0.9,
                    status: ClaimStatus::Active,
                    evidence: vec![Evidence {
                        item: item_ref(*key),
                        chunk_id: None,
                        quote: None,
                    }],
                    valid_from: None,
                    valid_to: None,
                })
                .unwrap();
        }
        Op::Audit { n } => {
            store
                .append_audit(&AuditEntry {
                    actor: "crash".into(),
                    action: "crash.test".into(),
                    target: format!("t{n}"),
                    detail: json!({"n": n}),
                })
                .unwrap();
        }
        Op::Egress { n } => store.record_egress(&egress_event(*n, "crash")).unwrap(),
        Op::EgressOpen { n } => {
            // Waits (briefly) for the row; the exchange is never finished.
            let _ = store.begin(egress_event(*n, "crash_open"));
        }
    }
}

// ---- the parent ------------------------------------------------------------------------------

/// Kills the worker on drop (a failed assertion) so it never outlives the test.
struct Worker(Option<Child>);

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn spawn_worker(dir: &Path, seed: u64) -> Worker {
    let exe = std::env::current_exe().unwrap();
    let child = Command::new(exe)
        .args(["crash_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env(WORKER_ENV, dir)
        .env(WORKER_SEED_ENV, seed.to_string())
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    Worker(Some(child))
}

fn journal_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

/// Hard-kill the worker and wait for the OS to reap it (so its file handles and locks are gone,
/// which matters on Windows). Panics with its stderr if it had died on its own.
fn kill(mut w: Worker) {
    let mut child = w.0.take().unwrap();
    if let Some(status) = child.try_wait().unwrap() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut err);
        }
        panic!("the worker died on its own ({status}):\n{err}");
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Whether item `key` is in state `want` (`None`: absent). `Err` says how it differs.
fn item_matches(store: &Store, key: u64, want: Option<&ItemState>) -> Result<(), String> {
    let r = item_ref(key);
    let got = store.get_item(&r).map_err(|e| e.to_string())?;
    let Some(want) = want else {
        return match got {
            None => Ok(()),
            Some(_) => Err(format!("k{key} exists but was deleted")),
        };
    };
    let Some(item) = got else {
        return Err(format!("k{key} is missing"));
    };
    if item.content_hash != hash_of(key, want.ver) {
        return Err(format!("k{key} has another content than v{}", want.ver));
    }
    let ords: Vec<u32> = (0..want.chunks + 2).collect();
    let got_chunks = store
        .get_chunks_by_ord(&r, &ords)
        .map_err(|e| e.to_string())?;
    let texts: Vec<String> = got_chunks.iter().map(|c| c.text.clone()).collect();
    let expect: Vec<String> = (0..want.chunks)
        .map(|i| chunk_text(key, want.chunk_ver, i))
        .collect();
    if texts != expect {
        return Err(format!(
            "k{key} chunks {texts:?}, expected {} of version {}",
            want.chunks, want.chunk_ver
        ));
    }
    let ids: Vec<i64> = got_chunks.iter().map(|c| c.id).collect();
    let vecs = store.get_embeddings(&ids).map_err(|e| e.to_string())?;
    for (i, id) in ids.iter().enumerate() {
        let v = vecs.get(id);
        match (want.emb, v) {
            (None, None) => {}
            (Some(tag), Some(v)) if v.first() == Some(&(tag as f32)) => {}
            (w, v) => {
                return Err(format!(
                    "k{key} chunk {i}: vector {:?}, expected tag {w:?}",
                    v.and_then(|v| v.first())
                ));
            }
        }
    }
    Ok(())
}

/// The full-text index finds the item by its chunk token (when it has chunks) and its title.
fn fts_matches(store: &Store, key: u64, want: &ItemState) -> Result<(), String> {
    let r = item_ref(key);
    // A hit is a chunk: an item without chunks cannot be found.
    if want.chunks == 0 {
        return Ok(());
    }
    let tokens = [item_token(key, want.ver), chunk_token(key, want.chunk_ver)];
    for tok in tokens {
        let hits = store.search_text(&tok, 5).map_err(|e| e.to_string())?;
        if !hits.iter().any(|h| h.item == r) {
            return Err(format!("search for {tok} misses k{key}: {hits:?}"));
        }
        if let Some(h) = hits.iter().find(|h| h.item != r) {
            return Err(format!("search for {tok} finds another item {:?}", h.item));
        }
    }
    Ok(())
}

fn count_audit(store: &Store) -> u64 {
    store
        .recent_audit(1_000_000)
        .unwrap()
        .iter()
        .filter(|r| r.action == "crash.test")
        .count() as u64
}

fn count_egress(store: &Store, feature: &str) -> u64 {
    store
        .recent_egress(1_000_000)
        .unwrap()
        .iter()
        .filter(|e| e.feature == feature)
        .count() as u64
}

fn graph_has(store: &Store, n: u64) -> bool {
    let g = store.graph();
    let ids = g.find_by_name(&format!("E{n}"), None).unwrap();
    ids.iter().any(|id| {
        g.claims_for(id, false)
            .unwrap()
            .iter()
            .any(|c| c.predicate == "works_on")
    })
}

/// Whether the in-flight op `op` took effect, given the store and the state before it.
fn applied(store: &Store, before: &Model, op: &Op) -> Result<bool, String> {
    let mut after = before.clone();
    after.apply(op);
    let ok = |key: u64, m: &Model| item_matches(store, key, m.items.get(&key)).is_ok();
    match op {
        Op::Upsert { key, .. }
        | Op::Rechunk { key, .. }
        | Op::Reembed { key, .. }
        | Op::Delete { key } => {
            let (pre, post) = (ok(*key, before), ok(*key, &after));
            match (pre, post) {
                (_, true) => Ok(true),
                (true, false) => Ok(false),
                (false, false) => Err(item_matches(store, *key, after.items.get(key))
                    .err()
                    .map(|e| {
                        format!(
                            "in-flight {op:?} left a state that is neither before nor after: {e}"
                        )
                    })
                    .unwrap_or_default()),
            }
        }
        // Entity, alias and claim are separate writes: a partial entity counts as not applied.
        Op::Graph { n, .. } => Ok(graph_has(store, *n)),
        Op::Audit { .. } => {
            let n = count_audit(store);
            if n == before.audits || n == after.audits {
                Ok(n == after.audits)
            } else {
                Err(format!(
                    "{n} audit entries, expected {} or {}",
                    before.audits, after.audits
                ))
            }
        }
        Op::Egress { .. } => {
            let n = count_egress(store, "crash");
            if n == before.egress || n == after.egress {
                Ok(n == after.egress)
            } else {
                Err(format!(
                    "{n} egress rows, expected {} or {}",
                    before.egress, after.egress
                ))
            }
        }
        Op::EgressOpen { .. } => {
            let n = count_egress(store, "crash_open");
            if n == before.egress_open || n == after.egress_open {
                Ok(n == after.egress_open)
            } else {
                Err(format!(
                    "{n} open egress rows, expected {} or {}",
                    before.egress_open, after.egress_open
                ))
            }
        }
    }
}

/// Check the reopened store against the model (after resolving its pending op). `keys` are
/// checked item by item; the counters always.
fn verify(store: &Store, m: &Model, keys: &[u64]) -> Vec<String> {
    let mut problems = Vec::new();
    for &key in keys {
        if let Err(e) = item_matches(store, key, m.items.get(&key)) {
            problems.push(e);
        } else if let Some(s) = m.items.get(&key)
            && let Err(e) = fts_matches(store, key, s)
        {
            problems.push(e);
        }
    }
    for (&n, _) in m.graph.iter().rev().take(SAMPLE) {
        if !graph_has(store, n) {
            problems.push(format!("entity E{n} or its claim is missing"));
        }
    }
    let (audits, egress, open) = (
        count_audit(store),
        count_egress(store, "crash"),
        count_egress(store, "crash_open"),
    );
    if audits != m.audits {
        problems.push(format!("{audits} audit entries, journal says {}", m.audits));
    }
    if egress != m.egress {
        problems.push(format!("{egress} egress rows, journal says {}", m.egress));
    }
    if open != m.egress_open {
        problems.push(format!(
            "{open} two-phase egress rows, journal says {}",
            m.egress_open
        ));
    }
    // The dead worker's open rows are interrupted now; nothing stays `open`.
    for row in store.unfinished_egress(1_000_000).unwrap() {
        if row.state == EgressState::Open {
            problems.push(format!("egress row {} still open after reopen", row.id));
        }
    }
    let counts = store.counts().unwrap();
    if counts.items != m.items.len() as u64 {
        problems.push(format!(
            "{} items, journal says {}",
            counts.items,
            m.items.len()
        ));
    }
    let chunks: u64 = m.items.values().map(|s| u64::from(s.chunks)).sum();
    if counts.chunks != chunks {
        problems.push(format!("{} chunks, journal says {chunks}", counts.chunks));
    }
    problems
}

#[test]
fn store_survives_hard_kills() {
    let iterations = env_u64("NUGGET_CRASH_ITERATIONS").unwrap_or(50);
    let seed = seed();
    println!("store crash harness: NUGGET_CRASH_SEED={seed}, {iterations} iterations");
    let tmp = tempfile::Builder::new()
        .prefix("nugget-crash-store-")
        .tempdir()
        .unwrap();
    let dir = tmp.path();
    let journal_path = dir.join("journal");
    let db = dir.join("nugget.db");
    let key = StoreKey::from_bytes(KEY);
    let mut rng = Rng::new(seed);
    let started = Instant::now();
    let mut ops_total = 0u64;
    let mut checked_from = 0u64;
    for iter in 0..iterations {
        let ctx = format!("iteration {iter}, NUGGET_CRASH_SEED={seed}");
        let before_len = journal_len(&journal_path);
        let worker = spawn_worker(dir, rng.next());
        // Most kills land in the write loop: wait until the worker has journaled something new,
        // then a random delay. Some land while it opens (and recovers) the store.
        if rng.below(5) != 0 {
            let deadline = Instant::now() + Duration::from_secs(30);
            while journal_len(&journal_path) == before_len {
                assert!(Instant::now() < deadline, "{ctx}: the worker never wrote");
                std::thread::sleep(Duration::from_millis(2));
            }
            std::thread::sleep(Duration::from_millis(rng.below(250)));
        } else {
            std::thread::sleep(Duration::from_millis(rng.below(60)));
        }
        kill(worker);

        let text = fs::read_to_string(&journal_path).unwrap_or_default();
        let mut model = Model::replay(&text);
        let store = Store::open(&db, &key).unwrap_or_else(|e| panic!("{ctx}: reopen: {e}"));
        let report = store
            .check_consistency()
            .unwrap_or_else(|e| panic!("{ctx}: consistency check: {e}"));
        assert!(
            report.ok,
            "{ctx}: inconsistent store: {:#?}",
            report.problems
        );

        // Resolve the op in flight, recording the outcome for the next worker.
        if let Some((seq, op)) = model.pending.take() {
            let took = applied(&store, &model, &op).unwrap_or_else(|e| panic!("{ctx}: {e}"));
            let mut j = OpenOptions::new().append(true).open(&journal_path).unwrap();
            // A line torn by the kill (no newline yet) is ended first.
            let sep = if text.ends_with('\n') || text.is_empty() {
                ""
            } else {
                "\n"
            };
            writeln!(j, "{sep}{} {seq}", if took { "A" } else { "X" }).unwrap();
            j.sync_data().unwrap();
            if took {
                model.apply(&op);
            }
        }

        // Every key touched since the last check, plus a sample of the others.
        let recent = Model::replay_keys_since(&text, checked_from);
        let mut keys: Vec<u64> = recent;
        let all: Vec<u64> = model.items.keys().copied().collect();
        for _ in 0..SAMPLE.min(all.len()) {
            keys.push(all[rng.below(all.len() as u64) as usize]);
        }
        keys.sort_unstable();
        keys.dedup();
        let problems = verify(&store, &model, &keys);
        assert!(
            problems.is_empty(),
            "{ctx}: lost or damaged writes: {problems:#?}"
        );
        checked_from = model.next_seq;
        ops_total = model.next_seq;
        drop(store);
    }

    // A last full check of every key ever written.
    let text = fs::read_to_string(&journal_path).unwrap_or_default();
    let model = Model::replay(&text);
    let store = Store::open(&db, &key).unwrap();
    let keys = Model::replay_keys_since(&text, 0);
    let problems = verify(&store, &model, &keys);
    assert!(
        problems.is_empty(),
        "final check (seed {seed}): {problems:#?}"
    );
    assert!(store.check_consistency().unwrap().ok);
    assert!(
        ops_total > iterations,
        "the workers wrote too little to mean anything ({ops_total} ops)"
    );
    println!(
        "store crash harness: {iterations} kills, {ops_total} ops, {} live items, {:.1}s",
        model.items.len(),
        started.elapsed().as_secs_f64()
    );
}

impl Model {
    /// Item keys named by ops begun at or after `seq`.
    fn replay_keys_since(text: &str, seq: u64) -> Vec<u64> {
        let mut keys = Vec::new();
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("B ") else {
                continue;
            };
            let Some((s, op)) = rest.split_once(' ') else {
                continue;
            };
            if s.parse::<u64>().is_ok_and(|s| s >= seq)
                && let Some(op) = Op::decode(op)
            {
                match op {
                    Op::Upsert { key, .. }
                    | Op::Rechunk { key, .. }
                    | Op::Reembed { key, .. }
                    | Op::Delete { key }
                    | Op::Graph { key, .. } => keys.push(key),
                    _ => {}
                }
            }
        }
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

#[test]
fn journal_replay_resolves_ops() {
    let j = "B 0 up 0 0 2 1\nC 0\nB 1 em 0 1\nC 1\nB 2 del 0\nX 2\nB 3 au 3\nA 3\nB 4 up 4 4 1 0";
    let m = Model::replay(j);
    assert_eq!(
        m.items.get(&0),
        Some(&ItemState {
            ver: 0,
            chunk_ver: 0,
            chunks: 2,
            emb: Some(1)
        })
    );
    assert_eq!(m.audits, 1);
    assert_eq!(
        m.pending,
        Some((
            4,
            Op::Upsert {
                key: 4,
                ver: 4,
                chunks: 1,
                emb: false
            }
        ))
    );
    assert_eq!(m.next_seq, 5);
}
