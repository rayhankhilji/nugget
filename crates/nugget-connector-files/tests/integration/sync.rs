use std::time::{Duration, SystemTime};

use nugget_connector::{CancellationToken, ConnectorFactory, Content, Registry};
use nugget_connector_files::{CHECKPOINT_EVERY, FilesFactory, ObsidianFactory};
use nugget_core::Error;
use serde_json::json;

use crate::common::*;

fn set_mtime(p: &std::path::Path, t: SystemTime) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(t).unwrap();
}

#[tokio::test]
async fn initial_scan_reports_documents() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "docs root");
    let a = write(&root, "a.md", "# A\n\nhello");
    write(&root, "notes/b.txt", "plain");
    write(&root, "notes/deep/c.pdf", "%PDF-1.4 fake");
    write(&root, "code/main.rs", "fn main() {}");
    write(&root, "bin/tool.exe", "MZ");
    write(&root, "no_extension", "x");

    let mut h = Harness::files(json!({ "roots": [root] }));
    let changes = h.sync().await;
    assert_eq!(
        upserts(&changes),
        set([
            "0:a.md",
            "0:code/main.rs",
            "0:notes/b.txt",
            "0:notes/deep/c.pdf"
        ])
    );
    assert!(deletes(&changes).is_empty());

    let d = &docs(&changes)["0:a.md"];
    assert_eq!(d.title.as_deref(), Some("a"));
    assert_eq!(d.mime, "text/markdown");
    assert_eq!(d.content, Content::File(a.clone()));
    assert_eq!(d.version.as_deref(), Some(version(&a).as_str()));
    let url = d.url.as_deref().unwrap();
    assert!(url.starts_with("file:///"), "{url}");
    assert!(url.ends_with("/docs%20root/a.md"), "{url}");
    assert_eq!(d.metadata["path"], "a.md");
    assert_eq!(d.metadata["size"], 10);
    assert_eq!(d.metadata["root"], root.to_string_lossy().as_ref());
    assert!(d.modified_at.is_some());
    assert_eq!(docs(&changes)["0:notes/deep/c.pdf"].mime, "application/pdf");

    let cursor = h.cursor.clone().unwrap();
    assert_eq!(cursor["files"], 4);
    assert!(cursor["roots"].is_string());
    assert!(cursor["scanned_at"].is_string());
    assert!(h.fake.checkpoints.lock().unwrap().is_empty());

    // Nothing changed: a second pass is silent.
    assert!(h.sync().await.is_empty());
}

#[tokio::test]
async fn edits_deletes_renames_and_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "one");
    write(&root, "b.txt", "bee");
    write(&root, "docs/deep/c.md", "see");
    write(&root, "docs/d.md", "dee");
    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(h.sync().await.len(), 4);

    // Content edit (size changes).
    append(&root, "a.md", " two");
    let c = h.sync().await;
    assert_eq!(upserts(&c), set(["0:a.md"]));
    assert!(deletes(&c).is_empty());

    // Same size, new mtime.
    let b = path(&root, "b.txt");
    set_mtime(&b, SystemTime::now() - Duration::from_secs(3600));
    let c = h.sync().await;
    assert_eq!(upserts(&c), set(["0:b.txt"]));
    assert_eq!(
        docs(&c)["0:b.txt"].version.as_deref(),
        Some(version(&b).as_str())
    );

    // Delete.
    std::fs::remove_file(&b).unwrap();
    let c = h.sync().await;
    assert_eq!(deletes(&c), set(["0:b.txt"]));
    assert!(upserts(&c).is_empty());

    // Rename in place, and a move between subdirectories.
    std::fs::rename(path(&root, "a.md"), path(&root, "renamed.md")).unwrap();
    std::fs::create_dir_all(path(&root, "archive")).unwrap();
    std::fs::rename(path(&root, "docs/deep/c.md"), path(&root, "archive/c.md")).unwrap();
    let c = h.sync().await;
    assert_eq!(deletes(&c), set(["0:a.md", "0:docs/deep/c.md"]));
    assert_eq!(upserts(&c), set(["0:archive/c.md", "0:renamed.md"]));

    // Moving a whole directory.
    std::fs::rename(path(&root, "docs"), path(&root, "documents")).unwrap();
    let c = h.sync().await;
    assert_eq!(deletes(&c), set(["0:docs/d.md"]));
    assert_eq!(upserts(&c), set(["0:documents/d.md"]));
    assert_eq!(
        h.store_keys(),
        set(["0:archive/c.md", "0:documents/d.md", "0:renamed.md"])
    );
}

