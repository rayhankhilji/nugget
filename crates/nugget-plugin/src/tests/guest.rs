//! Tests that load real plugins built from `sdk/`: the example feed reader and the hostile test
//! plugin. Each test returns early (with a warning printed once) when the guests can't be built.

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::{Change, ConnectorContext, ConnectorFactory, Content};
use nugget_core::Error;
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::{HostConfig, Limits, LoadedPlugin, PluginConnectorFactory, PluginHost};

struct Fx {
    dir: tempfile::TempDir,
    host: PluginHost,
    plugin: Arc<LoadedPlugin>,
    egress: Arc<VecEgress>,
    secrets: Arc<MemSecrets>,
    known: Arc<FakeKnown>,
}

impl Fx {
    fn ctx(&self) -> ConnectorContext {
        ConnectorContext {
            source: SourceId::new("src-1"),
            paths: Paths::rooted(self.dir.path().to_path_buf()),
            egress: self.egress.clone(),
            secrets: self.secrets.clone(),
            known: self.known.clone(),
        }
    }

    fn factory(&self) -> PluginConnectorFactory {
        PluginConnectorFactory::new(self.plugin.clone()).unwrap()
    }

    async fn sync(
        &self,
        settings: Value,
        cursor: Option<Value>,
    ) -> (nugget_core::Result<Value>, Arc<FakeSink>) {
        let sink = Arc::new(FakeSink::default());
        let conn = self.factory().build(&settings, self.ctx()).unwrap();
        let r = conn.sync(cursor, &*sink, &CancellationToken::new()).await;
        (r, sink)
    }

    async fn tool(&self, name: &str, input: Value) -> nugget_core::Result<Value> {
        self.plugin.call_tool(name, &input).await
    }
}

fn test_limits() -> Limits {
    Limits {
        memory_bytes: 32 * 1024 * 1024,
        call_fuel: 2_000_000_000,
        call_timeout: Duration::from_secs(20),
        max_http_response_bytes: 64 * 1024,
        ..Limits::default()
    }
}

