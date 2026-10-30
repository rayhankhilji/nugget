use std::cell::Cell;
use std::rc::Rc;

use nugget_plugin_sdk::testing::TestHost;
use nugget_plugin_sdk::{Error, http, json, log::Level};

use super::*;

const MANIFEST: &str = include_str!("../nugget-plugin.toml");
const RSS2: &str = include_str!("../tests/fixtures/rss2.xml");
const ATOM: &str = include_str!("../tests/fixtures/atom.xml");
const RDF: &str = include_str!("../tests/fixtures/rdf.xml");
const SLOPPY: &str = include_str!("../tests/fixtures/sloppy.xml");

const RSS_URL: &str = "https://blog.example.substack.com/feed";
const ATOM_URL: &str = "https://example.github.io/blog/atom.xml";
const RDF_URL: &str = "https://export.arxiv.org/rss/cs";

fn host() -> TestHost {
    TestHost::from_manifest(MANIFEST)
}

fn settings(feeds: &[&str]) -> Value {
    json!({ "feeds": feeds })
}

// ---- parsing ----

#[test]
fn parses_rss2() {
    let f = feed::parse(RSS2, RSS_URL).unwrap();
    assert_eq!(f.format, "rss");
    assert_eq!(f.title.as_deref(), Some("Example Blog"));
    assert_eq!(f.entries.len(), 4);

    let e = &f.entries[0];
    assert_eq!(e.id, "post-1");
    assert_eq!(e.title.as_deref(), Some("Hello & welcome"));
    assert_eq!(
        e.link.as_deref(),
        Some("https://blog.example.substack.com/p/hello")
    );
    assert_eq!(e.content, "<p>The <b>full</b> post.</p>");
    assert_eq!(e.kind, feed::Kind::Html);
    assert_eq!(e.author.as_deref(), Some("Ada"));
    assert_eq!(e.categories, ["intro", "meta"]);
    assert_eq!(e.published_ms, Some(1_136_214_245_000));

    // A permalink guid doubles as the link.
    let e = &f.entries[1];
    assert_eq!(e.id, "https://blog.example.substack.com/p/second");
    assert_eq!(e.link.as_deref(), Some(e.id.as_str()));
    assert_eq!(e.content, "<p>Escaped HTML body.</p>");
    assert_eq!(e.published_ms, Some(1_136_278_800_000));

    // No guid: the (resolved) link is the id.
    let e = &f.entries[2];
    assert_eq!(e.id, "https://blog.example.substack.com/p/third");

    // Nothing at all: a stable content hash.
    let e = &f.entries[3];
    assert!(e.id.starts_with("hash:"), "{}", e.id);
    assert_eq!(e.id, feed::parse(RSS2, RSS_URL).unwrap().entries[3].id);
}

#[test]
fn parses_atom() {
    let f = feed::parse(ATOM, ATOM_URL).unwrap();
    assert_eq!(f.format, "atom");
    assert_eq!(f.title.as_deref(), Some("Atom Example"));
    assert_eq!(f.link.as_deref(), Some("https://example.github.io/blog/"));
    assert_eq!(f.entries.len(), 4);

    let e = &f.entries[0];
    assert_eq!(e.id, "tag:example.github.io,2006:html");
    assert_eq!(
        e.link.as_deref(),
        Some("https://example.github.io/blog/posts/html.html")
    );
    assert_eq!(e.content, "<p>Hello <i>HTML</i></p>");
    assert_eq!(e.kind, feed::Kind::Html);
    assert_eq!(e.author.as_deref(), Some("Grace"));
    assert_eq!(e.categories, ["Rust", "wasm"]);
    assert_eq!(e.updated_ms, Some(1_136_214_245_000));
    assert_eq!(e.published_ms, Some(1_136_124_000_000));

    let e = &f.entries[1];
    assert_eq!(
        e.link.as_deref(),
        Some("https://example.github.io/elsewhere/xhtml")
    );
    assert!(e.content.contains("<b>XHTML</b>"), "{}", e.content);
    assert_eq!(e.kind, feed::Kind::Html);

    let e = &f.entries[2];
    assert_eq!(
        (e.content.as_str(), e.kind),
        ("Plain & simple", feed::Kind::Text)
    );

    // `content src=…` is not inline: the summary is used.
    let e = &f.entries[3];
    assert_eq!(e.content, "The summary instead");
    assert_eq!(e.updated, None);
}