#[tokio::test]
async fn gitignore_and_nuggetignore_are_honoured() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, ".gitignore", "secret/\n*.draft.md\n");
    write(&root, "secret/x.md", "x");
    write(&root, "y.draft.md", "y");
    write(&root, "keep.md", "k");
    write(&root, "sub/.nuggetignore", "private.md\n");
    write(&root, "sub/private.md", "p");
    write(&root, "sub/public.md", "p");
    write(&root, "other/private.md", "p");
    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(
        upserts(&h.sync().await),
        set(["0:keep.md", "0:other/private.md", "0:sub/public.md"])
    );

    // Tightening an ignore file removes what it now covers.
    write(&root, ".gitignore", "secret/\n*.draft.md\nsub/\n");
    let c = h.sync().await;
    assert_eq!(deletes(&c), set(["0:sub/public.md"]));
    assert!(upserts(&c).is_empty());
}

#[tokio::test]
async fn junk_vcs_dependency_and_cache_dirs_are_always_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    for rel in [
        ".git/notes.md",
        "node_modules/pkg/readme.md",
        "proj/target/doc.md",
        "proj/__pycache__/x.py",
        "proj/.venv/lib.py",
        ".DS_Store",
        "~$report.docx",
        "note.md.swp",
        "._note.md",
        "download.pdf.crdownload",
    ] {
        write(&root, rel, "junk");
    }
    write(&root, "proj/src/lib.rs", "ok");
    let mut h =
        Harness::files(json!({ "roots": [root], "include": ["*"], "include_hidden": true }));
    assert_eq!(upserts(&h.sync().await), set(["0:proj/src/lib.rs"]));
}

#[tokio::test]
async fn credential_files_and_dirs_are_never_indexed() {
    // A root at the home folder with everything included: keys and credentials stay out of the
    // index (where any prompt-injected request could search them).
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "home");
    for rel in [
        ".ssh/id_ed25519",
        ".ssh/config",
        ".gnupg/private-keys-v1.d/x.key",
        ".aws/credentials",
        ".kube/config",
        ".docker/config.json",
        "proj/.env",
        "proj/.env.production",
        "proj/.ENV",
        "proj/server.pem",
        "proj/tls.KEY",
        "proj/id_rsa",
        ".netrc",
        ".npmrc",
        ".git-credentials",
        "vault.kdbx",
    ] {
        write(&root, rel, "SECRET");
    }
    write(&root, "proj/README.md", "ok");
    write(&root, "proj/keys.md", "ok");
    let mut h =
        Harness::files(json!({ "roots": [root], "include": ["*"], "include_hidden": true }));
    assert_eq!(
        upserts(&h.sync().await),
        set(["0:proj/README.md", "0:proj/keys.md"])
    );
}

#[tokio::test]
async fn hidden_files_follow_the_setting() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, ".hidden.md", "h");
    write(&root, ".dir/x.md", "x");
    write(&root, "seen.md", "s");
    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(upserts(&h.sync().await), set(["0:seen.md"]));

    let mut h = Harness::files(json!({ "roots": [root], "include_hidden": true }));
    assert_eq!(
        upserts(&h.sync().await),
        set(["0:.dir/x.md", "0:.hidden.md", "0:seen.md"])
    );
}

#[tokio::test]
async fn include_and_exclude_globs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    for rel in [
        "a.md",
        "A2.MD",
        "notes/n.txt",
        "deep/notes/n.txt",
        "x.txt",
        "drafts/d.md",
        "deep/drafts/d.md",
        "keep/scratch.tmp.md",
        "pic.png",
    ] {
        write(&root, rel, "c");
    }
    let mut h = Harness::files(json!({
        "roots": [root],
        "include": ["*.md", "/notes/*.txt"],
        "exclude": ["drafts/", "*.tmp.md"],
    }));
    assert_eq!(
        upserts(&h.sync().await),
        set(["0:A2.MD", "0:a.md", "0:notes/n.txt"])
    );
}

