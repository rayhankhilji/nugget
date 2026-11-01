//! Knowledge end to end: Markdown and mail indexed through files sources, extraction, the
//! knowledge IPC methods, the vault (pages with citations, user edits becoming
//! `user_asserted` facts), merges, splits and claim-status changes, and entities known only from
//! sensitive items; MCP clients over loopback HTTP (scopes, revocation, audit).

mod common;

use std::path::{Path, PathBuf};

use common::*;
use nugget_core::ids::EntityId;
use nugget_core::ipc::{
    EntityDetail, EntityRef, EntitySummaryView, GraphViewIpc, KnowledgeStatus, McpClientCreated,
    McpClientInfo, method, notification,
};
use serde_json::{Value, json};

/// No local model here, so triage can never clear items: without `assume_unflagged_safe` they
/// all count as sensitive (unknown, GG5-2).
const CONFIG: &str = r#"
[runtime]
enabled = false

[ingest]
assume_unflagged_safe = true

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

fn lunch() -> (&'static str, String) {
    let body = "Shall we meet at noon? Alice";
    (
        "lunch.eml",
        mail(
            "Alice Smith <alice@acme.com>",
            "Lunch on Friday",
            "m1",
            body,
        ),
    )
}

fn invoice() -> (&'static str, String) {
    let body = "Please find the invoice attached.";
    (
        "invoice.eml",
        mail("Bob Jones <bob@globex.com>", "Globex invoice", "m2", body),
    )
}

async fn create_client(h: &Harness, name: &str, scopes: Value) -> McpClientCreated {
    h.call(
        method::MCP_CLIENTS_CREATE,
        json!({"name": name, "scopes": scopes}),
    )
    .await
}

async fn start() -> Harness {
    Harness::start_with(CONFIG, mcp_opts).await
}

async fn entities(h: &Harness, kind: &str) -> Vec<EntitySummaryView> {
    h.call(
        method::KNOWLEDGE_ENTITIES,
        json!({"kind": kind, "limit": 100}),
    )
    .await
}

async fn find(h: &Harness, kind: &str, name: &str) -> Option<EntitySummaryView> {
    entities(h, kind).await.into_iter().find(|e| e.name == name)
}

/// Waits until an entity of `kind` named `name` exists.
async fn wait_entity(h: &Harness, kind: &str, name: &str) -> EntitySummaryView {
    eventually(&format!("{kind} {name}"), || async {
        find(h, kind, name).await.is_some()
    })
    .await;
    find(h, kind, name).await.unwrap()
}

async fn entity(h: &Harness, id: &EntityId) -> EntityDetail {
    h.call(method::KNOWLEDGE_ENTITY, json!({"id": id})).await
}

async fn set_status(h: &Harness, claim: i64, status: &str) {
    let _: Value = h
        .call(
            method::KNOWLEDGE_CLAIM_STATUS,
            json!({"claim": claim, "status": status}),
        )
        .await;
}

