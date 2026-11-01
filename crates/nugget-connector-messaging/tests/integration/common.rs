//! Shared helpers: build connectors over a testkit `Replica`, run passes, copy fixtures.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nugget_connector::testkit::{MemSecrets, Replica, ReplicaSink, VecEgress, context, run_pass};
use nugget_connector::{Change, Connector, ConnectorFactory, Content, Document, KnownItems};
use nugget_connector_messaging::{IMessageFactory, TelegramFactory, WhatsAppFactory};
use nugget_core::paths::Paths;
use serde_json::Value;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    IMessage,
    WhatsApp,
    Telegram,
}

/// A temp home for the connector's cache dir.
pub struct Env {
    pub home: tempfile::TempDir,
    pub egress: Arc<VecEgress>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            egress: Arc::new(VecEgress::default()),
        }
    }

    pub fn build(&self, kind: Kind, settings: &Value, replica: Arc<Replica>) -> Arc<dyn Connector> {
        let ctx = context(
            "messaging:test",
            Paths::rooted(self.home.path().to_path_buf()),
            self.egress.clone(),
            Arc::new(MemSecrets::default()),
            replica as Arc<dyn KnownItems>,
        );
        match kind {
            Kind::IMessage => IMessageFactory.build(settings, ctx),
            Kind::WhatsApp => WhatsAppFactory.build(settings, ctx),
            Kind::Telegram => TelegramFactory.build(settings, ctx),
        }
        .expect("build connector")
    }

    /// One pass over `replica`; returns the sink (changes, checkpoints).
    pub async fn pass(&self, kind: Kind, settings: &Value, replica: &Arc<Replica>) -> ReplicaSink {
        let conn = self.build(kind, settings, replica.clone());
        let sink = ReplicaSink::new(replica.clone());
        run_pass(&*conn, None, &sink).await.expect("sync pass");
        assert!(
            self.egress.events().is_empty(),
            "local connectors never touch the network"
        );
        sink
    }

    /// The scratch dir the iMessage reader copies databases into.
    pub fn scratch(&self) -> PathBuf {
        self.home.path().join("cache").join("messaging")
    }
}

pub fn fixtures(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(sub)
}

/// Copy a fixture folder (recursively) into `dest`.
pub fn copy_dir(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dest.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

pub fn keys(replica: &Replica) -> BTreeSet<String> {
    replica.keys().into_iter().collect()
}

pub fn set(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

pub fn upserts(sink: &ReplicaSink) -> Vec<Document> {
    sink.upserts()
}

pub fn doc(sink: &ReplicaSink, key: &str) -> Document {
    sink.upserts()
        .into_iter()
        .find(|d| d.key.0 == key)
        .unwrap_or_else(|| panic!("no document {key}; got {:?}", sink.summary()))
}

pub fn text(d: &Document) -> &str {
    match &d.content {
        Content::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

pub fn deletes(sink: &ReplicaSink) -> BTreeSet<String> {
    sink.changes
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            Change::Delete(k) => Some(k.0.clone()),
            Change::Upsert(_) => None,
        })
        .collect()
}

/// Check the invariants every rendered document must hold.
pub fn assert_well_formed(d: &Document) {
    let t = text(d);
    assert!(t.starts_with("---\ntype: conversation\n"), "{t}");
    assert_eq!(t.matches("\n---\n").count(), 1, "one frontmatter end: {t}");
    let body = t.split_once("\n---\n").unwrap().1;
    let mut lines = body.lines();
    assert!(lines.next().unwrap().starts_with("# "), "{body}");
    for l in lines {
        assert!(
            l.is_empty()
                || l == ">"
                || l.starts_with("> ")
                || l.starts_with("**")
                || l.starts_with('_'),
            "unquoted line {l:?} in {}",
            d.key
        );
    }
    assert!(d.version.as_deref().is_some_and(|v| v.len() == 64));
    assert!(d.title.as_deref().is_some_and(|t| t.contains(" — ")));
}

pub fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}