#[tokio::test]
async fn size_cap_applies_and_growing_past_it_deletes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "small.md", "12345");
    write(&root, "big.md", &"x".repeat(20));
    let mut h = Harness::files(json!({ "roots": [root], "max_file_bytes": 10 }));
    assert_eq!(upserts(&h.sync().await), set(["0:small.md"]));
    append(&root, "small.md", "678901");
    assert_eq!(deletes(&h.sync().await), set(["0:small.md"]));
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_follow_the_policy() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    let outside = root_in(&tmp, "outside");
    write(&outside, "o.md", "outside");
    write(&root, "a.md", "a");
    symlink(&outside, root.join("linked_dir")).unwrap();
    symlink(root.join("a.md"), root.join("link.md")).unwrap();
    symlink(&root, root.join("loop")).unwrap();

    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(upserts(&h.sync().await), set(["0:a.md"]));

    let mut h = Harness::files(json!({ "roots": [root], "follow_symlinks": true }));
    let c = h.sync().await;
    assert_eq!(
        upserts(&c),
        set(["0:a.md", "0:link.md", "0:linked_dir/o.md"]),
        "links followed, the loop detected and skipped"
    );
}

#[tokio::test]
async fn reconciles_changes_made_while_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    let settings = json!({ "roots": [root] });
    write(&root, "keep.md", "k");
    write(&root, "edit.md", "e");
    write(&root, "gone.md", "g");
    write(&root, "dir/old.md", "o");
    let mut h = Harness::files(settings.clone());
    assert_eq!(h.sync().await.len(), 4);

    // "Downtime": the daemon is stopped while things change.
    h.restart(settings.clone());
    append(&root, "edit.md", " more");
    std::fs::remove_file(path(&root, "gone.md")).unwrap();
    std::fs::rename(path(&root, "dir"), path(&root, "moved")).unwrap();
    write(&root, "new.md", "n");

    let c = h.sync().await;
    assert_eq!(deletes(&c), set(["0:dir/old.md", "0:gone.md"]));
    assert_eq!(
        upserts(&c),
        set(["0:edit.md", "0:moved/old.md", "0:new.md"])
    );
    assert_eq!(
        h.store_keys(),
        set(["0:edit.md", "0:keep.md", "0:moved/old.md", "0:new.md"])
    );
}

#[tokio::test]
async fn changing_roots_forces_a_full_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let ra = root_in(&tmp, "a");
    let rb = root_in(&tmp, "b");
    write(&ra, "only_a.md", "a");
    let ca = write(&ra, "common.md", "same");
    write(&rb, "only_b.md", "b");
    let cb = write(&rb, "common.md", "same");
    // Identical versions: only a forced rescan notices `0:common.md` now means b's file.
    let t = SystemTime::now() - Duration::from_secs(60);
    set_mtime(&ca, t);
    set_mtime(&cb, t);
    assert_eq!(version(&ca), version(&cb));

    let mut h = Harness::files(json!({ "roots": [ra, rb] }));
    assert_eq!(
        upserts(&h.sync().await),
        set(["0:common.md", "0:only_a.md", "1:common.md", "1:only_b.md"])
    );

    h.restart(json!({ "roots": [rb] }));
    let c = h.sync().await;
    assert_eq!(upserts(&c), set(["0:common.md", "0:only_b.md"]));
    assert_eq!(
        deletes(&c),
        set(["0:only_a.md", "1:common.md", "1:only_b.md"])
    );
    assert_eq!(docs(&c)["0:common.md"].content, Content::File(cb.clone()));
    // Same roots again: back to incremental.
    assert!(h.sync().await.is_empty());
}

#[tokio::test]
async fn a_reset_cursor_with_known_items_is_a_forced_full_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "a");
    write(&root, "b.md", "b");
    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(h.sync().await.len(), 2);
    assert!(h.sync().await.is_empty());

    // A settings update resets the cursor while the store keeps the items: every file is
    // re-sent (the root list may have changed meaning), and vanished ones are deleted.
    std::fs::remove_file(path(&root, "b.md")).unwrap();
    h.cursor = None;
    let c = h.sync().await;
    assert_eq!(upserts(&c), set(["0:a.md"]));
    assert_eq!(deletes(&c), set(["0:b.md"]));
    // Back to incremental.
    assert!(h.sync().await.is_empty());

    // A first run (nothing known) is not "forced": nothing to reconcile against.
    let mut fresh = Harness::files(json!({ "roots": [root] }));
    assert_eq!(upserts(&fresh.sync().await), set(["0:a.md"]));
}

