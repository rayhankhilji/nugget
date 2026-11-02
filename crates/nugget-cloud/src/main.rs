//! `nugget-cloud`: the HTTP API, the internal metrics listener and the background sync loop.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::serve::ListenerExt;
use nugget_cloud::api::{AppState, metrics_router, router};
use nugget_cloud::auth::SupabaseAuth;
use nugget_cloud::composio::{Composio, HttpComposio};
use nugget_cloud::config::Config;
use nugget_cloud::db::Db;
use nugget_cloud::gateway::Gateway;
use nugget_cloud::llm::{LlmConfig, LlmState, upstream_client};
use nugget_cloud::ops;
use nugget_cloud::plans::Catalog;
use nugget_cloud::stripe::{HttpStripe, Stripe};

fn init_logs(format: &str) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if format == "text" {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        // One JSON object per line, with the request span (request_id) on each event.
        tracing_subscriber::fmt()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_env_filter(filter)
            .init();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env()?;
    init_logs(&cfg.log_format);
    ops::install_panic_hook();
    tracing::info!(?cfg, "starting");
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    if let Some(raw) = &cfg.sentry_dsn {
        match ops::Dsn::parse(raw) {
            Some(dsn) => {
                ops::Reporter::new(http.clone(), dsn, cfg.sentry_environment.clone()).install()
            }
            None => tracing::warn!("SENTRY_DSN is not a valid DSN; error reports are off"),
        }
    }
    let db = Db::connect_with(&cfg.database_url, cfg.database_ca_cert.as_deref(), cfg.pool)?;
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
    state.logos = nugget_cloud::apps::logos::LogoCache::new();
    state.site_origins = cfg.site_origins.clone();
    let state = Arc::new(
        state
            .with_limits(cfg.limits)
            .with_client_ip_header(cfg.client_ip_header.clone()),
    );

    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let sync_loop = state.ingest().map(|ingest| {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = tick.tick() => { ingest.run_due(50).await; }
                    _ = stop_rx.changed() => break,
                }
            }
        })
    });

    if let Some(bind) = &cfg.metrics_bind {
        let listener = tokio::net::TcpListener::bind(bind).await?;
        tracing::info!(addr = %bind, "metrics listening");
        let app = metrics_router(state.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::warn!(error = %e, "metrics listener stopped");
            }
        });
    }
    // Knowledge, cards and the 07:00 brief (see `intel`).
    let intel = state.intel();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            intel.tick().await;
        }
    });
    // The streaming model endpoint for the desktop (`/llm/v1`, see `llm`).
    let llm = Arc::new(LlmState::new(
        state.clone(),
        LlmConfig::from_config(&cfg, |k| std::env::var(k).ok()),
        upstream_client()?,
    ));
    // No Nagle delay: streamed tokens (ghost text) go out as soon as they are written.
    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await?
        .tap_io(|tcp| {
            let _ = tcp.set_nodelay(true);
        });
    tracing::info!(addr = %cfg.bind, "listening");
    let grace = Duration::from_secs(cfg.shutdown_grace_secs);
    let (drained_tx, drained_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        router(state)
            .merge(nugget_cloud::llm::router(llm))
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        ops::shutdown_signal().await;
        let _ = stop_tx.send(true);
        let _ = drained_tx.send(());
    });
    let serve = tokio::spawn(async move { server.await });
    // Wait for the server to drain; after a signal, give in-flight work at most `grace`.
    tokio::select! {
        r = serve => r??,
        _ = async {
            let _ = drained_rx.await;
            tokio::time::sleep(grace).await;
        } => tracing::warn!("grace period over; exiting with requests still in flight"),
    }
    if let Some(h) = sync_loop {
        // The current sync pass finishes or is cut off; its lease expires on its own.
        let _ = tokio::time::timeout(Duration::from_secs(5), h).await;
    }
    tracing::info!("stopped");
    Ok(())
}
