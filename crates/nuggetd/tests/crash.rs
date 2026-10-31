//! Crash safety of the whole daemon: the real `nuggetd` binary is hard-killed (`Child::kill`:
//! SIGKILL on Unix, `TerminateProcess` on Windows, so no cleanup runs) at random points while it
//! indexes a files source of Markdown notes and mail, embeds chunks through a fake
//! OpenAI-compatible embedder on 127.0.0.1, re-embeds items it indexed text-only while the
//! embedder was down, extracts knowledge (people, organisations, projects from frontmatter and
//! mail headers) and renders the Markdown vault.
//!
//! Between runs the corpus is mutated (files added, edited, deleted, renamed, also while the
//! daemon runs), and the embedder is switched between up, down and flaky, so every restart has
//! real resync, re-embed, extraction and vault work to do. After every kill the harness opens the
//! store directly and asserts [`Store::check_consistency`] (integrity_check, audit chain,
//! full-text and vector indexes against the tables, duplicates, dangling rows, change feed), and
//! that the vault holds only whole pages (frontmatter + body, no `.md` temp file, no two pages
//! for one entity) and no fact the user never wrote (a stale merge base after a kill must not
//! turn generated lines into user edits).
//!
//! At the end the daemon runs (in-process, so it can be stopped gracefully on every OS) until
//! it is idle, and the result must equal a clean single pass over the same final corpus in a
//! fresh home: the same items (one per file, none for deleted files), the same content hashes and
//! chunk counts, every chunk embedded, a unique token per file found by full-text search in
//! exactly that file, the same entities, and the audit chain intact.
//!
//! The corpus links people and projects from frontmatter and mail headers. Notes of
//! `type: project` are left out: they name an entity after their file, and entity resolution
//! then depends on the order files are seen, which an interrupted history and a clean pass
//! legitimately see differently (not a crash bug).
//!
//! Environment: `NUGGET_CRASH_ITERATIONS` (default 12), `NUGGET_CRASH_SEED` (default: the clock;
//! printed, so a failing corpus and schedule can be replayed; kill timing still varies with the
//! machine), `NUGGET_CRASH_LOG` (write the killed daemons' logs there when the test ends),
//! `NUGGET_CRASH_KEEP` (keep the temporary homes, corpus and vault for inspection).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nugget_core::capability::Capabilities;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{IngestStatus, KnowledgeStatus, SourceInfo, method};
use nugget_core::paths::Paths;
use nugget_graph::{ClaimStatus, EntityQuery, GraphStore};
use nugget_infer::ProviderConfig;
use nugget_ingest::{MemorySecrets, NoPowerPolicy};
use nugget_ipc::Client;
use nugget_store::{Store, StoreKey};
use nuggetd::probes::ProbeCache;
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use wiremock::matchers::{method as http_method, path as http_path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const EXE: &str = env!("CARGO_BIN_EXE_nuggetd");
const DIM: usize = 16;
const NOTES: usize = 36;
const MAILS: usize = 8;
const PEOPLE: [&str; 6] = [
    "Alice Smith",
    "Bob Jones",
    "Carol White",
    "Dan Brown",
    "Erin Green",
    "Frank Black",
];
const PROJECTS: [&str; 4] = ["Apollo", "Gemini", "Mercury", "Artemis"];
const DOMAINS: [&str; 3] = ["acme.com", "globex.com", "initech.com"];
/// Bound of every wait for a condition.
const EVENTUALLY: Duration = Duration::from_secs(120);

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

    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len() as u64) as usize]
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

// ---- the fake embedder -----------------------------------------------------------------------

const UP: u8 = 0;
const DOWN: u8 = 1;
const FLAKY: u8 = 2;

/// `POST /v1/embeddings`: deterministic vectors from the input text, or 503 while down (and
/// every other request while flaky).
struct Embedder {
    mode: Arc<AtomicU8>,
    requests: Arc<AtomicU64>,
}