fn status_of(d: &EntityDetail, id: i64) -> Option<String> {
    d.claims
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.status.clone())
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn knowledge_graph_vault_and_edits_end_to_end() {
    let mut h = start().await;
    let seen = collect(&h.client);
    h.add_files("notes", &[("Apollo.md", APOLLO)]).await;
    let mail_src = h.add_files("mail", &[lunch(), invoice()]).await.1.id;

    // People and organisations from mail headers; the project from Markdown frontmatter.
    let alice = wait_entity(&h, "person", "Alice Smith").await;
    let bob = wait_entity(&h, "person", "Bob Jones").await;
    wait_entity(&h, "organization", "Globex").await;
    wait_entity(&h, "project", "Apollo").await;
    assert!(
        find(&h, "person", "Tanuj").await.is_some(),
        "the user's entity"
    );
    let (_, err) = h
        .call_err(
            method::KNOWLEDGE_ENTITIES,
            json!({"kind": "planet", "limit": 5}),
        )
        .await;
    assert!(err.contains("planet"), "{err}");

    // Facts carry evidence with titles from the store; the vault renders right after.
    eventually("facts and the vault page", || async {
        let d = entity(&h, &alice.id).await;
        d.claims.iter().any(|c| c.predicate == "works_at")
            && d.claims.iter().any(|c| c.predicate == "works_on")
            && d.vault_path.is_some()
    })
    .await;
    let detail = entity(&h, &alice.id).await;
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
    // Mail files are titled by their Subject.
    let ev = &works_at.evidence[0];
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

    // The vault page, with citations.
    let vault = h.vault.clone().unwrap();
    let page = vault.join(detail.vault_path.clone().unwrap());
    let text = read(&page);
    assert!(
        text.contains(alice.id.as_str()) && text.contains("[^1]: "),
        "{text}"
    );
    assert!(
        text.contains("[Lunch on Friday]("),
        "cited by title with a link: {text}"
    );
    let st: KnowledgeStatus = h.call(method::KNOWLEDGE_STATUS, json!({})).await;
    assert!(
        st.entities >= 6 && st.claims > 0 && st.mentions > 0,
        "{st:?}"
    );
    assert!(
        st.vault.pages >= 6 && st.vault.invalid.is_empty(),
        "{:?}",
        st.vault
    );

    // Graph: overview of the most connected entities, a neighbourhood, a source filter.
    let g: GraphViewIpc = h
        .call(method::KNOWLEDGE_GRAPH, json!({"depth": 1, "limit": 3}))
        .await;
    assert!(g.nodes.len() == 3 && g.truncated);
    let around = json!({"center": alice.id, "depth": 1, "limit": 50});
    let g: GraphViewIpc = h.call(method::KNOWLEDGE_GRAPH, around.clone()).await;
    assert!(g.nodes.iter().any(|n| n.name == "Acme"), "{g:?}");
    assert!(g.edges.iter().any(|e| e.predicate == "works_at"));
    let mut by_mail = around.clone();
    by_mail["source"] = json!(mail_src);
    let g: GraphViewIpc = h.call(method::KNOWLEDGE_GRAPH, by_mail).await;
    assert!(g.nodes.iter().any(|n| n.name == "Acme"), "{g:?}");
    assert!(g.nodes.iter().all(|n| n.name != "Apollo"), "{g:?}");

    // A fact added in the vault becomes a user-asserted fact, and is broadcast.
    let before_edit = seen.lock().unwrap().len();
    let edited = read(&page).replace(
        "<!-- nugget:end facts -->",
        "\n### Hobbies\n- Plays chess\n<!-- nugget:end facts -->",
    );
    std::fs::write(&page, edited).unwrap();
    eventually("vault edit becomes a user_asserted fact", || async {
        entity(&h, &alice.id).await.claims.iter().any(|c| {
            c.text == "Plays chess" && c.status == "user_asserted" && c.evidence.is_empty()
        })
    })
    .await;
    eventually("page re-rendered with the user's fact", || async {
        read(&page).contains("- Plays chess (you)")
    })
    .await;
    eventually("the vault edit is broadcast", || async {
        seen.lock().unwrap()[before_edit..].iter().any(|n| {
            n.method == notification::KNOWLEDGE_CHANGED
                && n.params["entities"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == alice.id.as_str()))
        })
    })
    .await;

    // Rejecting and restoring a fact; the vault follows.
    set_status(&h, works_at.id, "rejected").await;
    assert_eq!(
        status_of(&entity(&h, &alice.id).await, works_at.id).as_deref(),
        Some("rejected")
    );
    assert!(!read(&page).contains(&format!("<!-- c:{} -->", works_at.id)));
    set_status(&h, works_at.id, "active").await;
    assert_eq!(
        status_of(&entity(&h, &alice.id).await, works_at.id).as_deref(),
        Some("active")
    );
    for (claim, status, want) in [
        (works_at.id, "superseded", "rejected".to_owned()),
        (works_at.id, "unasserted", "asserted".to_owned()),
        (999_999, "active", "999999".to_owned()),
    ] {
        let (_, err) = h
            .call_err(
                method::KNOWLEDGE_CLAIM_STATUS,
                json!({"claim": claim, "status": status}),
            )
            .await;
        assert!(err.contains(&want), "{err}");
    }

    // Undo a user-asserted fact, then redo it.
    let chess = entity(&h, &alice.id)
        .await
        .claims
        .into_iter()
        .find(|c| c.text == "Plays chess")
        .unwrap();
    set_status(&h, chess.id, "unasserted").await;
    assert_eq!(
        status_of(&entity(&h, &alice.id).await, chess.id).as_deref(),
        Some("retracted")
    );
    eventually("withdrawn fact left the page", || async {
        !read(&page).contains("Plays chess")
    })
    .await;
    set_status(&h, chess.id, "user_asserted").await;

    // Split: the chess fact moves to a new entity.
    let split: EntityRef = h
        .call(
            method::KNOWLEDGE_SPLIT,
            json!({"entity": alice.id, "claims": [chess.id], "name": "Alice (chess club)"}),
        )
        .await;
    let other = entity(&h, &split.id).await;
    assert_eq!(
        (other.name.as_str(), other.kind.as_str()),
        ("Alice (chess club)", "person")
    );
    assert!(other.claims.iter().any(|c| c.id == chess.id));
    assert!(status_of(&entity(&h, &alice.id).await, chess.id).is_none());
    let (_, err) = h
        .call_err(
            method::KNOWLEDGE_SPLIT,
            json!({"entity": alice.id, "claims": [], "name": "X"}),
        )
        .await;
    assert!(err.contains("at least one"), "{err}");

    // Merge: Bob into Alice. Bob's id resolves to Alice; his page becomes a redirect stub.
    let bob_page = entity(&h, &bob.id).await.vault_path.unwrap();
    let _: Value = h
        .call(
            method::KNOWLEDGE_MERGE,
            json!({"into": alice.id, "from": bob.id}),
        )
        .await;
    let merged = entity(&h, &bob.id).await;
    assert_eq!(merged.id, alice.id);
    assert!(find(&h, "person", "Bob Jones").await.is_none());
    assert!(
        merged
            .identifiers
            .contains(&("email".to_owned(), "bob@globex.com".to_owned()))
    );
    let stub = read(&vault.join(&bob_page));
    assert!(
        stub.contains(alice.id.as_str()) || stub.contains("Alice"),
        "{stub}"
    );
    let (_, err) = h
        .call_err(
            method::KNOWLEDGE_MERGE,
            json!({"into": alice.id, "from": alice.id}),
        )
        .await;
    assert!(err.contains("itself"), "{err}");

    let actions: Vec<String> = h.audit().into_iter().map(|a| a.0).collect();
    for a in [
        "knowledge.merge",
        "knowledge.claim.status",
        "knowledge.split",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} audited: {actions:?}");
    }
    eventually("the merge was notified", || async {
        seen.lock().unwrap().iter().any(|n| {
            n.method == notification::KNOWLEDGE_CHANGED
                && n.params["entities"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == bob.id.as_str()))
        })
    })
    .await;
    h.stop().await;
}

