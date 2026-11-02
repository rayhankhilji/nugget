//! Heed Cloud service (see `docs/architecture-cloud.md`).
//!
//! - [`auth`]: checks Supabase access tokens, deletes users (GDPR).
//! - [`db`]: Postgres access; every user query runs in a transaction scoped by RLS.
//! - [`composio`]: connected accounts and tool calls through Composio.
//! - [`apps`]: the Connections catalogue and one sync adapter per app (Gmail, Calendar, Drive,
//!   Outlook, Slack, Teams, Notion) behind one trait.
//! - [`ingest`]: runs an app's adapter into `items` and `chunks` (dedup, deletes, cursors).
//! - [`actions`]: outbound actions behind proposal, preview, approval and an audit chain.
//! - [`gateway`]: LLM and embedding calls with our keys, metering and quotas.
//! - [`llm`]: the Anthropic-compatible streaming model endpoint for the desktop (`/llm/v1`).
//! - [`ask`]: retrieval over the user's chunks and a cited answer.
//! - [`api`]: the `/v1` HTTP routes.
//! - [`webhook`]: Composio webhook signature check and replay guard.
//! - [`ratelimit`]: per-IP and per-user token buckets.
//! - [`ops`]: request ids, request logs, metrics, error reports, shutdown.
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
pub mod authors;
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
pub mod llm;
pub mod llm_bridge;
pub mod ops;
pub mod plans;
pub mod ratelimit;
pub mod search;
pub mod snapshot;
pub mod stripe;
pub mod webhook;

#[cfg(test)]
pub(crate) mod testkit;

/// Sign in with ChatGPT is for running Heed locally: OpenAI's open-source programme covers
/// locally run apps, and hosted or paid use needs OpenAI's approval, which Heed has not
/// requested. The hosted service therefore never builds `nugget_infer::chatgpt` (its
/// `chatgpt-plan` feature stays off here) and never refers to ChatGPT plan tokens.
#[cfg(test)]
mod no_chatgpt_plan_in_the_cloud {
    #[test]
    fn the_service_does_not_enable_or_use_chatgpt_plan_usage() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !manifest.contains("chatgpt-plan"),
            "nugget-cloud must not enable chatgpt-plan"
        );
        let mut stack = vec![root.join("src")];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("lib.rs") {
                    let text = std::fs::read_to_string(&p).unwrap();
                    for banned in [
                        "nugget_infer::chatgpt",
                        "chatgpt.tokens.use.direct",
                        "dynamic_agent_client",
                    ] {
                        assert!(!text.contains(banned), "{} mentions {banned}", p.display());
                    }
                }
            }
        }
    }
}
