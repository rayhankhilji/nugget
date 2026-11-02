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
//! - [`plans`]: plans and entitlements (`plans.toml`).
//! - [`stripe`]: Checkout, portal, subscriptions and Stripe webhook signatures.

pub mod actions;
pub mod api;
pub mod apps;
pub mod ask;
pub mod auth;
pub mod composio;
pub mod config;
pub mod db;
pub mod gateway;
pub mod ingest;
pub mod plans;
pub mod stripe;
pub mod webhook;

#[cfg(test)]
pub(crate) mod testkit;