impl Respond for Embedder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let n = self.requests.fetch_add(1, Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            DOWN => return ResponseTemplate::new(503),
            FLAKY if n.is_multiple_of(2) => return ResponseTemplate::new(503),
            _ => {}
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let inputs: Vec<String> = match &body["input"] {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_owned())
                .collect(),
            _ => Vec::new(),
        };
        let data: Vec<Value> = inputs
            .iter()
            .enumerate()
            .map(|(i, t)| json!({"object": "embedding", "index": i, "embedding": embed(t)}))
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "model": "fake-embed",
            "data": data,
            "usage": {"prompt_tokens": 1, "total_tokens": 1}
        }))
    }
}

/// A dense, normalised vector derived from the text's bytes (FNV-1a per dimension).
fn embed(text: &str) -> Vec<f32> {
    let mut v: Vec<f32> = (0..DIM as u64)
        .map(|d| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ d;
            for b in text.bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            (h % 2000) as f32 / 1000.0 - 1.0
        })
        .collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter_mut().for_each(|x| *x /= norm);
    v
}

// ---- the corpus ------------------------------------------------------------------------------

/// The files the corpus should hold: relative path (with `/`) → unique token of its content.
#[derive(Debug, Clone, Default)]
struct Corpus {
    root: PathBuf,
    files: BTreeMap<String, String>,
    next: u64,
}

impl Corpus {
    fn new(root: PathBuf) -> Self {
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(root.join("mail")).unwrap();
        Self {
            root,
            files: BTreeMap::new(),
            next: 0,
        }
    }

    fn abs(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, c| p.join(c))
    }

    fn token(&mut self) -> String {
        self.next += 1;
        format!("crashtok{}", self.next)
    }

    /// Write `rel` with fresh content (new token), atomically, so the watcher never indexes a
    /// half-written file (which would be the test's fault, not the daemon's).
    fn write(&mut self, rng: &mut Rng, rel: &str) {
        let tok = self.token();
        let text = if rel.ends_with(".eml") {
            mail_text(rng, &tok)
        } else {
            note_text(rng, rel, &tok)
        };
        let path = self.abs(rel);
        // Hidden, so the files source never indexes it.
        let tmp = path.with_file_name(format!(".{}.partial", rel.replace('/', "-")));
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        self.files.insert(rel.to_owned(), tok);
    }

    fn add(&mut self, rng: &mut Rng) {
        let n = self.next + 1;
        let rel = if rng.below(5) == 0 {
            format!("mail/m{n}.eml")
        } else {
            format!("notes/n{n}.md")
        };
        self.write(rng, &rel);
    }

    fn some(&self, rng: &mut Rng) -> Option<String> {
        let keys: Vec<&String> = self.files.keys().collect();
        (!keys.is_empty()).then(|| (*rng.pick(&keys)).clone())
    }

    fn edit(&mut self, rng: &mut Rng) {
        if let Some(rel) = self.some(rng) {
            self.write(rng, &rel);
        }
    }

    fn delete(&mut self, rng: &mut Rng) {
        if let Some(rel) = self.some(rng) {
            std::fs::remove_file(self.abs(&rel)).unwrap();
            self.files.remove(&rel);
        }
    }

    fn rename(&mut self, rng: &mut Rng) {
        if let Some(rel) = self.some(rng) {
            let n = self.next + 1;
            self.next += 1;
            let (dir, ext) = if rel.ends_with(".eml") {
                ("mail", "eml")
            } else {
                ("notes", "md")
            };
            let to = format!("{dir}/r{n}.{ext}");
            std::fs::rename(self.abs(&rel), self.abs(&to)).unwrap();
            let tok = self.files.remove(&rel).unwrap();
            self.files.insert(to, tok);
        }
    }

    /// A few random changes.
    fn mutate(&mut self, rng: &mut Rng) {
        for _ in 0..rng.range(1, 4) {
            self.add(rng);
        }
        for _ in 0..rng.range(1, 5) {
            self.edit(rng);
        }
        for _ in 0..rng.range(0, 3) {
            self.delete(rng);
        }
        if rng.below(2) == 0 {
            self.rename(rng);
        }
    }

    /// Item key of a file: root 0, `/`-separated relative path.
    fn key(rel: &str) -> String {
        format!("0:{rel}")
    }
}