/// Every file under `dir`, as `(path, text)`.
fn files_under(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push((p.clone(), std::fs::read_to_string(&p).unwrap_or_default()));
        }
    }
    out
}

/// GG5-3: an entity known only from an item the PII scan flags (an IBAN in the mail) is marked
/// in `knowledge.entity`, left out of `knowledge.graph` unless asked for, and gets no vault page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sensitive_only_entities_are_marked_hidden_from_the_graph_and_kept_out_of_the_vault() {
    let mut h = start().await;
    h.add_files(
        "mail",
        &[
            lunch(),
            (
                "counselling.eml",
                mail(
                    "Jonas Albers <jonas@albers-counselling.example>",
                    "Counselling invoice",
                    "m9",
                    "Thanks for Wednesday. Please pay to IBAN GB82 WEST 1234 5698 7654 32.",
                ),
            ),
        ],
    )
    .await;
    let alice = wait_entity(&h, "person", "Alice Smith").await;
    let jonas = wait_entity(&h, "person", "Jonas Albers").await;
    eventually("extraction and the vault caught up", || async {
        let st: KnowledgeStatus = h.call(method::KNOWLEDGE_STATUS, json!({})).await;
        let rendered = match (st.vault.last_render, st.last_run) {
            (Some(r), Some(l)) => r >= l,
            _ => false,
        };
        st.pending_items == 0 && rendered && entity(&h, &alice.id).await.vault_path.is_some()
    })
    .await;

    // The entity view marks it (and its facts); it has no vault page.
    let d = entity(&h, &jonas.id).await;
    assert!(d.sensitive, "{d:#?}");
    assert_eq!(d.vault_path, None);
    assert!(!d.claims.is_empty() || !d.mentions.is_empty(), "{d:#?}");
    assert!(
        d.claims
            .iter()
            .filter(|c| !c.evidence.is_empty())
            .all(|c| c.sensitive),
        "{d:#?}"
    );
    // No local model here, so triage never ran on the lunch mail: it is not confirmed sensitive
    // and is shown in the local views like any other item (whether it counts as sensitive for
    // the cloud depends on `assume_unflagged_safe`; the vault tests cover unsorted items).
    let lunch_ref = entity(&h, &alice.id)
        .await
        .claims
        .iter()
        .flat_map(|c| c.evidence.iter())
        .map(|e| e.item.clone())
        .next()
        .expect("Alice's facts are cited");
    let lunch = h.store.get_item(&lunch_ref).unwrap().unwrap();
    assert!(
        lunch.metadata["triage"]["state"] != "done"
            && !nugget_retrieve::is_confirmed_sensitive(&lunch.metadata),
        "{:?}",
        lunch.metadata
    );
    let a = entity(&h, &alice.id).await;
    assert!(
        !a.sensitive && a.claims.iter().all(|c| !c.sensitive),
        "{a:#?}"
    );

    // The graph leaves it out unless asked for; then it is marked.
    let names =
        |g: &GraphViewIpc| -> Vec<String> { g.nodes.iter().map(|n| n.name.clone()).collect() };
    let g: GraphViewIpc = h
        .call(method::KNOWLEDGE_GRAPH, json!({"depth": 1, "limit": 200}))
        .await;
    assert!(g.nodes.iter().any(|n| n.id == alice.id), "{:?}", names(&g));
    assert!(
        g.nodes.iter().all(|n| n.id != jonas.id && !n.sensitive),
        "{:?}",
        names(&g)
    );
    let all: GraphViewIpc = h
        .call(
            method::KNOWLEDGE_GRAPH,
            json!({"depth": 1, "limit": 200, "include_sensitive": true}),
        )
        .await;
    let j = all.nodes.iter().find(|n| n.id == jonas.id);
    assert!(j.is_some_and(|n| n.sensitive), "{:?}", names(&all));
    assert!(all.nodes.iter().any(|n| n.id == alice.id && !n.sensitive));
    // Neighbourhoods: a sensitive centre stays (marked) but brings nothing else along.
    let around: GraphViewIpc = h
        .call(
            method::KNOWLEDGE_GRAPH,
            json!({"center": jonas.id, "depth": 2, "limit": 50}),
        )
        .await;
    assert_eq!(
        names(&around),
        vec!["Jonas Albers".to_owned()],
        "{around:?}"
    );
    assert!(around.nodes[0].sensitive && around.edges.is_empty());
    let around: GraphViewIpc = h
        .call(
            method::KNOWLEDGE_GRAPH,
            json!({"center": jonas.id, "depth": 2, "limit": 50, "include_sensitive": true}),
        )
        .await;
    assert!(
        around.nodes.iter().any(|n| n.id == jonas.id && n.sensitive),
        "{around:?}"
    );
    if let Some(me) = find(&h, "person", "Tanuj").await {
        let g: GraphViewIpc = h
            .call(
                method::KNOWLEDGE_GRAPH,
                json!({"center": me.id, "depth": 2, "limit": 50}),
            )
            .await;
        assert!(
            g.nodes.iter().all(|n| n.id == me.id || !n.sensitive),
            "{g:?}"
        );
        assert!(g.nodes.iter().all(|n| n.id != jonas.id), "{g:?}");
    }

    // Nothing from the flagged mail anywhere in the vault.
    for (p, text) in files_under(h.vault.as_ref().unwrap()) {
        for leak in ["Jonas", "albers-counselling", "GB82", "Counselling invoice"] {
            assert!(!text.contains(leak), "{leak} in {}:\n{text}", p.display());
        }
    }

    // A default-scope MCP client cannot reach it through any tool.
    let created = create_client(&h, "default", all_scopes()).await;
    let url = format!("http://{}/mcp", h.mcp_addr.expect("http listener"));
    let mcp = http_client(&url, &created.token).await.unwrap();
    for (name, args) in [
        ("find_entities", json!({"text": "Jonas"})),
        ("find_entities", json!({})),
        ("get_entity", json!({"id": jonas.id})),
        ("get_neighbourhood", json!({"id": alice.id, "depth": 2})),
        ("search", json!({"query": "Counselling invoice IBAN"})),
        ("recent_changes", json!({})),
    ] {
        let text = tool(&mcp, name, args.clone()).await;
        assert!(!text.contains("Jonas"), "{name} {args}: {text}");
        if matches!(name, "get_neighbourhood") || args == json!({}) && name == "find_entities" {
            assert!(text.contains("Alice Smith"), "{name} {args}: {text}");
        }
    }
    let _ = mcp.cancel().await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_http_scopes_revocation_and_audit() {
    let mut h = start().await;
    let notes = h.add_files("notes", &[("Apollo.md", APOLLO)]).await.1;
    h.add_files("mail", &[lunch(), invoice()]).await;
    let alice = wait_entity(&h, "person", "Alice Smith").await;
    eventually("facts extracted", || async {
        let d = entity(&h, &alice.id).await;
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

    // The token is shown once, never listed.
    let created = create_client(&h, "Claude Code", all_scopes()).await;
    assert!(created.token.starts_with("ngt_"));
    let listed: Vec<McpClientInfo> = h.call(method::MCP_CLIENTS_LIST, json!({})).await;
    assert_eq!(listed.len(), 1);
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
    let narrow = create_client(
        &h,
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
    let (_, err) = h
        .call_err(method::MCP_CLIENTS_REVOKE, json!({"id": created.client.id}))
        .await;
    assert!(err.contains(&created.client.id), "{err}");

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
    let actions: Vec<String> = h.audit().into_iter().map(|a| a.0).collect();
    assert!(actions.iter().any(|a| a == "mcp.client.create"));
    assert!(actions.iter().any(|a| a == "mcp.client.revoke"));
    h.store.verify_audit_chain().unwrap();
    h.stop().await;
}
