//! `github`: issues, pull requests and discussions the user takes part in, through the GitHub
//! GraphQL API (v4).
//!
//! Scopes: a fine-grained personal access token with read-only *Issues*, *Pull requests*,
//! *Discussions* and *Metadata* on the chosen repositories; or, with the device flow, an OAuth
//! App token with `repo` (read access to private issues and pull requests has no narrower
//! classic scope) and `read:discussion`, or a GitHub App with the same read-only permissions.
//! Tokens come only from the keychain (or `token_env`).
//!
//! **Why GraphQL.** Discussions exist only in GraphQL, and one GraphQL search page returns each
//! issue or pull request *with* its labels, comments and reviews, where REST needs a search call
//! plus two to three calls per item (`/comments`, `/pulls/{n}/reviews`, `/pulls/{n}/comments`).
//! Fewer requests matter more than raw points here: GitHub's secondary rate limits count
//! requests, and the search endpoint is limited to 30 requests per minute.
//!
//! **Scope.** `involves:<viewer>` (author, assignee, commenter or mentioned), or with `repos`
//! the same per repository (`all_activity: true`: every issue in those repositories).
//!
//! **Documents.** One Markdown document per issue / pull request (`issue/<owner>/<repo>/<n>`)
//! or discussion (`discussion/<owner>/<repo>/<n>`): the body, then comments and reviews
//! (with their inline comments) in order, discussion replies indented under their comment.
//!
//! **Cursor** (JSON): `{"v":1,"viewer":"<login>","window_from":<unix>,"reconciled_at":<unix>,
//! "scopes":{"<type>|<query>":{"since":<unix>}}}`. Each scope pages `search` with
//! `updated:>=<since> sort:updated-asc` and checkpoints `since` after every page; when a query
//! reaches the 1000-result search cap it restarts from the last `updatedAt` seen.
//!
//! **Reconciling.** A first pass (or a reset cursor) lists the whole window and deletes indexed
//! items it did not return. Every [`RECONCILE_INTERVAL_SECS`] an ids-only listing does the same
//! (deleted, transferred or no-longer-visible items).
//!
//! **Rate limits.** `Retry-After`, `x-ratelimit-remaining: 0` with `x-ratelimit-reset`, the
//! "secondary rate limit" 403, and GraphQL `RATE_LIMITED` errors are all honoured; requests are
//! never concurrent. A pass stops early (after checkpointing) when the hourly budget runs low.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::http::{MeteredHttp, Response};
use nugget_connector::{
    AuthFlow, AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext,
    ConnectorFactory, Content, Document, KindInfo,
};
use nugget_core::ids::ItemKey;
use nugget_core::{Error, Result};
use nugget_oauth::TokenSource;
use reqwest::Method;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{
    self, Api, ItemFailures, Outcome, classify_common, header_u64, retry_after_ms, server_now,
};
use crate::auth::{self, Credential, DeviceFlow, Token};
use crate::settings::{self, MAX_WINDOW_DAYS};
use crate::text::{
    DAY_SECS, clean, date, now_unix, one_line, parse_rfc3339, rfc3339, truncate_chars,
};
use crate::{Endpoints, GITHUB_KIND, RetryPolicy};

const FEATURE: &str = "sync:github";
const SERVICE: &str = "GitHub";
pub(crate) const RECONCILE_INTERVAL_SECS: i64 = 24 * 3600;
const PAGE_SIZE: i64 = 20;
const ID_PAGE_SIZE: i64 = 100;
/// GitHub search returns at most 1000 results per query.
const SEARCH_CAP: usize = 1_000;
const MAX_PAGES_PER_SCOPE: usize = 500;
const MAX_COMMENTS: usize = 300;
const MAX_BODY_CHARS: usize = 60_000;
const MAX_DOC_CHARS: usize = 400_000;
/// Stop the pass (and let the supervisor wait) below this many remaining GraphQL points.
const MIN_POINTS: i64 = 50;
const MAX_REPOS: usize = 100;
/// How far before the watermark each incremental search starts (search indexing lag).
pub(crate) const WATERMARK_OVERLAP_SECS: i64 = 300;

