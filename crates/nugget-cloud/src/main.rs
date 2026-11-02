//! `nugget-cloud`: the HTTP API plus the background sync loop.

use std::sync::Arc;
use std::time::Duration;

use nugget_cloud::api::{AppState, router};
use nugget_cloud::auth::SupabaseAuth;
use nugget_cloud::composio::{Composio, HttpComposio};
use nugget_cloud::config::Config;
use nugget_cloud::db::Db;
use nugget_cloud::gateway::Gateway;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cfg = Config::from_env()?;
    tracing::info!(?cfg, "starting");
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let db = Db::connect(&cfg.database_url, cfg.database_ca_cert.as_deref())?;
    let composio = cfg.composio_api_key.as_ref().map(|key| {
        Arc::new(HttpComposio {
            http: http.clone(),
            base: cfg.composio_base.clone(),
            key: key.clone(),
        }) as Arc<dyn Composio>
    });
    let gateway = Gateway::from_config(&cfg, http.clone());
    if !gateway.has_llm() {
        tracing::warn!("no LLM key: /v1/ask will answer 503");
    }
    let auth = SupabaseAuth::new(
        http,
        cfg.supabase_url.clone(),
        cfg.supabase_anon_key.clone(),
        cfg.supabase_service_role_key.clone(),
    );
    let state = Arc::new(AppState::new(
        db,
        Arc::new(auth),
        composio,
        gateway,
        cfg.composio_auth_configs.clone(),
        cfg.composio_webhook_secret.clone(),
        cfg.connect_callback.clone(),
    ));
    if let Some(ingest) = state.ingest() {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(300));
            loop {
                tick.tick().await;
                ingest.run_due(20).await;
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, "listening");
    axum::serve(listener, router(state)).await?;
    Ok(())
}