/// Install `crate_name` with the given manifest through a [`PluginStore`] and load it.
fn load(crate_name: &str, kind: &str, caps: &str, limits: Limits) -> Option<Fx> {
    let wasm = guest_wasm(crate_name)?;
    let id = format!("dev.nugget.test.{}", crate_name.replace('_', "-"));
    let (dir, store) = temp_store();
    let k = key(9);
    store
        .install(
            &package(&manifest_toml(&id, "1.0.0", kind, caps), &wasm, &k),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    let egress = Arc::new(VecEgress::default());
    let secrets = Arc::new(MemSecrets::default());
    let host = PluginHost::new(HostConfig {
        limits,
        state_dir: store.state_root(),
        egress: egress.clone(),
        secrets: Some(secrets.clone()),
        http_client: None,
        // The tests' servers listen on 127.0.0.1.
        allow_private_network: true,
    })
    .unwrap();
    let plugin = host.load(&store.load(&id).unwrap()).unwrap();
    Some(Fx {
        dir,
        host,
        plugin,
        egress,
        secrets,
        known: Arc::new(FakeKnown::default()),
    })
}

fn hostile(caps: &str) -> Option<Fx> {
    hostile_with(caps, test_limits())
}

fn hostile_with(caps: &str, limits: Limits) -> Option<Fx> {
    load("hostile_plugin", "[\"connector\", \"tool\"]", caps, limits)
}

fn local_host(server: &MockServer) -> String {
    format!("hosts = [\"{}\"]", server.uri())
}

fn assert_denied(r: nugget_core::Result<Value>, what: &str) {
    match r {
        Err(Error::Other(m)) if m.contains("capability denied") => {}
        other => panic!("{what}: expected a capability denial, got {other:?}"),
    }
}

fn assert_err_contains(r: nugget_core::Result<Value>, needle: &str) {
    match r {
        Err(e) if e.to_string().contains(needle) => {}
        other => panic!("expected an error containing `{needle}`, got {other:?}"),
    }
}

const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Test feed</title>
<item><title>First</title><link>https://example.com/1</link><guid>id-1</guid>
<description>&lt;p&gt;one&lt;/p&gt;</description><pubDate>Tue, 27 Oct 2026 10:00:00 +0000</pubDate></item>
<item><title>Second</title><link>https://example.com/2</link><guid>id-2</guid>
<description>two</description></item>
</channel></rss>"#;

const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Atom test</title>
<entry><id>urn:a</id><title>Alpha</title><link href="https://example.com/a"/>
<updated>2026-10-27T10:00:00Z</updated><summary>alpha body</summary></entry>
</feed>"#;

// ---- the example plugin ----

#[tokio::test]
async fn example_feed_syncs_through_the_connector_factory() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v1\"")
                .set_body_string(RSS),
        )
        .mount(&server)
        .await;
    let caps = format!("{}\nkv_max_bytes = 4096", local_host(&server));
    let Some(fx) = load(
        "feed_reader",
        "[\"connector\", \"tool\"]",
        &caps,
        test_limits(),
    ) else {
        return;
    };
    let url = format!("{}/feed.xml", server.uri());
    // An item from a previous feed URL, to be reconciled away on the full pass.
    fx.known
        .0
        .lock()
        .unwrap()
        .insert(ItemKey::new("https://old.example/feed#x"), None);

    let factory = fx.factory();
    let info = factory.info();
    assert_eq!(info.kind, "plugin:dev.nugget.test.feed-reader");
    factory.validate(&json!({ "url": url })).unwrap();
    assert!(factory.validate(&json!({ "url": "ftp://x" })).is_err());
    assert!(factory.validate(&json!({})).is_err());

    let (cursor, sink) = fx.sync(json!({ "url": url }), None).await;
    let cursor = cursor.unwrap();
    assert_eq!(cursor["etag"], "\"v1\"");
    let changes = sink.changes.lock().unwrap().clone();
    assert_eq!(changes.len(), 3, "{changes:?}");
    let Change::Upsert(first) = &changes[0] else {
        panic!("{changes:?}")
    };
    assert_eq!(first.key.0, format!("{url}#id-1"));
    assert_eq!(first.title.as_deref(), Some("First"));
    assert_eq!(first.url.as_deref(), Some("https://example.com/1"));
    assert_eq!(first.content, Content::Text("<p>one</p>".into()));
    assert_eq!(first.metadata["feed_title"], "Test feed");
    assert_eq!(first.modified_at.unwrap().unix_timestamp(), 1_793_095_200);
    assert_eq!(
        changes[2],
        Change::Delete(ItemKey::new("https://old.example/feed#x"))
    );

    // Egress was recorded for the source, and the plugin's kv state persisted.
    let events = fx.egress.0.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].feature, "sync:plugin:dev.nugget.test.feed-reader");
    assert!(events[0].bytes_received > 0);
    let kv_dir = fx
        .dir
        .path()
        .join("plugins/state/dev.nugget.test.feed-reader");
    assert_eq!(std::fs::read_dir(kv_dir).unwrap().count(), 1);

    // Incremental pass: conditional GET, 304, unchanged cursor, no changes.
    let (again, sink) = fx.sync(json!({ "url": url }), Some(cursor.clone())).await;
    assert_eq!(again.unwrap(), cursor);
    assert!(sink.changes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn example_parses_atom_and_serves_its_tool() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/atom"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rss"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS))
        .mount(&server)
        .await;
    let caps = format!("{}\nkv_max_bytes = 4096", local_host(&server));
    let Some(fx) = load(
        "feed_reader",
        "[\"connector\", \"tool\"]",
        &caps,
        test_limits(),
    ) else {
        return;
    };
    let (r, sink) = fx
        .sync(
            json!({ "url": format!("{}/atom", server.uri()) }),
            Some(json!({})),
        )
        .await;
    r.unwrap();
    let changes = sink.changes.lock().unwrap().clone();
    assert_eq!(changes.len(), 1, "no reconciling deletes with a cursor");
    let Change::Upsert(doc) = &changes[0] else {
        panic!()
    };
    assert_eq!(doc.title.as_deref(), Some("Alpha"));
    assert_eq!(doc.version.as_deref(), Some("2026-10-27T10:00:00Z"));

    let tools = fx.plugin.tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "feed_headlines");
    assert_eq!(tools[0].input_schema["required"][0], "url");
    let out = fx
        .tool(
            "feed_headlines",
            json!({ "url": format!("{}/rss", server.uri()), "limit": 1 }),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        json!({ "feed": "Test feed", "items": [
            { "title": "First", "link": "https://example.com/1", "updated": "Tue, 27 Oct 2026 10:00:00 +0000" }
        ]})
    );
    assert_eq!(
        fx.egress.0.lock().unwrap().last().unwrap().feature,
        "tool:plugin:dev.nugget.test.feed-reader"
    );
    // The tool reaches only declared hosts.
    assert_denied(
        fx.tool(
            "feed_headlines",
            json!({ "url": "https://example.com/rss" }),
        )
        .await,
        "undeclared feed host",
    );
    assert!(matches!(
        fx.tool("nope", json!({})).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn sync_honours_cancellation() {
    let Some(fx) = hostile("") else { return };
    let sink = Arc::new(FakeSink::default());
    let conn = fx
        .factory()
        .build(&json!({ "mode": "wait_cancel" }), fx.ctx())
        .unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        c2.cancel();
    });
    let r = conn.sync(None, &*sink, &cancel).await;
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(!sink.changes.lock().unwrap().is_empty());
}

