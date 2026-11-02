//! End-to-end check of a self-hosted stack (`selfhost/docker-compose.yml`) with the desktop's
//! own client (`nugget-cloud-client`): magic-link sign-in through GoTrue and Mailpit, then
//! entitlement, connect, sync, a cited Ask, export, and account deletion. Run it with the stack
//! up and the server pointed at `fake_providers` (see the self-hosting guide).
//!
//! ```sh
//! HEED_URL=http://localhost:8000 HEED_ANON_KEY=... MAILPIT_URL=http://localhost:8025 \
//!   cargo run -p nugget-cloud --example selfhost_smoke
//! ```

use std::sync::Arc;
use std::time::Duration;

use nugget_cloud_client::session::{MemoryStore, Session, SystemClock};
use nugget_cloud_client::{Api, AskRequest, CloudConfig, parse_callback};
use serde_json::Value;

type R<T> = Result<T, Box<dyn std::error::Error>>;

fn step(name: &str) {
    eprintln!("==> {name}");
}

/// The newest magic link Mailpit received for `email`.
async fn magic_link(http: &reqwest::Client, mailpit: &str, email: &str) -> R<String> {
    for _ in 0..30 {
        let list: Value = http
            .get(format!("{mailpit}/api/v1/search?query=to:{email}"))
            .send()
            .await?
            .json()
            .await?;
        if let Some(id) = list["messages"][0]["ID"].as_str() {
            let msg: Value = http
                .get(format!("{mailpit}/api/v1/message/{id}"))
                .send()
                .await?
                .json()
                .await?;
            let body = format!(
                "{} {}",
                msg["Text"].as_str().unwrap_or(""),
                msg["HTML"].as_str().unwrap_or("")
            );
            let start = body.find("http").ok_or("no link in the email")?;
            let link: String = body[start..]
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '<' && *c != ')')
                .collect();
            return Ok(link.replace("&amp;", "&"));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("no email arrived in Mailpit".into())
}

#[tokio::main]
async fn main() -> R<()> {
    let base = std::env::var("HEED_URL").unwrap_or_else(|_| "http://localhost:8000".into());
    let anon = std::env::var("HEED_ANON_KEY")?;
    let mailpit = std::env::var("MAILPIT_URL").unwrap_or_else(|_| "http://localhost:8025".into());
    let email = format!("smoke+{}@example.com", std::process::id());
    let cfg = CloudConfig::from_lookup(|k| match k {
        "NUGGET_SUPABASE_URL" | "NUGGET_CLOUD_URL" => Some(base.clone()),
        "NUGGET_SUPABASE_ANON_KEY" => Some(anon.clone()),
        _ => None,
    })?;
    let http = nugget_cloud_client::http_client()?;
    let session = Arc::new(Session::new(
        cfg,
        http,
        Arc::new(MemoryStore::default()),
        Arc::new(SystemClock),
    ));

    step("magic link: request (PKCE)");
    session.begin_magic_link(&email).await?;
    let link = magic_link(&reqwest::Client::new(), &mailpit, &email).await?;
    step("magic link: open it, as the browser would");
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let res = no_redirect.get(&link).send().await?;
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or("no redirect from verify")?
        .to_owned();
    assert!(
        location.starts_with("heed://auth/callback?")
            || location.starts_with("nugget://auth/callback?"),
        "redirected to {location}"
    );
    step("deep link: exchange the code");
    let status = session.complete(parse_callback(&location)?).await?;
    assert!(status.signed_in, "{status:?}");

    let api = Api::new(session.clone());
    step("me: billing off, everyone entitled");
    let me = api.me().await?;
    eprintln!("    plan={:?} email={:?}", me.plan, me.email);
    assert_eq!(me.plan.as_deref(), Some("heed_max"));

    step("connect Gmail and Google Calendar (fake Composio)");
    for t in ["gmail", "googlecalendar"] {
        let start = api.connect(t).await?;
        assert!(start.redirect_url.starts_with("https://"));
    }
    let conns = api.connections().await?;
    assert_eq!(
        conns
            .connections
            .iter()
            .filter(|c| c.status == "active")
            .count(),
        2,
        "{conns:?}"
    );

    step("sync");
    let report = api.sync().await?;
    eprintln!("    {:?}", report.synced);
    for _ in 0..60 {
        let c = api.connections().await?;
        if c.connections
            .iter()
            .all(|c| !c.syncing && c.items_synced > 0)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    step("ask, with citations");
    let answer = api
        .ask(&AskRequest {
            question: "When does my lease renew?".into(),
            conversation_id: None,
            model: None,
        })
        .await?;
    eprintln!(
        "    {} (citations: {})",
        answer.answer,
        answer.citations.len()
    );
    assert_eq!(
        answer.citations.first().and_then(|c| c.title.as_deref()),
        Some("Lease renewal")
    );

    step("export");
    let export = api.export().await?;
    assert_eq!(export["items"].as_array().map(Vec::len), Some(3));

    step("delete account");
    api.delete_account().await?;
    assert!(api.me().await.is_err(), "the deleted user is signed out");
    eprintln!("selfhost smoke: OK");
    Ok(())
}
