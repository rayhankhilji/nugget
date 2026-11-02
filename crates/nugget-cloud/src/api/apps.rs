//! `GET /v1/apps`: the Connections catalogue. Every app this build knows, with its logo,
//! categories, whether it syncs and which actions it supports, and whether the user has it
//! connected. Logos come from Composio's catalogue (`meta.logo`) when it is reachable, else
//! Composio's logo CDN (`https://logos.composio.dev/api/<slug>`, which serves every slug but is
//! not documented, so it is only the fallback).

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use super::{ApiResult, AppState, user};
use crate::actions::ActionKind;
use crate::apps::{CATALOG, Category, SyncMode};

#[derive(Debug, Serialize)]
struct AppView {
    slug: &'static str,
    name: &'static str,
    /// A `data:` URI (the desktop allows no remote images): fetched, checked, sanitised,
    /// capped at 32 KB and cached; a generated monogram when that fails.
    logo: String,
    /// The remote logo, for the website.
    logo_url: String,
    /// The first of `categories`, for a UI that shows one.
    category: Category,
    categories: &'static [Category],
    syncs: bool,
    actions: &'static [ActionKind],
    sync_mode: SyncMode,
    /// Whether this deployment can connect it (it has a Composio auth config for it).
    connectable: bool,
    connected: bool,
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/v1/apps", get(list_apps))
}

async fn list_apps(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    let connected: Vec<String> =
        s.db.connections(u.id)
            .await?
            .into_iter()
            .filter(|c| c.status == "active")
            .map(|c| c.toolkit)
            .collect();
    let catalog = s.catalog().await;
    // Fetch (or read from the cache) every logo at once.
    let mut set = tokio::task::JoinSet::new();
    for (i, a) in CATALOG.iter().enumerate() {
        let url = catalog
            .iter()
            .find(|t| t.slug == a.slug)
            .and_then(|t| t.logo.clone())
            .unwrap_or_else(|| a.fallback_logo());
        let logos = s.logos.clone();
        set.spawn(async move {
            let data = logos.data_uri(&url, a.name).await;
            (i, url, data)
        });
    }
    let mut logos = vec![(String::new(), String::new()); CATALOG.len()];
    for (i, url, data) in set.join_all().await {
        logos[i] = (url, data);
    }
    let apps: Vec<AppView> = CATALOG
        .iter()
        .zip(logos)
        .map(|(a, (logo_url, logo))| {
            let mode = if a.default_mode == SyncMode::Trigger && s.webhook_secret.is_some() {
                SyncMode::Trigger
            } else {
                SyncMode::Poll
            };
            AppView {
                slug: a.slug,
                name: a.name,
                logo,
                logo_url,
                category: a.categories[0],
                categories: a.categories,
                syncs: a.syncs,
                actions: a.actions,
                sync_mode: mode,
                connectable: s.connectable(a.slug),
                connected: connected.iter().any(|c| c == a.slug),
            }
        })
        .collect();
    Ok(Json(json!({ "apps": apps })))
}