fn default_window() -> u32 {
    365
}
fn yes() -> bool {
    true
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitHubSettings {
    #[serde(default)]
    token_env: Option<String>,
    /// OAuth App / GitHub App client id for the device flow (instead of a pasted PAT).
    #[serde(default)]
    client_id: Option<String>,
    /// Only for refreshing expiring GitHub App user tokens; normally injected, never stored.
    #[serde(default)]
    client_secret: Option<String>,
    /// `owner/name` repositories. Empty: everything involving the user.
    #[serde(default)]
    repos: Option<Vec<String>>,
    /// With `repos`: every issue, PR and discussion, not only those involving the user.
    #[serde(default)]
    all_activity: bool,
    #[serde(default = "yes")]
    include_discussions: bool,
    #[serde(default = "default_window")]
    window_days: u32,
}

fn valid_repo(r: &str) -> bool {
    let Some((owner, name)) = r.split_once('/') else {
        return false;
    };
    let part = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && s != "."
            && s != ".."
    };
    part(owner) && part(name)
}

impl GitHubSettings {
    pub(crate) fn parse(v: &Value) -> Result<Self> {
        let s: Self = settings::parse(v)?;
        settings::check_token_env(s.token_env.as_deref())?;
        settings::check_client_id(s.client_id.as_deref())?;
        settings::check_range("window_days", s.window_days, 1, MAX_WINDOW_DAYS)?;
        if let Some(repos) = &s.repos {
            if repos.len() > MAX_REPOS {
                return Err(Error::Invalid("settings: too many repos".into()));
            }
            if let Some(bad) = repos.iter().find(|r| !valid_repo(r.trim())) {
                return Err(Error::Invalid(format!(
                    "settings: repos entry `{}` is not owner/name",
                    one_line(bad, 100)
                )));
            }
        }
        if s.all_activity && s.repos.as_ref().is_none_or(Vec::is_empty) {
            return Err(Error::Invalid(
                "settings: all_activity needs a list of repos".into(),
            ));
        }
        Ok(s)
    }

    pub(crate) fn schema() -> Value {
        settings::schema(
            "a fine-grained personal access token with read access to Issues, Pull requests, Discussions and Metadata.",
            json!({
                "client_id": settings::client_id_prop("Client ID of your own GitHub OAuth App or GitHub App with the device flow enabled. Set it to connect in the browser instead of pasting a token."),
                "repos": {"type": "array", "items": {"type": "string", "pattern": "^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$"},
                    "title": "Repositories", "maxItems": MAX_REPOS,
                    "description": "owner/name. Leave empty for every issue, pull request and discussion you take part in."},
                "all_activity": {"type": "boolean", "default": false, "title": "All activity in these repositories",
                    "description": "With repositories listed: index everything in them, not only what involves you."},
                "include_discussions": {"type": "boolean", "default": true, "title": "Discussions"},
                "window_days": settings::window_prop(365, "issues and pull requests")
            }),
        )
    }