#[test]
fn parses_rss1_rdf() {
    let f = feed::parse(RDF, RDF_URL).unwrap();
    assert_eq!(f.format, "rdf");
    assert_eq!(f.title.as_deref(), Some("cs updates on arXiv.org"));
    assert_eq!(f.entries.len(), 2);
    let e = &f.entries[0];
    assert_eq!(e.id, "http://arxiv.org/abs/2601.00001");
    assert_eq!(e.author.as_deref(), Some("Someone"));
    assert_eq!(e.categories, ["Computation"]);
    assert_eq!(e.updated_ms, Some(1_767_614_400_000));
}

#[test]
fn repairs_html_entities_and_bare_ampersands() {
    let f = feed::parse(SLOPPY, "https://hnrss.org/newest").unwrap();
    assert_eq!(f.title.as_deref(), Some("Tom & Jerry\u{a0}News"));
    let e = &f.entries[0];
    assert_eq!(e.title.as_deref(), Some("Café — opening…"));
    assert_eq!(e.link.as_deref(), Some("https://hnrss.org/item?id=1&b=2"));
    assert_eq!(e.content, "Fish & chips © 2006 &unknownentity;");
}

#[test]
fn decodes_latin1_bodies() {
    let mut body =
        b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><rss><channel><title>Caf".to_vec();
    body.extend([0xe9, 0x20, 0x93, b'q', 0x94]);
    body.extend(b"</title></channel></rss>");
    let f = feed::parse(&feed::decode(&body), RSS_URL).unwrap();
    assert_eq!(f.title.as_deref(), Some("Café “q”"));
    // A BOM is dropped.
    let f = feed::parse(&feed::decode(b"\xEF\xBB\xBF<rss><channel/></rss>"), RSS_URL).unwrap();
    assert!(f.entries.is_empty());
}

#[test]
fn refuses_non_feeds_and_entity_bombs() {
    assert!(matches!(
        feed::parse("<html><body/></html>", RSS_URL),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        feed::parse("not xml", RSS_URL),
        Err(Error::Invalid(_))
    ));
    let bomb = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [
 <!ENTITY lol "lol">
 <!ENTITY lol1 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
 <!ENTITY lol2 "&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;">
 <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
 <!ENTITY lol4 "&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;">
 <!ENTITY lol5 "&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;">
 <!ENTITY lol6 "&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;">
 <!ENTITY lol7 "&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;">
 <!ENTITY lol8 "&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;">
 <!ENTITY lol9 "&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;">
]>
<rss><channel><title>&lol9;</title></channel></rss>"#;
    // Refused, or (after entity repair) kept as literal text; never expanded.
    match feed::parse(bomb, RSS_URL) {
        Ok(f) => assert!(f.title.unwrap_or_default().len() < 100),
        Err(e) => assert!(matches!(e, Error::Invalid(_))),
    }
    // External entities are never fetched or read.
    let xxe = r#"<?xml version="1.0"?>
<!DOCTYPE r [<!ENTITY x SYSTEM "file:///etc/passwd">]>
<rss><channel><title>&x;</title></channel></rss>"#;
    if let Ok(f) = feed::parse(xxe, RSS_URL) {
        assert!(!f.title.unwrap_or_default().contains("root:"));
    }
}

#[test]
fn resolves_relative_links() {
    let b = "https://example.com/a/b/feed.xml?x=1";
    assert_eq!(feed::resolve(b, "c.html"), "https://example.com/a/b/c.html");
    assert_eq!(feed::resolve(b, "/c"), "https://example.com/c");
    assert_eq!(
        feed::resolve(b, "//cdn.example.com/c"),
        "https://cdn.example.com/c"
    );
    assert_eq!(feed::resolve(b, "https://other/x"), "https://other/x");
    assert_eq!(
        feed::resolve("https://example.com", "x"),
        "https://example.com/x"
    );
}

// ---- settings ----

