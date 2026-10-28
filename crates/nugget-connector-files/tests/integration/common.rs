//! Fakes and helpers: a recording `ChangeSink` that also plays the store (its applied versions
//! back `KnownItems`), and filesystem helpers.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nugget_connector::{
    CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory, Document,
    KnownItems, SecretStore,
};
use nugget_connector_files::{FilesFactory, ObsidianFactory};
use nugget_core::Result;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use serde_json::Value;
use tokio::task::JoinHandle;

/// Records every change and checkpoint, and applies changes to an in-memory "store".
#[derive(Default)]
pub struct Fake {
    pub changes: Mutex<Vec<Change>>,
    pub checkpoints: Mutex<Vec<Value>>,
    pub store: Mutex<HashMap<ItemKey, Option<String>>>,
}

#[async_trait]
impl ChangeSink for Fake {
    async fn send(&self, change: Change) -> Result<()> {
        {
            let mut store = self.store.lock().unwrap();
            match &change {
                Change::Upsert(d) => {
                    store.insert(d.key.clone(), d.version.clone());
                }
                Change::Delete(k) => {
                    store.remove(k);
                }
            }
        }
        self.changes.lock().unwrap().push(change);
        Ok(())
    }

    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        self.checkpoints.lock().unwrap().push(cursor);
        Ok(())
    }
}

#[async_trait]
impl KnownItems for Fake {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>> {
        Ok(self.store.lock().unwrap().clone())
    }
}

struct NoEgress;
impl EgressSink for NoEgress {
    fn record(&self, _: EgressEvent) {
        panic!("local connectors must not record egress");
    }
}

struct NoSecrets;
impl SecretStore for NoSecrets {
    fn get(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub enum Kind {
    Files,
    Obsidian,
}

/// One source under test: a temp home, the fake pipeline, a connector and its cursor.
pub struct Harness {
    pub home: tempfile::TempDir,
    pub fake: Arc<Fake>,
    pub conn: Arc<dyn Connector>,
    pub cursor: Option<Value>,
    kind: Kind,
}

impl Harness {
    pub fn new(kind: Kind, settings: Value) -> Self {
        let home = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let conn = build(kind, &settings, &home, &fake);
        Self {
            home,
            fake,
            conn,
            cursor: None,
            kind,
        }
    }

    pub fn files(settings: Value) -> Self {
        Self::new(Kind::Files, settings)
    }

    /// A fresh connector instance over the same store (a daemon restart).
    pub fn restart(&mut self, settings: Value) {
        self.conn = build(self.kind, &settings, &self.home, &self.fake);
    }

    /// Run one sync pass and return the changes it sent.
    pub async fn sync(&mut self) -> Vec<Change> {
        self.fake.changes.lock().unwrap().clear();
        let cursor = self
            .conn
            .sync(
                self.cursor.clone(),
                self.fake.as_ref(),
                &CancellationToken::new(),
            )
            .await
            .expect("sync");
        self.cursor = Some(cursor);
        self.take()
    }

    pub fn take(&self) -> Vec<Change> {
        std::mem::take(&mut *self.fake.changes.lock().unwrap())
    }

    pub fn snapshot(&self) -> Vec<Change> {
        self.fake.changes.lock().unwrap().clone()
    }

    pub fn store_keys(&self) -> BTreeSet<String> {
        self.fake
            .store
            .lock()
            .unwrap()
            .keys()
            .map(|k| k.0.clone())
            .collect()
    }

    pub fn start_watch(&self) -> (CancellationToken, JoinHandle<Result<()>>) {
        let cancel = CancellationToken::new();
        let conn = Arc::clone(&self.conn);
        let sink: Arc<dyn ChangeSink> = self.fake.clone();
        let c = cancel.clone();
        let handle = tokio::spawn(async move { conn.watch(sink, c).await });
        (cancel, handle)
    }

    /// Wait until the watcher delivers live events: keep touching a sentinel file until its
    /// upsert shows up, then clear the recorded changes.
    pub async fn wait_watching(&self, root: &Path, ext: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let name = format!("zz-sentinel.{ext}");
        let key = format!("0:{name}");
        let mut n = 0u32;
        loop {
            n += 1;
            write(root, &name, &"x".repeat(n as usize));
            let until = Instant::now() + Duration::from_millis(700);
            while Instant::now() < until {
                if upserts(&self.snapshot()).contains(&key) {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    self.take();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert!(Instant::now() < deadline, "watcher never became live");
        }
    }

    /// Poll the recorded changes until `pred` holds (generous deadline for slow CI).
    pub async fn wait_for(&self, what: &str, pred: impl Fn(&[Change]) -> bool) -> Vec<Change> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let snap = self.snapshot();
            if pred(&snap) {
                return snap;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; got {:#?}",
                summary(&snap)
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

fn build(
    kind: Kind,
    settings: &Value,
    home: &tempfile::TempDir,
    fake: &Arc<Fake>,
) -> Arc<dyn Connector> {
    let ctx = ConnectorContext {
        source: SourceId::new("files:test"),
        paths: Paths::rooted(home.path().to_path_buf()),
        egress: Arc::new(NoEgress),
        secrets: Arc::new(NoSecrets),
        known: fake.clone(),
    };
    match kind {
        Kind::Files => FilesFactory.build(settings, ctx),
        Kind::Obsidian => ObsidianFactory.build(settings, ctx),
    }
    .expect("build connector")
}

pub fn write(root: &Path, rel: &str, content: &str) -> PathBuf {
    let p = rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
    p
}

pub fn append(root: &Path, rel: &str, content: &str) {
    use std::io::Write;
    let p = path(root, rel);
    let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

pub fn path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// The version the connector should report for a file right now.
pub fn version(p: &Path) -> String {
    let md = std::fs::metadata(p).unwrap();
    let ns = md
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{ns}:{}", md.len())
}

pub fn upserts(changes: &[Change]) -> BTreeSet<String> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::Upsert(d) => Some(d.key.0.clone()),
            Change::Delete(_) => None,
        })
        .collect()
}

pub fn deletes(changes: &[Change]) -> BTreeSet<String> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::Delete(k) => Some(k.0.clone()),
            Change::Upsert(_) => None,
        })
        .collect()
}

pub fn docs(changes: &[Change]) -> BTreeMap<String, Document> {
    changes
        .iter()
        .filter_map(|c| match c {
            Change::Upsert(d) => Some((d.key.0.clone(), d.clone())),
            Change::Delete(_) => None,
        })
        .collect()
}

pub fn summary(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .map(|c| match c {
            Change::Upsert(d) => format!("+ {} {:?}", d.key, d.version),
            Change::Delete(k) => format!("- {k}"),
        })
        .collect()
}

pub fn set<const N: usize>(keys: [&str; N]) -> BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

/// A root folder inside a temp dir (not itself hidden, unlike `tempfile`'s `.tmpXXXX`).
pub fn root_in(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    let p = dir.path().join(name);
    std::fs::create_dir_all(&p).unwrap();
    p
}