const FILLER: &str = "We reviewed the indexer, the write-ahead log and the recovery path. \
The plan is to ship the crash harness first, then measure how long a resync takes after a hard \
kill, and finally document what a power loss can and cannot lose.";

fn note_text(rng: &mut Rng, rel: &str, tok: &str) -> String {
    let project = *rng.pick(&PROJECTS);
    let a = *rng.pick(&PEOPLE);
    let b = *rng.pick(&PEOPLE);
    let paras = rng.range(1, 6);
    let mut body = String::new();
    for i in 0..paras {
        body.push_str(&format!(
            "\n## Part {i}\n\n{FILLER} Paragraph {i} of {rel}.\n"
        ));
    }
    // Projects and people only as links: `type: project` notes name an entity after their file
    // and entity resolution then depends on the order files are seen, which a clean pass and
    // an interrupted history legitimately see differently.
    format!(
        "---\nproject: \"[[{project}]]\"\npeople: [\"[[{a}]]\", \"[[{b}]]\"]\ntags: [crash]\n---\n# Meeting about {project}\n\nNotes {tok} from a meeting with {a} and {b}.\n{body}"
    )
}

fn mail_text(rng: &mut Rng, tok: &str) -> String {
    let who = *rng.pick(&PEOPLE);
    let domain = *rng.pick(&DOMAINS);
    let addr = who.to_lowercase().replace(' ', ".");
    format!(
        "From: {who} <{addr}@{domain}>\r\nTo: Tanuj <tanuj@trylumi.co>\r\nSubject: Update {tok}\r\nMessage-ID: <{tok}@example.com>\r\nDate: Mon, 26 Oct 2026 10:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello, the update is {tok}. {FILLER}\r\n"
    )
}

// ---- homes -----------------------------------------------------------------------------------

/// A Nugget home with the harness's config.
struct Home {
    root: PathBuf,
    paths: Paths,
    vault: PathBuf,
}

impl Home {
    fn new(root: PathBuf, embedder: &str) -> Self {
        let paths = Paths::rooted(root.clone());
        let vault = root.join("vault");
        std::fs::create_dir_all(&paths.config).unwrap();
        let provider = format!(
            "id = \"embed\"\nkind = \"openai_compatible\"\nbase_url = \"{embedder}/v1\"\nmodel = \"fake-embed\"\ntiers = [\"embedding\"]\nmax_retries = 0\n"
        );
        let config = format!(
            r#"[runtime]
enabled = false

[ingest]
triage = false
reembed_interval_ms = 700
backoff_base_ms = 100
backoff_max_ms = 1000
chunk_target_tokens = 48
chunk_max_tokens = 64
chunk_overlap_tokens = 4

[knowledge]
debounce_ms = 100
interval_minutes = 0

[knowledge.me]
name = "Tanuj"
emails = ["tanuj@trylumi.co"]

[vault]
path = '{}'
debounce = 100

[mcp]
http = false

[[providers]]
{provider}"#,
            vault.display()
        );
        std::fs::write(paths.config_file(), config).unwrap();
        // The embedder's probe result, so the router trusts it with embeddings.
        let cfg = ProviderConfig::from_toml_str(&provider).unwrap();
        ProbeCache::load(ProbeCache::path_for(&paths))
            .put(
                &cfg,
                Capabilities {
                    embeddings: true,
                    ..Default::default()
                },
            )
            .unwrap();
        Self { root, paths, vault }
    }

    /// The store key (`NUGGET_KEYCHAIN=off` keeps it in `<data>/store.key`, hex).
    fn key(&self) -> StoreKey {
        let hex = std::fs::read_to_string(self.paths.data.join("store.key")).unwrap();
        let hex = hex.trim();
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        StoreKey::from_bytes(bytes)
    }