#[test]
fn validates_settings() {
    let h = host();
    assert!(h.validate::<Feeds>(settings(&[RSS_URL])).is_ok());
    // The single-URL form of the old example plugin still works.
    assert!(h.validate::<Feeds>(json!({ "url": RSS_URL })).is_ok());
    for bad in [
        json!({}),
        json!({ "feeds": [] }),
        json!({ "feeds": "https://x.example.com" }),
        json!({ "feeds": [1] }),
        json!({ "feeds": ["ftp://example.com/feed"] }),
        json!({ "feeds": ["https://"] }),
        json!({ "feeds": ["https://user:pw@example.com/feed"] }),
        json!({ "feeds": ["https://example.com/a feed"] }),
        json!({ "feeds": [format!("https://example.com/{}", "a".repeat(1000))] }),
        json!({ "feeds": [RSS_URL], "max_entries": 0 }),
        json!({ "feeds": [RSS_URL], "max_entries": 501 }),
        json!({ "feeds": vec![RSS_URL; 101] }),
    ] {
        assert!(
            matches!(h.validate::<Feeds>(bad.clone()), Err(Error::Invalid(_))),
            "{bad}"
        );
    }
    let s = Settings::parse(&json!({ "feeds": [RSS_URL, format!("{RSS_URL}#frag"), RSS_URL] }))
        .unwrap();
    assert_eq!(s.feeds, [RSS_URL]);
    assert_eq!(s.max_entries, DEFAULT_MAX_ENTRIES);
}

// ---- the connector ----

#[test]
fn full_pass_indexes_every_format() {
    let h = host();
    h.respond("GET", RSS_URL, http::Response::ok(RSS2));
    h.respond("GET", ATOM_URL, http::Response::ok(ATOM));
    h.respond("GET", RDF_URL, http::Response::ok(RDF));
    let pass = h
        .sync::<Feeds>(settings(&[RSS_URL, ATOM_URL, RDF_URL]), None)
        .unwrap();
    assert_eq!(pass.upserts.len(), 10);
    assert_eq!(pass.cursor["feeds_ok"], 3);

    let d = pass.upserted(&format!("{RSS_URL}#post-1")).unwrap();
    assert_eq!(d.title.as_deref(), Some("Hello & welcome"));
    assert_eq!(d.mime, "text/html");
    assert_eq!(d.text_content(), Some("<p>The <b>full</b> post.</p>"));
    assert_eq!(
        d.url.as_deref(),
        Some("https://blog.example.substack.com/p/hello")
    );
    assert_eq!(d.modified_at, Some(1_136_214_245_000));
    assert_eq!(d.metadata["feed_title"], "Example Blog");
    assert_eq!(d.metadata["author"], "Ada");
    assert_eq!(d.metadata["categories"], json!(["intro", "meta"]));
    assert!(d.version.as_deref().unwrap().starts_with("d:"));

    // Plain-text Atom content is text/plain; an entry with no body falls back to its title.
    let d = pass
        .upserted(&format!("{ATOM_URL}#tag:example.github.io,2006:summary"))
        .unwrap();
    assert_eq!(d.mime, "text/plain");
    let notitle = pass
        .upserts
        .iter()
        .find(|d| d.key.starts_with(&format!("{RSS_URL}#hash:")))
        .unwrap();
    assert_eq!(notitle.title, None);

    // Requests carry the accept header and no validators on a full pass.
    let reqs = h.requests();
    assert_eq!(reqs.len(), 3);
    assert!(reqs[0].header_value("accept").unwrap().contains("rss+xml"));
    assert!(reqs[0].header_value("if-none-match").is_none());
}