// ---- capability denials ----

#[tokio::test]
async fn undeclared_hosts_are_denied() {
    let allowed = MockServer::start().await;
    let other = MockServer::start().await;
    for s in [&allowed, &other] {
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
            .mount(s)
            .await;
    }
    let Some(fx) = hostile(&local_host(&allowed)) else {
        return;
    };
    let ok = fx
        .tool("fetch", json!({ "url": allowed.uri() }))
        .await
        .unwrap();
    assert_eq!(ok["status"], 200);
    // Same IP, other port; other scheme; other host name; no scheme at all.
    for url in [
        other.uri(),
        allowed.uri().replace("http://", "https://"),
        allowed.uri().replace("127.0.0.1", "localhost"),
        "file:///etc/passwd".to_string(),
        "https://example.com/".to_string(),
    ] {
        assert_denied(fx.tool("fetch", json!({ "url": url })).await, &url);
    }
    assert!(other.received_requests().await.unwrap().is_empty());
    // Only the allowed exchange reached the ledger.
    assert_eq!(fx.egress.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn redirects_are_not_followed_off_host() {
    let allowed = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/x", target.uri())),
        )
        .mount(&allowed)
        .await;
    let Some(fx) = hostile(&local_host(&allowed)) else {
        return;
    };
    let r = fx
        .tool("fetch", json!({ "url": allowed.uri() }))
        .await
        .unwrap();
    assert_eq!(r["status"], 302);
    assert!(target.received_requests().await.unwrap().is_empty());
    // Following it by hand is a new, checked request.
    assert_denied(
        fx.tool("fetch", json!({ "url": r["location"] })).await,
        "redirect target",
    );
}

#[tokio::test]
async fn example_text_tools_need_no_capabilities() {
    let Some(fx) = load("text_tools", "[\"tool\"]", "", test_limits()) else {
        return;
    };
    let names: Vec<_> = fx
        .plugin
        .tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["text_stats", "top_words"]);
    let text = "The cat sat. The cat ran!\n\nA dog barked?";
    assert_eq!(
        fx.tool("text_stats", json!({ "text": text }))
            .await
            .unwrap(),
        json!({ "words": 9, "characters": 40, "characters_no_spaces": 31,
                "sentences": 3, "paragraphs": 2, "reading_time_seconds": 3 })
    );
    assert_eq!(
        fx.tool("top_words", json!({ "text": text, "limit": 2 }))
            .await
            .unwrap(),
        json!({ "words": [{ "word": "cat", "count": 2 }, { "word": "the", "count": 2 }] })
    );
    assert!(matches!(
        fx.tool("text_stats", json!({})).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        fx.tool("nope", json!({})).await,
        Err(Error::NotFound(_))
    ));
    assert!(PluginConnectorFactory::new(fx.plugin.clone()).is_err());
    assert!(fx.egress.0.lock().unwrap().is_empty(), "no network");
}

#[tokio::test]
async fn tools_are_read_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let caps = format!(
        "{}\nkv_max_bytes = 1024\nsecrets = [\"token\"]",
        local_host(&server)
    );
    let Some(fx) = hostile(&caps) else { return };
    for m in ["POST", "PUT", "DELETE"] {
        assert_denied(
            fx.tool("fetch", json!({ "url": server.uri(), "method": m }))
                .await,
            m,
        );
    }
    assert_denied(fx.tool("kv_set", json!({})).await, "kv write in a tool");
    assert_eq!(fx.tool("kv_get", json!({})).await.unwrap(), Value::Null);
    assert_denied(fx.tool("sink", json!({})).await, "sink outside a sync");
}

#[tokio::test]
async fn forbidden_headers_are_denied() {
    let server = MockServer::start().await;
    let Some(fx) = hostile(&local_host(&server)) else {
        return;
    };
    for h in ["Host", "proxy-authorization", "Transfer-Encoding"] {
        assert_denied(
            fx.tool(
                "fetch",
                json!({ "url": server.uri(), "header": [h, "evil.com"] }),
            )
            .await,
            h,
        );
    }
}

#[tokio::test]
async fn oversized_responses_are_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 128 * 1024]))
        .mount(&server)
        .await;
    let Some(fx) = hostile(&local_host(&server)) else {
        return;
    };
    assert_err_contains(
        fx.tool("fetch", json!({ "url": server.uri() })).await,
        "exceeds",
    );
}

