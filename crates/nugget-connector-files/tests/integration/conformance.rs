//! Conformance: the shared robustness properties (`nugget_connector::testkit`) on real temp
//! directories: many files, renames and moves, deletes, edits, size caps, odd names, unreadable
//! files, and resumption after a crash at any send or checkpoint.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nugget_connector::testkit::{
    MemSecrets, Replica, ReplicaSink, UNICODE_SAMPLES, VecEgress, assert_crash_resumable, context,
    run_pass,
};
use nugget_connector::{Connector, ConnectorFactory, Content, KnownItems};
use nugget_connector_files::{CHECKPOINT_EVERY, FilesFactory, ObsidianFactory, item_key};
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

    fn build(&self, obsidian: bool, settings: &Value, replica: Arc<Replica>) -> Arc<dyn Connector> {
        let ctx = context(
            "files:conformance",
            Paths::rooted(self.home.path().to_path_buf()),
            self.egress.clone(),
            Arc::new(MemSecrets::default()),
            replica as Arc<dyn KnownItems>,
        );
        if obsidian {
            ObsidianFactory.build(settings, ctx)
        } else {
            FilesFactory.build(settings, ctx)
        }
        .unwrap()
    }
}

/// The key of `rel` (given with `/`) under root 0, built through the platform path API so the
/// assertion never hard-codes a separator.
fn key(rel: &str) -> String {
    let p: PathBuf = rel.split('/').collect();
    item_key(0, &p)
}

fn keys(rels: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = rels.iter().map(|r| key(r)).collect();
    v.sort();
    v
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
async fn many_files_across_nested_dirs_checkpoint_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "many");
    let total = CHECKPOINT_EVERY as usize * 2 + 37;
    for i in 0..total {
        write(
            &root,
            &format!("d{}/e{}/f{i:04}.txt", i % 7, i % 3),
            &format!("file {i}"),
        );
    }
    let env = Env::new();
    let settings = json!({ "roots": [root] });
    let replica = Replica::new();
    let conn = env.build(false, &settings, replica.clone());
    let (cursor, sink) = pass(&*conn, None, &replica).await;
    assert_eq!(replica.len(), total);
    assert_eq!(cursor["files"], total);
    // Mid-pass checkpoints every CHECKPOINT_EVERY files, plus the final commit.
    assert_eq!(
        sink.checkpoint_calls(),
        total / CHECKPOINT_EVERY as usize + 1
    );
    assert!(sink.summary().iter().all(|s| s.starts_with('+')));

    // A crash after the first mid-pass checkpoint resumes from it without re-sending the
    // files that were already applied, and ends with the same index.
    let partial = Replica::new();
    let conn = env.build(false, &settings, partial.clone());
    let crash_at = CHECKPOINT_EVERY as usize + 10;
    let crashing = ReplicaSink::new(partial.clone()).crash_at_send(crash_at);
    assert!(run_pass(&*conn, None, &crashing).await.is_err());
    let committed = crashing.committed().expect("a mid-pass checkpoint");
    assert_eq!(committed["in_progress"], true);
    let conn = env.build(false, &settings, partial.clone());
    let (_, resumed) = pass(&*conn, Some(committed), &partial).await;
    assert_eq!(resumed.sends(), total - crash_at);
    assert_eq!(partial.snapshot(), replica.snapshot());

    // Unchanged tree: a later pass is silent.
    let conn = env.build(false, &settings, replica.clone());
    let (_, quiet) = pass(&*conn, Some(cursor), &replica).await;
    assert_eq!(quiet.sends(), 0);
    assert!(
        env.egress.events().is_empty(),
        "local connectors record no egress"
    );
}

#[tokio::test]
async fn renames_moves_deletes_and_edits_after_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "alpha");
    write(&root, "dir/b.md", "bravo");
    write(&root, "dir/sub/c.md", "charlie");
    write(&root, "gone.md", "delete me");
    let env = Env::new();
    let settings = json!({ "roots": [root] });
    let replica = Replica::new();
    let conn = env.build(false, &settings, replica.clone());
    let (cursor, _) = pass(&*conn, None, &replica).await;
    let before_c = replica.get(&key("dir/sub/c.md")).unwrap();

    std::fs::rename(path(&root, "a.md"), path(&root, "renamed.md")).unwrap();
    std::fs::create_dir_all(path(&root, "moved")).unwrap();
    std::fs::rename(path(&root, "dir/sub"), path(&root, "moved/sub")).unwrap();
    std::fs::remove_file(path(&root, "gone.md")).unwrap();
    std::fs::write(path(&root, "dir/b.md"), "bravo, edited after the sync").unwrap();

    let (_, sink) = pass(&*conn, Some(cursor), &replica).await;
    assert_eq!(
        replica.keys(),
        keys(&["dir/b.md", "moved/sub/c.md", "renamed.md"])
    );
    let mut summary = sink.summary();
    summary.sort();
    let mut want: Vec<String> = [
        "+dir/b.md",
        "+moved/sub/c.md",
        "+renamed.md",
        "-a.md",
        "-dir/sub/c.md",
        "-gone.md",
    ]
    .iter()
    .map(|s| format!("{}{}", &s[..1], key(&s[1..])))
    .collect();
    want.sort();
    assert_eq!(summary, want);
    // A move keeps the content; only the path (key, URL) changes.
    let moved = replica.get(&key("moved/sub/c.md")).unwrap();
    assert_eq!(moved.content, before_c.content);
    assert_eq!(moved.title, before_c.title);
    assert_ne!(moved.url, before_c.url);
    let b = replica.get(&key("dir/b.md")).unwrap();
    assert_eq!(
        b.content,
        format!("file:{}", "bravo, edited after the sync".len())
    );
}

