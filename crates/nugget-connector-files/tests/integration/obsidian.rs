use nugget_connector::Content;
use serde_json::json;

use crate::common::*;

fn vault(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    let v = root_in(tmp, "My Vault");
    write(&v, ".obsidian/app.json", "{}");
    write(&v, ".obsidian/plugins/x/README.md", "plugin readme");
    write(&v, ".trash/old.md", "deleted note");
    write(
        &v,
        "Notes/Note One.md",
        "---\ntags: [a]\n---\n# One\n[[Two]]",
    );
    write(&v, "Two.md", "# Two");
    write(&v, "attachments/img.png", "\u{89}PNG");
    write(&v, "attachments/paper.pdf", "%PDF-1.7");
    write(&v, "other.txt", "not a note");
    write(&v, "board.canvas", "{}");
    v
}

#[tokio::test]
async fn vault_presets_urls_and_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let v = vault(&tmp);
    let mut h = Harness::new(Kind::Obsidian, json!({ "vault": v }));
    assert_eq!(h.conn.kind(), "obsidian");
    let c = h.sync().await;
    assert_eq!(
        upserts(&c),
        set([
            "0:Notes/Note One.md",
            "0:Two.md",
            "0:attachments/img.png",
            "0:attachments/paper.pdf"
        ])
    );

    let d = &docs(&c)["0:Notes/Note One.md"];
    assert_eq!(d.mime, "text/markdown");
    assert_eq!(d.title.as_deref(), Some("Note One"));
    assert_eq!(
        d.url.as_deref(),
        Some("obsidian://open?vault=My%20Vault&file=Notes%2FNote%20One.md")
    );
    assert_eq!(d.metadata["vault"], "My Vault");
    assert_eq!(d.metadata["path"], "Notes/Note One.md");
    let furl = d.metadata["file_url"].as_str().unwrap();
    assert!(furl.starts_with("file:///"), "{furl}");
    assert!(furl.ends_with("/My%20Vault/Notes/Note%20One.md"), "{furl}");
    assert_eq!(d.content, Content::File(path(&v, "Notes/Note One.md")));
    assert_eq!(docs(&c)["0:attachments/paper.pdf"].mime, "application/pdf");

    // A note renamed in Obsidian.
    std::fs::rename(path(&v, "Two.md"), path(&v, "Notes/Two renamed.md")).unwrap();
    let c = h.sync().await;
    assert_eq!(deletes(&c), set(["0:Two.md"]));
    assert_eq!(upserts(&c), set(["0:Notes/Two renamed.md"]));
}

#[tokio::test]
async fn attachments_can_be_left_out() {
    let tmp = tempfile::tempdir().unwrap();
    let v = vault(&tmp);
    let mut h = Harness::new(
        Kind::Obsidian,
        json!({ "vault": v, "include_attachments": false, "exclude": ["Notes/"] }),
    );
    assert_eq!(upserts(&h.sync().await), set(["0:Two.md"]));
}

#[tokio::test]
async fn obsidian_internals_stay_excluded_even_with_hidden_files() {
    let tmp = tempfile::tempdir().unwrap();
    let v = vault(&tmp);
    write(&v, ".hidden note.md", "h");
    let mut h = Harness::new(
        Kind::Obsidian,
        json!({ "vault": v, "include_hidden": true }),
    );
    let keys = upserts(&h.sync().await);
    assert!(keys.contains("0:.hidden note.md"));
    assert!(
        !keys
            .iter()
            .any(|k| k.contains(".obsidian") || k.contains(".trash")),
        "{keys:?}"
    );
}

#[tokio::test]
async fn vault_edits_are_watched() {
    let tmp = tempfile::tempdir().unwrap();
    let v = vault(&tmp);
    let mut h = Harness::new(
        Kind::Obsidian,
        json!({ "vault": v, "debounce_ms": 100, "rescan_interval_secs": 0 }),
    );
    h.sync().await;
    let (cancel, task) = h.start_watch();
    h.wait_watching(&v, "md").await;

    append(&v, "Notes/Note One.md", "\nmore");
    write(&v, ".obsidian/workspace.json", "{\"changed\":true}");
    let want = version(&path(&v, "Notes/Note One.md"));
    let c = h
        .wait_for("note upsert", |c| {
            docs(c)
                .get("0:Notes/Note One.md")
                .is_some_and(|d| d.version.as_deref() == Some(want.as_str()))
        })
        .await;
    assert!(!upserts(&c).iter().any(|k| k.contains(".obsidian")));
    cancel.cancel();
    let _ = task.await.unwrap();
}
