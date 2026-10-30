//! FF1: backups, retention, the scheduler, export, restore (same home, fresh home with the
//! passphrase, damaged database / recovery mode, tampered backups), the `backup`, `restore` and
//! `export` CLI, and a committed golden backup that must restore on every OS.
//!
//! In-process daemons get an injected store key and never install another one (a restore with
//! the same key); everything that installs a key runs the real binary with
//! `NUGGET_KEYCHAIN=off`, so no OS keychain is touched. No network.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use nugget_core::ipc::{StatusResponse, method};
use nugget_core::paths::Paths;
use nugget_ingest::{MemorySecrets, NoPowerPolicy};
use nugget_ipc::{Client, IpcError};
use nugget_store::{KeyEnvelope, NewChunk, NewItem, Store, StoreKey, content_hash};
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const EXE: &str = env!("CARGO_BIN_EXE_nuggetd");
const CALL: Duration = Duration::from_secs(120);
const PASS: &str = "tidewater lantern quarry";
const KEY: [u8; 32] = [3; 32];
/// The golden fixture's store key and passphrase (see tests/fixtures/backup-v1/README.md).
const FIXTURE_KEY: [u8; 32] = [9; 32];
const FIXTURE_PASS: &str = "golden fixture passphrase";
const FIXTURE_ITEMS: usize = 5;

const CONFIG: &str =
    "[runtime]\nenabled = false\n\n[knowledge]\ninterval_minutes = 0\n\n[mcp]\nenabled = false\n";

fn sha256(path: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

fn write_config(paths: &Paths) {
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.config_file(), CONFIG).unwrap();
}

/// Seal `key` under `pass` with tiny Argon2 parameters into `<data>/backup-key.json` (what
/// `backup.set_passphrase` does, minus the cost).
fn write_envelope(paths: &Paths, key: [u8; 32], pass: &str) {
    let env =
        KeyEnvelope::seal_insecure_for_tests(&StoreKey::from_bytes(key), pass, 8, 1, 1).unwrap();
    std::fs::create_dir_all(&paths.data).unwrap();
    std::fs::write(
        paths.data.join("backup-key.json"),
        serde_json::to_vec(&env).unwrap(),
    )
    .unwrap();
}

fn seed(store: &Store, from: usize, n: usize, word: &str) {
    for i in from..from + n {
        let text = format!("note {i} about the {word} harbour ledger");
        let item = NewItem::new("files:seed", format!("{i}.txt"), content_hash(&text))
            .with_title(format!("Note {i}"))
            .with_text(text.clone());
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text(text)])
            .unwrap();
    }
}

fn dirs_named(folder: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(folder)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with(prefix)
                && e.path().join("manifest.json").exists()
        })
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

fn data_files_starting(paths: &Paths, prefix: &str) -> Vec<PathBuf> {
    data_files_starting_in(&paths.data, prefix)
}

async fn call(client: &Client, m: &str, params: Value) -> Value {
    tokio::time::timeout(CALL, client.call_value(m, params))
        .await
        .unwrap_or_else(|_| panic!("{m} timed out"))
        .unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

async fn call_err(client: &Client, m: &str, params: Value) -> nugget_ipc::RpcError {
    match tokio::time::timeout(CALL, client.call_value(m, params)).await {
        Ok(Err(IpcError::Rpc(e))) => e,
        other => panic!("{m}: expected an RPC error, got {other:?}"),
    }
}

async fn status(client: &Client) -> StatusResponse {
    serde_json::from_value(call(client, method::STATUS, json!({})).await).unwrap()
}

/// Wait until the startup audit check finished, and return its result.
async fn audit_ok(client: &Client) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(ok) = status(client).await.audit_chain_ok {
            return ok;
        }
        assert!(Instant::now() < deadline, "audit check never finished");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---- in-process daemons ----

struct Running {
    client: Client,
    store: Store,
    vault: Option<PathBuf>,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<nugget_core::Result<()>>,
}

async fn start(paths: &Paths, key: [u8; 32]) -> Running {
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes(key));
    opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
    opts.power = Some(Arc::new(NoPowerPolicy));
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let store = daemon.store();
    let vault = daemon.vault_root();
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    let client = Client::connect(paths).await.unwrap();
    Running {
        client,
        store,
        vault,
        stop: Some(tx),
        server,
    }
}

impl Running {
    async fn stop(mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        drop(self.client);
        drop(self.store);
        tokio::time::timeout(Duration::from_secs(30), self.server)
            .await
            .expect("daemon did not stop")
            .unwrap()
            .unwrap();
    }

    /// Wait for the daemon to stop by itself (after a staged restore).
    async fn exited(self) {
        drop(self.client);
        drop(self.store);
        tokio::time::timeout(Duration::from_secs(30), self.server)
            .await
            .expect("daemon did not stop by itself after staging the restore")
            .unwrap()
            .unwrap();
    }
}

struct Home {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
}

fn home(name: &str) -> Home {
    let tmp = tempfile::Builder::new()
        .prefix(&format!("nugget-backup-{name}-"))
        .tempdir()
        .unwrap();
    let root = tmp.path().to_path_buf();
    let paths = Paths::rooted(root.join("home"));
    write_config(&paths);
    Home {
        _tmp: tmp,
        root,
        paths,
    }
}

