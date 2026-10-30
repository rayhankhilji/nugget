use nugget_plugin_sdk::testing::{MemFs, TestHost};
use nugget_plugin_sdk::{Error, json, log::Level};

use super::*;

const MANIFEST: &str = include_str!("../nugget-plugin.toml");
const POCKET_HTML: &str = include_str!("../tests/fixtures/ril_export.html");
const INSTAPAPER: &str = include_str!("../tests/fixtures/instapaper-export.csv");
const POCKET_CSV: &str = include_str!("../tests/fixtures/part_000000.csv");
const BOOKMARKS: &str = include_str!("../tests/fixtures/bookmarks.html");

const EXPORT: &str = "/home/ada/Downloads/ril_export.html";

fn host_with(fs: MemFs) -> TestHost {
    let h = TestHost::from_manifest(MANIFEST);
    h.mount(ROOT, fs);
    h
}

fn settings() -> Value {
    json!({ "export": EXPORT })
}

// ---- formats ----

#[test]
fn parses_pocket_html() {
    let (src, a) = formats::parse(POCKET_HTML);
    assert_eq!(src, Source::Pocket);
    assert_eq!(a.len(), 6);
    assert_eq!(
        a[0].url,
        "https://example.com/article?id=7&utm_source=pocket_mylist"
    );
    assert_eq!(a[0].title, "An & Article");
    assert_eq!(a[0].tags, ["rust", "programming"]);
    assert_eq!(a[0].folder.as_deref(), Some("Unread"));
    assert_eq!(a[0].added_ms, Some(1_500_000_000_000));
    assert!(a[1].tags.is_empty());
    assert_eq!(a[3].title, "");
    assert_eq!(a[4].folder.as_deref(), Some("Read Archive"));
}

#[test]
fn parses_instapaper_csv() {
    let (src, a) = formats::parse(INSTAPAPER);
    assert_eq!(src, Source::Instapaper);
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].title, "Title, with comma");
    assert_eq!(
        a[0].selection.as_deref(),
        Some("He said \"hi\"\non two lines")
    );
    assert_eq!(a[0].folder.as_deref(), Some("Unread"));
    assert_eq!(a[0].tags, ["ml", "papers"]);
    assert_eq!(a[0].added_ms, Some(1_600_000_000_000));
    assert!(a[1].tags.is_empty());
    assert_eq!(a[2].folder.as_deref(), Some("Starred"));

    // The older five-column export, without Tags.
    let old = "URL,Title,Selection,Folder,Timestamp\nhttps://x.example/,X,,Unread,1600000000\n";
    let (src, a) = formats::parse(old);
    assert_eq!(src, Source::Instapaper);
    assert_eq!(a[0].title, "X");
}

#[test]
fn parses_pocket_csv() {
    let (src, a) = formats::parse(POCKET_CSV);
    assert_eq!(src, Source::Pocket);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].tags, ["ai", "reading"]);
    assert_eq!(a[0].folder.as_deref(), Some("Unread"));
    assert_eq!(a[1].folder.as_deref(), Some("Read Archive"));
}

#[test]
fn parses_netscape_bookmarks() {
    let (src, a) = formats::parse(BOOKMARKS);
    assert_eq!(src, Source::Bookmarks);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].title, "Pin <one>");
    assert_eq!(a[0].folder.as_deref(), Some("Reading"));
    assert_eq!(a[0].tags, ["a", "b"]);
    assert_eq!(a[0].selection.as_deref(), Some("A description & note"));
    assert_eq!(a[1].url, "https://pinboard.example/y");
    assert_eq!(a[1].added_ms, Some(1_600_000_400_000));
    assert_eq!(a[1].selection, None);
}

#[test]
fn tolerates_garbage() {
    for s in [
        "",
        "<",
        "<a",
        "<a href=",
        "<a href='x",
        "<!--",
        "<h1>",
        "\"",
        "url\n\"",
        "a,b\n1,2",
        "<script><a href='https://x.example/'>x</a>",
        "<<<>>>",
        "&#xFFFFFFFF; &#0; &",
    ] {
        formats::parse(s);
    }
    // Script bodies are not articles.
    let (_, a) = formats::parse(
        "<script>var a='<a href=\"https://x/\">'</script><a href='https://y.example/'>y</a>",
    );
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].url, "https://y.example/");
}