#[tokio::test]
async fn secrets_are_scoped_to_declared_names() {
    let Some(fx) = hostile("secrets = [\"token\"]") else {
        return;
    };
    fx.secrets.0.lock().unwrap().extend([
        (
            crate::secret_account(fx.plugin.id(), "token"),
            "s3cret".to_string(),
        ),
        ("oauth:gmail".to_string(), "other source".to_string()),
        ("provider:anthropic".to_string(), "api key".to_string()),
    ]);
    assert_eq!(
        fx.tool("secret", json!({ "name": "token" })).await.unwrap(),
        "s3cret"
    );
    for name in ["password", "oauth:gmail", "../provider:anthropic"] {
        assert_denied(fx.tool("secret", json!({ "name": name })).await, name);
    }
    // In a sync, the source's keychain is used under the same scoping.
    let (r, sink) = fx
        .sync(json!({ "mode": "secret", "name": "token" }), None)
        .await;
    r.unwrap();
    let changes = sink.changes.lock().unwrap().clone();
    let Change::Upsert(d) = &changes[0] else {
        panic!()
    };
    assert_eq!(d.content, Content::Text("s3cret".into()));
    let (r, _) = fx
        .sync(json!({ "mode": "secret", "name": "nope" }), None)
        .await;
    assert_denied(r, "undeclared secret in sync");
}

#[tokio::test]
async fn no_kv_without_declaration_and_quota_holds() {
    let Some(fx) = hostile("") else { return };
    let (r, _) = fx.sync(json!({ "mode": "kv" }), None).await;
    assert_denied(r, "kv without kv_max_bytes");

    let Some(fx) = hostile("kv_max_bytes = 100") else {
        return;
    };
    let (r, _) = fx.sync(json!({ "mode": "kv", "bytes": 50 }), None).await;
    r.unwrap();
    let (r, _) = fx.sync(json!({ "mode": "kv", "bytes": 200 }), None).await;
    assert_err_contains(r, "full");
    // Tools read the tools namespace, not the source's.
    assert_eq!(fx.tool("kv_get", json!({})).await.unwrap(), Value::Null);
}

#[tokio::test]
async fn no_wasi_filesystem_env_or_clock() {
    let Some(fx) = hostile("") else { return };
    for tool in ["env", "fs", "clock"] {
        let r = fx.tool(tool, json!({})).await;
        assert_err_contains(r, "unknown import");
    }
    // A trapped call leaves no damage: the next call gets a fresh instance.
    assert_eq!(
        fx.tool("echo", json!({ "a": 1 })).await.unwrap(),
        json!({ "a": 1 })
    );
}

