//! The first-party plugins in `plugins/` (rss, obsidian, read-later), loaded in the real
//! [`PluginHost`] with their own manifests and synced against local fixtures.
//!
//! These need the release components built beforehand, and run only when
//! `NUGGET_FIRST_PARTY_WASM_DIR` names the folder holding `nugget_rss.wasm`,
//! `nugget_obsidian.wasm` and `nugget_read_later.wasm`:
//!
//! ```text
//! (cd plugins && CARGO_TARGET_DIR=$PWD/../target/plugins cargo build --release --workspace)
//! NUGGET_FIRST_PARTY_WASM_DIR=$PWD/target/plugins/wasm32-wasip2/release cargo test -p nugget-plugin first_party
//! ```
//!
//! Without the variable each test prints a note and passes.

use std::path::PathBuf;
use std::sync::Arc;

use nugget_connector::{Change, ConnectorContext, ConnectorFactory, Content, Document};
use nugget_core::ids::SourceId;
use nugget_core::paths::Paths;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::{HostConfig, Limits, LoadedPlugin, PluginConnectorFactory, PluginHost};

const ENV: &str = "NUGGET_FIRST_PARTY_WASM_DIR";

struct Fx {
    dir: tempfile::TempDir,
    _host: PluginHost,
    plugin: Arc<LoadedPlugin>,
}

fn repo_plugins() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// The built component, or `None` (with a note) when the variable is unset.
fn wasm(crate_name: &str) -> Option<Vec<u8>> {
    let Some(dir) = std::env::var_os(ENV) else {
        eprintln!("note: {ENV} is not set; skipping the first-party plugin test");
        return None;
    };
    let file = PathBuf::from(dir).join(format!("{crate_name}.wasm"));
    Some(std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display())))
}

/// Install a plugin with its own manifest (optionally edited) and load it.
fn load(folder: &str, crate_name: &str, edit: impl FnOnce(String) -> String) -> Option<Fx> {
    let wasm = wasm(crate_name)?;
    let manifest =
        std::fs::read_to_string(repo_plugins().join(folder).join("nugget-plugin.toml")).unwrap();
    let manifest = edit(manifest);
    let id = crate::Manifest::parse(manifest.as_bytes()).unwrap().id;
    let (dir, store) = temp_store();
    let k = key(11);
    store
        .install(
            &package(&manifest, &wasm, &k),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    let host = PluginHost::new(HostConfig {
        limits: Limits::default(),
        state_dir: store.state_root(),
        egress: Arc::new(VecEgress::default()),
        secrets: Some(Arc::new(MemSecrets::default())),
        http_client: None,
        // The mock feed server listens on 127.0.0.1.
        allow_private_network: true,
    })
    .unwrap();
    let plugin = host.load(&store.load(&id).unwrap()).unwrap();
    Some(Fx {
        dir,
        _host: host,
        plugin,
    })
}

impl Fx {
    async fn sync(&self, settings: Value) -> Vec<Document> {
        let ctx = ConnectorContext {
            source: SourceId::new("src-1"),
            paths: Paths::rooted(self.dir.path().to_path_buf()),
            egress: Arc::new(VecEgress::default()),
            secrets: Arc::new(MemSecrets::default()),
            known: Arc::new(FakeKnown::default()),
        };
        let factory = PluginConnectorFactory::new(self.plugin.clone()).unwrap();
        let conn = factory.build(&settings, ctx).unwrap();
        let sink = FakeSink::default();
        conn.sync(None, &sink, &CancellationToken::new())
            .await
            .unwrap();
        let changes = sink.changes.lock().unwrap().clone();
        changes
            .into_iter()
            .filter_map(|c| match c {
                Change::Upsert(d) => Some(d),
                Change::Delete(_) => None,
            })
            .collect()
    }
}

fn find<'a>(docs: &'a [Document], key: &str) -> &'a Document {
    docs.iter()
        .find(|d| d.key.as_str() == key)
        .unwrap_or_else(|| panic!("no document `{key}` in {:?}", keys(docs)))
}

fn keys(docs: &[Document]) -> Vec<&str> {
    docs.iter().map(|d| d.key.as_str()).collect()
}