    fn open_store(&self) -> Store {
        Store::open(self.paths.db_file(), &self.key()).unwrap()
    }
}

/// The real binary, killed on drop (a failed assertion).
struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn spawn(home: &Home, log: &Path) -> Proc {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    let child = Command::new(EXE)
        .arg("--home")
        .arg(&home.root)
        .env("NUGGET_KEYCHAIN", "off")
        .env("NUGGET_LOG", "info")
        .env_remove("RUST_LOG")
        .env_remove("NUGGET_HOME")
        .stdin(Stdio::null())
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .spawn()
        .unwrap();
    Proc(Some(child))
}

/// Hard-kill and reap (so file handles and locks are released, which matters on Windows).
fn kill(mut p: Proc, log: &Path) {
    let mut c = p.0.take().unwrap();
    if let Some(status) = c.try_wait().unwrap() {
        panic!("nuggetd exited on its own ({status}); log:\n{}", tail(log));
    }
    c.kill().unwrap();
    c.wait().unwrap();
}

/// The daemons' stderr (`log`) and their JSON log files (`<home>/data/logs`, next to it).
fn all_logs(log: &Path) -> String {
    let mut text = std::fs::read_to_string(log).unwrap_or_default();
    let dir = log.with_file_name("home").join("data").join("logs");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    for f in files {
        text.push_str(&std::fs::read_to_string(f).unwrap_or_default());
    }
    text
}

/// Copies the killed daemons' logs to `NUGGET_CRASH_LOG` when the test ends, passed or failed.
struct LogCopy(PathBuf);

impl Drop for LogCopy {
    fn drop(&mut self) {
        if let Some(dest) = std::env::var_os("NUGGET_CRASH_LOG") {
            let _ = std::fs::write(dest, all_logs(&self.0));
        }
    }
}

fn tail(log: &Path) -> String {
    let text = all_logs(log);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(60)..].join("\n")
}

async fn call<R: serde::de::DeserializeOwned>(c: &Client, m: &str, params: Value) -> R {
    tokio::time::timeout(Duration::from_secs(60), c.call(m, &params))
        .await
        .unwrap_or_else(|_| panic!("{m} timed out"))
        .unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

/// Connect to the daemon of `paths`, polling until it answers (bounded). `None` if `proc` died.
async fn connect(paths: &Paths, proc: Option<&mut Proc>, log: &Path) -> Client {
    let deadline = Instant::now() + EVENTUALLY;
    let mut proc = proc;
    loop {
        if let Ok(c) = Client::connect(paths).await
            && c.call::<_, Value>(method::STATUS, &Value::Null)
                .await
                .is_ok()
        {
            return c;
        }
        if let Some(p) = proc.as_deref_mut()
            && let Some(status) = p.0.as_mut().unwrap().try_wait().unwrap()
        {
            panic!("nuggetd exited ({status}); log:\n{}", tail(log));
        }
        assert!(Instant::now() < deadline, "nuggetd never answered");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn add_source(c: &Client, corpus: &Path) -> SourceInfo {
    call(
        c,
        method::SOURCES_ADD,
        json!({"kind": "files", "name": "corpus",
               "settings": {"roots": [corpus], "exclude": [], "debounce_ms": 50}}),
    )
    .await
}

// ---- checks ----------------------------------------------------------------------------------

/// Pages of the vault: relative path (with `/`) → text, skipping `.nugget/`.
fn vault_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), root.to_path_buf())];
    while let Some((rel, dir)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                stack.push((r, e.path()));
            } else if ft.is_file() {
                out.push((r, e.path()));
            }
        }
    }
    out.sort();
    out
}

