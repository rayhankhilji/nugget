use nugget_plugin_sdk::testing::{MemFs, TestHost};
use nugget_plugin_sdk::{Error, json, log::Level};

use super::*;

const MANIFEST: &str = include_str!("../nugget-plugin.toml");
const VAULT: &str = "/home/ada/Notes Vault";

const DAILY: &str = "---
title: \"Daily: 2026-01-05\"
tags: [journal, 'work/meetings']
aliases:
  - Monday
  - \"Jan 5th, 2026\"
created: 2026-01-05
summary: |
  Two lines
  of summary
nested:
  key: value
---
# Monday

Met with [[Grace Hopper|Grace]] about [[Projects/Compiler#Design]] and ![[diagram.png]].
See also [the plan](Plans/The%20Plan.md) and [a site](https://example.com/x.md).
Tagged #focus and #work/deep-work, not #123 or a URL like https://example.com/#anchor.
Heading markers like ## are not tags, nor is C#.

```rust
// [[NotALink]] #notatag
```

Inline `[[AlsoNot]] #nope` code and ~~~ fences:

~~~
#hidden [[Hidden]]
~~~

    [[IndentedCode]]
";

fn vault() -> MemFs {
    MemFs::new()
        .file_at("Daily/2026-01-05.md", DAILY, 1_000)
        .file_at(
            "Projects/Compiler.md",
            "# Compiler\n\nLinks to [[Daily/2026-01-05]].\n",
            2_000,
        )
        .file_at(
            "Inbox.MD",
            "---\ntags: idea, later\n---\nQuick #thought",
            3_000,
        )
        .file_at("Templates/Daily.md", "{{date}}", 4_000)
        .file_at("Archive/Private/secret.md", "private", 4_000)
        .file_at("Archive/public.md", "public", 4_000)
        .file_at(".obsidian/workspace.md", "settings", 5_000)
        .file_at(".obsidian/app.json", "{}", 5_000)
        .file_at(".trash/old.md", "deleted", 5_000)
        .file_at(".hidden.md", "hidden", 5_000)
        .file_at("diagram.png", vec![0x89, b'P', b'N', b'G'], 5_000)
        .file_at("Notes.txt", "not markdown", 5_000)
}

fn host() -> TestHost {
    let h = TestHost::from_manifest(MANIFEST);
    h.mount(ROOT, vault());
    h
}

fn settings() -> Value {
    json!({ "vault": VAULT, "exclude": ["Templates", "archive/private/"] })
}

// ---- parsing ----

#[test]
fn parses_front_matter() {
    let n = note::parse(DAILY);
    assert_eq!(n.title.as_deref(), Some("Daily: 2026-01-05"));
    assert_eq!(n.aliases, ["Monday", "Jan 5th, 2026"]);
    assert_eq!(n.properties["created"], "2026-01-05");
    assert_eq!(n.properties["summary"], "Two lines\nof summary");
    // Nested maps are skipped, not misread.
    assert_eq!(n.properties.get("nested").map(String::as_str), Some(""));
    assert!(!n.properties.contains_key("key"));
    assert!(n.body.starts_with("# Monday"));
}

#[test]
fn extracts_links_embeds_and_tags_outside_code() {
    let n = note::parse(DAILY);
    assert_eq!(
        n.links,
        ["Grace Hopper", "Projects/Compiler", "Plans/The Plan"]
    );
    assert_eq!(n.embeds, ["diagram.png"]);
    assert_eq!(
        n.tags,
        ["journal", "work/meetings", "focus", "work/deep-work"]
    );
}

#[test]
fn front_matter_variants() {
    let n = note::parse("---\ntags: \"#a, b\"\naliases: Just One, Alias\ntitle:\n---\nbody");
    assert_eq!(n.tags, ["a", "b"]);
    assert_eq!(n.aliases, ["Just One, Alias"]);
    assert_eq!(n.title, None);
    assert_eq!(n.body, "body");

    let n = note::parse("---\r\ntag:\r\n  - '#x'\r\n  - Y\r\n...\r\nbody #x #y");
    assert_eq!(n.tags, ["x", "Y"]);

    // Not front matter: unclosed, or not at the very start.
    let n = note::parse("---\ntitle: x\nno end");
    assert_eq!(n.title, None);
    assert!(n.body.starts_with("---"));
    let n = note::parse("\n---\ntitle: x\n---\n");
    assert_eq!(n.title, None);

    // A BOM is fine.
    let n = note::parse("\u{feff}---\ntitle: Bom\n---\n");
    assert_eq!(n.title.as_deref(), Some("Bom"));
}

#[test]
fn hostile_notes_stay_bounded() {
    let many: String = (0..5000).map(|i| format!("[[n{i}]] #t{i} ")).collect();
    let n = note::parse(&many);
    assert_eq!(n.links.len(), 500);
    assert_eq!(n.tags.len(), 500);
    // Unbalanced syntax does not panic or loop.
    for s in [
        "[[",
        "[[]]",
        "![[",
        "]]",
        "`",
        "``x",
        "#",
        "](",
        "[x](",
        "---",
        "---\n---",
        "é#é[[é",
    ] {
        note::parse(s);
    }
}

#[test]
fn percent_coding() {
    assert_eq!(note::percent_encode("Daily/A b&c"), "Daily%2FA%20b%26c");
    assert_eq!(note::percent_decode("The%20Plan%2"), "The Plan%2");
    assert_eq!(note::percent_decode("%E2%9C%93"), "✓");
}

// ---- settings ----

#[test]
fn validates_settings() {
    let h = host();
    assert!(h.validate::<Vault>(settings()).is_ok());
    for bad in [
        json!({}),
        json!({ "vault": "" }),
        json!({ "vault": "relative/path" }),
        json!({ "vault": VAULT, "exclude": "Templates" }),
        json!({ "vault": VAULT, "exclude": ["../outside"] }),
        json!({ "vault": VAULT, "exclude": [""] }),
    ] {
        assert!(
            matches!(h.validate::<Vault>(bad.clone()), Err(Error::Invalid(_))),
            "{bad}"
        );
    }
    assert!(
        h.validate::<Vault>(json!({ "vault": "C:\\Users\\ada\\Vault" }))
            .is_ok()
    );

    let file = TestHost::from_manifest(MANIFEST);
    file.mount(ROOT, MemFs::single_file("x", 0));
    assert!(matches!(
        file.validate::<Vault>(settings()),
        Err(Error::Invalid(_))
    ));

    // No mount: the host reads the path in the settings, which does not exist.
    let missing = TestHost::from_manifest(MANIFEST);
    assert!(matches!(
        missing.validate::<Vault>(json!({ "vault": "/definitely/not/here/vault" })),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn vault_name_from_any_path_style() {
    let s = |v: &str| Settings::parse(&json!({ "vault": v })).unwrap();
    assert_eq!(s("/home/ada/Notes Vault/").vault_name(), "Notes Vault");
    assert_eq!(s("C:\\Users\\ada\\Work").vault_name(), "Work");
}

// ---- the connector ----

#[test]
fn walk_skips_dot_folders_excludes_and_other_files() {
    let h = host();
    let s = Settings::parse(&settings()).unwrap();
    let paths: Vec<String> = walk(&s)
        .unwrap()
        .notes
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(
        paths,
        [
            "Archive/public.md",
            "Daily/2026-01-05.md",
            "Inbox.MD",
            "Projects/Compiler.md"
        ]
    );
    drop(h);
}

#[test]
fn first_pass_imports_every_note() {
    let h = host();
    let pass = h.sync::<Vault>(settings(), None).unwrap();
    assert_eq!(pass.upserts.len(), 4);
    assert_eq!(pass.cursor["notes"], 4);

    let d = pass.upserted("Daily/2026-01-05.md").unwrap();
    assert_eq!(d.title.as_deref(), Some("Daily: 2026-01-05"));
    assert_eq!(d.mime, "text/markdown");
    assert!(d.text_content().unwrap().starts_with("# Monday"));
    assert_eq!(
        d.version.as_deref(),
        Some(format!("1000:{}", DAILY.len()).as_str())
    );
    assert_eq!(d.modified_at, Some(1_000));
    assert_eq!(
        d.url.as_deref(),
        Some("obsidian://open?vault=Notes%20Vault&file=Daily%2F2026-01-05")
    );
    assert_eq!(d.metadata["folder"], "Daily");
    assert_eq!(d.metadata["vault"], "Notes Vault");
    assert_eq!(d.metadata["links"][1], "Projects/Compiler");
    assert_eq!(d.metadata["aliases"][0], "Monday");
    assert_eq!(d.metadata["properties"]["created"], "2026-01-05");

    // No front matter title: the file name.
    let d = pass.upserted("Projects/Compiler.md").unwrap();
    assert_eq!(d.title.as_deref(), Some("Compiler"));
    let d = pass.upserted("Inbox.MD").unwrap();
    assert_eq!(d.title.as_deref(), Some("Inbox"));
    assert_eq!(d.metadata["tags"], json!(["idea", "later", "thought"]));
    assert_eq!(d.metadata["folder"], "");
}

#[test]
fn later_passes_send_only_changes_and_delete_removed_notes() {
    let h = host();
    let first = h.sync::<Vault>(settings(), None).unwrap();

    let second = h
        .sync::<Vault>(settings(), Some(first.cursor.clone()))
        .unwrap();
    assert!(second.upserts.is_empty() && second.deletes.is_empty());

    h.edit_mount(ROOT, |fs| {
        *fs = std::mem::take(fs)
            .file_at("Inbox.MD", "edited", 9_000)
            .file_at("New/Idea.md", "new", 9_000);
        fs.remove("Projects");
    });
    let third = h.sync::<Vault>(settings(), Some(second.cursor)).unwrap();
    assert_eq!(third.upserted_keys(), ["Inbox.MD", "New/Idea.md"]);
    assert_eq!(third.deletes, ["Projects/Compiler.md"]);

    // Excluding a folder later removes its notes.
    let s = json!({ "vault": VAULT, "exclude": ["Templates", "Archive"] });
    let fourth = h.sync::<Vault>(s, None).unwrap();
    assert!(fourth.upserts.is_empty());
    assert_eq!(fourth.deletes, ["Archive/public.md"]);
}

#[test]
fn unlistable_folders_and_unreadable_notes_never_delete() {
    let h = host();
    h.sync::<Vault>(settings(), None).unwrap();
    h.edit_mount(ROOT, |fs| {
        *fs = std::mem::take(fs)
            .fail("Daily")
            .file_at("Projects/Compiler.md", "changed", 10_000)
            .fail("Projects/Compiler.md");
        fs.remove("Archive");
    });
    let pass = h.sync::<Vault>(settings(), None).unwrap();
    assert!(pass.deletes.is_empty(), "{:?}", pass.deletes);
    assert!(pass.upserts.is_empty());
    assert!(h.logged(Level::Warn, "cannot list `Daily`"));
    assert!(h.logged(Level::Warn, "cannot read `Projects/Compiler.md`"));
    assert_eq!(h.known_items().len(), 4);

    // A vault root that cannot be listed fails the pass.
    let h = TestHost::from_manifest(MANIFEST);
    h.mount(ROOT, MemFs::new().fail(""));
    assert!(h.sync::<Vault>(settings(), None).is_err());
}

#[test]
fn cancellation_is_prompt() {
    let h = host();
    h.cancel_after_upserts(2);
    assert_eq!(
        h.sync::<Vault>(settings(), None).unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(h.documents().len(), 2);
    assert!(
        h.sink_events()
            .iter()
            .all(|e| !matches!(e, nugget_plugin_sdk::testing::SinkEvent::Delete(_)))
    );
}

#[test]
fn reads_a_real_folder_and_ignores_symlinks_out_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir_all(root.join(".obsidian")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.md"), "# A\n[[sub/b]]").unwrap();
    std::fs::write(root.join("sub").join("b.md"), "b #tag").unwrap();
    std::fs::write(root.join(".obsidian").join("x.md"), "x").unwrap();
    std::fs::write(dir.path().join("outside.md"), "outside").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path().join("outside.md"), root.join("link.md")).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("up")).unwrap();
    }

    // No mount: the harness reads the path in the settings, like the host.
    let h = TestHost::from_manifest(MANIFEST);
    let s = json!({ "vault": root.to_str().unwrap() });
    h.validate::<Vault>(s.clone()).unwrap();
    let pass = h.sync::<Vault>(s, None).unwrap();
    assert_eq!(pass.upserted_keys(), ["a.md", "sub/b.md"]);
    assert_eq!(
        pass.upserted("sub/b.md").unwrap().metadata["tags"],
        json!(["tag"])
    );
}