#[tokio::test]
async fn huge_files_are_streamed_by_path_and_capped_without_stopping_the_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "big");
    let cap: u64 = 64 * 1024;
    let under = write(&root, "under.txt", &"u".repeat(cap as usize));
    write(&root, "over.txt", &"o".repeat(cap as usize + 1));
    write(&root, "small.md", "small");
    let env = Env::new();
    let settings = json!({ "roots": [root], "max_file_bytes": cap });
    let replica = Replica::new();
    let conn = env.build(false, &settings, replica.clone());
    let (cursor, sink) = pass(&*conn, None, &replica).await;
    assert_eq!(replica.keys(), keys(&["small.md", "under.txt"]));
    // Content is never read into memory by the connector: the pipeline opens the path.
    let docs = sink.upserts();
    let d = docs.iter().find(|d| d.key.0 == key("under.txt")).unwrap();
    assert_eq!(d.content, Content::File(under.clone()));
    assert_eq!(d.metadata["size"], cap);

    // A file that grows past the cap leaves the index; shrinking brings it back.
    std::fs::write(&under, "u".repeat(cap as usize + 10)).unwrap();
    let (cursor, _) = pass(&*conn, Some(cursor), &replica).await;
    assert_eq!(replica.keys(), keys(&["small.md"]));
    std::fs::write(&under, "back").unwrap();
    pass(&*conn, Some(cursor), &replica).await;
    assert_eq!(replica.keys(), keys(&["small.md", "under.txt"]));
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
    let conn = env.build(false, &settings, replica.clone());
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

    // Obsidian links encode the same names.
    let vault = Replica::new();
    let conn = env.build(true, &json!({ "vault": root }), vault.clone());
    let (_, sink) = pass(&*conn, None, &vault).await;
    for d in sink.upserts() {
        let u = url::Url::parse(d.url.as_deref().unwrap()).unwrap();
        assert_eq!(u.scheme(), "obsidian");
        let file: String = u
            .query_pairs()
            .find(|(k, _)| k == "file")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(d.metadata["path"], file.as_str(), "{u}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_file_and_dir_never_stop_the_sync() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "perm");
    write(&root, "ok.md", "fine");
    let locked = write(&root, "locked.md", "secret");
    write(&root, "closed/inner.md", "hidden");
    write(&root, "z-last.md", "after the failures");
    let env = Env::new();
    let settings = json!({ "roots": [root] });
    let replica = Replica::new();
    let conn = env.build(false, &settings, replica.clone());
    // First pass indexes everything.
    let (cursor, _) = pass(&*conn, None, &replica).await;
    assert_eq!(replica.len(), 4);

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let closed = path(&root, "closed");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root ignores permission bits; then there is nothing to prove beyond "still completes".
    let enforced = std::fs::read(&locked).is_err();
    std::fs::write(
        path(&root, "z-last.md"),
        "edited while others are unreadable",
    )
    .unwrap();
    std::fs::write(path(&root, "new.md"), "new").unwrap();

    let result = {
        let sink = ReplicaSink::new(replica.clone());
        run_pass(&*conn, Some(cursor), &sink).await
    };
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
    result.expect("an unreadable file must not fail the pass");
    // Later files were still synced, and the unreadable ones keep their indexed copy (a
    // permission glitch must not wipe them).
    assert_eq!(
        replica.keys(),
        keys(&[
            "closed/inner.md",
            "locked.md",
            "new.md",
            "ok.md",
            "z-last.md"
        ])
    );
    assert_eq!(
        replica.get(&key("z-last.md")).unwrap().content,
        format!("file:{}", "edited while others are unreadable".len())
    );
    if enforced {
        // A fresh source never indexes what it cannot read, and still finishes.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let fresh = Replica::new();
        let conn = env.build(false, &settings, fresh.clone());
        let r = {
            let sink = ReplicaSink::new(fresh.clone());
            run_pass(&*conn, None, &sink).await
        };
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        r.unwrap();
        assert!(fresh.get(&key("locked.md")).is_none());
        assert!(fresh.get(&key("z-last.md")).is_some());
    }
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
            let c = env.build(false, &settings, r);
            async move { c }
        }
    };
    let report = assert_crash_resumable(&build, None, BTreeMap::new()).await;
    assert_eq!(report.sends, 13);

    // An incremental pass with edits, moves and deletes since the last commit.
    let replica = Replica::new();
    let conn = env.build(false, &settings, replica.clone());
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
    let conn = env.build(false, &settings, replica.clone());
    let (cursor, _) = pass(&*conn, None, &replica).await;
    assert_eq!(replica.len(), 3, "{:?}", replica.keys());
    let (_, quiet) = pass(&*conn, Some(cursor), &replica).await;
    assert_eq!(quiet.sends(), 0, "{:?}", quiet.summary());
}