/// Every vault page is whole; temp files are never pages; one page per entity.
fn vault_problems(root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for (rel, path) in vault_files(root) {
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        if name.ends_with(".nugget-tmp") {
            if name.ends_with(".md") {
                problems.push(format!("temp file named like a page: {rel}"));
            }
            continue;
        }
        // Nobody edits the vault here, so nothing can conflict: a conflict means a page was
        // merged against the wrong base.
        if let Some(page) = rel.strip_prefix(".nugget/conflicts/") {
            let sidecar = std::fs::read_to_string(&path).unwrap_or_default();
            let mine =
                std::fs::read_to_string(page.split('/').fold(root.to_path_buf(), |p, c| p.join(c)))
                    .unwrap_or_default();
            problems.push(format!(
                "conflict on {page}:\n--- sidecar:\n{sidecar}\n--- page:\n{mine}"
            ));
            continue;
        }
        if !rel.ends_with(".md") || rel.starts_with(".nugget/") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("{rel}: unreadable: {e}"));
                continue;
            }
        };
        if rel == "README.md" {
            if !text.contains("<!-- nugget:end") {
                problems.push(format!("{rel}: truncated index"));
            }
            continue;
        }
        let Some(rest) = text.strip_prefix("---\n") else {
            problems.push(format!("{rel}: no frontmatter"));
            continue;
        };
        let Some((fm, body)) = rest.split_once("\n---\n") else {
            problems.push(format!("{rel}: unterminated frontmatter"));
            continue;
        };
        if let Err(e) = regions_balanced(body) {
            problems.push(format!("{rel}: truncated body ({e})"));
        }
        match fm
            .lines()
            .find_map(|l| l.strip_prefix("nugget_id:"))
            .map(|v| v.trim().trim_matches('"').to_owned())
        {
            Some(id) => {
                if let Some(other) = ids.insert(id.clone(), rel.clone()) {
                    problems.push(format!("two pages for entity {id}: {other} and {rel}"));
                }
                // Without user edits a page is its last render, which is its merge base (or the
                // base still pending from a write the kill interrupted). Anything else would be
                // merged as if the user had edited it. (Not for stubs of removed entities: they
                // keep the old page's user text under the generated redirect.)
                if !body.contains("<!-- nugget:begin redirect -->")
                    && let Some(short) = rel
                        .strip_suffix(").md")
                        .and_then(|s| s.rsplit_once(" ("))
                        .map(|(_, s)| s)
                {
                    let base = root
                        .join(".nugget")
                        .join("base")
                        .join("entities")
                        .join(format!("{short}.md"));
                    let mut pending = base.clone().into_os_string();
                    pending.push(".pending");
                    let base_text = std::fs::read_to_string(&base).ok();
                    let pending_text = std::fs::read_to_string(PathBuf::from(pending))
                        .ok()
                        .and_then(|p| p.split_once('\n').map(|(_, g)| g.to_owned()));
                    if base_text.as_deref() != Some(text.as_str())
                        && pending_text.as_deref() != Some(text.as_str())
                    {
                        problems.push(format!(
                            "{rel} differs from its merge base:\n--- page:\n{text}\n--- base:\n{}\n--- pending:\n{}",
                            base_text.unwrap_or_default(),
                            pending_text.unwrap_or_default()
                        ));
                    }
                }
            }
            None => problems.push(format!("{rel}: no nugget_id")),
        }
    }
    problems
}

/// Every generated region that begins also ends, and there is at least one (a page cut short
/// by a torn write fails this).
fn regions_balanced(body: &str) -> Result<(), String> {
    let mut open: Option<&str> = None;
    let mut regions = 0;
    for line in body.lines() {
        let l = line.trim();
        if let Some(name) = l
            .strip_prefix("<!-- nugget:begin ")
            .and_then(|r| r.strip_suffix(" -->"))
        {
            if let Some(o) = open {
                return Err(format!("{name} begins inside {o}"));
            }
            open = Some(name);
        } else if let Some(name) = l
            .strip_prefix("<!-- nugget:end ")
            .and_then(|r| r.strip_suffix(" -->"))
        {
            if open != Some(name) {
                return Err(format!("{name} ends but {open:?} is open"));
            }
            open = None;
            regions += 1;
        }
    }
    match (open, regions) {
        (Some(o), _) => Err(format!("{o} never ends")),
        (None, 0) => Err("no generated region".into()),
        _ => Ok(()),
    }
}