#[test]
fn tags_times_and_dates() {
    assert_eq!(formats::parse_tags("a, b,,A"), ["a", "b"]);
    assert_eq!(formats::parse_tags("x|y"), ["x", "y"]);
    assert_eq!(formats::parse_tags("#x;y"), ["x", "y"]);
    assert_eq!(formats::parse_tags(r#"["q","r"]"#), ["q", "r"]);
    assert_eq!(formats::parse_tags("[q, r]"), ["q", "r"]);
    assert_eq!(formats::parse_time("1500000000"), Some(1_500_000_000_000));
    assert_eq!(
        formats::parse_time("1500000000123"),
        Some(1_500_000_000_123)
    );
    assert_eq!(
        formats::parse_time("2017-07-14T02:40:00Z"),
        Some(1_500_000_000_000)
    );
    assert_eq!(
        formats::parse_time("2017-07-14T04:40:00+02:00"),
        Some(1_500_000_000_000)
    );
    assert_eq!(formats::parse_time("2017-07-14"), Some(1_499_990_400_000));
    assert_eq!(formats::parse_time("0"), None);
    assert_eq!(formats::parse_time("soon"), None);
    assert_eq!(formats::format_date(1_500_000_000_000), "2017-07-14");
    assert_eq!(formats::format_date(0), "1970-01-01");
    assert_eq!(formats::format_date(951_782_400_000), "2000-02-29");
}

#[test]
fn normalises_urls() {
    let n = |u: &str| normalize_url(u);
    assert_eq!(
        n("HTTPS://Example.COM:443/Path?utm_source=x&id=7&fbclid=y#frag").as_deref(),
        Some("https://example.com/Path?id=7")
    );
    assert_eq!(
        n("http://example.com").as_deref(),
        Some("http://example.com/")
    );
    assert_eq!(
        n("http://example.com:8080/a").as_deref(),
        Some("http://example.com:8080/a")
    );
    assert_eq!(
        n("https://user:pw@example.com/a").as_deref(),
        Some("https://example.com/a")
    );
    assert_eq!(
        n("  https://example.com/a?  ").as_deref(),
        Some("https://example.com/a")
    );
    for bad in [
        "javascript:alert(1)",
        "mailto:a@b",
        "ftp://x/",
        "https://",
        "notaurl",
    ] {
        assert_eq!(n(bad), None, "{bad}");
    }
    let long = format!("https://example.com/{}", "a".repeat(2000));
    assert!(key_for(&long).starts_with("url-hash:"));
}

// ---- settings ----

#[test]
fn validates_settings() {
    let h = host_with(MemFs::single_file(POCKET_HTML, 1));
    assert!(h.validate::<ReadLater>(settings()).is_ok());
    for bad in [
        json!({}),
        json!({ "export": "relative.html" }),
        json!({ "export": EXPORT, "skip_archived": "yes" }),
    ] {
        assert!(
            matches!(h.validate::<ReadLater>(bad.clone()), Err(Error::Invalid(_))),
            "{bad}"
        );
    }
    let empty = host_with(MemFs::new().file("notes.txt", "x"));
    assert!(matches!(
        empty.validate::<ReadLater>(json!({ "export": "/home/ada/exports" })),
        Err(Error::Invalid(_))
    ));
    let missing = TestHost::from_manifest(MANIFEST);
    assert!(matches!(
        missing.validate::<ReadLater>(json!({ "export": "/definitely/not/here.html" })),
        Err(Error::Invalid(_))
    ));
}

// ---- the connector ----

#[test]
fn imports_a_pocket_export_file() {
    let h = host_with(MemFs::single_file(POCKET_HTML, 1_000));
    let pass = h.sync::<ReadLater>(settings(), None).unwrap();
    // Six links: one is not http(s), two are the same article.
    assert_eq!(pass.upserts.len(), 4);
    let d = pass.upserted("https://example.com/article?id=7").unwrap();
    assert_eq!(d.title.as_deref(), Some("An & Article"));
    assert_eq!(d.mime, "text/markdown");
    assert_eq!(
        d.url.as_deref(),
        Some("https://example.com/article?id=7&utm_source=pocket_mylist")
    );
    assert_eq!(d.metadata["source"], "pocket");
    // Merged: union of tags (case-insensitively), earliest save.
    assert_eq!(d.metadata["tags"], json!(["rust", "programming", "later"]));
    assert_eq!(d.modified_at, Some(1_400_000_000_000));
    let card = d.text_content().unwrap();
    assert!(
        card.starts_with(
            "# An & Article\n\n<https://example.com/article?id=7&utm_source=pocket_mylist>"
        ),
        "{card}"
    );
    assert!(
        card.contains("Saved 2014-05-13 · Folder: Unread · Tags: rust, programming, later"),
        "{card}"
    );

    // No title: the URL stands in.
    let d = pass.upserted("https://example.com/untitled").unwrap();
    assert_eq!(d.title.as_deref(), Some("https://example.com/untitled"));
    let d = pass.upserted("http://news.example.net/story").unwrap();
    assert_eq!(d.metadata["folder"], "Read Archive");
}

#[test]
fn unchanged_exports_are_skipped_and_changes_reconciled() {
    let h = host_with(MemFs::single_file(POCKET_HTML, 1_000));
    let first = h.sync::<ReadLater>(settings(), None).unwrap();

    let second = h
        .sync::<ReadLater>(settings(), Some(first.cursor.clone()))
        .unwrap();
    assert!(second.upserts.is_empty() && second.deletes.is_empty());
    assert_eq!(second.cursor, first.cursor);

    // A new export: one article gone, one retitled, one new.
    let next = POCKET_HTML
        .replace(r#"<li><a href="http://news.example.net/story" time_added="1300000000" tags="news">Old story</a></li>"#, "")
        .replace(">A Post<", ">A Post, retitled<")
        .replace("</ul>\n\t\t<h1>Read", "<li><a href=\"https://new.example.com/\" time_added=\"1600000000\">New</a></li></ul>\n\t\t<h1>Read");
    h.mount(ROOT, MemFs::single_file(next, 2_000));
    let third = h
        .sync::<ReadLater>(settings(), Some(second.cursor))
        .unwrap();
    assert_eq!(
        third.upserted_keys(),
        ["https://blog.example.org/post", "https://new.example.com/"]
    );
    assert_eq!(third.deletes, ["http://news.example.net/story"]);

    // A full pass sends nothing that the store already holds at the same version.
    let full = h.sync::<ReadLater>(settings(), None).unwrap();
    assert!(full.upserts.is_empty() && full.deletes.is_empty());
}

#[test]
fn reads_a_folder_of_exports_and_skips_archived_on_request() {
    let fs = MemFs::new()
        .file("pocket/ril_export.html", POCKET_HTML)
        .file("pocket/part_000000.csv", POCKET_CSV)
        .file("instapaper-export.csv", INSTAPAPER)
        .file("bookmarks.html", BOOKMARKS)
        .file("readme.txt", "ignored")
        .file(".cache/old.csv", "url\nhttps://hidden.example/\n");
    let h = host_with(fs.clone());
    let s = json!({ "export": "/home/ada/exports" });
    let pass = h.sync::<ReadLater>(s, None).unwrap();
    assert_eq!(pass.upserts.len(), 4 + 2 + 3 + 2);
    assert!(pass.upserted("https://hidden.example/").is_none());
    let d = pass.upserted("https://example.com/a").unwrap();
    assert_eq!(d.metadata["source"], "instapaper");
    assert_eq!(d.metadata["export_file"], "instapaper-export.csv");
    assert!(
        d.text_content()
            .unwrap()
            .contains("> He said \"hi\"\n> on two lines\n")
    );

    let h = host_with(fs);
    let s = json!({ "export": "/home/ada/exports", "skip_archived": true });
    let pass = h.sync::<ReadLater>(s, None).unwrap();
    let keys = pass.upserted_keys();
    assert!(!keys.contains(&"http://news.example.net/story"));
    assert!(!keys.contains(&"https://example.org/q"));
    assert!(!keys.contains(&"https://example.com/b"));
    assert_eq!(keys.len(), 11 - 3);
}

#[test]
fn unreadable_export_files_never_delete() {
    let fs = MemFs::new()
        .file("a.html", POCKET_HTML)
        .file("b.csv", INSTAPAPER);
    let h = host_with(fs);
    let s = json!({ "export": "/home/ada/exports" });
    let first = h.sync::<ReadLater>(s.clone(), None).unwrap();
    assert_eq!(first.upserts.len(), 7);
    h.edit_mount(ROOT, |fs| {
        *fs = std::mem::take(fs)
            .file_at("b.csv", "changed", 99)
            .fail("b.csv");
    });
    let pass = h.sync::<ReadLater>(s.clone(), Some(first.cursor)).unwrap();
    assert!(pass.deletes.is_empty());
    assert!(h.logged(Level::Warn, "cannot read export `b.csv`"));
    // Not recorded as seen: the next pass tries again.
    assert_eq!(pass.cursor["fingerprint"], Value::Null);
}

#[test]
fn cancellation_is_prompt() {
    let h = host_with(MemFs::single_file(POCKET_HTML, 1));
    h.cancel_after_upserts(1);
    assert_eq!(
        h.sync::<ReadLater>(settings(), None).unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(h.documents().len(), 1);
}

#[test]
fn reads_a_real_export_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("instapaper-export.csv");
    std::fs::write(&file, INSTAPAPER).unwrap();
    let h = TestHost::from_manifest(MANIFEST);
    let s = json!({ "export": file.to_str().unwrap() });
    h.validate::<ReadLater>(s.clone()).unwrap();
    assert_eq!(h.sync::<ReadLater>(s, None).unwrap().upserts.len(), 3);
}
