//! Client registry, token storage and setup snippets.

mod common;

use common::*;
use nugget_mcp::auth::{TOKEN_LEN, looks_like_token};
use nugget_mcp::{ClientRegistry, ServerKey, SnippetOptions, setup_snippets};
use std::sync::Arc;

#[test]
fn tokens_have_the_documented_shape_and_are_unique() {
    let (reg, _) = registry();
    let (_, a) = reg.create("A", all_scopes()).unwrap();
    let (_, b) = reg.create("B", all_scopes()).unwrap();
    for t in [&a, &b] {
        assert!(t.expose().starts_with("ngt_"));
        assert_eq!(t.expose().len(), TOKEN_LEN);
        assert!(looks_like_token(t.expose()));
        assert_eq!(format!("{t:?}"), "ClientToken(<redacted>)");
    }
    assert_ne!(a.expose(), b.expose());
}

#[test]
fn token_is_never_stored_in_clear() {
    let (reg, store) = registry();
    let (info, token) = reg.create("Claude Desktop", all_scopes()).unwrap();
    let records = store.records();
    assert_eq!(records.len(), 1);
    let dump = serde_json::to_string(&records).unwrap();
    let secret_part = &token.expose()[4..];
    assert!(!dump.contains(token.expose()));
    assert!(!dump.contains(secret_part));
    // What is stored is HMAC-SHA256 under the server key, not a bare hash of the token.
    let rec = &records[0];
    assert_eq!(rec.info.id, info.id);
    assert_eq!(rec.token_hash.len(), 64);
    assert_eq!(
        rec.token_hash,
        ServerKey::from_bytes([7; 32]).hash_token(token.expose())
    );
    assert_ne!(
        rec.token_hash,
        ServerKey::from_bytes([8; 32]).hash_token(token.expose())
    );
    // The same table under another server key authenticates nothing.
    let other = ClientRegistry::new(store.clone(), ServerKey::from_bytes([8; 32]));
    assert!(other.authenticate(token.expose()).unwrap().is_none());
}

#[test]
fn authenticate_list_revoke_and_last_used() {
    let (reg, _) = registry();
    let (info, token) = reg
        .create("  Cursor  ", scopes(Some(&[FILES]), &["search"], false))
        .unwrap();
    assert_eq!(info.name, "Cursor");
    assert!(info.id.starts_with("mcp_"));
    assert!(info.last_used.is_none());

    let got = reg.authenticate(token.expose()).unwrap().unwrap();
    assert_eq!(got.id, info.id);
    assert_eq!(got.scopes, info.scopes);
    assert!(got.last_used.is_some());
    assert!(reg.list().unwrap()[0].last_used.is_some());

    assert!(
        reg.authenticate_client(&info.id, token.expose())
            .unwrap()
            .is_some()
    );
    assert!(
        reg.authenticate_client("mcp_other", token.expose())
            .unwrap()
            .is_none()
    );
    assert!(reg.authenticate("ngt_short").unwrap().is_none());
    assert!(reg.authenticate("").unwrap().is_none());

    let updated = reg.set_scopes(&info.id, all_scopes()).unwrap();
    assert!(updated.scopes.tools.is_empty());

    assert!(reg.revoke(&info.id).unwrap());
    assert!(!reg.revoke(&info.id).unwrap());
    assert!(reg.authenticate(token.expose()).unwrap().is_none());
    assert!(reg.list().unwrap().is_empty());
}

#[test]
fn last_used_writes_are_throttled() {
    let store = Arc::new(nugget_mcp::MemClientStore::new());
    let reg = ClientRegistry::new(store.clone(), ServerKey::from_bytes([1; 32]));
    let (_, token) = reg.create("A", all_scopes()).unwrap();
    let first = reg
        .authenticate(token.expose())
        .unwrap()
        .unwrap()
        .last_used
        .unwrap();
    let second = reg
        .authenticate(token.expose())
        .unwrap()
        .unwrap()
        .last_used
        .unwrap();
    assert_eq!(first, second);
}

#[test]
fn create_validates_name_and_scopes() {
    let (reg, _) = registry();
    assert!(reg.create("", all_scopes()).is_err());
    assert!(reg.create("a\nb", all_scopes()).is_err());
    assert!(reg.create(&"x".repeat(101), all_scopes()).is_err());
    assert!(
        reg.create("ok", scopes(None, &["send_email"], false))
            .is_err()
    );
    assert!(reg.create("ok", scopes(Some(&[" "]), &[], false)).is_err());
    assert!(reg.list().unwrap().is_empty());
}