#[test]
fn conditional_get_and_incremental_entries() {
    let h = host();
    let fetches = Rc::new(Cell::new(0));
    let body = Rc::new(std::cell::RefCell::new(RSS2.to_string()));
    {
        let fetches = fetches.clone();
        let body = body.clone();
        h.respond_with("GET", RSS_URL, move |req| {
            fetches.set(fetches.get() + 1);
            if req.header_value("if-none-match") == Some("\"v1\"") && fetches.get() == 2 {
                return Ok(http::Response::new(304));
            }
            Ok(http::Response::ok(body.borrow().clone())
                .with_header("ETag", "\"v1\"")
                .with_header("Last-Modified", "Mon, 02 Jan 2006 15:04:05 GMT"))
        });
    }
    let s = settings(&[RSS_URL]);
    let first = h.sync::<Feeds>(s.clone(), None).unwrap();
    assert_eq!(first.upserts.len(), 4);

    // Second pass: 304, nothing sent.
    h.advance(60_000);
    let second = h
        .sync::<Feeds>(s.clone(), Some(first.cursor.clone()))
        .unwrap();
    assert!(second.upserts.is_empty());
    let req = h.requests().pop().unwrap();
    assert_eq!(req.header_value("if-none-match"), Some("\"v1\""));
    assert_eq!(
        req.header_value("if-modified-since"),
        Some("Mon, 02 Jan 2006 15:04:05 GMT")
    );

    // Third pass: the feed changed; only the new and the edited entries are sent.
    *body.borrow_mut() = RSS2
        .replace("A short summary.", "ignored: content:encoded wins")
        .replace(
            "<pubDate>Tue, 03 Jan 2006 10:00:00 +0100</pubDate>",
            "<pubDate>Wed, 04 Jan 2006 10:00:00 +0100</pubDate>",
        )
        .replace(
            "<item>\n      <title>Relative link</title>",
            "<item><guid>new-one</guid><title>New</title></item>\n    <item>\n      <title>Relative link</title>",
        );
    h.advance(60_000);
    let third = h
        .sync::<Feeds>(s.clone(), Some(second.cursor.clone()))
        .unwrap();
    assert_eq!(
        third.upserted_keys(),
        [
            format!("{RSS_URL}#https://blog.example.substack.com/p/second"),
            format!("{RSS_URL}#new-one"),
        ]
    );

    // A full pass re-sends everything, unconditionally.
    let full = h.sync::<Feeds>(s, None).unwrap();
    assert_eq!(full.upserts.len(), 5);
    assert!(
        h.requests()
            .pop()
            .unwrap()
            .header_value("if-none-match")
            .is_none()
    );
}

#[test]
fn state_of_an_uncommitted_pass_is_not_trusted() {
    let h = host();
    h.respond(
        "GET",
        RSS_URL,
        http::Response::ok(RSS2).with_header("ETag", "\"v1\""),
    );
    let s = settings(&[RSS_URL]);
    let first = h.sync::<Feeds>(s.clone(), None).unwrap();
    // A second pass ran (and wrote state) but its cursor was never committed...
    h.advance(1000);
    h.sync::<Feeds>(s.clone(), Some(first.cursor.clone()))
        .unwrap();
    // ...so the host hands the first cursor back again. Nothing may be skipped.
    h.advance(1000);
    let retry = h.sync::<Feeds>(s, Some(first.cursor)).unwrap();
    assert_eq!(retry.upserts.len(), 4);
    assert!(
        h.requests()
            .pop()
            .unwrap()
            .header_value("if-none-match")
            .is_none()
    );
}

#[test]
fn reconciling_pass_drops_removed_feeds() {
    let h = host();
    h.respond("GET", RSS_URL, http::Response::ok(RSS2));
    h.respond("GET", ATOM_URL, http::Response::ok(ATOM));
    h.sync::<Feeds>(settings(&[RSS_URL, ATOM_URL]), None)
        .unwrap();
    assert_eq!(h.kv_keys().len(), 2);

    let pass = h.sync::<Feeds>(settings(&[ATOM_URL]), None).unwrap();
    assert_eq!(pass.deletes.len(), 4);
    assert!(pass.deletes.iter().all(|k| k.starts_with(RSS_URL)));
    assert!(h.known_items().iter().all(|(k, _)| k.starts_with(ATOM_URL)));
    assert_eq!(h.kv_keys().len(), 1);
}

#[test]
fn one_failing_feed_does_not_fail_the_pass() {
    let h = host();
    h.respond("GET", RSS_URL, http::Response::ok(RSS2));
    h.respond("GET", ATOM_URL, http::Response::new(500));
    // Undeclared host: denied by the host.
    let off = "https://blog.example.net/feed";
    let pass = h
        .sync::<Feeds>(settings(&[RSS_URL, ATOM_URL, off]), None)
        .unwrap();
    assert_eq!(pass.upserts.len(), 4);
    assert_eq!(pass.cursor["feeds_ok"], 1);
    assert!(h.logged(Level::Warn, ATOM_URL));
    assert!(h.logged(Level::Warn, "not a host declared"));

    // When every feed fails, the pass fails with the first error.
    let err = h
        .sync::<Feeds>(settings(&[off, ATOM_URL]), None)
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "{err:?}");
}