/// Claims in states only a user (vault edits, the app) can cause. The harness never edits.
fn user_claims(store: &Store) -> Vec<String> {
    let g = store.graph();
    let mut out = Vec::new();
    for e in g
        .list_entities(&EntityQuery {
            text: None,
            kind: None,
            limit: 100_000,
            offset: 0,
        })
        .unwrap()
    {
        for c in g.claims_for(&e.id, true).unwrap() {
            if matches!(
                c.status,
                ClaimStatus::UserAsserted | ClaimStatus::Rejected | ClaimStatus::Superseded
            ) {
                out.push(format!("{} [{:?}] {}", e.name, c.status, c.text));
            }
        }
    }
    out
}

/// What a home ended up with, for comparing with the reference.
#[derive(Debug, PartialEq)]
struct Summary {
    /// Item key → (content hash, chunk count).
    items: BTreeMap<String, (String, usize)>,
    /// (kind, name) of entities with at least one active claim or mention.
    entities: BTreeSet<String>,
    /// Vault page paths without the id suffix.
    pages: BTreeSet<String>,
}

fn summary(store: &Store, source: &SourceId, vault: &Path) -> Summary {
    let mut items = BTreeMap::new();
    for key in store.item_versions(source).unwrap().into_keys() {
        let r = ItemRef {
            source: source.clone(),
            key: key.clone(),
        };
        let item = store.get_item(&r).unwrap().unwrap();
        let chunks = store
            .get_chunks_by_ord(&r, &(0..512).collect::<Vec<u32>>())
            .unwrap()
            .len();
        items.insert(key.as_str().to_owned(), (item.content_hash, chunks));
    }
    let g = store.graph();
    let entities = g
        .list_entities(&EntityQuery {
            text: None,
            kind: None,
            limit: 100_000,
            offset: 0,
        })
        .unwrap()
        .into_iter()
        .filter(|e| e.claims > 0 || e.mentions > 0)
        .map(|e| format!("{:?}/{}", e.kind, e.name))
        .collect();
    let pages = vault_files(vault)
        .into_iter()
        .map(|(rel, _)| rel)
        .filter(|r| r.ends_with(".md") && !r.starts_with(".nugget/"))
        .map(|r| match r.rsplit_once(" (") {
            Some((stem, _)) => stem.to_owned(),
            None => r,
        })
        .collect();
    Summary {
        items,
        entities,
        pages,
    }
}