#[test]
fn snippets_cover_every_app_and_keep_the_token_out_of_paths_and_urls() {
    let (reg, _) = registry();
    let (info, token) = reg.create("Laptop", all_scopes()).unwrap();
    let t = token.expose();
    let opts = SnippetOptions {
        nuggetd: "/Applications/Nugget.app/Contents/MacOS/nuggetd".into(),
        http_port: Some(7319),
        os: "macos".into(),
        env: Vec::new(),
    };
    let snippets = setup_snippets(&info, t, &opts);
    let apps: Vec<_> = snippets.iter().map(|s| s.app.as_str()).collect();
    assert_eq!(
        apps,
        [
            "claude-desktop",
            "claude-code",
            "cursor",
            "generic-stdio",
            "generic-http"
        ]
    );

    for s in &snippets {
        assert!(s.text.contains(t), "{} carries the token", s.app);
        if let Some(p) = &s.config_path {
            assert!(!p.contains(t));
        }
        for url in s.text.split_whitespace().filter(|w| w.starts_with("http")) {
            assert!(!url.contains(t), "token in URL: {url}");
        }
    }

    let desktop: serde_json::Value = serde_json::from_str(&snippets[0].text).unwrap();
    let entry = &desktop["mcpServers"]["nugget"];
    assert_eq!(entry["command"], opts.nuggetd);
    assert_eq!(
        entry["args"],
        serde_json::json!(["mcp", "--client", info.id])
    );
    assert_eq!(entry["env"]["NUGGET_MCP_TOKEN"], t);
    assert_eq!(
        snippets[0].config_path.as_deref(),
        Some("~/Library/Application Support/Claude/claude_desktop_config.json")
    );

    let cc = &snippets[1].text;
    assert!(
        cc.starts_with("claude mcp add --transport stdio --scope user --env NUGGET_MCP_TOKEN=")
    );
    assert!(cc.ends_with(&format!(
        "nugget -- /Applications/Nugget.app/Contents/MacOS/nuggetd mcp --client {}",
        info.id
    )));

    let cursor: serde_json::Value = serde_json::from_str(&snippets[2].text).unwrap();
    assert_eq!(cursor, desktop);
    assert_eq!(
        snippets[2].config_path.as_deref(),
        Some("~/.cursor/mcp.json")
    );

    let http = &snippets[4].text;
    assert!(http.contains("url: http://127.0.0.1:7319/mcp\n"));
    assert!(http.contains(&format!("header: Authorization: Bearer {t}")));

    // No HTTP snippet when the listener is off; Windows and Linux paths.
    let win = setup_snippets(
        &info,
        t,
        &SnippetOptions {
            os: "windows".into(),
            ..SnippetOptions::default()
        },
    );
    assert_eq!(win.len(), 4);
    assert_eq!(
        win[0].config_path.as_deref(),
        Some("%APPDATA%\\Claude\\claude_desktop_config.json")
    );
    let linux = setup_snippets(
        &info,
        t,
        &SnippetOptions {
            os: "linux".into(),
            ..SnippetOptions::default()
        },
    );
    assert_eq!(
        linux[0].config_path.as_deref(),
        Some("~/.config/Claude/claude_desktop_config.json")
    );
}

#[test]
fn paths_with_spaces_are_quoted_for_the_shell() {
    let (reg, _) = registry();
    let (info, token) = reg.create("Laptop", all_scopes()).unwrap();
    let s = setup_snippets(
        &info,
        token.expose(),
        &SnippetOptions {
            nuggetd: "/opt/My Apps/nuggetd".into(),
            http_port: None,
            os: "linux".into(),
            env: Vec::new(),
        },
    );
    assert!(s[1].text.contains("-- '/opt/My Apps/nuggetd' mcp"));
}

#[test]
fn env_extras_reach_every_stdio_snippet() {
    let (reg, _) = registry();
    let (info, token) = reg.create("Laptop", all_scopes()).unwrap();
    let t = token.expose();
    let s = setup_snippets(
        &info,
        t,
        &SnippetOptions {
            os: "linux".into(),
            env: vec![
                ("NUGGET_HOME".into(), "/my home".into()),
                // Never overrides the client's token.
                ("NUGGET_MCP_TOKEN".into(), "spoofed".into()),
            ],
            ..SnippetOptions::default()
        },
    );
    for json in [&s[0].text, &s[2].text] {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let env = &v["mcpServers"]["nugget"]["env"];
        assert_eq!(env["NUGGET_HOME"], "/my home");
        assert_eq!(env["NUGGET_MCP_TOKEN"], t);
        assert_eq!(env.as_object().unwrap().len(), 2);
    }
    assert!(
        s[1].text.contains(&format!(
            "--scope user --env NUGGET_HOME='/my home' --env NUGGET_MCP_TOKEN={t} nugget -- "
        )),
        "{}",
        s[1].text
    );
    assert!(!s[1].text.contains("spoofed"));
    assert!(
        s[3].text.ends_with(&format!(
            "\nenv: NUGGET_HOME=/my home\nenv: NUGGET_MCP_TOKEN={t}"
        )),
        "{}",
        s[3].text
    );
}

#[test]
fn client_created_response_redacts_the_token_in_debug() {
    let (reg, _) = registry();
    let (info, token) = reg.create("Laptop", all_scopes()).unwrap();
    let created = nugget_mcp::client_created(info.clone(), &token, &SnippetOptions::default());
    assert_eq!(created.client, info);
    assert_eq!(created.token, token.expose());
    assert!(created.setup.len() >= 4);
    assert!(!format!("{created:?}").contains(token.expose()));
}