#[test]
fn redirects_are_reported_not_followed() {
    let h = host();
    h.respond(
        "GET",
        RSS_URL,
        http::Response::new(301).with_header("Location", "https://new.example.com/feed"),
    );
    let err = h.sync::<Feeds>(settings(&[RSS_URL]), None).unwrap_err();
    assert!(
        matches!(&err, Error::Other(m) if m.contains("new.example.com")),
        "{err:?}"
    );
    assert_eq!(h.requests().len(), 1);
}

#[test]
fn honours_max_entries_and_cancellation() {
    let h = host();
    h.respond("GET", RSS_URL, http::Response::ok(RSS2));
    let pass = h
        .sync::<Feeds>(json!({ "feeds": [RSS_URL], "max_entries": 2 }), None)
        .unwrap();
    assert_eq!(pass.upserts.len(), 2);

    let h = host();
    h.respond("GET", RSS_URL, http::Response::ok(RSS2));
    h.cancel_after_upserts(1);
    assert_eq!(
        h.sync::<Feeds>(settings(&[RSS_URL]), None).unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(h.documents().len(), 1);
}

#[test]
fn long_ids_get_hashed_keys() {
    let long = "x".repeat(2000);
    let k = entry_key(RSS_URL, &long);
    assert!(k.len() <= 1024 && k.starts_with(&format!("{RSS_URL}#h:")));
    assert_eq!(k, entry_key(RSS_URL, &long));
    assert_eq!(entry_key(RSS_URL, "a"), format!("{RSS_URL}#a"));
}

#[test]
fn state_stays_within_the_declared_kv_size() {
    // 500 entries in each of 100 feeds must fit the manifest's kv_max_bytes.
    let mut items = String::new();
    for i in 0..500 {
        items.push_str(&format!(
            "<item><guid>https://example.substack.com/p/{i}-{}</guid><pubDate>Mon, 02 Jan 2006 15:04:05 GMT</pubDate></item>",
            "slug".repeat(10)
        ));
    }
    let body = format!("<rss><channel><title>Big</title>{items}</channel></rss>");
    let h = host();
    let feeds: Vec<String> = (0..100)
        .map(|i| format!("https://feed{i}.substack.com/feed"))
        .collect();
    for f in &feeds {
        h.respond("GET", f, http::Response::ok(body.clone()));
    }
    let s = json!({ "feeds": feeds, "max_entries": 500 });
    h.sync::<Feeds>(s, None).unwrap();
    assert!(!h.logged(Level::Warn, "could not save state"));
    assert_eq!(h.kv_keys().len(), 100);
}

// ---- the tool ----

#[test]
fn feed_headlines_tool() {
    let h = host();
    let tools = h.tools::<Feeds>();
    assert_eq!(tools[0].name, "feed_headlines");
    h.respond("GET", ATOM_URL, http::Response::ok(ATOM));
    let out = h
        .call::<Feeds>("feed_headlines", json!({ "url": ATOM_URL, "limit": 2 }))
        .unwrap();
    assert_eq!(out["feed"], "Atom Example");
    assert_eq!(out["format"], "atom");
    assert_eq!(out["items"].as_array().unwrap().len(), 2);
    assert_eq!(out["items"][0]["title"], "HTML content");
    assert_eq!(
        out["items"][0]["link"],
        "https://example.github.io/blog/posts/html.html"
    );
    assert_eq!(out["items"][0]["date_ms"], 1_136_214_245_000i64);
    // Tools never write state.
    assert!(h.kv_keys().is_empty());

    assert!(matches!(
        h.call::<Feeds>("feed_headlines", json!({})),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        h.call::<Feeds>("nope", json!({})),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        h.call::<Feeds>(
            "feed_headlines",
            json!({ "url": "https://example.org/feed" })
        ),
        Err(Error::Denied(_))
    ));
}

#[test]
fn manifest_declares_no_wildcard_tld_and_stays_under_limits() {
    let hosts: Vec<&str> = MANIFEST
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('"') && l.ends_with("\","))
        .collect();
    assert!(hosts.len() >= 10 && hosts.len() <= 64, "{}", hosts.len());
    // The manifest parses in the test host (which applies the host's rules).
    host();
}