    /// `(scope id, search type, query)` for every scope.
    fn scopes(&self, viewer: &str) -> Vec<(String, &'static str, String)> {
        let bases: Vec<String> = match self.repos.as_ref().filter(|r| !r.is_empty()) {
            Some(repos) => repos
                .iter()
                .map(|r| {
                    if self.all_activity {
                        format!("repo:{}", r.trim())
                    } else {
                        format!("repo:{} involves:{viewer}", r.trim())
                    }
                })
                .collect(),
            None => vec![format!("involves:{viewer}")],
        };
        let mut out = Vec::new();
        for b in bases {
            out.push((format!("issue|{b}"), "ISSUE", b.clone()));
            if self.include_discussions {
                out.push((format!("discussion|{b}"), "DISCUSSION", b));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ScopeState {
    since: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viewer: Option<String>,
    #[serde(default)]
    window_from: i64,
    #[serde(default)]
    reconciled_at: i64,
    #[serde(default)]
    scopes: BTreeMap<String, ScopeState>,
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Factory for `github` sources.
#[derive(Debug, Clone)]
pub struct GitHubFactory {
    endpoints: Endpoints,
    retry: RetryPolicy,
    device_poll: Duration,
}

impl Default for GitHubFactory {
    fn default() -> Self {
        Self {
            endpoints: Endpoints::default(),
            retry: RetryPolicy::default(),
            device_poll: Duration::from_secs(5),
        }
    }
}

impl GitHubFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Shortest wait between device-flow polls (GitHub's `interval` wins when longer). Tests.
    pub fn with_device_poll_interval(mut self, d: Duration) -> Self {
        self.device_poll = d;
        self
    }
}

impl ConnectorFactory for GitHubFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: GITHUB_KIND,
            display_name: "GitHub",
            auth: AuthKind::Password,
            settings_schema: GitHubSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        GitHubSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = GitHubSettings::parse(settings)?;
        let http = api::metered(ctx.egress.clone(), FEATURE)?;
        let tokens = match &settings.client_id {
            Some(id) => Some(auth::token_source(
                auth::github::provider(&self.endpoints.github_login),
                id,
                auth::client_secret(
                    &ctx.secrets,
                    ctx.source.as_str(),
                    settings.client_secret.as_deref(),
                ),
                http.clone(),
                ctx.secrets.clone(),
                ctx.source.as_str(),
            )?),
            None => None,
        };
        let cred = Arc::new(Credential::new(
            SERVICE,
            ctx.secrets.clone(),
            ctx.source.as_str(),
            settings.token_env.clone(),
            tokens.clone(),
        ));
        Ok(Arc::new(GitHubConnector {
            api: Api::new(http, cred, self.retry, SERVICE),
            graphql: self.endpoints.github_graphql.clone(),
            login_root: self.endpoints.github_login.clone(),
            device_poll: self.device_poll,
            tokens,
            settings,
            ctx,
        }))
    }
}

/// One GitHub account's issues, pull requests and discussions.
pub struct GitHubConnector {
    api: Api,
    graphql: String,
    login_root: String,
    device_poll: Duration,
    tokens: Option<Arc<TokenSource>>,
    settings: GitHubSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for GitHubConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

fn classify(resp: Response) -> Outcome {
    let status = resp.status.as_u16();
    let h = &resp.headers;
    // Measured against the response's `Date`, not the local clock (which may be off).
    let reset_ms = || {
        header_u64(h, "x-ratelimit-reset").map(|reset| {
            let now = u64::try_from(server_now(h)).unwrap_or(0);
            reset
                .saturating_sub(now)
                .saturating_add(1)
                .saturating_mul(1000)
        })
    };
    if resp.status.is_success() {
        let body: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
        let limited = body["errors"]
            .as_array()
            .is_some_and(|e| e.iter().any(|x| x["type"].as_str() == Some("RATE_LIMITED")));
        if limited {
            return Outcome::Retry(Error::RateLimited {
                retry_after_ms: retry_after_ms(h).or_else(reset_ms).or(Some(60_000)),
            });
        }
        return Outcome::Done(resp);
    }
    if matches!(status, 403 | 429) {
        if let Some(ms) = retry_after_ms(h) {
            return Outcome::Retry(Error::RateLimited {
                retry_after_ms: Some(ms),
            });
        }
        if header_u64(h, "x-ratelimit-remaining") == Some(0) {
            return Outcome::Retry(Error::RateLimited {
                retry_after_ms: reset_ms(),
            });
        }
        let text = String::from_utf8_lossy(&resp.body[..resp.body.len().min(4096)]).to_lowercase();
        if text.contains("secondary rate limit") || status == 429 {
            // GitHub: wait at least a minute when no header says otherwise.
            return Outcome::Retry(Error::RateLimited {
                retry_after_ms: Some(60_000),
            });
        }
    }
    classify_common(resp, SERVICE)
}

/// Shared selection of an issue's or pull request's fields.
const ISSUE_FIELDS: &str = "
  number title url state body createdAt updatedAt
  author { login }
  repository { nameWithOwner }
  labels(first: 20) { nodes { name } }
  assignees(first: 10) { nodes { login } }
  comments(first: 50) { totalCount pageInfo { hasNextPage endCursor } nodes { author { login } body createdAt url } }";

fn search_query() -> String {
    format!(
        "query($q: String!, $type: SearchType!, $first: Int!, $after: String) {{
  rateLimit {{ remaining resetAt }}
  search(query: $q, type: $type, first: $first, after: $after) {{
    pageInfo {{ hasNextPage endCursor }}
    nodes {{
      __typename
      ... on Issue {{ id {ISSUE_FIELDS} }}
      ... on PullRequest {{ id {ISSUE_FIELDS} isDraft merged
        reviews(first: 20) {{ nodes {{ author {{ login }} state body submittedAt url
          comments(first: 20) {{ nodes {{ author {{ login }} body path createdAt }} }} }} }} }}
      ... on Discussion {{ id number title url body createdAt updatedAt
        author {{ login }} repository {{ nameWithOwner }} category {{ name }} answer {{ id }}
        labels(first: 20) {{ nodes {{ name }} }}
        comments(first: 30) {{ totalCount nodes {{ id author {{ login }} body createdAt url
          replies(first: 20) {{ nodes {{ author {{ login }} body createdAt }} }} }} }} }}
    }}
  }}
}}"
    )
}

const ID_QUERY: &str = "query($q: String!, $type: SearchType!, $first: Int!, $after: String) {
  rateLimit { remaining resetAt }
  search(query: $q, type: $type, first: $first, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes {
      __typename
      ... on Issue { number updatedAt repository { nameWithOwner } }
      ... on PullRequest { number updatedAt repository { nameWithOwner } }
      ... on Discussion { number updatedAt repository { nameWithOwner } }
    }
  }
}";

const MORE_COMMENTS: &str = "query($id: ID!, $after: String) {
  node(id: $id) {
    ... on Issue { comments(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { author { login } body createdAt url } } }
    ... on PullRequest { comments(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { author { login } body createdAt url } } }
  }
}";

