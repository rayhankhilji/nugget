//! SaaS connectors: Slack, Notion, GitHub and Linear, read through their official APIs with the
//! user's own tokens. No intermediary service and no model sits between Nugget and the API, so a
//! sync is deterministic, free and works with any (or no) AI plan.
//!
//! | kind | API | incremental by | reconciled by |
//! |---|---|---|---|
//! | `slack` | Web API (`conversations.history` / `.replies`) | per-conversation `latest` ts | re-reading a recent window every few hours |
//! | `notion` | REST `2022-06-28` (`search`, `blocks.children`) | `last_edited_time` | a full `search` listing every few hours |
//! | `github` | GraphQL v4 (`search`) | `updated:>=` | an ids-only `search` listing daily |
//! | `linear` | GraphQL (`issues`) | `updatedAt.gte` | an ids-only `issues` listing every few hours |
//!
//! **Credentials.** Every kind is [`AuthKind::Password`](nugget_connector::AuthKind): the user
//! pastes a token through `sources.secret.set`, stored in the OS keychain under
//! `password:<source>`. `token_env` in settings names an environment variable instead (headless
//! use). GitHub (device flow) and Linear (PKCE) can also be connected interactively through
//! `begin_auth` when settings carry a `client_id`; those tokens live under `oauth:<source>`.
//! See [`auth`] for the lookup order.
//!
//! **Egress.** All HTTP goes through [`MeteredHttp`](nugget_connector::http::MeteredHttp) with
//! feature `sync:<kind>`, so every exchange is in the egress ledger and no redirect is followed.
//! Endpoints are injectable with [`Endpoints`] for tests.
//!
//! **Voice hints.** Every document carries `from_me` (the user wrote at least part of it) and,
//! where it applies, `my_message_count` in its metadata, plus the user's handle under `me`, so
//! the voice profile can pick out what the user wrote.

mod api;
pub mod auth;
mod github;
mod linear;
mod notion;
mod settings;
mod slack;
mod text;

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::Registry;

pub use github::{GitHubConnector, GitHubFactory};
pub use linear::{LinearConnector, LinearFactory};
pub use notion::{NotionConnector, NotionFactory};
pub use slack::{SlackConnector, SlackFactory};

pub const SLACK_KIND: &str = "slack";
pub const NOTION_KIND: &str = "notion";
pub const GITHUB_KIND: &str = "github";
pub const LINEAR_KIND: &str = "linear";

/// API endpoints. [`Default`] is production; tests point everything at a mock server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Slack Web API root, e.g. `https://slack.com/api`.
    pub slack_api: String,
    /// Notion API root, e.g. `https://api.notion.com/v1`.
    pub notion_api: String,
    /// GitHub GraphQL endpoint, e.g. `https://api.github.com/graphql`.
    pub github_graphql: String,
    /// GitHub OAuth root (device flow), e.g. `https://github.com/login`.
    pub github_login: String,
    /// Linear GraphQL endpoint, e.g. `https://api.linear.app/graphql`.
    pub linear_graphql: String,
    pub linear_auth_url: String,
    pub linear_token_url: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            slack_api: "https://slack.com/api".into(),
            notion_api: "https://api.notion.com/v1".into(),
            github_graphql: "https://api.github.com/graphql".into(),
            github_login: "https://github.com/login".into(),
            linear_graphql: "https://api.linear.app/graphql".into(),
            linear_auth_url: auth::linear::AUTH_URL.into(),
            linear_token_url: auth::linear::TOKEN_URL.into(),
        }
    }
}

impl Endpoints {
    /// Everything under one base URL (`<base>/slack`, `<base>/notion/v1`, `<base>/github/graphql`,
    /// `<base>/github/login`, `<base>/linear/graphql`, `<base>/linear/authorize`,
    /// `<base>/linear/token`). For tests.
    pub fn with_base(base: &str) -> Self {
        let b = base.trim_end_matches('/');
        Self {
            slack_api: format!("{b}/slack"),
            notion_api: format!("{b}/notion/v1"),
            github_graphql: format!("{b}/github/graphql"),
            github_login: format!("{b}/github/login"),
            linear_graphql: format!("{b}/linear/graphql"),
            linear_auth_url: format!("{b}/linear/authorize"),
            linear_token_url: format!("{b}/linear/token"),
        }
    }
}

/// In-pass retries for rate limits and transient failures. A `Retry-After` longer than
/// `max_delay` is not slept on: the pass returns `RateLimited { retry_after_ms }` at once and
/// the ingest supervisor backs off; progress up to the last checkpoint is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// Register the `slack`, `notion`, `github` and `linear` kinds with production endpoints.
pub fn register(registry: &mut Registry) {
    registry.register(Arc::new(SlackFactory::new()));
    registry.register(Arc::new(NotionFactory::new()));
    registry.register(Arc::new(GitHubFactory::new()));
    registry.register(Arc::new(LinearFactory::new()));
}

#[cfg(test)]
mod tests;
