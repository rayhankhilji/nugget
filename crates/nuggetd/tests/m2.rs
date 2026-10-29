//! M2 end-to-end on a temporary home: Markdown and mail fixtures indexed through files sources,
//! knowledge extraction, the knowledge IPC methods, the vault (pages with citations, user edits
//! becoming `user_asserted` facts), merges and claim-status changes; MCP clients over IPC, the
//! loopback HTTP server (search, get_entity, scopes, revocation, audit), the real `nuggetd mcp
//! --client` stdio bridge, and `nuggetd mcp install`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use nugget_core::ids::EntityId;
use nugget_core::ipc::{
    EntityDetail, EntityRef, EntitySummaryView, GraphViewIpc, KnowledgeStatus, McpClientCreated,
    McpClientInfo, SourceInfo, method, notification,
};
use nugget_core::paths::Paths;
use nugget_ingest::{MemorySecrets, NoPowerPolicy};
use nugget_ipc::{Client, IpcError, Notification};
use nugget_store::{Store, StoreKey};
use nuggetd::{Daemon, DaemonOptions, Startup};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const EXE: &str = env!("CARGO_BIN_EXE_nuggetd");
const CALL: Duration = Duration::from_secs(30);
const EVENTUALLY: Duration = Duration::from_secs(60);

const CONFIG: &str = r#"
[runtime]
enabled = false

[knowledge]
debounce_ms = 100
interval_minutes = 0

[knowledge.me]
name = "Tanuj"
emails = ["tanuj@trylumi.co"]

[vault]
debounce = 150

[mcp]
port = 0
"#;

const APOLLO: &str = "---\ntype: project\nstatus: active\naliases: [Apollo]\npeople: [\"[[Alice Smith]]\"]\ntags: [rust]\n---\n# Project Apollo\n\nWe are rebuilding the indexer in #rust with Alice.\n";

