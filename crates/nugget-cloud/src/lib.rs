//! Nugget Cloud service (see `docs/architecture-cloud.md`).
//!
//! - [`auth`]: checks Supabase access tokens, deletes users (GDPR).
//! - [`db`]: Postgres access; every user query runs in a transaction scoped by RLS.
//! - [`composio`]: connected accounts and tool calls through Composio.
//! - [`apps`]: the Connections catalogue and one sync adapter per app (Gmail, Calendar, Drive,
//!   Outlook, Slack, Teams, Notion) behind one trait.
//! - [`ingest`]: runs an app's adapter into `items` and `chunks` (dedup, deletes, cursors).
//! - [`actions`]: outbound actions behind proposal, preview, approval and an audit chain.
//! - [`gateway`]: LLM and embedding calls with our keys, metering and quotas.
//! - [`ask`]: retrieval over the user's chunks and a cited answer.
//! - [`api`]: the `/v1` HTTP routes.
//! - [`webhook`]: Composio webhook signature check.
//! - [`snapshot`]: a user's items as an in-memory local store, for the reused crates below.
//! - [`knowledge`]: `nugget-knowledge` extraction into `entities` and `claims`.
//! - [`cards`]: `nugget-proactive` cards ("Needs you") with accept, dismiss and snooze.
//! - [`brief`]: the daily morning brief with cited prose.
//! - [`search`]: hybrid search with facets and a timeline.
//! - [`intel`]: the per-user jobs and their scheduler; [`llm_bridge`]: the gateway as a
//!   `nugget-infer` provider (metered, cached).
//! - [`plans`]: plans and entitlements (`plans.toml`).
//! - [`stripe`]: Checkout, portal, subscriptions and Stripe webhook signatures.

pub mod actions;
pub mod api;
pub mod apps;
pub mod ask;
pub mod auth;
pub mod brief;
pub mod card_actions;
pub mod cards;
pub mod composio;
pub mod config;
pub mod db;
pub mod gateway;
pub mod ingest;
pub mod intel;
pub mod knowledge;
pub mod llm_bridge;
pub mod plans;
pub mod search;
pub mod snapshot;
pub mod stripe;
pub mod webhook;

#[cfg(test)]
pub(crate) mod testkit;