#[cfg(unix)]
#[tokio::test]
async fn the_pipeline_refuses_a_file_swapped_after_the_scan() {
    use nugget_extract::{ExtractOptions, FileExpect, Source, extract, is_file_changed};
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    let outside = root_in(&tmp, "outside");
    let secret = write(&outside, "secret.md", "outside secret");
    let p = write(&root, "note.md", "note");
    let mut h = Harness::files(json!({ "roots": [root] }));
    let c = h.sync().await;
    let doc = docs(&c)["0:note.md"].clone();
    let Content::File(file) = &doc.content else {
        panic!("{:?}", doc.content)
    };
    let expect = FileExpect::from_metadata(&doc.metadata).expect("file_check metadata");
    let checked = |f: &std::path::Path| {
        extract(
            &doc.mime,
            Source::CheckedFile { path: f, expect },
            &ExtractOptions::default(),
        )
    };
    assert_eq!(checked(file).unwrap().text, "note");
    // Between scan and extraction the file becomes a link to something outside the root.
    std::fs::remove_file(&p).unwrap();
    std::os::unix::fs::symlink(&secret, &p).unwrap();
    let err = checked(file).unwrap_err();
    assert!(is_file_changed(&err), "{err}");
}

#[tokio::test]
async fn long_passes_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    let n = CHECKPOINT_EVERY * 2 + 3;
    for i in 0..n {
        write(&root, &format!("d{}/f{i}.txt", i % 7), "x");
    }
    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(h.sync().await.len() as u64, n);
    let cps = h.fake.checkpoints.lock().unwrap().clone();
    assert_eq!(cps.len(), 2);
    assert_eq!(cps[0]["files"], CHECKPOINT_EVERY);
    assert_eq!(cps[0]["in_progress"], true);
    assert_eq!(cps[0]["roots"], h.cursor.as_ref().unwrap()["roots"]);
    assert_eq!(h.cursor.as_ref().unwrap()["files"], n);
    assert!(h.cursor.as_ref().unwrap().get("in_progress").is_none());
}

#[tokio::test]
async fn sync_honours_cancellation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "a");
    let h = Harness::files(json!({ "roots": [root] }));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = h
        .conn
        .sync(None, h.fake.as_ref(), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

#[tokio::test]
async fn a_missing_root_keeps_its_items() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "a");
    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(h.sync().await.len(), 1);
    let away = tmp.path().join("unmounted");
    std::fs::rename(&root, &away).unwrap();
    assert!(
        h.sync().await.is_empty(),
        "an unmounted drive must not wipe the index"
    );
    std::fs::rename(&away, &root).unwrap();
    assert!(h.sync().await.is_empty());
}

/// Permission tests mean nothing when running as root (CI containers often do).
#[cfg(unix)]
fn permissions_enforced(p: &std::path::Path) -> bool {
    std::fs::File::open(p).is_err()
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_files_and_dirs_are_skipped_without_failing() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "ok.md", "ok");
    let known = write(&root, "known.md", "k");
    write(&root, "locked/inside.md", "i");
    let mut h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(h.sync().await.len(), 3);

    let fresh = write(&root, "fresh.md", "f");
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o000)).unwrap();
    append(&root, "known.md", " changed");
    std::fs::set_permissions(&known, std::fs::Permissions::from_mode(0o000)).unwrap();
    let locked = path(&root, "locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = permissions_enforced(&fresh);

    let c = h.sync().await;
    for p in [&fresh, &known] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !enforced {
        eprintln!("running with permission bypass (root?); skipping assertions");
        return;
    }
    assert!(upserts(&c).is_empty(), "{:?}", summary(&c));
    assert!(
        deletes(&c).is_empty(),
        "unreadable known items are kept: {:?}",
        summary(&c)
    );

    // Readable again: picked up.
    let c = h.sync().await;
    assert_eq!(upserts(&c), set(["0:fresh.md", "0:known.md"]));
}

#[tokio::test]
async fn factories_validate_and_register() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    assert!(FilesFactory.validate(&json!({ "roots": [root] })).is_ok());
    assert!(
        FilesFactory
            .validate(&json!({ "roots": [root.join("missing")] }))
            .is_err()
    );
    assert!(
        FilesFactory
            .validate(&json!({ "roots": ["relative"] }))
            .is_err()
    );
    assert!(ObsidianFactory.validate(&json!({ "vault": root })).is_ok());
    assert!(
        ObsidianFactory
            .validate(&json!({ "roots": [root] }))
            .is_err()
    );

    let mut reg = Registry::new();
    nugget_connector_files::register(&mut reg);
    let kinds: Vec<_> = reg.kinds().into_iter().map(|k| k.kind).collect();
    assert_eq!(kinds, ["files", "obsidian"]);
    let info = reg.get("files").unwrap().info();
    assert_eq!(info.settings_schema["required"], json!(["roots"]));

    let h = Harness::files(json!({ "roots": [root] }));
    assert_eq!(h.conn.kind(), "files");
    assert!(h.conn.supports_watch());
}