fn text(d: &Document) -> &str {
    match &d.content {
        Content::Text(t) => t,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn rss_syncs_a_feed_from_a_declared_host() {
    let server = MockServer::start().await;
    let rss = r#"<?xml version="1.0"?>
<!DOCTYPE rss [<!ENTITY big "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA">]>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel>
<title>Café feed</title><link>https://example.com/</link>
<item><title>Ünïcode</title><link>https://example.com/1</link><guid>id-1</guid>
<content:encoded><![CDATA[<p>full &nbsp; text</p>]]></content:encoded>
<pubDate>Tue, 27 Oct 2026 10:00:00 +0000</pubDate></item>
<item><title>Far future</title><guid>id-2</guid><description>&big;&big;</description>
<pubDate>Mon, 02 Jan 99999 15:04:05 GMT</pubDate></item>
</channel></rss>"#;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(rss))
        .mount(&server)
        .await;
    let origin = server.uri();
    let Some(fx) = load("rss", "nugget_rss", |m| {
        m.replacen("hosts = [", &format!("hosts = [\n    \"{origin}\","), 1)
    }) else {
        return;
    };
    let url = format!("{origin}/feed.xml");
    let docs = fx.sync(json!({ "feeds": [url] })).await;
    assert_eq!(docs.len(), 2, "{:?}", keys(&docs));

    let d = find(&docs, &format!("{url}#id-1"));
    assert_eq!(d.title.as_deref(), Some("Ünïcode"));
    assert_eq!(text(d), "<p>full &nbsp; text</p>");
    assert!(d.modified_at.is_some());
    assert_eq!(d.metadata["feed_title"], "Café feed");

    // A date the store cannot hold is dropped, not fatal; the DTD entity is never expanded.
    let d = find(&docs, &format!("{url}#id-2"));
    assert_eq!(d.modified_at, None);
    assert!(!text(d).contains("AAAA"), "{}", text(d));

    let out = fx
        .plugin
        .call_tool("feed_headlines", &json!({ "url": url, "limit": 1 }))
        .await
        .unwrap();
    assert_eq!(out["items"][0]["title"], "Ünïcode");
}

#[tokio::test]
async fn obsidian_imports_a_vault_folder() {
    let Some(fx) = load("obsidian", "nugget_obsidian", |m| m) else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("Vault");
    std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
    std::fs::create_dir_all(vault.join("Дневник")).unwrap();
    std::fs::write(vault.join(".obsidian").join("app.md"), "settings").unwrap();
    std::fs::write(
        vault.join("Привет.md"),
        "---\ntags: [hello]\n---\n# Hi\nSee [[Дневник/день]] #тег",
    )
    .unwrap();
    std::fs::write(vault.join("Дневник").join("день.md"), "day").unwrap();
    std::fs::write(vault.join("image.png"), [0u8; 4]).unwrap();

    let docs = fx.sync(json!({ "vault": vault.to_str().unwrap() })).await;
    let mut got = keys(&docs);
    got.sort();
    assert_eq!(got, ["Дневник/день.md", "Привет.md"]);
    let d = find(&docs, "Привет.md");
    assert_eq!(d.title.as_deref(), Some("Привет"));
    assert_eq!(d.mime, "text/markdown");
    assert_eq!(d.metadata["tags"], json!(["hello", "тег"]));
    assert_eq!(d.metadata["links"], json!(["Дневник/день"]));
    assert!(d.modified_at.is_some());
}

#[tokio::test]
async fn read_later_imports_an_export_file() {
    let Some(fx) = load("read-later", "nugget_read_later", |m| m) else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("ril_export.html");
    let pad = "é".repeat(300); // multibyte text across the format-sniffing boundary
    std::fs::write(
        &file,
        format!(
            "<!DOCTYPE html><html><head><title>Pocket Export {pad}</title></head><body>\
             <h1>Unread</h1><ul>\
             <li><a href=\"https://example.com/a?utm_source=x\" time_added=\"1500000000\" tags=\"rust,ünï\">Ärticle</a></li>\
             <li><a href=\"https://user:pw@example.com/b\" time_added=\"999999999999\">Far</a></li>\
             </ul></body></html>"
        ),
    )
    .unwrap();
    let docs = fx.sync(json!({ "export": file.to_str().unwrap() })).await;
    assert_eq!(docs.len(), 2, "{:?}", keys(&docs));
    let d = find(&docs, "https://example.com/a");
    assert_eq!(d.title.as_deref(), Some("Ärticle"));
    assert_eq!(d.metadata["tags"], json!(["rust", "ünï"]));
    assert!(d.modified_at.is_some());
    // Out-of-range save time dropped; credentials stripped.
    let d = find(&docs, "https://example.com/b");
    assert_eq!(d.modified_at, None);
    assert_eq!(d.url.as_deref(), Some("https://example.com/b"));
}