#[async_trait]
impl Connector for GitHubConnector {
    fn kind(&self) -> &'static str {
        GITHUB_KIND
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(600)
    }

    async fn begin_auth(&self) -> Result<AuthFlow> {
        let Some(client_id) = &self.settings.client_id else {
            return Err(Error::Unsupported(
                "set client_id (an OAuth App with the device flow enabled) or paste a personal access token".into(),
            ));
        };
        let flow = auth::github_device_flow(
            &self.api.http,
            DeviceFlow {
                login_root: self.login_root.clone(),
                client_id: client_id.trim().to_string(),
                min_interval: self.device_poll,
                timeout: nugget_oauth::DEFAULT_AUTH_TIMEOUT,
            },
            self.ctx.secrets.clone(),
            self.ctx.source.as_str(),
        )
        .await?;
        Ok(auth::clearing(
            flow,
            self.api.cred.clone(),
            self.tokens.clone(),
        ))
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.api.cred.reset();
        let mut cur = cursor
            .and_then(|v| match serde_json::from_value::<Cursor>(v) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(source = %self.ctx.source, "unreadable github cursor ({e}); starting over");
                    None
                }
            })
            .unwrap_or_default();
        cur.v = 1;
        let viewer = self.viewer(cancel).await?;
        let now = now_unix();
        let fresh = cur.viewer.as_deref() != Some(viewer.as_str()) || cur.window_from == 0;
        if fresh {
            cur = Cursor {
                v: 1,
                viewer: Some(viewer.clone()),
                window_from: now - i64::from(self.settings.window_days) * DAY_SECS,
                reconciled_at: 0,
                scopes: BTreeMap::new(),
            };
        }
        let scopes = self.settings.scopes(&viewer);
        let wanted: HashSet<&str> = scopes.iter().map(|(id, _, _)| id.as_str()).collect();
        cur.scopes.retain(|k, _| wanted.contains(k.as_str()));
        let known = self.ctx.known.versions().await?;

        // Periodic reconcile: an ids-only listing of the window.
        let periodic = !fresh && now - cur.reconciled_at >= RECONCILE_INTERVAL_SECS;
        let mut live: HashSet<String> = HashSet::new();
        if periodic {
            for (_, ty, base) in &scopes {
                self.list_ids(ty, base, cur.window_from, &mut live, cancel)
                    .await?;
            }
        }

        for (id, ty, base) in &scopes {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.sync_scope(
                id, ty, base, &viewer, &mut cur, &known, &mut live, sink, cancel,
            )
            .await?;
        }
        if fresh || periodic {
            for key in known.keys().filter(|k| !live.contains(k.as_str())) {
                sink.send(Change::Delete(key.clone())).await?;
            }
            cur.reconciled_at = now;
        }
        Ok(cur.to_value())
    }
}