#[tokio::test]
async fn emitted_documents_are_validated() {
    let Some(fx) = hostile("") else { return };
    let limits = fx.host.limits().clone();
    assert!(limits.max_document_bytes < 32 << 20);
    let (r, _) = fx.sync(json!({ "mode": "bad_metadata" }), None).await;
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    let (r, sink) = fx.sync(json!({ "mode": "count", "count": 2 }), None).await;
    assert_eq!(r.unwrap(), json!({ "done": true }));
    assert_eq!(sink.changes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn oversized_documents_are_refused() {
    let limits = Limits {
        max_document_bytes: 1024 * 1024,
        ..test_limits()
    };
    let Some(fx) = hostile_with("", limits) else {
        return;
    };
    let (r, sink) = fx.sync(json!({ "mode": "huge_doc", "mib": 2 }), None).await;
    assert_err_contains(r, "document content exceeds");
    assert!(sink.changes.lock().unwrap().is_empty());
}

// ---- resource limits ----

#[tokio::test]
async fn fuel_exhaustion_stops_a_spinning_plugin() {
    let limits = Limits {
        call_fuel: 50_000_000,
        ..test_limits()
    };
    let Some(fx) = hostile_with("", limits) else {
        return;
    };
    assert_err_contains(fx.tool("spin", json!({})).await, "out of fuel");
}

#[tokio::test]
async fn the_epoch_deadline_stops_a_plugin_with_fuel_to_spare() {
    let limits = Limits {
        call_fuel: u64::MAX / 2,
        call_timeout: Duration::from_millis(300),
        ..test_limits()
    };
    let Some(fx) = hostile_with("", limits) else {
        return;
    };
    // List the tools first so the timed call is the spin itself.
    fx.plugin.tools().await.unwrap();
    let started = std::time::Instant::now();
    assert_err_contains(fx.tool("spin", json!({})).await, "time limit");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn validate_is_bounded_too() {
    let limits = Limits {
        call_fuel: u64::MAX / 2,
        validate_timeout: Duration::from_millis(200),
        ..test_limits()
    };
    let Some(fx) = hostile_with("", limits) else {
        return;
    };
    let factory = fx.factory();
    factory.validate(&json!({ "ok": true })).unwrap();
    assert!(matches!(
        factory.validate(&json!({})),
        Err(Error::Invalid(_))
    ));
    let err = factory.validate(&json!({ "spin": true })).unwrap_err();
    assert!(err.to_string().contains("time limit"), "{err}");
}

#[tokio::test]
async fn memory_bombs_hit_the_cap() {
    let Some(fx) = hostile("") else { return };
    assert_err_contains(
        fx.tool("alloc", json!({ "mib": 512 })).await,
        "memory limit",
    );
    // Within the cap is fine.
    assert_eq!(
        fx.tool("alloc", json!({ "mib": 4 })).await.unwrap(),
        json!({ "allocated_mib": 4 })
    );
}

#[tokio::test]
async fn panics_become_errors() {
    let Some(fx) = hostile("") else { return };
    assert_err_contains(fx.tool("panic", json!({})).await, "failed");
}

#[tokio::test]
async fn kind_is_enforced() {
    let Some(fx) = load("hostile_plugin", "[\"tool\"]", "", test_limits()) else {
        return;
    };
    assert!(PluginConnectorFactory::new(fx.plugin.clone()).is_err());
    fx.plugin.tools().await.unwrap();
    let Some(fx) = load("hostile_plugin", "[\"connector\"]", "", test_limits()) else {
        return;
    };
    assert!(matches!(
        fx.plugin.tools().await,
        Err(Error::Unsupported(_))
    ));
}

#[tokio::test]
async fn components_with_foreign_imports_are_refused() {
    let wat = r#"(component
        (import "evil:net/socket" (instance (export "connect" (func (param "a" string)))))
    )"#;
    let wasm = wat::parse_str(wat).unwrap();
    let (_d, store) = temp_store();
    let k = key(3);
    store
        .install(
            &package(
                &manifest_toml("dev.nugget.evil", "1.0.0", "[\"tool\"]", ""),
                &wasm,
                &k,
            ),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    let host = PluginHost::new(HostConfig {
        limits: test_limits(),
        state_dir: store.state_root(),
        egress: Arc::new(VecEgress::default()),
        secrets: None,
        http_client: None,
        allow_private_network: false,
    })
    .unwrap();
    let err = host
        .load(&store.load("dev.nugget.evil").unwrap())
        .unwrap_err();
    assert!(err.to_string().contains("evil:net/socket"), "{err}");
    // A component that exports nothing does not match the world either.
    store.remove("dev.nugget.evil").unwrap();
    store
        .install(
            &package(
                &manifest_toml("dev.nugget.evil", "1.0.0", "[\"tool\"]", ""),
                EMPTY_COMPONENT,
                &k,
            ),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    assert!(host.load(&store.load("dev.nugget.evil").unwrap()).is_err());
}

// ---- capabilities beyond what the user approved ----

/// Load `crate_name` with `caps` declared in its manifest but only `approved` approved (as if
/// the index recorded a narrower approval): the host must enforce the intersection.
fn load_with_approval(caps: &str, approved: crate::Capabilities) -> Option<Fx> {
    let mut fx = hostile(caps)?;
    let store = crate::PluginStore::open_at(fx.dir.path().join("plugins")).unwrap();
    let mut verified = store.load(fx.plugin.id()).unwrap();
    verified.installed.approved = approved;
    fx.plugin = fx.host.load(&verified).unwrap();
    Some(fx)
}

#[tokio::test]
async fn declared_but_unapproved_capabilities_are_denied() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let caps = format!(
        "{}\nsecrets = [\"token\"]\nkv_max_bytes = 100\nfile_roots = [\"folder\"]",
        local_host(&server)
    );
    // Approved: nothing but an extra host and root the manifest does not declare.
    let approved = crate::Capabilities {
        hosts: vec!["https://example.com".into()],
        file_roots: vec!["other".into()],
        ..Default::default()
    };
    let Some(fx) = load_with_approval(&caps, approved) else {
        return;
    };
    fx.secrets.0.lock().unwrap().insert(
        crate::secret_account(fx.plugin.id(), "token"),
        "s3cret".into(),
    );
    assert_denied(
        fx.tool("fetch", json!({ "url": server.uri() })).await,
        "declared, unapproved host",
    );
    // Approved but no longer declared: also denied (no network attempt either way).
    assert_denied(
        fx.tool("fetch", json!({ "url": "https://example.com/" }))
            .await,
        "approved, undeclared host",
    );
    assert_denied(
        fx.tool("secret", json!({ "name": "token" })).await,
        "unapproved secret",
    );
    let (r, _) = fx.sync(json!({ "mode": "kv" }), None).await;
    assert_denied(r, "unapproved kv store");
    let path = dir.path().to_str().unwrap();
    for root in ["folder", "other"] {
        let (r, _) = fx
            .sync(
                json!({ "mode": "fs", "root": root, "path": "a.txt", "folder": path, "other": path }),
                None,
            )
            .await;
        assert_denied(r, root);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(fx.egress.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn host_rules_hold_end_to_end() {
    let caps = "hosts = [\"example.com\", \"*.example.org\", \"http://127.0.0.1:9\"]";
    let Some(fx) = hostile(caps) else { return };
    for url in [
        // Scheme downgrade: `example.com` means https only.
        "http://example.com/",
        // Port mismatch.
        "https://example.com:8443/",
        "http://127.0.0.1:10/",
        "https://127.0.0.1:9/",
        // The wildcard does not cover the apex or lookalikes.
        "https://example.org/",
        "https://evilexample.org/",
        "https://example.org.evil.net/",
        // Credentials in the URL, and host confusion through userinfo.
        "https://user:pw@example.com/",
        "https://user@example.com/",
        "https://example.com@evil.net/",
        "https://evil.net/#@example.com",
        "https://evil.net\\@example.com/",
        // Other schemes.
        "ftp://example.com/",
        "ws://example.com/",
        "data:text/plain,hi",
    ] {
        assert_denied(fx.tool("fetch", json!({ "url": url })).await, url);
    }
    // Nothing was sent, so nothing reached the ledger.
    assert!(fx.egress.0.lock().unwrap().is_empty());
}

// ---- file roots (`nugget:plugin/fs`) ----

const FS_CAPS: &str = "file_roots = [\"folder\"]";

/// A temp folder `vault` with a few files, and a secret next to it.
fn vault() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), "alpha").unwrap();
    std::fs::write(root.join(".hidden"), "h").unwrap();
    std::fs::write(root.join("sub").join("b.md"), "# bee").unwrap();
    std::fs::write(dir.path().join("secret.txt"), "top secret").unwrap();
    let root = root.to_str().unwrap().to_string();
    (dir, root)
}

/// Run one `fs` op in a sync and return the JSON the guest emitted.
async fn fs_sync(fx: &Fx, settings: Value) -> nugget_core::Result<Value> {
    let mut settings = settings;
    settings["mode"] = json!("fs");
    let (r, sink) = fx.sync(settings, None).await;
    r?;
    let changes = sink.changes.lock().unwrap().clone();
    let Some(Change::Upsert(doc)) = changes.first() else {
        panic!("{changes:?}")
    };
    let Content::Text(t) = &doc.content else {
        panic!()
    };
    Ok(serde_json::from_str(t).unwrap())
}

fn fs_settings(root: &str, op: &str, path: &str) -> Value {
    json!({ "folder": root, "root": "folder", "op": op, "path": path })
}

#[tokio::test]
async fn connectors_read_inside_their_file_roots() {
    let (_tmp, root) = vault();
    let Some(fx) = hostile(FS_CAPS) else { return };
    let out = fs_sync(&fx, fs_settings(&root, "read", "a.txt"))
        .await
        .unwrap();
    assert_eq!(out, json!({ "len": 5, "text": "alpha" }));
    let out = fs_sync(&fx, fs_settings(&root, "read", "sub/b.md"))
        .await
        .unwrap();
    assert_eq!(out["text"], "# bee");
    let list = fs_sync(&fx, fs_settings(&root, "list", "")).await.unwrap();
    assert_eq!(
        list,
        json!([
            { "name": ".hidden", "is_dir": false, "size": 1 },
            { "name": "a.txt", "is_dir": false, "size": 5 },
            { "name": "sub", "is_dir": true, "size": 0 },
        ])
    );
    let stat = fs_sync(&fx, fs_settings(&root, "stat", "sub"))
        .await
        .unwrap();
    assert_eq!(stat, json!({ "name": "sub", "is_dir": true, "size": 0 }));
    let stat = fs_sync(&fx, fs_settings(&root, "stat", "")).await.unwrap();
    assert_eq!(stat["name"], "");

    // A single file can be the root.
    let file = std::path::Path::new(&root).join("a.txt");
    let out = fs_sync(&fx, fs_settings(file.to_str().unwrap(), "read", ""))
        .await
        .unwrap();
    assert_eq!(out["text"], "alpha");
    let r = fs_sync(&fx, fs_settings(file.to_str().unwrap(), "list", "")).await;
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");

    // `validate` may read too (to check the folder), through the same checks.
    let factory = fx.factory();
    factory
        .validate(&json!({ "folder": root, "fs": { "root": "folder", "op": "stat", "path": "" } }))
        .unwrap();
    assert!(matches!(
        factory.validate(
            &json!({ "folder": root, "fs": { "root": "folder", "op": "read", "path": "../secret.txt" } })
        ),
        Err(Error::Invalid(_))
    ));
    assert!(
        factory
            .validate(
                &json!({ "folder": root, "fs": { "root": "nope", "op": "stat", "path": "" } })
            )
            .unwrap_err()
            .to_string()
            .contains("capability denied")
    );
    assert!(
        fx.egress.0.lock().unwrap().is_empty(),
        "file reads are not egress"
    );
}

#[tokio::test]
async fn file_access_needs_a_declared_root() {
    let (_tmp, root) = vault();
    // No file_roots at all.
    let Some(fx) = hostile("") else { return };
    assert_denied(
        fs_sync(&fx, fs_settings(&root, "read", "a.txt")).await,
        "no file_roots",
    );
    // A root that is not declared, even when the settings have a field of that name.
    let Some(fx) = hostile(FS_CAPS) else { return };
    let r = fs_sync(
        &fx,
        json!({ "other": root, "root": "other", "op": "read", "path": "a.txt" }),
    )
    .await;
    assert_denied(r, "undeclared root");
    // Root names are matched exactly (no traversal through the name).
    for name in ["../folder", "folder/..", "FOLDER", ""] {
        let r = fs_sync(
            &fx,
            json!({ "folder": root, "root": name, "op": "read", "path": "a.txt" }),
        )
        .await;
        assert_denied(r, name);
    }
}

#[tokio::test]
async fn tools_never_get_file_access() {
    let (_tmp, root) = vault();
    let Some(fx) = hostile(FS_CAPS) else { return };
    for op in ["read", "list", "stat"] {
        assert_denied(fx.tool("file", fs_settings(&root, op, "a.txt")).await, op);
    }
}

#[tokio::test]
async fn relative_paths_cannot_leave_the_root() {
    let (_tmp, root) = vault();
    let Some(fx) = hostile(FS_CAPS) else { return };
    let abs_secret = std::path::Path::new(&root)
        .parent()
        .unwrap()
        .join("secret.txt");
    for bad in [
        "../secret.txt",
        "sub/../../secret.txt",
        "..",
        "/etc/passwd",
        abs_secret.to_str().unwrap(),
        "..\\secret.txt",
        "sub\\b.md",
        "a.txt\0",
        "C:\\Windows\\win.ini",
        "C:/Windows/win.ini",
        "\\\\server\\share\\x",
        "./a.txt",
        "sub//b.md",
    ] {
        for op in ["read", "list", "stat"] {
            let r = fs_sync(&fx, fs_settings(&root, op, bad)).await;
            assert!(matches!(r, Err(Error::Invalid(_))), "{op} {bad:?}: {r:?}");
        }
    }
    let r = fs_sync(&fx, fs_settings(&root, "read", "missing.txt")).await;
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
}

#[tokio::test]
async fn the_root_setting_must_be_an_absolute_path() {
    let (_tmp, root) = vault();
    let Some(fx) = hostile(FS_CAPS) else { return };
    let missing = std::path::Path::new(&root).join("nope");
    for (folder, want) in [
        (json!("relative/path"), "absolute"),
        (json!("vault"), "absolute"),
        (json!(""), "not set"),
        (Value::Null, "not set"),
        (json!(["/etc"]), "path string"),
        (json!(42), "path string"),
    ] {
        let r = fs_sync(
            &fx,
            json!({ "folder": folder, "root": "folder", "op": "list", "path": "" }),
        )
        .await;
        match r {
            Err(Error::Invalid(m)) if m.contains(want) => {}
            other => panic!("{folder}: expected Invalid({want}), got {other:?}"),
        }
    }
    // The setting is absent altogether.
    let r = fs_sync(&fx, json!({ "root": "folder", "op": "list", "path": "" })).await;
    assert!(
        matches!(r, Err(Error::Invalid(ref m)) if m.contains("not set")),
        "{r:?}"
    );
    let r = fs_sync(&fx, fs_settings(missing.to_str().unwrap(), "list", "")).await;
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_escape_the_root() {
    use std::os::unix::fs::symlink;
    let (tmp, root) = vault();
    let r = std::path::Path::new(&root);
    symlink(tmp.path().join("secret.txt"), r.join("leak.txt")).unwrap();
    symlink(tmp.path(), r.join("up")).unwrap();
    symlink("/", r.join("slash")).unwrap();
    symlink(r.join("a.txt"), r.join("alias.txt")).unwrap();
    let Some(fx) = hostile(FS_CAPS) else { return };
    for (op, path) in [
        ("read", "leak.txt"),
        ("stat", "leak.txt"),
        ("read", "up/secret.txt"),
        ("list", "up"),
        ("list", "slash"),
        ("read", "slash/etc/hosts"),
    ] {
        let res = fs_sync(&fx, fs_settings(&root, op, path)).await;
        match res {
            Err(Error::Other(m)) if m.contains("capability denied") => {}
            Err(Error::NotFound(_)) if path.starts_with("slash/") => {}
            other => panic!("{op} {path}: expected a denial, got {other:?}"),
        }
    }
    // Links inside the root work; links out of it are left out of listings.
    let out = fs_sync(&fx, fs_settings(&root, "read", "alias.txt"))
        .await
        .unwrap();
    assert_eq!(out["text"], "alpha");
    let names: Vec<String> = fs_sync(&fx, fs_settings(&root, "list", ""))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, [".hidden", "a.txt", "alias.txt", "sub"]);
}

#[cfg(unix)]
#[tokio::test]
async fn special_files_are_not_read() {
    let (_tmp, root) = vault();
    let fifo = std::path::Path::new(&root).join("pipe");
    let ok = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("WARNING: mkfifo unavailable; skipping");
        return;
    }
    let Some(fx) = hostile(FS_CAPS) else { return };
    // Reading a FIFO would block forever; it is refused up front.
    let r = fs_sync(&fx, fs_settings(&root, "read", "pipe")).await;
    assert!(
        matches!(r, Err(Error::Invalid(ref m)) if m.contains("regular")),
        "{r:?}"
    );
}

#[tokio::test]
async fn file_size_listing_and_read_budget_caps_hold() {
    let (_tmp, root) = vault();
    let r = std::path::Path::new(&root);
    std::fs::write(r.join("big.bin"), vec![b'x'; 2048]).unwrap();
    std::fs::write(r.join("mid.bin"), vec![b'y'; 1000]).unwrap();
    let crowded = r.join("crowded");
    std::fs::create_dir(&crowded).unwrap();
    for i in 0..11 {
        std::fs::write(crowded.join(format!("f{i}")), "").unwrap();
    }
    let limits = Limits {
        max_fs_file_bytes: 1024,
        max_fs_dir_entries: 10,
        max_fs_read_bytes_per_sync: 2500,
        max_fs_read_bytes_per_call: 1500,
        ..test_limits()
    };
    let Some(fx) = hostile_with(FS_CAPS, limits) else {
        return;
    };
    let res = fs_sync(&fx, fs_settings(&root, "read", "big.bin")).await;
    assert!(
        matches!(res, Err(Error::Invalid(ref m)) if m.contains("larger than 1024")),
        "{res:?}"
    );
    assert_eq!(
        fs_sync(&fx, fs_settings(&root, "read", "mid.bin"))
            .await
            .unwrap()["len"],
        1000
    );
    // 11 entries > 10: the listing fails rather than being cut short silently.
    let res = fs_sync(&fx, fs_settings(&root, "list", "crowded")).await;
    assert!(
        matches!(res, Err(Error::Invalid(ref m)) if m.contains("more than 10")),
        "{res:?}"
    );
    std::fs::remove_file(crowded.join("f10")).unwrap();
    let list = fs_sync(&fx, fs_settings(&root, "list", "crowded"))
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 10);

    // Two reads of 1000 fit the sync budget of 2500; a third does not.
    let mut many = fs_settings(&root, "read_many", "");
    many["paths"] = json!(["mid.bin", "mid.bin"]);
    assert_eq!(fs_sync(&fx, many.clone()).await.unwrap()["len"], 2000);
    many["paths"] = json!(["mid.bin", "mid.bin", "mid.bin"]);
    assert_denied(fs_sync(&fx, many).await, "sync read budget");
    // The budget is per call: the next sync starts afresh. `validate` has its own, smaller one.
    let factory = fx.factory();
    factory
        .validate(&json!({ "folder": root, "fs": { "root": "folder", "op": "read_many", "paths": ["mid.bin"] } }))
        .unwrap();
    let err = factory
        .validate(&json!({ "folder": root, "fs": { "root": "folder", "op": "read_many", "paths": ["mid.bin", "mid.bin"] } }))
        .unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
}

// ---- CPU and memory limits in a connector sync ----

#[tokio::test]
async fn sync_fuel_stops_a_spinning_connector() {
    let limits = Limits {
        sync_fuel: 50_000_000,
        ..test_limits()
    };
    let Some(fx) = hostile_with("", limits) else {
        return;
    };
    let (r, _) = fx.sync(json!({ "mode": "spin" }), None).await;
    assert_err_contains(r, "out of fuel");
}

#[tokio::test]
async fn the_sync_deadline_stops_a_connector_with_fuel_to_spare() {
    let limits = Limits {
        sync_fuel: u64::MAX / 2,
        sync_timeout: Duration::from_millis(300),
        ..test_limits()
    };
    let Some(fx) = hostile_with("", limits) else {
        return;
    };
    let started = std::time::Instant::now();
    let (r, _) = fx.sync(json!({ "mode": "spin" }), None).await;
    assert_err_contains(r, "time limit");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn sync_memory_is_capped() {
    let Some(fx) = hostile("") else { return };
    let (r, _) = fx.sync(json!({ "mode": "alloc", "mib": 512 }), None).await;
    assert_err_contains(r, "memory limit");
    let (r, _) = fx.sync(json!({ "mode": "alloc", "mib": 4 }), None).await;
    r.unwrap();
}
