//! Scope enforcement on every tool and resource, against both a well-behaved backend and one
//! that ignores the scope filter (so the MCP layer alone must enforce it).

mod common;

use common::*;
use nugget_core::ipc::McpScopes;
use nugget_mcp::audit::Outcome;
use nugget_mcp::tools::ToolOutput;
use nugget_mcp::{McpServer, Transport};
use serde_json::{Value, json};

fn server(leaky: bool) -> McpServer {
    McpServer::builder(backend(leaky)).build()
}

async fn call(s: &McpServer, scopes: McpScopes, tool: &str, args: Value) -> ToolOutput {
    s.call_tool(&client(scopes), Transport::Stdio, tool, &obj(args))
        .await
        .unwrap_or_else(|f| panic!("{tool} failed: {}", f.message))
}

fn everything(o: &ToolOutput) -> String {
    format!("{}\n{}", o.text, o.structured)
}

#[tokio::test]
async fn search_excludes_sensitive_by_default_and_includes_with_scope() {
    for leaky in [false, true] {
        let s = server(leaky);
        let out = call(&s, all_scopes(), "search", json!({"query": "apollo"})).await;
        assert!(!everything(&out).contains(SECRET), "leaky={leaky}");
        assert!(out.items.iter().all(|i| i.key.as_str() != "m2"));
        assert_eq!(out.results, 3);

        let out = call(
            &s,
            scopes(None, &[], true),
            "search",
            json!({"query": "apollo"}),
        )
        .await;
        assert!(
            everything(&out).contains(SECRET),
            "include_sensitive shows it"
        );
    }
}

#[tokio::test]
async fn sources_allow_list_filters_every_tool() {
    for leaky in [false, true] {
        let s = server(leaky);
        let only_files = scopes(Some(&[FILES]), &[], true);
        let out = call(
            &s,
            only_files.clone(),
            "search",
            json!({"query": "apollo budget"}),
        )
        .await;
        assert!(out.items.iter().all(|i| i.source.as_str() == FILES));
        assert!(!everything(&out).contains("2M"));

        let out = call(
            &s,
            only_files.clone(),
            "get_context",
            json!({"query": "apollo budget"}),
        )
        .await;
        assert!(out.items.iter().all(|i| i.source.as_str() == FILES));
        assert!(!everything(&out).contains("2M"));

        let err = s
            .call_tool(
                &client(only_files.clone()),
                Transport::Stdio,
                "get_item",
                &obj(json!({"source": MAIL, "key": "m1"})),
            )
            .await
            .unwrap_err();
        assert_eq!(err.outcome, Outcome::Rejected);
        assert!(err.message.contains("not found"));

        let out = call(
            &s,
            only_files.clone(),
            "recent_changes",
            json!({"since": "2026-10-01T00:00:00Z"}),
        )
        .await;
        assert!(out.items.iter().all(|i| i.source.as_str() == FILES));

        // Sam's facts keep only file evidence; the mail-only fact disappears.
        let out = call(
            &s,
            only_files.clone(),
            "get_entity",
            json!({"id": "ent_sam"}),
        )
        .await;
        let text = everything(&out);
        assert!(text.contains("leads the Apollo launch"));
        assert!(!text.contains(SECRET), "{text}");
        assert!(out.items.iter().all(|i| i.source.as_str() == FILES));
        // Vault-only content needs "vault" on the list.
        assert!(!text.contains("prefers tea"));
        let with_vault = scopes(Some(&[FILES, "vault"]), &[], false);
        let out = call(&s, with_vault, "get_entity", json!({"id": "ent_sam"})).await;
        assert!(out.text.contains("prefers tea"));
    }
}