impl GitHubConnector {
    async fn graphql(
        &self,
        query: &str,
        variables: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let body = json!({"query": query, "variables": variables}).to_string();
        let url = self.graphql.clone();
        let resp = self
            .api
            .execute(cancel, classify, |http: &MeteredHttp, token: &Token| {
                http.request(Method::POST, &url)
                    .bearer_auth(token.secret())
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json")
                    .body(body.clone())
            })
            .await?;
        let v: Value = api::json(&resp, "github graphql")?;
        let server_clock = server_now(&resp.headers);
        if let Some(errors) = v["errors"].as_array().filter(|e| !e.is_empty()) {
            if v["data"].is_null()
                || v["data"]
                    .as_object()
                    .is_some_and(|d| d.values().all(Value::is_null))
            {
                let msg = errors[0]["message"]
                    .as_str()
                    .map(|m| one_line(m, 200))
                    .unwrap_or_default();
                return Err(match errors[0]["type"].as_str() {
                    Some("FORBIDDEN" | "INSUFFICIENT_SCOPES") => {
                        Error::Unauthorized(format!("GitHub: {msg}"))
                    }
                    _ => Error::Other(format!("GitHub GraphQL error: {msg}")),
                });
            }
            tracing::debug!(source = %self.ctx.source, "github graphql returned partial errors");
        }
        let remaining = v["data"]["rateLimit"]["remaining"].as_i64();
        if remaining.is_some_and(|r| r < MIN_POINTS) {
            let reset = v["data"]["rateLimit"]["resetAt"]
                .as_str()
                .and_then(parse_rfc3339)
                .map(|t| t.unix_timestamp());
            return Err(Error::RateLimited {
                retry_after_ms: reset
                    .map(|r| u64::try_from((r - server_clock).max(1)).unwrap_or(60) * 1000),
            });
        }
        Ok(v)
    }

