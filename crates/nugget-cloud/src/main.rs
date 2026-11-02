//! `nugget-cloud`: the HTTP API plus the background sync loop.

use std::sync::Arc;
use std::time::Duration;

use nugget_cloud::api::{AppState, router};
use nugget_cloud::auth::SupabaseAuth;
use nugget_cloud::composio::{Composio, HttpComposio};
use nugget_cloud::config::Config;
use nugget_cloud::db::Db;
use nugget_cloud::gateway::Gateway;
use nugget_cloud::plans::Catalog;
use nugget_cloud::stripe::{HttpStripe, Stripe};

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
    let logo_http = http.clone();
    let auth = SupabaseAuth::new(
        http.clone(),
        cfg.supabase_url.clone(),
        cfg.supabase_anon_key.clone(),
        cfg.supabase_service_role_key.clone(),
    );
    let plans = Catalog::load(|k| std::env::var(k).ok())?;
    let stripe = cfg.stripe_secret_key.as_ref().map(|key| {
        Arc::new(HttpStripe {
            http: http.clone(),
            base: cfg.stripe_base.clone(),
            key: key.clone(),
        }) as Arc<dyn Stripe>
    });
    let mut state = AppState::new(db, Arc::new(auth), gateway, Arc::new(plans));
    state.composio = composio;
    state.auth_configs = cfg.composio_auth_configs.clone();
    state.webhook_secret = cfg.composio_webhook_secret.clone();
    state.connect_callback = cfg.connect_callback.clone();
    state.stripe = stripe;
    state.stripe_webhook_secret = cfg.stripe_webhook_secret.clone();
    state.public_url = cfg.public_url.clone();
    state.app_scheme = cfg.app_scheme.clone();
    state.logos = nugget_cloud::apps::logos::LogoCache::new(logo_http);
    let state = Arc::new(state);
    if let Some(ingest) = state.ingest() {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                ingest.run_due(50).await;
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, "listening");
    axum::serve(listener, router(state)).await?;
    Ok(())
}
