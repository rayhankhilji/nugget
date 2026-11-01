//! Conformance (`nugget_connector::testkit`) on real temp directories: odd names and resumption
//! after a crash at any send or checkpoint.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use nugget_connector::testkit::{
    MemSecrets, Replica, ReplicaSink, UNICODE_SAMPLES, VecEgress, assert_crash_resumable, context,
    run_pass,
};
use nugget_connector::{Connector, ConnectorFactory, Content, KnownItems};
use nugget_connector_files::{FilesFactory, item_key};
use nugget_core::paths::Paths;
use serde_json::{Value, json};

use crate::common::{path, root_in, write};

/// A connector over `replica` (as the store's known versions).
struct Env {
    home: tempfile::TempDir,
    egress: Arc<VecEgress>,
}

impl Env {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            egress: Arc::new(VecEgress::default()),
        }
    }

    fn build(&self, settings: &Value, replica: Arc<Replica>) -> Arc<dyn Connector> {
        let ctx = context(
            "files:conformance",
            Paths::rooted(self.home.path().to_path_buf()),
            self.egress.clone(),
            Arc::new(MemSecrets::default()),
            replica as Arc<dyn KnownItems>,
        );
        FilesFactory.build(settings, ctx).unwrap()
    }
}

async fn pass(
    conn: &dyn Connector,
    cursor: Option<Value>,
    replica: &Arc<Replica>,
) -> (Value, ReplicaSink) {
    let sink = ReplicaSink::new(replica.clone());
    let c = run_pass(conn, cursor, &sink).await.expect("sync pass");
    (c, sink)
}

#[tokio::test]
async fn unicode_and_odd_file_names() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "uni 名前");
    let nfc = "Caf\u{e9}.md"; // é precomposed
    let nfd = "Cafe\u{301}.md"; // e + combining acute
    let mut names: Vec<String> = vec![
        nfc.into(),
        "with  two  spaces .md".into(),
        "emoji 👩‍👩‍👧‍👦 🇯🇵.md".into(),
        "日本語/ノート.md".into(),
        "مرحبا.txt".into(),
        "semi;colon#hash%percent&amp.md".into(),
        "leading-dash -x.md".into(),
        "trailing.dots..md".into(),
    ];
    // NFC and NFD are distinct names on most Linux filesystems; macOS and Windows may fold them
    // into one file. Either way the keys must match what is on disk.
    let fold = {
        write(&root, nfc, "nfc");
        std::fs::metadata(path(&root, nfd)).is_ok()
    };
    if !fold {
        names.push(nfd.into());
    }
    for (i, n) in names.iter().enumerate() {
        write(
            &root,
            n,
            &format!("{} {i}", UNICODE_SAMPLES[i % UNICODE_SAMPLES.len()]),
        );
    }
    let env = Env::new();
    let settings = json!({ "roots": [root] });
    let replica = Replica::new();
    let conn = env.build(&settings, replica.clone());
    let (cursor, sink) = pass(&*conn, None, &replica).await;
    assert_eq!(replica.len(), names.len(), "{:?}", replica.keys());
    for d in sink.upserts() {
        // Every key round-trips to the file it names, and the URL is a valid file URL.
        let Content::File(p) = &d.content else {
            panic!("content by path")
        };
        assert!(p.is_file(), "{}", p.display());
        let rel = p.strip_prefix(&root).unwrap();
        assert_eq!(d.key.0, item_key(0, rel));
        let url = url::Url::parse(d.url.as_deref().unwrap()).unwrap();
        assert_eq!(url.to_file_path().unwrap(), *p);
    }
    // Stable: names are not re-normalised between passes (no churn).
    let (_, quiet) = pass(&*conn, Some(cursor), &replica).await;
    assert_eq!(quiet.sends(), 0, "{:?}", quiet.summary());
}

/// Builds the same tree for every crash run (the matrix needs identical input each time).
fn crash_tree(root: &Path) {
    for i in 0..12 {
        write(root, &format!("n{}/f{i}.md", i % 3), &format!("note {i}"));
    }
    write(root, "emoji 🎉.md", "party");
}

#[tokio::test]
async fn crash_at_every_send_and_checkpoint_resumes_to_the_same_index() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "crash");
    crash_tree(&root);
    let env = Arc::new(Env::new());
    let settings = json!({ "roots": [root] });

    // First sync from nothing.
    let build = {
        let (env, settings) = (env.clone(), settings.clone());
        move |r: Arc<Replica>| {
            let c = env.build(&settings, r);
            async move { c }
        }
    };
    let report = assert_crash_resumable(&build, None, BTreeMap::new()).await;
    assert_eq!(report.sends, 13);

    // An incremental pass with edits, moves and deletes since the last commit.
    let replica = Replica::new();
    let conn = env.build(&settings, replica.clone());
    let (cursor, _) = pass(&*conn, None, &replica).await;
    std::fs::write(path(&root, "n0/f0.md"), "edited").unwrap();
    std::fs::rename(path(&root, "n1/f1.md"), path(&root, "n2/f1-moved.md")).unwrap();
    std::fs::remove_file(path(&root, "n2/f2.md")).unwrap();
    let report = assert_crash_resumable(&build, Some(cursor), replica.snapshot()).await;
    assert_eq!(report.sends, 4, "edit + move (delete and upsert) + delete");
}

/// Non-UTF-8 names (possible on Linux) must neither collide nor churn.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn regression_non_utf8_file_names_do_not_collide() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "bytes");
    for raw in [&b"bad\xff.md"[..], b"bad\xfe.md", b"bad\xef\xbf\xbd.md"] {
        std::fs::write(root.join(OsStr::from_bytes(raw)), raw).unwrap();
    }
    let env = Env::new();
    let settings = json!({ "roots": [root] });
    let replica = Replica::new();
    let conn = env.build(&settings, replica.clone());
    let (cursor, _) = pass(&*conn, None, &replica).await;
    assert_eq!(replica.len(), 3, "{:?}", replica.keys());
    let (_, quiet) = pass(&*conn, Some(cursor), &replica).await;
    assert_eq!(quiet.sends(), 0, "{:?}", quiet.summary());
}