fn mail(from: &str, subject: &str, id: &str, body: &str) -> String {
    format!(
        "From: {from}\r\nTo: Tanuj <tanuj@trylumi.co>\r\nSubject: {subject}\r\nMessage-ID: <{id}@example.com>\r\nDate: Mon, 26 Oct 2026 10:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
}

struct Harness {
    tmp: tempfile::TempDir,
    home: PathBuf,
    store: Store,
    client: Client,
    vault: PathBuf,
    mcp_addr: Option<std::net::SocketAddr>,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

impl Harness {
    async fn start() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("nugget-m2-")
            .tempdir()
            .unwrap();
        let home = tmp.path().join("home");
        let paths = Paths::rooted(home.clone());
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), CONFIG).unwrap();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes([3; 32]));
        opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
        opts.power = Some(Arc::new(NoPowerPolicy));
        opts.home = Some(home.clone());
        opts.nuggetd_exe = Some(PathBuf::from(EXE));
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let store = daemon.store();
        let vault = daemon.vault_root().expect("vault enabled by default");
        let mcp_addr = daemon.mcp_http_addr();
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&paths).await.unwrap();
        Self {
            tmp,
            home,
            store,
            client,
            vault,
            mcp_addr,
            stop: Some(tx),
            server: Some(server),
        }
    }

    async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(CALL, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    async fn call_err(&self, m: &str, params: Value) -> String {
        match self.client.call_value(m, params).await {
            Err(IpcError::Rpc(e)) => e.message,
            other => panic!("{m}: expected an RPC error, got {other:?}"),
        }
    }

    /// Add a files source over a fresh directory holding `files`.
    async fn add_files(&self, dir: &str, files: &[(&str, String)]) -> SourceInfo {
        let root = self.tmp.path().join(dir);
        std::fs::create_dir_all(&root).unwrap();
        for (name, text) in files {
            std::fs::write(root.join(name), text).unwrap();
        }
        self.call(
            method::SOURCES_ADD,
            json!({"kind": "files", "name": dir, "settings": {"roots": [root], "exclude": []}}),
        )
        .await
    }

    async fn entities(&self, kind: &str) -> Vec<EntitySummaryView> {
        self.call(
            method::KNOWLEDGE_ENTITIES,
            json!({"kind": kind, "limit": 100}),
        )
        .await
    }

    async fn find(&self, kind: &str, name: &str) -> Option<EntitySummaryView> {
        self.entities(kind)
            .await
            .into_iter()
            .find(|e| e.name == name)
    }

    /// Wait until an entity of `kind` named `name` exists.
    async fn wait_entity(&self, kind: &str, name: &str) -> EntitySummaryView {
        let deadline = tokio::time::Instant::now() + EVENTUALLY;
        loop {
            if let Some(e) = self.find(kind, name).await {
                return e;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {kind} {name}; have {:?}",
                self.entities(kind).await
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn entity(&self, id: &EntityId) -> EntityDetail {
        self.call(method::KNOWLEDGE_ENTITY, json!({"id": id})).await
    }

    /// Wait until the entity's detail satisfies `ok`.
    async fn wait_detail(&self, id: &EntityId, ok: impl Fn(&EntityDetail) -> bool) -> EntityDetail {
        let deadline = tokio::time::Instant::now() + EVENTUALLY;
        loop {
            let d = self.entity(id).await;
            if ok(&d) {
                return d;
            }
            assert!(tokio::time::Instant::now() < deadline, "timed out: {d:#?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn audit_actions(&self) -> Vec<String> {
        self.store
            .recent_audit(1000)
            .unwrap()
            .into_iter()
            .map(|r| r.action)
            .collect()
    }

    async fn create_client(&self, name: &str, scopes: Value) -> McpClientCreated {
        self.call(
            method::MCP_CLIENTS_CREATE,
            json!({"name": name, "scopes": scopes}),
        )
        .await
    }

    async fn stop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(CALL, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }
}

async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + EVENTUALLY;
    loop {
        if f().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn collect(client: &Client) -> Arc<Mutex<Vec<Notification>>> {
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut notes = client.notifications();
    let o = out.clone();
    tokio::spawn(async move {
        while let Some(n) = notes.next().await {
            o.lock().unwrap().push(n);
        }
    });
    out
}

fn mail_corpus() -> Vec<(&'static str, String)> {
    vec![
        (
            "lunch.eml",
            mail(
                "Alice Smith <alice@acme.com>",
                "Lunch on Friday",
                "m1",
                "Shall we meet at noon? Alice",
            ),
        ),
        (
            "invoice.eml",
            mail(
                "Bob Jones <bob@globex.com>",
                "Globex invoice",
                "m2",
                "Please find the invoice attached.",
            ),
        ),
    ]
}

fn markdown_corpus() -> Vec<(&'static str, String)> {
    vec![("Apollo.md", APOLLO.to_owned())]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn knowledge_graph_vault_and_edits_end_to_end() {
    let mut h = Harness::start().await;
    let seen = collect(&h.client);
    h.add_files("notes", &markdown_corpus()).await;
    let mail_src = h.add_files("mail", &mail_corpus()).await.id;

    // People and organisations from mail headers; the project from Markdown frontmatter.
    let alice = h.wait_entity("person", "Alice Smith").await;
    h.wait_entity("organization", "Acme").await;
    h.wait_entity("organization", "Globex").await;
    let bob = h.wait_entity("person", "Bob Jones").await;
    h.wait_entity("project", "Apollo").await;
    assert!(
        h.find("person", "Tanuj").await.is_some(),
        "the user's entity"
    );
    let err = h
        .call_err(
            method::KNOWLEDGE_ENTITIES,
            json!({"kind": "planet", "limit": 5}),
        )
        .await;
    assert!(err.contains("planet"), "{err}");

    // Facts carry evidence with titles from the store.
    let detail = h
        .wait_detail(&alice.id, |d| {
            // The vault renders right after extraction; wait for both.
            d.claims.iter().any(|c| c.predicate == "works_at")
                && d.claims.iter().any(|c| c.predicate == "works_on")
                && d.vault_path.is_some()
        })
        .await;
    let works_at = detail
        .claims
        .iter()
        .find(|c| c.predicate == "works_at")
        .unwrap()
        .clone();
    assert_eq!(works_at.status, "active");
    assert_eq!(
        works_at.object.entity.as_ref().map(|e| e.name.as_str()),
        Some("Acme")
    );
    let ev = &works_at.evidence[0];
    // Mail files are titled by their Subject (other files by their file name).
    assert_eq!(ev.title.as_deref(), Some("Lunch on Friday"), "{ev:?}");
    assert!(
        ev.url.as_deref().is_some_and(|u| u.starts_with("file:")),
        "{ev:?}"
    );
    assert!(
        detail
            .identifiers
            .contains(&("email".to_owned(), "alice@acme.com".to_owned()))
    );
    assert!(!detail.mentions.is_empty());
    assert!(detail.mentions.iter().any(|m| m.title.is_some()));

    // The vault page exists, with citations.
    let rel = detail.vault_path.clone().expect("alice has a vault page");
    let page = h.vault.join(&rel);
    let text = std::fs::read_to_string(&page).unwrap();
    assert!(text.contains(alice.id.as_str()), "{text}");
    assert!(text.contains("[^1]: "), "citation footnotes: {text}");
    assert!(
        text.contains("[Lunch on Friday]("),
        "cited by title with a link: {text}"
    );

    let st: KnowledgeStatus = h.call(method::KNOWLEDGE_STATUS, json!({})).await;
    assert!(
        st.entities >= 6 && st.claims > 0 && st.mentions > 0,
        "{st:?}"
    );
    assert!(st.last_run.is_some());
    assert_eq!(st.extractor_provider, None);
    assert!(st.vault.pages >= 6, "{:?}", st.vault);
    assert!(st.vault.path.is_some());
    assert!(st.vault.invalid.is_empty(), "{:?}", st.vault.invalid);

    // Graph: overview of the most connected entities, and a neighbourhood.
    let g: GraphViewIpc = h
        .call(method::KNOWLEDGE_GRAPH, json!({"depth": 1, "limit": 3}))
        .await;
    assert_eq!(g.nodes.len(), 3);
    assert!(g.truncated);
    let g: GraphViewIpc = h
        .call(
            method::KNOWLEDGE_GRAPH,
            json!({"center": alice.id, "depth": 1, "limit": 50}),
        )
        .await;
    assert!(g.nodes.iter().any(|n| n.name == "Acme"), "{g:?}");
    assert!(g.edges.iter().any(|e| e.predicate == "works_at"));
    let only_orgs: GraphViewIpc = h
        .call(
            method::KNOWLEDGE_GRAPH,
            json!({"center": alice.id, "depth": 1, "limit": 50, "kinds": ["organization"]}),
        )
        .await;
    assert!(
        only_orgs
            .nodes
            .iter()
            .all(|n| n.kind == "organization" || n.id == alice.id)
    );

    // A fact added in the vault becomes a user-asserted fact.
    let before_edit = seen.lock().unwrap().len();
    let edited = std::fs::read_to_string(&page).unwrap().replace(
        "<!-- nugget:end facts -->",
        "\n### Hobbies\n- Plays chess\n<!-- nugget:end facts -->",
    );
    std::fs::write(&page, edited).unwrap();
    eventually("vault edit becomes a user_asserted fact", || async {
        h.entity(&alice.id).await.claims.iter().any(|c| {
            c.text == "Plays chess" && c.status == "user_asserted" && c.evidence.is_empty()
        })
    })
    .await;
    eventually("page re-rendered with the user's fact", || async {
        std::fs::read_to_string(&page)
            .unwrap()
            .contains("- Plays chess (you)")
    })
    .await;
    // The watcher signals applied edits; the knowledge worker (which does not poll) tells
    // clients.
    eventually(
        "the vault edit is broadcast as knowledge.changed",
        || async {
            seen.lock().unwrap()[before_edit..].iter().any(|n| {
                n.method == notification::KNOWLEDGE_CHANGED
                    && n.params["entities"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| v == alice.id.as_str()))
            })
        },
    )
    .await;

    // Rejecting and restoring a fact; both audited, and the vault follows.
    let _: Value = h
        .call(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": works_at.id, "status": "rejected"}),
        )
        .await;
    let d = h.entity(&alice.id).await;
    let c = d.claims.iter().find(|c| c.id == works_at.id).unwrap();
    assert_eq!(c.status, "rejected");
    assert!(
        !std::fs::read_to_string(&page)
            .unwrap()
            .contains(&format!("<!-- c:{} -->", works_at.id)),
        "rejected fact left the page"
    );
    let _: Value = h
        .call(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": works_at.id, "status": "active"}),
        )
        .await;
    let d = h.entity(&alice.id).await;
    assert_eq!(
        d.claims
            .iter()
            .find(|c| c.id == works_at.id)
            .unwrap()
            .status,
        "active"
    );
    let err = h
        .call_err(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": works_at.id, "status": "superseded"}),
        )
        .await;
    assert!(err.contains("rejected"), "{err}");

    // Undo a user-asserted fact, then redo it.
    let chess = h
        .entity(&alice.id)
        .await
        .claims
        .into_iter()
        .find(|c| c.text == "Plays chess")
        .unwrap();
    let err = h
        .call_err(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": works_at.id, "status": "unasserted"}),
        )
        .await;
    assert!(err.contains("asserted"), "{err}");
    let status_of = |d: &EntityDetail, id: i64| {
        d.claims
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.status.clone())
    };
    let _: Value = h
        .call(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": chess.id, "status": "unasserted"}),
        )
        .await;
    let d = h.entity(&alice.id).await;
    assert_eq!(status_of(&d, chess.id).as_deref(), Some("retracted"));
    eventually("withdrawn fact left the page", || async {
        !std::fs::read_to_string(&page)
            .unwrap()
            .contains("Plays chess")
    })
    .await;
    let _: Value = h
        .call(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": chess.id, "status": "user_asserted"}),
        )
        .await;
    let d = h.entity(&alice.id).await;
    assert_eq!(status_of(&d, chess.id).as_deref(), Some("user_asserted"));

    // Source filter: the mail source supports `works_at`, not the Markdown project.
    let g: GraphViewIpc = h
        .call(
            method::KNOWLEDGE_GRAPH,
            json!({"center": alice.id, "depth": 1, "limit": 50, "source": mail_src}),
        )
        .await;
    assert!(g.nodes.iter().any(|n| n.name == "Acme"), "{g:?}");
    assert!(g.nodes.iter().all(|n| n.name != "Apollo"), "{g:?}");
    assert!(g.edges.iter().all(|e| e.predicate != "works_on"), "{g:?}");

    // Split: the chess fact moves to a new entity.
    let split: EntityRef = h
        .call(
            method::KNOWLEDGE_SPLIT,
            json!({"entity": alice.id, "claims": [chess.id], "name": "Alice (chess club)"}),
        )
        .await;
    assert_ne!(split.id, alice.id);
    let other = h.entity(&split.id).await;
    assert_eq!(other.name, "Alice (chess club)");
    assert_eq!(other.kind, "person");
    assert!(other.claims.iter().any(|c| c.id == chess.id));
    assert!(status_of(&h.entity(&alice.id).await, chess.id).is_none());
    assert!(
        h.call_err(
            method::KNOWLEDGE_SPLIT,
            json!({"entity": alice.id, "claims": [], "name": "X"})
        )
        .await
        .contains("at least one")
    );
    assert!(
        h.call_err(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": 999_999, "status": "active"})
        )
        .await
        .contains("999999")
    );

    // Merge: Bob into Alice. Bob's id resolves to Alice; his page becomes a redirect stub.
    let bob_page = h.entity(&bob.id).await.vault_path.unwrap();
    let _: Value = h
        .call(
            method::KNOWLEDGE_MERGE,
            json!({"into": alice.id, "from": bob.id}),
        )
        .await;
    let merged = h.entity(&bob.id).await;
    assert_eq!(merged.id, alice.id);
    assert!(h.find("person", "Bob Jones").await.is_none());
    assert!(
        merged
            .identifiers
            .contains(&("email".to_owned(), "bob@globex.com".to_owned()))
    );
    let stub = std::fs::read_to_string(h.vault.join(&bob_page)).unwrap();
    assert!(
        stub.contains(alice.id.as_str()) || stub.contains("Alice"),
        "{stub}"
    );
    assert!(
        h.call_err(
            method::KNOWLEDGE_MERGE,
            json!({"into": alice.id, "from": alice.id})
        )
        .await
        .contains("itself")
    );

    let actions = h.audit_actions();
    for a in [
        "knowledge.merge",
        "knowledge.claim.status",
        "knowledge.split",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} audited: {actions:?}");
    }
    let changed: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|n| n.method == notification::KNOWLEDGE_CHANGED)
        .map(|n| n.params.clone())
        .collect();
    assert!(!changed.is_empty());
    assert!(
        changed.iter().any(|p| p["entities"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == bob.id.as_str()))),
        "the merge was notified: {changed:?}"
    );
    h.stop().await;
}

// ---- MCP ----

type McpClient = RunningService<rmcp::RoleClient, ()>;

async fn http_client(url: &str, token: &str) -> Result<McpClient, String> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned()),
    );
    ().serve(transport).await.map_err(|e| e.to_string())
}