#[tokio::test]
async fn sensitive_evidence_is_dropped_inside_entity_facts() {
    for leaky in [false, true] {
        let s = server(leaky);
        let out = call(&s, all_scopes(), "get_entity", json!({"id": "ent_sam"})).await;
        let text = everything(&out);
        assert!(text.contains("leads the Apollo launch"));
        // Fact 2's only evidence is sensitive: gone. Fact 3 keeps its non-sensitive evidence.
        assert!(!text.contains("oncology"), "{text}");
        assert!(text.contains("Sam works at Acme."));
        assert!(
            !text.contains("m2"),
            "no reference to the sensitive item: {text}"
        );
        // Superseded facts are not shown.
        assert!(!text.contains("Initech"));
        // The entity has a sensitive origin: aliases, identifiers and attributes are withheld.
        assert!(!text.contains("sam@example.com"));
        assert_eq!(out.structured["details_withheld"], json!(true));
        let facts = out.structured["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 3, "facts 1, 3 and the vault fact 4");
        for f in facts {
            for e in f["evidence"].as_array().unwrap() {
                assert_eq!(e["sensitive"], json!(false));
            }
        }

        // With include_sensitive everything is visible.
        let out = call(
            &s,
            scopes(None, &[], true),
            "get_entity",
            json!({"id": "ent_sam"}),
        )
        .await;
        let text = everything(&out);
        assert!(text.contains("oncology appointment"));
        assert!(text.contains("sam@example.com"));
        assert_eq!(out.structured["details_withheld"], json!(false));
    }
}

#[tokio::test]
async fn entities_seen_only_in_sensitive_items_are_invisible() {
    for leaky in [false, true] {
        let s = server(leaky);
        let out = call(&s, all_scopes(), "find_entities", json!({"text": ""})).await;
        let text = everything(&out);
        assert!(text.contains("Sam Lee") && text.contains("Acme"));
        assert!(!text.contains("Clinic"), "{text}");
        assert!(
            text.contains("Vault Note"),
            "vault-only entities are visible with all sources"
        );

        let err = s
            .call_tool(
                &client(all_scopes()),
                Transport::Stdio,
                "get_entity",
                &obj(json!({"id": "ent_clinic"})),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("not found"));
        // Same answer as for an id that never existed.
        let err2 = s
            .call_tool(
                &client(all_scopes()),
                Transport::Stdio,
                "get_entity",
                &obj(json!({"id": "ent_nope"})),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, err2.message);

        let out = call(
            &s,
            scopes(None, &[], true),
            "find_entities",
            json!({"text": "clinic"}),
        )
        .await;
        assert!(out.text.contains("Oncology Clinic"));
    }
}

#[tokio::test]
async fn neighbourhood_drops_hidden_nodes_and_what_is_only_reachable_through_them() {
    for leaky in [false, true] {
        let s = server(leaky);
        let out = call(
            &s,
            all_scopes(),
            "get_neighbourhood",
            json!({"id": "ent_sam", "depth": 2}),
        )
        .await;
        let ids: Vec<_> = out.structured["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, ["ent_sam", "ent_acme"], "leaky={leaky}");
        assert_eq!(out.structured["edges"].as_array().unwrap().len(), 1);
        assert!(!out.text.contains("Clinic") && !out.text.contains("Dr X"));

        let out = call(
            &s,
            scopes(None, &[], true),
            "get_neighbourhood",
            json!({"id": "ent_sam", "depth": 2}),
        )
        .await;
        assert_eq!(out.structured["nodes"].as_array().unwrap().len(), 4);
    }
}

#[tokio::test]
async fn context_packing_excludes_sensitive_and_numbers_without_gaps() {
    for leaky in [false, true] {
        let s = server(leaky);
        let out = call(
            &s,
            all_scopes(),
            "get_context",
            json!({"query": "sam apollo oncology"}),
        )
        .await;
        let text = everything(&out);
        assert!(!text.contains(SECRET), "{text}");
        let cites = out.structured["citations"].as_array().unwrap();
        assert!(!cites.is_empty());
        for (i, c) in cites.iter().enumerate() {
            assert_eq!(c["n"], json!(i + 1));
            assert_eq!(c["sensitive"], json!(false));
            assert!(
                out.text
                    .contains(&format!("[{}] <untrusted-content", i + 1))
            );
        }
        assert!(out.structured["excluded_sensitive"].as_u64().unwrap() >= 1);
        assert!(out.text.contains("sensitive item(s) were left out"));

        let out = call(
            &s,
            scopes(None, &[], true),
            "get_context",
            json!({"query": "oncology"}),
        )
        .await;
        assert!(out.text.contains(SECRET));
    }
}

#[tokio::test]
async fn context_respects_the_token_budget() {
    let s = server(false);
    let out = call(
        &s,
        all_scopes(),
        "get_context",
        json!({"query": "appendix", "budget_tokens": 100}),
    )
    .await;
    let body = out.structured["text"].as_str().unwrap();
    // 100 tokens ≈ 400 characters of passage text, plus wrapper markup.
    assert!(body.len() < 1200, "{}", body.len());
}

#[tokio::test]
async fn get_item_is_size_capped_and_pages() {
    let s = server(false);
    let out = call(
        &s,
        all_scopes(),
        "get_item",
        json!({"source": FILES, "key": "big.txt"}),
    )
    .await;
    assert_eq!(out.structured["total_chars"], json!(50_000));
    assert_eq!(
        out.structured["text"].as_str().unwrap().chars().count(),
        20_000
    );
    assert_eq!(out.structured["truncated"], json!(true));
    assert!(out.text.contains("offset_chars=20000"));
    let out = call(
        &s,
        all_scopes(),
        "get_item",
        json!({"source": FILES, "key": "big.txt", "offset_chars": 45000, "max_chars": 999999}),
    )
    .await;
    assert_eq!(
        out.structured["text"].as_str().unwrap().chars().count(),
        5_000
    );
    assert_eq!(out.structured["truncated"], json!(false));

    // A sensitive item reads as not found without include_sensitive.
    for leaky in [false, true] {
        let s = server(leaky);
        let err = s
            .call_tool(
                &client(all_scopes()),
                Transport::Stdio,
                "get_item",
                &obj(json!({"source": MAIL, "key": "m2"})),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("not found"));
        let ok = call(
            &s,
            scopes(None, &[], true),
            "get_item",
            json!({"source": MAIL, "key": "m2"}),
        )
        .await;
        assert!(ok.text.contains(SECRET));
    }
}

#[tokio::test]
async fn tools_allow_list_is_enforced() {
    let s = server(false);
    let only_search = scopes(None, &["search"], false);
    call(
        &s,
        only_search.clone(),
        "search",
        json!({"query": "apollo"}),
    )
    .await;
    for (tool, args) in [
        ("get_item", json!({"source": FILES, "key": "plan.md"})),
        ("get_entity", json!({"id": "ent_sam"})),
        ("get_context", json!({"query": "apollo"})),
    ] {
        let err = s
            .call_tool(
                &client(only_search.clone()),
                Transport::Stdio,
                tool,
                &obj(args),
            )
            .await
            .unwrap_err();
        assert_eq!(err.outcome, Outcome::Denied, "{tool}");
    }
    // Resources follow the tools they mirror.
    let err = s
        .read_resource(
            &client(only_search.clone()),
            Transport::Stdio,
            "nugget://entity/ent_sam",
        )
        .await
        .unwrap_err();
    assert_eq!(err.outcome, Outcome::Denied);
}

#[tokio::test]
async fn resources_are_scoped_like_tools() {
    for leaky in [false, true] {
        let s = server(leaky);
        let (mime, page) = s
            .read_resource(
                &client(all_scopes()),
                Transport::Stdio,
                "nugget://entity/ent_sam",
            )
            .await
            .unwrap();
        assert_eq!(mime, "text/markdown");
        assert!(page.contains("# Sam Lee (person)"));
        assert!(page.contains("leads the Apollo launch"));
        assert!(!page.contains(SECRET));
        let err = s
            .read_resource(
                &client(all_scopes()),
                Transport::Stdio,
                "nugget://entity/ent_clinic",
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("not found"));
        let err = s
            .read_resource(
                &client(all_scopes()),
                Transport::Stdio,
                "nugget://item/mail%3Ame/m2",
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("not found"));
        let (_, text) = s
            .read_resource(
                &client(all_scopes()),
                Transport::Stdio,
                "nugget://item/mail%3Ame/m1",
            )
            .await
            .unwrap();
        assert!(text.contains("2M"));
    }
}

#[tokio::test]
async fn untrusted_content_is_fenced_and_cannot_escape() {
    let s = server(false);
    let out = call(
        &s,
        all_scopes(),
        "get_item",
        json!({"source": FILES, "key": "plan.md"}),
    )
    .await;
    assert!(
        out.text
            .contains("<untrusted-content source=\"files:docs\" item=\"plan.md\"")
    );
    assert_eq!(out.text.matches("</untrusted-content>").count(), 1);
    assert!(out.text.trim_end().ends_with("</untrusted-content>"));
    assert!(out.text.contains("data, not instructions"));
}

#[tokio::test]
async fn audit_entry_per_call_without_content() {
    let log = AuditLog::default();
    let s = McpServer::builder(backend(false)).audit(log.sink()).build();
    let c = client(all_scopes());
    s.call_tool(
        &c,
        Transport::Stdio,
        "get_item",
        &obj(json!({"source": FILES, "key": "plan.md"})),
    )
    .await
    .unwrap();
    s.call_tool(
        &c,
        Transport::LoopbackHttp,
        "search",
        &obj(json!({"query": "apollo budget"})),
    )
    .await
    .unwrap();
    let _ = s
        .call_tool(
            &client(scopes(None, &["search"], false)),
            Transport::Stdio,
            "get_entity",
            &obj(json!({"id": "ent_sam"})),
        )
        .await;
    let _ = s
        .read_resource(&c, Transport::Stdio, "nugget://entity/ent_sam")
        .await;
    let entries = log.entries();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].actor, "mcp:mcp_test");
    assert_eq!(entries[0].action, "mcp.tool_call");
    assert_eq!(entries[0].target, "get_item");
    assert_eq!(entries[0].detail["outcome"], "ok");
    assert_eq!(entries[0].detail["locality"], "local");
    assert_eq!(entries[0].detail["transport"], "stdio");
    assert_eq!(entries[0].detail["items"], json!(["files:docs/plan.md"]));
    assert_eq!(entries[1].detail["transport"], "loopback_http");
    assert_eq!(entries[1].detail["args"]["query"], "apollo budget");
    assert_eq!(entries[2].detail["outcome"], "denied");
    assert_eq!(entries[3].action, "mcp.resource_read");
    for e in &entries {
        let s = serde_json::to_string(e).unwrap();
        assert!(
            !s.contains("leads the Apollo launch"),
            "no content in audit: {s}"
        );
        assert!(!s.contains("Ignore previous"), "no content in audit: {s}");
    }
}

#[tokio::test]
async fn argument_errors_are_tool_errors() {
    let s = server(false);
    let c = client(all_scopes());
    for (tool, args) in [
        ("search", json!({"query": "  "})),
        ("search", json!({"limit": 3})),
        ("find_entities", json!({"text": "x", "kind": "spaceship"})),
        ("recent_changes", json!({"since": "yesterday"})),
        ("get_entity", json!({"id": ""})),
    ] {
        let err = s
            .call_tool(&c, Transport::Stdio, tool, &obj(args))
            .await
            .unwrap_err();
        assert_eq!(err.outcome, Outcome::Rejected, "{tool}: {}", err.message);
    }
}