/// Run the daemon in-process on `home` until idle (bounded), then stop it gracefully. Adds the
/// files source over `corpus` first if `add` is set. Returns the source id.
fn settle(rt: &Runtime, home: &Home, corpus: &Corpus, requests: &AtomicU64, add: bool) -> SourceId {
    rt.block_on(async {
        let mut opts = DaemonOptions::new(home.paths.clone());
        opts.store_key = Some(home.key());
        opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
        opts.power = Some(Arc::new(NoPowerPolicy));
        opts.home = Some(home.root.clone());
        opts.nuggetd_exe = Some(PathBuf::from(EXE));
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("a daemon is still running on {socket}"),
        };
        let store = daemon.store();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&home.paths).await.unwrap();
        if add {
            add_source(&client, &corpus.root).await;
        }
        let source = {
            let status: IngestStatus = call(&client, method::INGEST_STATUS, Value::Null).await;
            assert_eq!(status.sources.len(), 1, "{status:?}");
            status.sources[0].id.clone()
        };
        let want: BTreeSet<String> = corpus.files.keys().map(|r| Corpus::key(r)).collect();
        // Idle: the items are exactly the corpus, nothing queued or pending, every chunk
        // embedded, the vault rendered after the last run, and nothing changing for a while.
        let deadline = Instant::now() + EVENTUALLY;
        let mut last = None;
        let mut stable = 0;
        loop {
            let st: IngestStatus = call(&client, method::INGEST_STATUS, Value::Null).await;
            let k: KnowledgeStatus = call(&client, method::KNOWLEDGE_STATUS, Value::Null).await;
            let keys: BTreeSet<String> = store
                .item_versions(&source)
                .unwrap()
                .into_keys()
                .map(|k| k.as_str().to_owned())
                .collect();
            let missing = store.items_missing_embeddings(None, 1).unwrap().len();
            let counts = store.counts().unwrap();
            let rendered = k.last_run.is_some()
                && k.vault.last_render.is_some_and(|r| Some(r) >= k.last_run);
            let idle = st.queue_depth == 0
                && k.pending_items == 0
                && missing == 0
                && rendered
                && keys == want
                && st.sources.iter().all(|s| s.last_sync.is_some());
            let snap = (
                counts.items,
                counts.chunks,
                k.entities,
                k.claims,
                k.vault.pages,
                requests.load(Ordering::SeqCst),
            );
            if idle && last.as_ref() == Some(&snap) {
                stable += 1;
            } else {
                stable = 0;
            }
            if stable >= 8 {
                break;
            }
            last = Some(snap);
            assert!(
                Instant::now() < deadline,
                "never idle: queue {}, pending {}, missing embeddings {missing}, rendered {rendered}, \
                 extra items {:?}, missing items {:?}",
                st.queue_depth,
                k.pending_items,
                keys.difference(&want).collect::<Vec<_>>(),
                want.difference(&keys).collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        drop(client);
        let _ = tx.send(());
        tokio::time::timeout(Duration::from_secs(60), server)
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
        source
    })
}