impl Home {
    fn dir(&self, name: &str) -> PathBuf {
        let d = self.root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backups_are_verified_listed_and_rotated() {
    let h = home("run");
    let folder = h.dir("backups");
    let d = start(&h.paths, KEY).await;
    seed(&d.store, 0, 12, "lighthouse");
    let mut notes = d.client.notifications();

    let s = call(&d.client, "backup.status", json!({})).await;
    assert_eq!(s["interval_hours"], 24);
    assert_eq!(s["keep"], 7);
    assert_eq!(s["passphrase_set"], false);
    assert!(s["folder"].is_null() && s["recovery"].is_null() && s["next_due"].is_null());

    // No folder, then no passphrase.
    let e = call_err(&d.client, "backup.run", json!({})).await;
    assert!(e.message.contains("folder"), "{e:?}");
    let s = call(
        &d.client,
        "backup.configure",
        json!({"folder": folder, "keep": 2, "interval_hours": 0}),
    )
    .await;
    assert_eq!(s["keep"], 2);
    assert_eq!(s["folder"], folder.display().to_string());
    let e = call_err(&d.client, "backup.run", json!({})).await;
    assert!(e.message.contains("passphrase"), "{e:?}");
    let e = call_err(
        &d.client,
        "backup.set_passphrase",
        json!({"passphrase": "short"}),
    )
    .await;
    assert_eq!(e.code, nugget_ipc::code::INVALID_PARAMS);
    let e = call_err(&d.client, "backup.configure", json!({"keep": 0})).await;
    assert_eq!(e.code, nugget_ipc::code::INVALID_PARAMS);
    // Never inside Nugget's own folders.
    let inside = h.paths.data.join("backups");
    let e = call_err(&d.client, "backup.configure", json!({"folder": inside})).await;
    assert_eq!(e.code, nugget_ipc::code::INVALID_PARAMS, "{e:?}");
    let vault = d.vault.clone().unwrap().join("b");
    let e = call_err(&d.client, "backup.run", json!({"folder": vault})).await;
    assert_eq!(e.code, nugget_ipc::code::INVALID_PARAMS, "{e:?}");
    let e = call_err(&d.client, "export.run", json!({"folder": h.paths.config})).await;
    assert_eq!(e.code, nugget_ipc::code::INVALID_PARAMS);
    // The real passphrase call (default Argon2 cost), once.
    assert_eq!(
        call(
            &d.client,
            "backup.set_passphrase",
            json!({"passphrase": PASS})
        )
        .await,
        json!({"ok": true})
    );
    assert_eq!(
        call(&d.client, "backup.status", json!({})).await["passphrase_set"],
        true
    );

    // Things retention must never delete.
    let foreign = [
        folder.join("nugget-backup-19990101T000000Z"),
        folder.join("holiday-photos"),
    ];
    for f in &foreign {
        std::fs::create_dir(f).unwrap();
        std::fs::write(f.join("keep.txt"), b"mine").unwrap();
    }
    std::fs::write(
        folder
            .join("nugget-backup-19990101T000000Z")
            .join("manifest.json"),
        b"{}",
    )
    .unwrap();

    let mut infos = Vec::new();
    for _ in 0..3 {
        infos.push(call(&d.client, "backup.run", json!({})).await);
    }
    let first = &infos[0];
    assert_eq!(first["items"], 12);
    assert_eq!(first["os"], std::env::consts::OS);
    assert_eq!(first["schema_version"], nugget_store::SCHEMA_VERSION);
    let dir = PathBuf::from(first["path"].as_str().unwrap());
    assert_eq!(dir.parent().unwrap(), folder);

    // Retention kept the newest two; foreign directories are untouched.
    let complete = dirs_named(&folder, "nugget-backup-2");
    assert_eq!(complete.len(), 2, "{complete:?}");
    assert!(!dir.exists(), "the oldest backup should be rotated out");
    for f in &foreign {
        assert!(f.join("keep.txt").exists(), "{} was touched", f.display());
    }
    let newest = PathBuf::from(infos[2]["path"].as_str().unwrap());
    for name in ["store.nbk", "key.json", "manifest.json"] {
        assert!(newest.join(name).is_file(), "{name} missing");
    }
    let m: Value =
        serde_json::from_slice(&std::fs::read(newest.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(m["format"], 1);
    assert_eq!(m["items"], 12);
    assert_eq!(m["key_id"], StoreKey::from_bytes(KEY).key_id());
    for f in ["store.nbk", "key.json"] {
        assert_eq!(m["files"][f]["sha256"], sha256(&newest.join(f)), "{f}");
        assert_eq!(
            m["files"][f]["bytes"],
            std::fs::metadata(newest.join(f)).unwrap().len()
        );
    }
    assert!(!std::fs::read_dir(&folder).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".partial")
    }));

    let list = call(&d.client, "backup.list", json!({})).await;
    let backups = list["backups"].as_array().unwrap();
    assert_eq!(backups.len(), 2);
    assert_eq!(backups[0]["path"], infos[2]["path"]);
    let st = call(&d.client, "backup.status", json!({})).await;
    assert_eq!(st["last_backup"]["path"], infos[2]["path"]);
    assert!(st["last_error"].is_null() && st["running"] == false);
    let other = call(&d.client, "backup.list", json!({"folder": h.dir("empty")})).await;
    assert_eq!(other["backups"], json!([]));

    // A backup elsewhere (`--to`) does not rotate the configured folder.
    let elsewhere = h.dir("elsewhere");
    let info = call(&d.client, "backup.run", json!({"folder": elsewhere})).await;
    assert!(
        info["path"]
            .as_str()
            .unwrap()
            .starts_with(&*elsewhere.display().to_string())
    );
    assert_eq!(dirs_named(&folder, "nugget-backup-2").len(), 2);

    // A failure is recorded and broadcast.
    call(
        &d.client,
        "backup.configure",
        json!({"folder": h.root.join("missing")}),
    )
    .await;
    let e = call_err(&d.client, "backup.run", json!({})).await;
    assert!(e.message.contains("does not exist"), "{e:?}");
    let st = call(&d.client, "backup.status", json!({})).await;
    assert!(
        st["last_error"]
            .as_str()
            .unwrap()
            .contains("does not exist")
    );

    let mut changed = 0;
    while let Ok(Some(n)) = tokio::time::timeout(Duration::from_millis(500), notes.next()).await {
        if n.method == "backup.changed" {
            assert!(n.params["keep"].is_number());
            changed += 1;
        }
    }
    assert!(
        changed >= 8,
        "expected start and end notifications, got {changed}"
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn export_writes_every_file_with_counts() {
    let h = home("export");
    let out = h.dir("export");
    let d = start(&h.paths, KEY).await;
    seed(&d.store, 0, 7, "meridian");
    d.store
        .append_audit(&nugget_store::AuditEntry {
            actor: "test".into(),
            action: "test.export".into(),
            target: "store".into(),
            detail: json!({}),
        })
        .unwrap();
    let vault = d.vault.clone().expect("vault enabled");
    std::fs::create_dir_all(vault.join("notes")).unwrap();
    std::fs::write(vault.join("notes").join("mine.md"), "# Mine\n").unwrap();
    std::fs::create_dir_all(vault.join(".nugget")).unwrap();
    std::fs::write(vault.join(".nugget").join("state.json"), "{}").unwrap();
    let mut notes = d.client.notifications();

    let r = call(&d.client, "export.run", json!({"folder": out})).await;
    assert_eq!(r["items"], 7);
    assert_eq!(r["damaged_items"], 0);
    assert!(r["audit_entries"].as_u64().unwrap() >= 1);
    let dir = PathBuf::from(r["path"].as_str().unwrap());
    assert_eq!(dir.parent().unwrap(), out);
    assert!(
        dir.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("nugget-export-")
    );
    let lines: Vec<String> = std::fs::read_to_string(dir.join("items.jsonl"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 7);
    let first: Value = serde_json::from_str(&lines[0]).unwrap();
    assert!(first["text"].as_str().unwrap().contains("meridian"));
    let graph: Value =
        serde_json::from_slice(&std::fs::read(dir.join("graph.json")).unwrap()).unwrap();
    assert_eq!(graph["format"], 1);
    assert_eq!(
        graph["entities"].as_array().unwrap().len() as u64,
        r["entities"].as_u64().unwrap()
    );
    let audit = std::fs::read_to_string(dir.join("audit.csv")).unwrap();
    assert!(audit.starts_with("seq,at,actor,action,target,detail,prev_hash,hash"));
    assert_eq!(
        audit.lines().count() as u64 - 1,
        r["audit_entries"].as_u64().unwrap()
    );
    assert!(dir.join("egress.csv").is_file());
    assert!(
        std::fs::read_to_string(dir.join("README.md"))
            .unwrap()
            .contains("not encrypted")
    );
    assert!(dir.join("vault").join("notes").join("mine.md").is_file());
    assert!(
        !dir.join("vault").join(".nugget").exists(),
        "vault internals are not exported"
    );
    assert!(r["vault_files"].as_u64().unwrap() >= 1);

    let mut phases = Vec::new();
    while let Ok(Some(n)) = tokio::time::timeout(Duration::from_millis(500), notes.next()).await {
        if n.method == "export.progress" {
            phases.push(n.params["phase"].as_str().unwrap().to_string());
        }
    }
    for p in ["items", "graph", "audit", "egress", "vault"] {
        assert!(
            phases.iter().any(|x| x == p),
            "no {p} progress in {phases:?}"
        );
    }
    let e = call_err(&d.client, "export.run", json!({"folder": "relative/dir"})).await;
    assert_eq!(e.code, nugget_ipc::code::INVALID_PARAMS);
    d.stop().await;
}

// Unix only: the stopped in-process daemon keeps its database handles (its state lives on in
// the test process), which Windows does not let the restore rename. The real binary restores on
// every OS (`restore_into_a_fresh_home_with_the_passphrase`, `cli_backup_restore_and_export`,
// `golden_backup_restores_on_this_os`).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_on_the_same_home_restarts_and_keeps_the_old_data() {
    let h = home("same");
    let folder = h.dir("backups");
    write_envelope(&h.paths, KEY, PASS);
    let d = start(&h.paths, KEY).await;
    seed(&d.store, 0, 5, "saffron");
    call(&d.client, "backup.configure", json!({"folder": folder})).await;
    let info = call(&d.client, "backup.run", json!({})).await;
    // Changes after the backup: they go away with the restore (kept in the old database).
    seed(&d.store, 5, 4, "cobalt");
    assert_eq!(status(&d.client).await.items, 9);

    // Tampered: one flipped byte in a copy of store.nbk.
    let bad = folder.join("nugget-backup-20000101T000000Z");
    std::fs::create_dir(&bad).unwrap();
    let src = PathBuf::from(info["path"].as_str().unwrap());
    for f in ["store.nbk", "key.json", "manifest.json"] {
        std::fs::copy(src.join(f), bad.join(f)).unwrap();
    }
    let mut bytes = std::fs::read(bad.join("store.nbk")).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x40;
    std::fs::write(bad.join("store.nbk"), bytes).unwrap();
    let e = call_err(&d.client, "backup.restore", json!({"path": bad})).await;
    assert_eq!(e.code, nugget_ipc::code::INTEGRITY, "{e:?}");
    assert!(e.message.contains("checksum"), "{e:?}");
    // Not a backup at all.
    let e = call_err(
        &d.client,
        "backup.restore",
        json!({"path": h.dir("nothing")}),
    )
    .await;
    assert_eq!(e.code, nugget_ipc::code::NOT_FOUND, "{e:?}");
    let st = call(&d.client, "backup.status", json!({})).await;
    assert_eq!(st["restore_pending"], false);
    assert!(!h.paths.data.join("restore-pending").exists());
    assert_eq!(
        status(&d.client).await.items,
        9,
        "a failed restore changes nothing"
    );

    // The real one: same key, no passphrase needed. The daemon stops by itself.
    let r = call(&d.client, "backup.restore", json!({"path": info["path"]})).await;
    assert_eq!(r, json!({"staged": true, "restart_required": true}));
    assert_eq!(
        call(&d.client, "backup.status", json!({})).await["restore_pending"],
        true
    );
    d.exited().await;

    let d = start(&h.paths, KEY).await;
    assert_eq!(status(&d.client).await.items, 5);
    assert!(
        audit_ok(&d.client).await,
        "restored audit chain must verify"
    );
    let actions: Vec<String> = d
        .store
        .recent_audit(10)
        .unwrap()
        .into_iter()
        .map(|r| r.action)
        .collect();
    assert!(actions.iter().any(|a| a == "backup.restore"), "{actions:?}");
    let aside = data_files_starting(&h.paths, "nugget.db.pre-restore-");
    let main = aside
        .iter()
        .find(|p| !p.to_string_lossy().ends_with("-wal") && !p.to_string_lossy().ends_with("-shm"))
        .expect("old database kept");
    let old = Store::open(main, &StoreKey::from_bytes(KEY)).unwrap();
    assert_eq!(old.counts().unwrap().items, 9, "the old database is intact");
    drop(old);
    assert!(!h.paths.data.join("restore-pending").exists());
    let st = call(&d.client, "backup.status", json!({})).await;
    assert_eq!(st["restore_pending"], false);
    assert_eq!(st["passphrase_set"], true);
    assert!(
        data_files_starting_in(&h.paths.config, "config.toml.pre-restore-").len() == 1,
        "the previous config.toml is kept"
    );
    let hits = call(
        &d.client,
        method::SEARCH,
        json!({"query": "saffron", "limit": 10}),
    )
    .await;
    assert!(!hits.as_array().unwrap().is_empty(), "{hits}");
    d.stop().await;
}

fn data_files_starting_in(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_passphrase_restores_nothing() {
    let a = home("wp-a");
    let folder = a.dir("backups");
    write_envelope(&a.paths, KEY, PASS);
    let d = start(&a.paths, KEY).await;
    seed(&d.store, 0, 3, "umber");
    call(&d.client, "backup.configure", json!({"folder": folder})).await;
    let info = call(&d.client, "backup.run", json!({})).await;
    d.stop().await;

    // Another home with another key: the passphrase is needed, and a wrong one fails.
    let b = home("wp-b");
    let d = start(&b.paths, [4; 32]).await;
    let e = call_err(&d.client, "backup.restore", json!({"path": info["path"]})).await;
    assert_eq!(e.code, nugget_ipc::code::UNAUTHORIZED, "{e:?}");
    let e = call_err(
        &d.client,
        "backup.restore",
        json!({"path": info["path"], "passphrase": "not the passphrase"}),
    )
    .await;
    assert_eq!(e.code, nugget_ipc::code::INTEGRITY, "{e:?}");
    assert!(!b.paths.data.join("restore-pending").exists());
    assert!(data_files_starting_in(&b.paths.data, "nugget.db.pre-restore").is_empty());
    assert!(data_files_starting_in(&b.paths.data, "store.key.replaced").is_empty());
    // Still serving.
    assert_eq!(status(&d.client).await.items, 0);
    d.stop().await;
}

#[cfg(unix)]
fn side(db: &Path, suffix: &str) -> PathBuf {
    let mut p = db.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

#[cfg(unix)]
/// Replace `db` (and its WAL) by a checkpointed copy without side files.
fn checkpointed_copy_in_place(db: &Path, key: [u8; 32]) {
    let scratch = db.with_file_name("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let copy = scratch.join("nugget.db");
    std::fs::copy(db, &copy).unwrap();
    if side(db, "-wal").exists() {
        std::fs::copy(side(db, "-wal"), side(&copy, "-wal")).unwrap();
    }
    let store = Store::open(&copy, &StoreKey::from_bytes(key)).unwrap();
    store.flush_egress();
    drop(store);
    let wal = side(&copy, "-wal");
    assert!(
        !wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0,
        "closing the copy should checkpoint it"
    );
    std::fs::rename(&copy, db).unwrap();
    for s in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(side(db, s));
        let _ = std::fs::remove_file(side(&copy, s));
    }
    std::fs::remove_dir(&scratch).unwrap();
}

/// Overwrite everything after the first page with garbage: the key still opens the file, so it
/// reads as damaged rather than as another key's.
#[cfg(unix)]
fn corrupt(db: &Path) {
    let mut bytes = std::fs::read(db).unwrap();
    assert!(bytes.len() > 8192, "database too small to damage");
    for (i, b) in bytes.iter_mut().enumerate().skip(4096) {
        *b = (i as u8).wrapping_mul(31) ^ 0xA5;
    }
    std::fs::write(db, bytes).unwrap();
}

// Unix only, for the same reason as `restore_on_the_same_home_restarts_and_keeps_the_old_data`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_database_is_left_alone_and_restored_from_a_backup() {
    let h = home("corrupt");
    let folder = h.dir("backups");
    write_envelope(&h.paths, KEY, PASS);
    let d = start(&h.paths, KEY).await;
    seed(&d.store, 0, 40, "vermilion");
    call(&d.client, "backup.configure", json!({"folder": folder})).await;
    let info = call(&d.client, "backup.run", json!({})).await;
    d.stop().await;

    // The stopped in-process daemon still holds its connections (its state lives on in this
    // process), so fold the WAL into a clean copy first and put that in place: what a cleanly
    // closed database looks like on disk.
    let db = h.paths.db_file();
    checkpointed_copy_in_place(&db, KEY);
    corrupt(&db);
    let damaged = sha256(&db);

    let d = start(&h.paths, KEY).await;
    let st = call(&d.client, "backup.status", json!({})).await;
    let rec = &st["recovery"];
    assert!(rec.is_object(), "{st}");
    assert_eq!(rec["db_path"], db.display().to_string());
    assert!(!rec["reason"].as_str().unwrap().is_empty());
    assert_eq!(rec["latest_backup"]["path"], info["path"]);
    assert!(st["next_due"].is_null());
    assert_eq!(status(&d.client).await.items, 0, "an empty temporary store");
    assert!(d.vault.is_none(), "no vault in recovery mode");
    // No source kinds: nothing can reuse a real source's id (and its keychain credentials).
    let e = call_err(
        &d.client,
        method::SOURCES_ADD,
        json!({"kind": "files", "name": "x", "settings": {"roots": [h.root], "exclude": []}}),
    )
    .await;
    assert_ne!(e.code, 0, "{e:?}");
    let e = call_err(&d.client, "backup.run", json!({})).await;
    assert!(e.message.contains("damaged"), "{e:?}");
    let e = call_err(&d.client, "export.run", json!({"folder": h.dir("x")})).await;
    assert!(e.message.contains("damaged"), "{e:?}");
    assert_eq!(sha256(&db), damaged, "the damaged file must not be written");

    call(&d.client, "backup.restore", json!({"path": info["path"]})).await;
    d.exited().await;
    assert_eq!(sha256(&db), damaged, "still untouched until the restart");

    let d = start(&h.paths, KEY).await;
    assert_eq!(status(&d.client).await.items, 40);
    assert!(audit_ok(&d.client).await);
    let st = call(&d.client, "backup.status", json!({})).await;
    assert!(st["recovery"].is_null());
    let aside = data_files_starting(&h.paths, "nugget.db.corrupt-");
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(
        sha256(&aside[0]),
        damaged,
        "the damaged file is kept byte for byte"
    );
    d.stop().await;
}

// Unix only, for the same reason as `restore_on_the_same_home_restarts_and_keeps_the_old_data`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_database_the_key_does_not_open_starts_in_recovery_mode() {
    let h = home("wrongkey");
    let d = start(&h.paths, KEY).await;
    seed(&d.store, 0, 10, "ochre");
    d.stop().await;
    let db = h.paths.db_file();
    checkpointed_copy_in_place(&db, KEY);
    // Damage the first page: the key no longer "opens" the file.
    let mut bytes = std::fs::read(&db).unwrap();
    for b in bytes.iter_mut().take(4096) {
        *b ^= 0x5A;
    }
    std::fs::write(&db, bytes).unwrap();
    let before = sha256(&db);
    let d = start(&h.paths, KEY).await;
    let st = call(&d.client, "backup.status", json!({})).await;
    assert!(
        st["recovery"]["reason"]
            .as_str()
            .unwrap()
            .contains("store key"),
        "{st}"
    );
    assert_eq!(status(&d.client).await.items, 0);
    assert_eq!(sha256(&db), before, "never written");
    d.stop().await;
    assert_eq!(sha256(&db), before);
}

// ---- the real binary ----

fn nuggetd(home: &Path) -> Command {
    let mut c = Command::new(EXE);
    c.env("NUGGET_HOME", home)
        .env("NUGGET_KEYCHAIN", "off")
        .env_remove("NUGGET_BACKUP_PASSPHRASE")
        .env_remove("NUGGET_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null());
    c
}

fn run(home: &Path, args: &[&str]) -> Output {
    nuggetd(home).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "failed: {}\n{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    text(&out.stdout)
}

struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Proc {
    async fn spawn(home: &Path, env: &[(&str, &str)]) -> (Self, Client) {
        let mut cmd = nuggetd(home);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut p = Proc(Some(child));
        let paths = Paths::rooted(home.to_path_buf());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(c) = Client::connect(&paths).await
                && c.call_value(method::STATUS, json!({})).await.is_ok()
            {
                return (p, c);
            }
            if let Some(s) = p.0.as_mut().unwrap().try_wait().unwrap() {
                panic!("daemon exited early with {s}");
            }
            assert!(Instant::now() < deadline, "daemon never answered");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait for the daemon to exit by itself.
    async fn exited(mut self) {
        let mut c = self.0.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(s) = c.try_wait().unwrap() {
                assert!(s.success(), "daemon exited with {s}");
                return;
            }
            assert!(Instant::now() < deadline, "daemon did not exit by itself");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Stop the daemon (SIGTERM on Unix, so it shuts down cleanly).
    async fn stop(mut self) {
        let mut c = self.0.take().unwrap();
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .args(["-TERM", &c.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(30);
            while c.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "daemon did not stop");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A backup made in-process (key [`KEY`], passphrase [`PASS`]) of `n` items about `word`.
async fn make_backup(h: &Home, n: usize, word: &str) -> PathBuf {
    let folder = h.dir("backups");
    write_envelope(&h.paths, KEY, PASS);
    let d = start(&h.paths, KEY).await;
    seed(&d.store, 0, n, word);
    call(&d.client, "backup.configure", json!({"folder": folder})).await;
    let info = call(&d.client, "backup.run", json!({})).await;
    d.stop().await;
    PathBuf::from(info["path"].as_str().unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_into_a_fresh_home_with_the_passphrase() {
    let a = home("fresh-a");
    let backup = make_backup(&a, 6, "juniper").await;

    // A new computer: its own new key (a key file), an empty store.
    let b = home("fresh-b");
    let bhome = b.paths.data.parent().unwrap().to_path_buf();
    let (p, client) = Proc::spawn(&bhome, &[]).await;
    assert_eq!(status(&client).await.items, 0);
    let r = call(
        &client,
        "backup.restore",
        json!({"path": backup, "passphrase": PASS}),
    )
    .await;
    assert_eq!(r["staged"], true);
    drop(client);
    p.exited().await;

    let (p, client) = Proc::spawn(&bhome, &[]).await;
    assert_eq!(status(&client).await.items, 6);
    assert!(audit_ok(&client).await);
    let hits = call(
        &client,
        method::SEARCH,
        json!({"query": "juniper", "limit": 10}),
    )
    .await;
    assert!(!hits.as_array().unwrap().is_empty(), "{hits}");
    let st = call(&client, "backup.status", json!({})).await;
    assert_eq!(
        st["passphrase_set"], true,
        "the backup's envelope seals the new key"
    );
    assert_eq!(st["restore_pending"], false);
    drop(client);
    p.stop().await;
    assert!(!data_files_starting(&b.paths, "nugget.db.pre-restore-").is_empty());
    assert_eq!(
        data_files_starting(&b.paths, "store.key.replaced-").len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_backups_fire_on_their_own() {
    let h = home("sched");
    let folder = h.dir("backups");
    let home_dir = h.paths.data.parent().unwrap().to_path_buf();
    // One schedule hour is one second.
    let (p, client) = Proc::spawn(&home_dir, &[("NUGGET_BACKUP_TEST_HOUR_SECS", "1")]).await;
    let pass = h.root.join("pass.txt");
    std::fs::write(&pass, format!("{PASS}\n")).unwrap();
    // Through the running daemon.
    ok(run(
        &home_dir,
        &[
            "backup",
            "set-passphrase",
            "--passphrase-file",
            pass.to_str().unwrap(),
        ],
    ));
    call(
        &client,
        "backup.configure",
        json!({"folder": folder, "interval_hours": 1, "keep": 3}),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let n = dirs_named(&folder, "nugget-backup-").len();
        if n >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "no scheduled backups ({n})");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let st = call(&client, "backup.status", json!({})).await;
    assert!(st["last_backup"].is_object());
    assert!(st["next_due"].is_string());
    // Manual only: no more.
    call(&client, "backup.configure", json!({"interval_hours": 0})).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let n = dirs_named(&folder, "nugget-backup-").len();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(dirs_named(&folder, "nugget-backup-").len(), n);
    assert!(n <= 3, "retention keeps 3");
    assert!(call(&client, "backup.status", json!({})).await["next_due"].is_null());
    drop(client);
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_backup_restore_and_export() {
    let h = home("cli");
    let home_dir = h.paths.data.parent().unwrap().to_path_buf();
    let corpus = h.dir("corpus");
    for i in 0..4 {
        std::fs::write(
            corpus.join(format!("n{i}.md")),
            format!("# Note {i}\n\nThe quillwort survey, part {i}.\n"),
        )
        .unwrap();
    }
    let pass = h.root.join("pass.txt");
    std::fs::write(&pass, PASS).unwrap();
    let (p, client) = Proc::spawn(&home_dir, &[]).await;
    call(
        &client,
        method::SOURCES_ADD,
        json!({"kind": "files", "name": "corpus", "settings": {"roots": [corpus], "exclude": []}}),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(60);
    while status(&client).await.items < 4 {
        assert!(Instant::now() < deadline, "corpus never indexed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Through IPC.
    ok(run(
        &home_dir,
        &[
            "backup",
            "set-passphrase",
            "--passphrase-file",
            pass.to_str().unwrap(),
        ],
    ));
    let via_ipc = h.dir("via-ipc");
    let out = ok(run(
        &home_dir,
        &["backup", "--to", via_ipc.to_str().unwrap()],
    ));
    assert!(out.contains("backed up 4 items"), "{out}");
    let first = dirs_named(&via_ipc, "nugget-backup-");
    assert_eq!(first.len(), 1);
    // Restore refuses under a running daemon.
    let refused = run(
        &home_dir,
        &["restore", "--from", first[0].to_str().unwrap()],
    );
    assert!(!refused.status.success());
    assert!(
        text(&refused.stderr).contains("running"),
        "{}",
        text(&refused.stderr)
    );
    drop(client);
    p.stop().await;

    // Offline.
    let offline = h.dir("offline");
    let out = ok(run(
        &home_dir,
        &["backup", "--to", offline.to_str().unwrap()],
    ));
    assert!(out.contains("backed up 4 items"), "{out}");
    let exp = h.dir("exports");
    let out = ok(run(&home_dir, &["export", "--to", exp.to_str().unwrap()]));
    assert!(out.contains("exported 4 items"), "{out}");
    let exported: Vec<_> = std::fs::read_dir(&exp).unwrap().collect();
    assert_eq!(exported.len(), 1);
    let no_folder = run(&home_dir, &["backup"]);
    assert!(!no_folder.status.success(), "no folder configured");

    // Offline restore into a fresh home (passphrase from a file), then the daemon serves it.
    let fresh = home("cli-fresh");
    let fresh_dir = fresh.paths.data.parent().unwrap().to_path_buf();
    let no_pass = run(
        &fresh_dir,
        &["restore", "--from", first[0].to_str().unwrap()],
    );
    assert!(!no_pass.status.success());
    assert!(
        text(&no_pass.stderr).contains("passphrase"),
        "{}",
        text(&no_pass.stderr)
    );
    let out = ok(run(
        &fresh_dir,
        &[
            "restore",
            "--from",
            first[0].to_str().unwrap(),
            "--passphrase-file",
            pass.to_str().unwrap(),
        ],
    ));
    assert!(out.contains("4 items"), "{out}");
    let (p, client) = Proc::spawn(&fresh_dir, &[]).await;
    assert_eq!(status(&client).await.items, 4);
    assert!(audit_ok(&client).await);
    drop(client);
    p.stop().await;

    // And over the original home (same key: no passphrase), old data kept aside.
    let out = ok(run(
        &home_dir,
        &["restore", "--from", first[0].to_str().unwrap()],
    ));
    assert!(out.contains("kept as"), "{out}");
    assert!(!data_files_starting(&h.paths, "nugget.db.pre-restore-").is_empty());
}

/// A backup (key [`KEY`], passphrase [`PASS`]) of `n` items with two vault pages, whose
/// `config.toml` sets `vault_path` when given (written after the daemon started, as if edited).
async fn make_backup_with_vault(h: &Home, n: usize, vault_path: Option<&str>) -> PathBuf {
    let folder = h.dir("backups");
    write_envelope(&h.paths, KEY, PASS);
    let d = start(&h.paths, KEY).await;
    seed(&d.store, 0, n, "tamarind");
    let vault = d.vault.clone().unwrap();
    std::fs::create_dir_all(vault.join("people")).unwrap();
    std::fs::write(vault.join("people").join("Sam Lee.md"), "# Sam Lee\n").unwrap();
    std::fs::write(vault.join("Index.md"), "# Index\n").unwrap();
    if let Some(p) = vault_path {
        let text = format!("{CONFIG}\n[vault]\npath = {}\n", toml_string(p));
        std::fs::write(h.paths.config_file(), text).unwrap();
    }
    call(&d.client, "backup.configure", json!({"folder": folder})).await;
    let info = call(&d.client, "backup.run", json!({})).await;
    d.stop().await;
    PathBuf::from(info["path"].as_str().unwrap())
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The restored home serves the backup: items, vault pages, passphrase, nothing pending.
async fn check_restored(paths: &Paths, items: u64) {
    let home_dir = paths.data.parent().unwrap().to_path_buf();
    let (p, client) = Proc::spawn(&home_dir, &[]).await;
    assert_eq!(
        status(&client).await.items,
        items,
        "{}",
        paths.data.display()
    );
    assert!(audit_ok(&client).await);
    let st = call(&client, "backup.status", json!({})).await;
    assert_eq!(st["restore_pending"], false);
    assert_eq!(st["passphrase_set"], true, "{st}");
    assert!(st["recovery"].is_null());
    drop(client);
    p.stop().await;
    assert!(!paths.data.join("restore-pending").exists());
    let vault = paths.data.join("vault");
    assert!(
        vault.join("people").join("Sam Lee.md").is_file(),
        "vault restored"
    );
    assert!(vault.join("Index.md").is_file());
}

/// A crash after any step of applying a restore is finished by the next start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupted_restore_is_finished_at_the_next_start() {
    let a = home("crash-src");
    let backup = make_backup_with_vault(&a, 4, None).await;
    let pass = a.root.join("pass.txt");
    std::fs::write(&pass, PASS).unwrap();
    let restore_args = |b: &Path| {
        vec![
            "restore".to_string(),
            "--from".into(),
            b.display().to_string(),
            "--passphrase-file".into(),
            pass.display().to_string(),
        ]
    };
    // An empty home (no key): the key is installed first.
    for step in [
        "key-first",
        "db",
        "config",
        "vault-copied",
        "vault",
        "envelope",
    ] {
        let h = home(&format!("crash-{step}"));
        let home_dir = h.paths.data.parent().unwrap().to_path_buf();
        let args = restore_args(&backup);
        let out = nuggetd(&home_dir)
            .env("NUGGET_RESTORE_TEST_STOP_AFTER", step)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "{step}: the injected crash should fail the command"
        );
        assert!(
            text(&out.stderr).contains(step),
            "{step}: {}",
            text(&out.stderr)
        );
        check_restored(&h.paths, 4).await;
    }
    // A home with its own key and data: the key is installed after the database moved in.
    for step in ["db", "key"] {
        let h = home(&format!("crash-keyed-{step}"));
        let home_dir = h.paths.data.parent().unwrap().to_path_buf();
        let (p, client) = Proc::spawn(&home_dir, &[]).await;
        drop(client);
        p.stop().await;
        let args = restore_args(&backup);
        let out = nuggetd(&home_dir)
            .env("NUGGET_RESTORE_TEST_STOP_AFTER", step)
            .args(&args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{step}");
        assert!(
            text(&out.stderr).contains(step),
            "{step}: {}",
            text(&out.stderr)
        );
        // Its vault directory is still empty (nothing to render yet), so the backup's goes in.
        check_restored(&h.paths, 4).await;
        assert!(!data_files_starting(&h.paths, "nugget.db.pre-restore-").is_empty());
        assert_eq!(
            data_files_starting(&h.paths, "store.key.replaced-").len(),
            1
        );
    }
}

/// A config from another OS (its vault path is not a path here) is made to work, not refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_config_from_another_os_is_localised() {
    let foreign = if cfg!(windows) {
        "/Users/sam/Nugget Vault"
    } else {
        "C:\\Users\\sam\\Nugget Vault"
    };
    let a = home("foreign-src");
    let backup = make_backup_with_vault(&a, 3, Some(foreign)).await;
    let pass = a.root.join("pass.txt");
    std::fs::write(&pass, PASS).unwrap();
    let h = home("foreign");
    let home_dir = h.paths.data.parent().unwrap().to_path_buf();
    ok(run(
        &home_dir,
        &[
            "restore",
            "--from",
            backup.to_str().unwrap(),
            "--passphrase-file",
            pass.to_str().unwrap(),
        ],
    ));
    let config = std::fs::read_to_string(h.paths.config_file()).unwrap();
    assert!(config.contains("# path = "), "{config}");
    check_restored(&h.paths, 3).await;
}

// ---- golden fixture ----

fn fixture_dir() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("backup-v1");
    let mut dirs = dirs_named(&root, "nugget-backup-");
    assert_eq!(dirs.len(), 1, "one backup in {}", root.display());
    dirs.remove(0)
}

/// A backup made by an earlier build on Linux restores here, whatever the OS.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn golden_backup_restores_on_this_os() {
    let src = fixture_dir();
    let h = home("golden");
    let home_dir = h.paths.data.parent().unwrap().to_path_buf();
    // Copy it so a read-only checkout is fine.
    let copy = h.dir("fixture").join(src.file_name().unwrap());
    std::fs::create_dir(&copy).unwrap();
    for f in ["store.nbk", "key.json", "manifest.json"] {
        std::fs::copy(src.join(f), copy.join(f)).unwrap();
    }
    let pass = h.root.join("pass.txt");
    std::fs::write(&pass, FIXTURE_PASS).unwrap();
    let out = ok(run(
        &home_dir,
        &[
            "restore",
            "--from",
            copy.to_str().unwrap(),
            "--passphrase-file",
            pass.to_str().unwrap(),
        ],
    ));
    assert!(out.contains(&format!("{FIXTURE_ITEMS} items")), "{out}");
    let store = Store::open(h.paths.db_file(), &StoreKey::from_bytes(FIXTURE_KEY)).unwrap();
    assert_eq!(store.counts().unwrap().items, FIXTURE_ITEMS as u64);
    store.verify_audit_chain().unwrap();
    let report = store.check_consistency().unwrap();
    assert!(report.ok, "{:?}", report.problems);
    let actions: Vec<String> = store
        .recent_audit(10)
        .unwrap()
        .into_iter()
        .map(|r| r.action)
        .collect();
    assert!(actions.iter().any(|a| a == "backup.restore"));
    drop(store);
    assert!(h.paths.config_file().is_file(), "config restored");
}

/// Regenerate the golden fixture: `NUGGET_WRITE_BACKUP_FIXTURE=1 cargo test -p nuggetd --test
/// backup -- --ignored write_golden_fixture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "writes tests/fixtures/backup-v1"]
async fn write_golden_fixture() {
    if std::env::var_os("NUGGET_WRITE_BACKUP_FIXTURE").is_none() {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("backup-v1");
    std::fs::create_dir_all(&root).unwrap();
    for d in dirs_named(&root, "nugget-backup-") {
        std::fs::remove_dir_all(d).unwrap();
    }
    let h = home("write-golden");
    write_envelope(&h.paths, FIXTURE_KEY, FIXTURE_PASS);
    let d = start(&h.paths, FIXTURE_KEY).await;
    seed(&d.store, 0, FIXTURE_ITEMS, "zephyrine");
    d.store
        .append_audit(&nugget_store::AuditEntry {
            actor: "test".into(),
            action: "fixture.made".into(),
            target: "store".into(),
            detail: json!({}),
        })
        .unwrap();
    call(&d.client, "backup.run", json!({"folder": root})).await;
    d.stop().await;
}