    async fn viewer(&self, cancel: &CancellationToken) -> Result<String> {
        let v = self
            .graphql("query { viewer { login } }", json!({}), cancel)
            .await?;
        v["data"]["viewer"]["login"]
            .as_str()
            .filter(|l| {
                !l.is_empty()
                    && l.len() <= 64
                    && l.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'[' | b']')
                    })
            })
            .map(str::to_string)
            .ok_or_else(|| Error::Invalid("GitHub returned no viewer login".into()))
    }

    /// Keys of every item a scope's search returns from `from`, ids only.
    async fn list_ids(
        &self,
        ty: &str,
        base: &str,
        from: i64,
        live: &mut HashSet<String>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut since = from;
        let mut after: Option<String> = None;
        let mut in_query = 0usize;
        for _ in 0..MAX_PAGES_PER_SCOPE {
            let q = format!(
                "{base} updated:>={} sort:updated-asc",
                rfc3339(since).unwrap_or_default()
            );
            let v = self
                .graphql(
                    ID_QUERY,
                    json!({"q": q, "type": ty, "first": ID_PAGE_SIZE, "after": after}),
                    cancel,
                )
                .await?;
            let search = &v["data"]["search"];
            let nodes = search["nodes"].as_array().cloned().unwrap_or_default();
            let mut last = since;
            for n in &nodes {
                if let Some(key) = item_key(n) {
                    live.insert(key);
                }
                if let Some(t) = n["updatedAt"].as_str().and_then(parse_rfc3339) {
                    last = last.max(t.unix_timestamp());
                }
            }
            in_query += nodes.len();
            let next = search["pageInfo"]["endCursor"].as_str().map(str::to_string);
            if search["pageInfo"]["hasNextPage"].as_bool() != Some(true)
                || next.is_none()
                || nodes.is_empty()
            {
                return Ok(());
            }
            if in_query + nodes.len() > SEARCH_CAP {
                if last <= since {
                    tracing::warn!(source = %self.ctx.source, "github: more than 1000 items share one timestamp; listing cut");
                    return Ok(());
                }
                since = last;
                after = None;
                in_query = 0;
            } else {
                after = next;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn sync_scope(
        &self,
        id: &str,
        ty: &str,
        base: &str,
        viewer: &str,
        cur: &mut Cursor,
        known: &std::collections::HashMap<ItemKey, Option<String>>,
        live: &mut HashSet<String>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        // Re-read a little before the watermark: search indexing lags behind `updatedAt`, so an
        // item updated just before the last item seen can show up in search after it.
        let mut since = cur
            .scopes
            .get(id)
            .map_or(cur.window_from, |s| s.since - WATERMARK_OVERLAP_SECS)
            .max(cur.window_from);
        let mut after: Option<String> = None;
        let mut in_query = 0usize;
        let mut failures = ItemFailures::default();
        // The earliest `updatedAt` of an item that failed to load: the watermark stays at or
        // below it so the next pass retries the item.
        let mut hold: Option<i64> = None;
        for _ in 0..MAX_PAGES_PER_SCOPE {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let q = format!(
                "{base} updated:>={} sort:updated-asc",
                rfc3339(since).unwrap_or_default()
            );
            let v = self
                .graphql(
                    &search_query(),
                    json!({"q": q, "type": ty, "first": PAGE_SIZE, "after": after}),
                    cancel,
                )
                .await?;
            let search = &v["data"]["search"];
            let nodes = search["nodes"].as_array().cloned().unwrap_or_default();
            let mut last = since;
            for n in &nodes {
                let Some(key) = item_key(n) else { continue };
                live.insert(key.clone());
                let updated = n["updatedAt"].as_str().unwrap_or_default();
                if let Some(t) = parse_rfc3339(updated) {
                    last = last.max(t.unix_timestamp());
                }
                if known
                    .get(&ItemKey::new(key.as_str()))
                    .is_some_and(|v| v.as_deref() == Some(updated))
                {
                    continue;
                }
                let doc = match self.document(n, key.clone(), viewer, cancel).await {
                    Ok(d) => {
                        failures.ok();
                        d
                    }
                    Err(e) => {
                        failures.skip(e, SERVICE, &key)?;
                        let t = parse_rfc3339(updated).map_or(since, |t| t.unix_timestamp());
                        hold = Some(hold.map_or(t, |h| h.min(t)));
                        continue;
                    }
                };
                sink.send(Change::Upsert(doc)).await?;
            }
            in_query += nodes.len();
            cur.scopes.insert(
                id.to_string(),
                ScopeState {
                    since: hold.map_or(last, |h| h.min(last)),
                },
            );
            sink.checkpoint(cur.to_value()).await?;
            let next = search["pageInfo"]["endCursor"].as_str().map(str::to_string);
            if search["pageInfo"]["hasNextPage"].as_bool() != Some(true)
                || next.is_none()
                || nodes.is_empty()
            {
                return Ok(());
            }
            if in_query + nodes.len() > SEARCH_CAP {
                if last <= since {
                    tracing::warn!(source = %self.ctx.source, "github: more than 1000 items share one timestamp; skipping ahead");
                    return Ok(());
                }
                since = last;
                after = None;
                in_query = 0;
            } else {
                after = next;
            }
        }
        Ok(())
    }

    async fn more_comments(&self, node: &Value, cancel: &CancellationToken) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let Some(id) = node["id"].as_str() else {
            return Ok(out);
        };
        let mut after = node["comments"]["pageInfo"]["endCursor"]
            .as_str()
            .map(str::to_string);
        let mut fetched = node["comments"]["nodes"].as_array().map_or(0, Vec::len);
        while let Some(a) = after.take() {
            if fetched >= MAX_COMMENTS {
                break;
            }
            let v = self
                .graphql(MORE_COMMENTS, json!({"id": id, "after": a}), cancel)
                .await?;
            let c = &v["data"]["node"]["comments"];
            let nodes = c["nodes"].as_array().cloned().unwrap_or_default();
            if nodes.is_empty() {
                break;
            }
            fetched += nodes.len();
            out.extend(nodes);
            if c["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
                after = c["pageInfo"]["endCursor"].as_str().map(str::to_string);
            }
        }
        Ok(out)
    }

    async fn document(
        &self,
        n: &Value,
        key: String,
        viewer: &str,
        cancel: &CancellationToken,
    ) -> Result<Document> {
        let typename = n["__typename"].as_str().unwrap_or_default();
        let repo = n["repository"]["nameWithOwner"]
            .as_str()
            .unwrap_or_default();
        let number = n["number"].as_i64().unwrap_or_default();
        let title = one_line(n["title"].as_str().unwrap_or("Untitled"), 300);
        let author = login(&n["author"]);
        let kind = match typename {
            "PullRequest" => "pull_request",
            "Discussion" => "discussion",
            _ => "issue",
        };
        let labels: Vec<String> = names(&n["labels"]["nodes"], "name");
        let assignees: Vec<String> = names(&n["assignees"]["nodes"], "login");
        let state = if typename == "PullRequest" && n["merged"].as_bool() == Some(true) {
            "MERGED".to_string()
        } else if typename == "Discussion" {
            if n["answer"].is_object() {
                "ANSWERED".into()
            } else {
                "OPEN".into()
            }
        } else {
            one_line(n["state"].as_str().unwrap_or("UNKNOWN"), 20)
        };

        // (time, markdown) entries after the body, in order.
        let mut entries: Vec<(String, String)> = Vec::new();
        let mut participants: Vec<String> = Vec::new();
        let mut mine = usize::from(author == viewer);
        let note = |who: &str, participants: &mut Vec<String>, mine: &mut usize| {
            if who == viewer {
                *mine += 1;
            }
            if !participants.iter().any(|p| p == who) && participants.len() < 100 {
                participants.push(who.to_string());
            }
        };
        let mut comments = n["comments"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if typename != "Discussion"
            && n["comments"]["pageInfo"]["hasNextPage"].as_bool() == Some(true)
        {
            comments.extend(self.more_comments(n, cancel).await?);
        }
        for c in comments.iter().take(MAX_COMMENTS) {
            let who = login(&c["author"]);
            note(&who, &mut participants, &mut mine);
            let at = c["createdAt"].as_str().unwrap_or_default().to_string();
            let mut md = format!(
                "**@{who}** commented on {}:\n\n{}",
                short_date(&at),
                body(&c["body"])
            );
            for r in c["replies"]["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .take(50)
            {
                let rw = login(&r["author"]);
                note(&rw, &mut participants, &mut mine);
                let reply = body(&r["body"]).replace('\n', "\n> ");
                md.push_str(&format!(
                    "\n\n> **@{rw}** replied on {}:\n>\n> {reply}",
                    short_date(r["createdAt"].as_str().unwrap_or_default())
                ));
            }
            entries.push((at, md));
        }
        for r in n["reviews"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .take(100)
        {
            let who = login(&r["author"]);
            note(&who, &mut participants, &mut mine);
            let at = r["submittedAt"].as_str().unwrap_or_default().to_string();
            let review_state = one_line(r["state"].as_str().unwrap_or("COMMENTED"), 30);
            let mut md = format!(
                "**@{who}** reviewed ({review_state}) on {}:",
                short_date(&at)
            );
            let text = body(&r["body"]);
            if !text.is_empty() {
                md.push_str(&format!("\n\n{text}"));
            }
            for c in r["comments"]["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .take(50)
            {
                let path = one_line(c["path"].as_str().unwrap_or_default(), 300);
                let cw = login(&c["author"]);
                let t = body(&c["body"]).replace('\n', "\n> ");
                md.push_str(&format!("\n\n> `{path}` (@{cw}): {t}"));
            }
            entries.push((at, md));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let what = match kind {
            "pull_request" => {
                if n["isDraft"].as_bool() == Some(true) {
                    "Draft pull request"
                } else {
                    "Pull request"
                }
            }
            "discussion" => "Discussion",
            _ => "Issue",
        };
        let mut out = format!(
            "# {title}\n\n{what} {repo}#{number} · {state} · opened by @{author} on {}\n",
            short_date(n["createdAt"].as_str().unwrap_or_default())
        );
        if let Some(cat) = n["category"]["name"].as_str() {
            out.push_str(&format!("Category: {}\n", one_line(cat, 100)));
        }
        if !labels.is_empty() {
            out.push_str(&format!("Labels: {}\n", labels.join(", ")));
        }
        if !assignees.is_empty() {
            out.push_str(&format!(
                "Assignees: {}\n",
                assignees
                    .iter()
                    .map(|a| format!("@{a}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let b = body(&n["body"]);
        if !b.is_empty() {
            out.push_str(&format!("\n{b}\n"));
        }
        for (_, md) in entries {
            if out.len() >= MAX_DOC_CHARS {
                out.push_str("\n… (more comments not shown)\n");
                break;
            }
            out.push_str("\n---\n\n");
            out.push_str(&md);
            out.push('\n');
        }
        let updated = n["updatedAt"].as_str().unwrap_or_default().to_string();
        let mut doc = Document::new(
            key,
            "text/markdown",
            Content::Text(truncate_chars(&out, MAX_DOC_CHARS)),
        );
        doc.title = Some(format!("{title} ({repo}#{number})"));
        doc.modified_at = parse_rfc3339(&updated);
        doc.version = Some(updated);
        doc.url = n["url"]
            .as_str()
            .filter(|u| u.starts_with("https://") && u.len() < 2_000)
            .map(str::to_string);
        doc.metadata = json!({
            "service": "github",
            "type": kind,
            "repo": repo,
            "number": number,
            "state": state,
            "author": author,
            "labels": labels,
            "assignees": assignees,
            "participants": participants,
            "from_me": author == viewer,
            "my_message_count": mine,
            "me": viewer,
        });
        Ok(doc)
    }
}

/// `issue/<owner>/<repo>/<n>` or `discussion/<owner>/<repo>/<n>`; `None` for anything else.
fn item_key(n: &Value) -> Option<String> {
    let prefix = match n["__typename"].as_str()? {
        "Issue" | "PullRequest" => "issue",
        "Discussion" => "discussion",
        _ => return None,
    };
    let repo = n["repository"]["nameWithOwner"]
        .as_str()
        .filter(|r| valid_repo(r))?;
    let number = n["number"].as_i64().filter(|n| *n > 0)?;
    Some(format!("{prefix}/{repo}/{number}"))
}

fn login(v: &Value) -> String {
    v["login"]
        .as_str()
        .map(|l| one_line(l, 64))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "ghost".into())
}

fn names(v: &Value, field: &str) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .take(50)
                .filter_map(|x| x[field].as_str().map(|s| one_line(s, 100)))
                .collect()
        })
        .unwrap_or_default()
}

fn body(v: &Value) -> String {
    truncate_chars(clean(v.as_str().unwrap_or_default()).trim(), MAX_BODY_CHARS)
}

fn short_date(rfc: &str) -> String {
    parse_rfc3339(rfc).map_or_else(|| "?".into(), |t| date(t.unix_timestamp()))
}