#[test]
fn daemon_survives_hard_kills() {
    let iterations = env_u64("NUGGET_CRASH_ITERATIONS").unwrap_or(12);
    let seed = env_u64("NUGGET_CRASH_SEED").unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
    });
    println!("daemon crash harness: NUGGET_CRASH_SEED={seed}, {iterations} iterations");
    let started = Instant::now();
    let mut rng = Rng::new(seed);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let mode = Arc::new(AtomicU8::new(UP));
    let requests = Arc::new(AtomicU64::new(0));
    let server = rt.block_on(async {
        let s = MockServer::start().await;
        Mock::given(http_method("POST"))
            .and(http_path("/v1/embeddings"))
            .respond_with(Embedder {
                mode: mode.clone(),
                requests: requests.clone(),
            })
            .mount(&s)
            .await;
        s
    });

    let mut tmp = tempfile::Builder::new()
        .prefix("nugget-crash-")
        .tempdir()
        .unwrap();
    if std::env::var_os("NUGGET_CRASH_KEEP").is_some() {
        println!("keeping {}", tmp.path().display());
        tmp.disable_cleanup(true);
    }
    let home = Home::new(tmp.path().join("home"), &server.uri());
    let log = tmp.path().join("nuggetd.log");
    let _copy_log = LogCopy(log.clone());
    let mut corpus = Corpus::new(tmp.path().join("corpus"));
    for _ in 0..NOTES + MAILS {
        corpus.add(&mut rng);
    }

    let mut kills_during_work = 0;
    for iter in 0..iterations {
        let ctx = format!("iteration {iter}, NUGGET_CRASH_SEED={seed}");
        let m = match rng.below(10) {
            0..=5 => UP,
            6..=7 => DOWN,
            _ => FLAKY,
        };
        mode.store(m, Ordering::SeqCst);
        let mut proc = spawn(&home, &log);
        // The first run adds the source (and must get that far); later ones are killed either
        // at a random moment after start (startup, store recovery) or after the daemon answers.
        let early = iter > 0 && rng.below(4) == 0;
        if early {
            std::thread::sleep(Duration::from_millis(rng.range(50, 1500)));
        } else {
            let client = rt.block_on(connect(&home.paths, Some(&mut proc), &log));
            if iter == 0 {
                rt.block_on(add_source(&client, &corpus.root));
            }
            drop(client);
            if rng.below(2) == 0 {
                std::thread::sleep(Duration::from_millis(rng.range(0, 400)));
                corpus.mutate(&mut rng); // live changes, through the watcher
            }
            std::thread::sleep(Duration::from_millis(rng.range(0, 1200)));
            kills_during_work += 1;
        }
        kill(proc, &log);

        let store = home.open_store();
        let report = store
            .check_consistency()
            .unwrap_or_else(|e| panic!("{ctx}: consistency check: {e}"));
        assert!(
            report.ok,
            "{ctx}: inconsistent store: {:#?}\nlog:\n{}",
            report.problems,
            tail(&log)
        );
        let fake = user_claims(&store);
        assert!(fake.is_empty(), "{ctx}: facts nobody wrote: {fake:#?}");
        drop(store);
        let problems = vault_problems(&home.vault);
        assert!(problems.is_empty(), "{ctx}: damaged vault: {problems:#?}");
        corpus.mutate(&mut rng);
    }

    // Settle with a working embedder, then compare with a clean single pass.
    mode.store(UP, Ordering::SeqCst);
    let source = settle(&rt, &home, &corpus, &requests, false);
    let store = home.open_store();
    let report = store.check_consistency().unwrap();
    assert!(report.ok, "after settling: {:#?}", report.problems);
    store.verify_audit_chain().unwrap();
    assert!(user_claims(&store).is_empty());
    let problems = vault_problems(&home.vault);
    assert!(problems.is_empty(), "after settling: {problems:#?}");
    let leftovers: Vec<String> = vault_files(&home.vault)
        .into_iter()
        .map(|(r, _)| r)
        .filter(|r| r.ends_with(".nugget-tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left in the vault: {leftovers:?}"
    );
    for (rel, tok) in &corpus.files {
        let want = ItemRef {
            source: source.clone(),
            key: ItemKey::new(Corpus::key(rel)),
        };
        let hits = store.search_text(tok, 20).unwrap();
        assert!(
            hits.iter().any(|h| h.item == want),
            "search for {tok} misses {rel}"
        );
        let others: Vec<_> = hits.iter().filter(|h| h.item != want).collect();
        assert!(
            others.is_empty(),
            "search for {tok} ({rel}) also finds {others:?}"
        );
    }
    let got = summary(&store, &source, &home.vault);
    drop(store);

    let ref_corpus_root = tmp.path().join("reference-corpus");
    let mut reference = Corpus::new(ref_corpus_root.clone());
    for rel in corpus.files.keys() {
        std::fs::copy(corpus.abs(rel), reference.abs(rel)).unwrap();
    }
    reference.files = corpus.files.clone();
    // The reference home needs a key file before the in-process start reads it.
    let ref_home = Home::new(tmp.path().join("reference-home"), &server.uri());
    std::fs::create_dir_all(&ref_home.paths.data).unwrap();
    std::fs::write(ref_home.paths.data.join("store.key"), "07".repeat(32)).unwrap();
    let ref_source = settle(&rt, &ref_home, &reference, &requests, true);
    let ref_store = ref_home.open_store();
    let want = summary(&ref_store, &ref_source, &ref_home.vault);
    assert_eq!(
        got.items, want.items,
        "items differ from a clean index of the same corpus"
    );
    assert_eq!(
        got.entities, want.entities,
        "entities differ from a clean index of the same corpus"
    );
    assert_eq!(
        got.pages, want.pages,
        "vault pages differ from a clean index of the same corpus"
    );
    println!(
        "daemon crash harness: {iterations} kills ({kills_during_work} while serving), {} files, \
         {} entities, {:.1}s",
        corpus.files.len(),
        got.entities.len(),
        started.elapsed().as_secs_f64()
    );
}