async fn tool(client: &McpClient, name: &str, args: Value) -> String {
    let res: CallToolResult = client
        .call_tool(
            CallToolRequestParams::new(name.to_owned())
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut out = String::new();
    for c in &res.content {
        if let Some(t) = c.as_text() {
            out.push_str(&t.text);
        }
    }
    if let Some(s) = &res.structured_content {
        out.push_str(&s.to_string());
    }
    out
}

fn all_scopes() -> Value {
    json!({"sources": null, "tools": [], "include_sensitive": false})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_http_scopes_revocation_and_audit() {
    let mut h = Harness::start().await;
    let notes = h.add_files("notes", &markdown_corpus()).await;
    h.add_files("mail", &mail_corpus()).await;
    let alice = h.wait_entity("person", "Alice Smith").await;
    h.wait_detail(&alice.id, |d| {
        d.claims.iter().any(|c| c.predicate == "works_at")
            && d.claims.iter().any(|c| c.predicate == "works_on")
    })
    .await;
    eventually("mail searchable", || async {
        let hits: Vec<Value> = h
            .call(method::SEARCH, json!({"query": "invoice", "limit": 5}))
            .await;
        !hits.is_empty()
    })
    .await;

    let addr = h.mcp_addr.expect("http listener on a free port");
    assert!(addr.ip().is_loopback());
    let url = format!("http://{addr}/mcp");

    // Create: token shown once, with snippets for every app, launching this nuggetd on this home.
    let created = h.create_client("Claude Code", all_scopes()).await;
    assert!(created.token.starts_with("ngt_"));
    let apps: Vec<_> = created.setup.iter().map(|s| s.app.as_str()).collect();
    for app in [
        "claude-desktop",
        "claude-code",
        "cursor",
        "generic-stdio",
        "generic-http",
    ] {
        assert!(apps.contains(&app), "{apps:?}");
    }
    let desktop = created
        .setup
        .iter()
        .find(|s| s.app == "claude-desktop")
        .unwrap();
    let v: Value = serde_json::from_str(&desktop.text).unwrap();
    assert_eq!(v["mcpServers"]["nugget"]["command"], EXE);
    assert_eq!(
        v["mcpServers"]["nugget"]["env"]["NUGGET_HOME"],
        h.home.display().to_string()
    );
    let generic = created
        .setup
        .iter()
        .find(|s| s.app == "generic-http")
        .unwrap();
    assert!(generic.text.contains(&format!("127.0.0.1:{}", addr.port())));
    let listed: Vec<McpClientInfo> = h.call(method::MCP_CLIENTS_LIST, json!({})).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.client.id);
    assert!(
        !serde_json::to_string(&listed)
            .unwrap()
            .contains(&created.token)
    );

    // Search and get_entity over HTTP with the token.
    let c = http_client(&url, &created.token).await.unwrap();
    let out = tool(&c, "search", json!({"query": "invoice"})).await;
    assert!(out.contains("Please find the invoice"), "{out}");
    let out = tool(&c, "get_entity", json!({"id": alice.id})).await;
    assert!(out.contains("Alice Smith"), "{out}");
    assert!(out.contains("Acme"), "{out}");
    assert!(
        out.contains("\"Lunch on Friday\""),
        "evidence titles: {out}"
    );
    let out = tool(&c, "find_entities", json!({"text": "acme"})).await;
    assert!(out.contains("Acme"), "{out}");
    let out = tool(
        &c,
        "get_context",
        json!({"query": "rebuilding the indexer"}),
    )
    .await;
    assert!(out.contains("indexer"), "{out}");
    let out = tool(&c, "get_neighbourhood", json!({"id": alice.id})).await;
    assert!(out.contains("Acme"), "{out}");
    let out = tool(&c, "recent_changes", json!({})).await;
    assert!(out.contains("Alice Smith"), "{out}");
    c.cancel().await.unwrap();

    // A client scoped to the notes source sees Alice through the Markdown note only.
    let narrow = h
        .create_client(
            "Notes only",
            json!({"sources": [notes.id], "tools": [], "include_sensitive": false}),
        )
        .await;
    let c = http_client(&url, &narrow.token).await.unwrap();
    let out = tool(&c, "get_entity", json!({"id": alice.id})).await;
    assert!(out.contains("Alice Smith"), "{out}");
    assert!(out.contains("Apollo"), "{out}");
    assert!(!out.contains("Acme"), "mail facts are out of scope: {out}");
    assert!(!out.contains("alice@acme.com"), "{out}");
    let out = tool(&c, "search", json!({"query": "invoice"})).await;
    assert!(!out.contains("invoice.eml"), "{out}");
    c.cancel().await.unwrap();

    // Revoked tokens get 401 on the next request.
    let _: Value = h
        .call(method::MCP_CLIENTS_REVOKE, json!({"id": created.client.id}))
        .await;
    let r = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .bearer_auth(&created.token)
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"1"}}}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(http_client(&url, &created.token).await.is_err());
    assert!(
        h.call_err(method::MCP_CLIENTS_REVOKE, json!({"id": created.client.id}))
            .await
            .contains(&created.client.id)
    );

    // Every MCP call is in the store's audit log; so are client changes.
    let records = h.store.recent_audit(1000).unwrap();
    let calls: Vec<_> = records
        .iter()
        .filter(|r| r.action == "mcp.tool_call")
        .collect();
    assert!(calls.len() >= 8, "{}", calls.len());
    assert!(
        calls
            .iter()
            .any(|r| r.actor == format!("mcp:{}", narrow.client.id))
    );
    assert!(
        calls
            .iter()
            .all(|r| !r.detail.to_string().contains("Shall we meet")),
        "audit entries never hold content"
    );
    let actions = h.audit_actions();
    assert!(actions.iter().any(|a| a == "mcp.client.create"));
    assert!(actions.iter().any(|a| a == "mcp.client.revoke"));
    h.store.verify_audit_chain().unwrap();
    h.stop().await;
}

/// An MCP client talking to a spawned process over its stdio.
async fn spawn_stdio(
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> (McpClient, tokio::process::Child) {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .env_remove("NUGGET_HOME")
        .env_remove(nugget_mcp::TOKEN_ENV)
        .env("NUGGET_KEYCHAIN", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let client = tokio::time::timeout(CALL, ().serve((stdout, stdin)))
        .await
        .expect("bridge initializes in time")
        .expect("bridge initializes");
    (client, child)
}

fn bridge_env(h: &Harness, token: &str) -> Vec<(String, String)> {
    vec![
        ("NUGGET_HOME".into(), h.home.display().to_string()),
        (nugget_mcp::TOKEN_ENV.into(), token.into()),
    ]
}

async fn run_bin(home: &Path, args: &[&str]) -> std::process::Output {
    tokio::process::Command::new(EXE)
        .args(args)
        .env("NUGGET_HOME", home)
        .env("NUGGET_KEYCHAIN", "off")
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdio_bridge_and_install_helper() {
    let mut h = Harness::start().await;
    h.add_files("mail", &mail_corpus()).await;
    h.wait_entity("organization", "Acme").await;

    // The real binary as an app launches it: `nuggetd mcp --client <id>` + NUGGET_MCP_TOKEN.
    let created = h.create_client("Cursor", all_scopes()).await;
    let args = vec![
        "mcp".to_owned(),
        "--client".to_owned(),
        created.client.id.clone(),
    ];
    let (c, mut child) = spawn_stdio(EXE, &args, &bridge_env(&h, &created.token)).await;
    let tools = c.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "get_entity"), "{tools:?}");
    let out = tool(&c, "find_entities", json!({"text": "acme"})).await;
    assert!(out.contains("Acme"), "{out}");
    let out = tool(&c, "search", json!({"query": "invoice"})).await;
    assert!(out.contains("Please find the invoice"), "{out}");
    c.cancel().await.unwrap();
    let status = tokio::time::timeout(CALL, child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.success(),
        "bridge exits cleanly when the app closes it"
    );
    // MCP audit entries go to a background writer over a channel, so the rows can land just
    // after the bridge has exited (seen on Windows CI); wait for them.
    eventually("both stdio tool calls are audited", || async {
        h.store
            .recent_audit(1000)
            .unwrap()
            .into_iter()
            .filter(|r| r.action == "mcp.tool_call" && r.detail["transport"] == "stdio")
            .count()
            >= 2
    })
    .await;

    // A wrong token (or no token) is refused before any MCP traffic.
    let out = tokio::process::Command::new(EXE)
        .args(&args)
        .env("NUGGET_HOME", &h.home)
        .env(nugget_mcp::TOKEN_ENV, format!("ngt_{}", "A".repeat(43)))
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "nothing but MCP on stdout");
    assert!(String::from_utf8_lossy(&out.stderr).contains("revoked"));
    let out = run_bin(&h.home, &["mcp", "--client", &created.client.id]).await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(nugget_mcp::TOKEN_ENV));

    // `mcp install` merges into an existing Claude Desktop config, keeping other servers.
    let cfg = h
        .tmp
        .path()
        .join("Claude")
        .join("claude_desktop_config.json");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg,
        r#"{"theme": "dark", "mcpServers": {"weather": {"command": "weather-mcp"}}}"#,
    )
    .unwrap();
    let cfg_s = cfg.display().to_string();
    // Without a terminal, --yes is required.
    let out = run_bin(
        &h.home,
        &[
            "mcp",
            "install",
            "--client",
            "claude-desktop",
            "--config",
            &cfg_s,
        ],
    )
    .await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    let out = run_bin(
        &h.home,
        &[
            "mcp",
            "install",
            "--client",
            "claude-desktop",
            "--config",
            &cfg_s,
            "--name",
            "Desktop test",
            "--yes",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Backup of the previous file"), "{stdout}");
    assert!(
        !stdout.contains("ngt_"),
        "the token is not printed: {stdout}"
    );
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(v["theme"], "dark");
    assert_eq!(v["mcpServers"]["weather"]["command"], "weather-mcp");
    let entry = v["mcpServers"]["nugget"].clone();
    assert_eq!(entry["command"], EXE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let backups: Vec<_> = std::fs::read_dir(cfg.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("nugget-backup"))
        .collect();
    assert_eq!(backups.len(), 1);
    let listed: Vec<McpClientInfo> = h.call(method::MCP_CLIENTS_LIST, json!({})).await;
    let first_id = listed
        .iter()
        .find(|c| c.name == "Desktop test")
        .expect("install created a client")
        .id
        .clone();

    // The installed entry works exactly as Claude Desktop would launch it.
    let args: Vec<String> = entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let env: Vec<(String, String)> = entry["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .collect();
    let (c, mut child) = spawn_stdio(entry["command"].as_str().unwrap(), &args, &env).await;
    let out = tool(&c, "find_entities", json!({"text": "alice"})).await;
    assert!(out.contains("Alice Smith"), "{out}");
    c.cancel().await.unwrap();
    let _ = child.wait().await;

    // Installing again replaces the entry and revokes the client it replaced.
    let out = run_bin(
        &h.home,
        &[
            "mcp",
            "install",
            "--client",
            "claude-desktop",
            "--config",
            &cfg_s,
            "--yes",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains(&first_id));
    let listed: Vec<McpClientInfo> = h.call(method::MCP_CLIENTS_LIST, json!({})).await;
    assert!(listed.iter().all(|c| c.id != first_id));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["weather"]["command"], "weather-mcp");

    // Claude Code: the command is printed, not run.
    let out = run_bin(
        &h.home,
        &["mcp", "install", "--client", "claude-code", "--yes"],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("claude mcp add --transport stdio"),
        "{stdout}"
    );
    assert!(stdout.contains("NUGGET_HOME="), "{stdout}");
    h.stop().await;
}
