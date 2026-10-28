//! `linear`: issues the user is assigned to, created or subscribes to, with their comments,
//! through Linear's GraphQL API.
//!
//! **Documents.** One Markdown document per issue (`issue/<uuid>`): identifier, title, team,
//! state, priority, project, people and labels, the description, then comments in order with
//! replies indented under their parent.
//!
//! **Cursor** (JSON): `{"v":1,"viewer":"<id>","window_from":<unix>,"since":"<rfc3339>",
//! "reconciled_at":<unix>,"pass":{"after":"<cursor>","max_seen":"<rfc3339>"}}`. A pass lists
//! `issues(filter: {updatedAt: {gte: since}, …}, includeArchived: true)`; `pass` records the
//! page reached so an interrupted pass resumes, and `since` moves to the newest `updatedAt`
//! once the pass completes. Archived and trashed issues become deletes.
//!
//! **Reconciling.** A first pass (or reset cursor) lists the whole window and deletes indexed
//! issues it did not return; every [`RECONCILE_INTERVAL_SECS`] an ids-only listing does the
//! same, which drops issues that left the scope (unassigned, unsubscribed, moved to an
//! excluded team).
//!
//! **Rate limits.** Linear answers HTTP 400 with `RATELIMITED` (or 429); the reset time comes
//! from `X-RateLimit-Requests-Reset` (Unix ms) or `Retry-After`.

use std::collections::{HashMap, HashSet};
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
use nugget_oauth::{AuthorizeOptions, TokenSource};
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{self, Api, Outcome, classify_common, header_u64, retry_after_ms};
use crate::auth::{self, Credential, Token};
use crate::settings::{self, MAX_WINDOW_DAYS};
use crate::text::{
    DAY_SECS, clean, date, now_unix, one_line, parse_rfc3339, rfc3339, safe_id, truncate_chars,
};
use crate::{Endpoints, LINEAR_KIND, RetryPolicy};

const FEATURE: &str = "sync:linear";
const SERVICE: &str = "Linear";
pub(crate) const RECONCILE_INTERVAL_SECS: i64 = 6 * 3600;
const PAGE_SIZE: i64 = 50;
const ID_PAGE_SIZE: i64 = 250;
const MAX_PAGES: usize = 2_000;
const MAX_COMMENTS: usize = 300;
const MAX_BODY_CHARS: usize = 60_000;
const MAX_DOC_CHARS: usize = 400_000;
const INCLUDE: &[&str] = &["assigned", "created", "subscribed"];

fn default_window() -> u32 {
    365
}
fn default_include() -> Vec<String> {
    INCLUDE.iter().map(|s| (*s).to_string()).collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinearSettings {
    #[serde(default)]
    token_env: Option<String>,
    /// OAuth application client id (PKCE), instead of a pasted API key.
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    /// Which of the user's issues: `assigned`, `created`, `subscribed`.
    #[serde(default = "default_include")]
    include: Vec<String>,
    /// Team keys (`ENG`). Empty: every team.
    #[serde(default)]
    teams: Option<Vec<String>>,
    #[serde(default = "default_window")]
    window_days: u32,
}

impl LinearSettings {
    pub(crate) fn parse(v: &Value) -> Result<Self> {
        let s: Self = settings::parse(v)?;
        settings::check_token_env(s.token_env.as_deref())?;
        settings::check_client_id(s.client_id.as_deref())?;
        settings::check_range("window_days", s.window_days, 1, MAX_WINDOW_DAYS)?;
        settings::check_list("teams", s.teams.as_deref(), 200)?;
        if s.include.is_empty() || s.include.iter().any(|i| !INCLUDE.contains(&i.as_str())) {
            return Err(Error::Invalid(
                "settings: include must list assigned, created and/or subscribed".into(),
            ));
        }
        if s.teams
            .iter()
            .flatten()
            .any(|t| !t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        {
            return Err(Error::Invalid("settings: teams must be team keys".into()));
        }
        Ok(s)
    }

    pub(crate) fn schema() -> Value {
        settings::schema(
            "a personal API key (lin_api_…) from Linear → Settings → Security & access.",
            json!({
                "client_id": settings::client_id_prop("Client ID of your own Linear OAuth application (redirect URI http://127.0.0.1). Set it to connect in the browser instead of pasting an API key."),
                "include": {"type": "array", "items": {"enum": INCLUDE}, "uniqueItems": true, "minItems": 1,
                    "default": INCLUDE, "title": "Issues",
                    "description": "Issues assigned to you, created by you, and/or that you subscribe to."},
                "teams": {"type": "array", "items": {"type": "string", "pattern": "^[A-Za-z0-9_]+$"},
                    "title": "Teams", "description": "Team keys (e.g. ENG). Leave empty for every team."},
                "window_days": settings::window_prop(365, "issues")
            }),
        )
    }

    /// The scope part of the issue filter (who, which teams).
    fn scope_filter(&self) -> Vec<Value> {
        let me = json!({"isMe": {"eq": true}});
        let who: Vec<Value> = self
            .include
            .iter()
            .map(|i| match i.as_str() {
                "assigned" => json!({"assignee": me}),
                "created" => json!({"creator": me}),
                _ => json!({"subscribers": {"some": me}}),
            })
            .collect();
        let mut and = vec![json!({"or": who})];
        if let Some(teams) = self.teams.as_ref().filter(|t| !t.is_empty()) {
            and.push(json!({"team": {"key": {"in": teams}}}));
        }
        and
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Pass {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_seen: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viewer: Option<String>,
    #[serde(default)]
    window_from: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    since: Option<String>,
    #[serde(default)]
    reconciled_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pass: Option<Pass>,
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Factory for `linear` sources.
#[derive(Debug, Clone, Default)]
pub struct LinearFactory {
    endpoints: Endpoints,
    retry: RetryPolicy,
}

impl LinearFactory {
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
}

impl ConnectorFactory for LinearFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: LINEAR_KIND,
            display_name: "Linear",
            auth: AuthKind::Password,
            settings_schema: LinearSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        LinearSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = LinearSettings::parse(settings)?;
        let http = api::metered(ctx.egress.clone(), FEATURE)?;
        let tokens = match &settings.client_id {
            Some(id) => Some(auth::token_source(
                auth::linear::provider_with_endpoints(
                    &self.endpoints.linear_auth_url,
                    &self.endpoints.linear_token_url,
                ),
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
        Ok(Arc::new(LinearConnector {
            api: Api::new(http, cred, self.retry, SERVICE),
            graphql: self.endpoints.linear_graphql.clone(),
            tokens,
            settings,
            ctx,
        }))
    }
}

/// One Linear user's issues.
pub struct LinearConnector {
    api: Api,
    graphql: String,
    tokens: Option<Arc<TokenSource>>,
    settings: LinearSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for LinearConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

fn error_codes(body: &Value) -> Vec<String> {
    body["errors"]
        .as_array()
        .map(|e| {
            e.iter()
                .filter_map(|x| {
                    x["extensions"]["code"]
                        .as_str()
                        .map(|c| c.to_ascii_uppercase())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn classify(resp: Response) -> Outcome {
    let status = resp.status.as_u16();
    let body: Value = if matches!(status, 200 | 400 | 401 | 403 | 429) {
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let codes = error_codes(&body);
    let has = |c: &str| codes.iter().any(|x| x == c);
    if has("RATELIMITED") || status == 429 {
        let reset = header_u64(&resp.headers, "x-ratelimit-requests-reset").map(|ms| {
            let now_ms = u64::try_from(now_unix()).unwrap_or(0).saturating_mul(1000);
            ms.saturating_sub(now_ms).max(1000)
        });
        return Outcome::Retry(Error::RateLimited {
            retry_after_ms: retry_after_ms(&resp.headers).or(reset),
        });
    }
    if has("AUTHENTICATION_ERROR") || status == 401 {
        return Outcome::Refresh;
    }
    if has("FORBIDDEN") || status == 403 {
        return Outcome::Fatal(Error::Unauthorized(
            "Linear denied access; check the key's permissions".into(),
        ));
    }
    if status == 200 {
        return Outcome::Done(resp);
    }
    classify_common(resp, SERVICE)
}

const ISSUES_QUERY: &str = "query($filter: IssueFilter, $first: Int!, $after: String) {
  issues(filter: $filter, first: $first, after: $after, includeArchived: true, orderBy: updatedAt) {
    pageInfo { hasNextPage endCursor }
    nodes {
      id identifier title description url priorityLabel createdAt updatedAt archivedAt trashed
      state { name type } team { key name } project { name }
      assignee { id name displayName } creator { id name displayName }
      labels(first: 20) { nodes { name } }
      comments(first: 50) { pageInfo { hasNextPage endCursor }
        nodes { id body createdAt user { id name displayName } parent { id } } }
    }
  }
}";

const IDS_QUERY: &str = "query($filter: IssueFilter, $first: Int!, $after: String) {
  issues(filter: $filter, first: $first, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes { id }
  }
}";

const MORE_COMMENTS: &str = "query($id: String!, $after: String) {
  issue(id: $id) {
    comments(first: 100, after: $after) { pageInfo { hasNextPage endCursor }
      nodes { id body createdAt user { id name displayName } parent { id } } }
  }
}";

struct Viewer {
    id: String,
    name: String,
}

#[async_trait]
impl Connector for LinearConnector {
    fn kind(&self) -> &'static str {
        LINEAR_KIND
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
    }

    async fn begin_auth(&self) -> Result<AuthFlow> {
        let Some(tokens) = &self.tokens else {
            return Err(Error::Unsupported(
                "set client_id (your Linear OAuth application) or paste a personal API key".into(),
            ));
        };
        let flow = tokens
            .client()
            .begin(
                self.ctx.secrets.clone(),
                nugget_core::secrets::oauth_account(self.ctx.source.as_str()),
                AuthorizeOptions::default(),
            )
            .await?;
        Ok(auth::clearing(
            flow,
            self.api.cred.clone(),
            Some(tokens.clone()),
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
                    tracing::warn!(source = %self.ctx.source, "unreadable linear cursor ({e}); starting over");
                    None
                }
            })
            .unwrap_or_default();
        let viewer = self.viewer(cancel).await?;
        let now = now_unix();
        let fresh = cur.viewer.as_deref() != Some(viewer.id.as_str()) || cur.window_from == 0;
        if fresh {
            let from = now - i64::from(self.settings.window_days) * DAY_SECS;
            cur = Cursor {
                v: 1,
                viewer: Some(viewer.id.clone()),
                window_from: from,
                since: rfc3339(from),
                reconciled_at: 0,
                pass: None,
            };
        }
        cur.v = 1;
        let known = self.ctx.known.versions().await?;
        let periodic = !fresh && now - cur.reconciled_at >= RECONCILE_INTERVAL_SECS;
        let mut live: HashSet<ItemKey> = HashSet::new();
        if periodic {
            self.list_ids(cur.window_from, &mut live, cancel).await?;
        }

        // A fresh pass started part-way (resumed) has not seen the earlier pages: only a pass
        // that ran from its first page may delete what it did not see.
        let complete_listing = fresh && cur.pass.is_none();
        self.list_changes(&viewer, &mut cur, &known, &mut live, sink, cancel)
            .await?;
        if periodic || complete_listing {
            for key in known.keys().filter(|k| !live.contains(*k)) {
                sink.send(Change::Delete(key.clone())).await?;
            }
            cur.reconciled_at = now;
        }
        Ok(cur.to_value())
    }
}

impl LinearConnector {
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
                let value = match token {
                    // Personal API keys go in the header as is; OAuth tokens are bearer tokens.
                    Token::Pasted(k) if !k.starts_with("lin_oauth_") => k.clone(),
                    t => format!("Bearer {}", t.secret()),
                };
                http.request(Method::POST, &url)
                    .header(AUTHORIZATION, value)
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
            })
            .await?;
        let v: Value = api::json(&resp, "linear graphql")?;
        if let Some(errors) = v["errors"].as_array().filter(|e| !e.is_empty())
            && v["data"]
                .as_object()
                .is_none_or(|d| d.values().all(Value::is_null))
        {
            let msg = errors[0]["message"]
                .as_str()
                .map(|m| one_line(m, 200))
                .unwrap_or_default();
            return Err(Error::Other(format!("Linear GraphQL error: {msg}")));
        }
        Ok(v)
    }

    async fn viewer(&self, cancel: &CancellationToken) -> Result<Viewer> {
        let v = self
            .graphql(
                "query { viewer { id name displayName } }",
                json!({}),
                cancel,
            )
            .await?;
        let u = &v["data"]["viewer"];
        let id = u["id"]
            .as_str()
            .filter(|i| safe_id(i, 64))
            .ok_or_else(|| Error::Invalid("Linear returned no viewer".into()))?;
        Ok(Viewer {
            id: id.to_string(),
            name: person(u),
        })
    }

    fn filter(&self, updated_from: &str) -> Value {
        let mut and = self.settings.scope_filter();
        and.push(json!({"updatedAt": {"gte": updated_from}}));
        json!({"and": and})
    }

    async fn list_ids(
        &self,
        from: i64,
        live: &mut HashSet<ItemKey>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let filter = self.filter(&rfc3339(from).unwrap_or_default());
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let v = self
                .graphql(
                    IDS_QUERY,
                    json!({"filter": filter, "first": ID_PAGE_SIZE, "after": after}),
                    cancel,
                )
                .await?;
            let issues = &v["data"]["issues"];
            for n in issues["nodes"].as_array().into_iter().flatten() {
                if let Some(id) = n["id"].as_str().filter(|i| safe_id(i, 64)) {
                    live.insert(ItemKey::new(format!("issue/{id}")));
                }
            }
            match issues["pageInfo"]["endCursor"].as_str() {
                Some(c) if issues["pageInfo"]["hasNextPage"].as_bool() == Some(true) => {
                    after = Some(c.to_string());
                }
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    async fn list_changes(
        &self,
        viewer: &Viewer,
        cur: &mut Cursor,
        known: &HashMap<ItemKey, Option<String>>,
        live: &mut HashSet<ItemKey>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let since = cur
            .since
            .clone()
            .or_else(|| rfc3339(cur.window_from))
            .unwrap_or_default();
        let filter = self.filter(&since);
        let mut pass = cur.pass.clone().unwrap_or_default();
        let mut retried = false;
        for _ in 0..MAX_PAGES {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let vars = json!({"filter": filter, "first": PAGE_SIZE, "after": pass.after});
            let v = match self.graphql(ISSUES_QUERY, vars, cancel).await {
                Ok(v) => v,
                // A stale page cursor from an interrupted pass: start the pass over.
                Err(Error::Other(m)) if pass.after.is_some() && !retried => {
                    tracing::info!(source = %self.ctx.source, "linear: restarting pass ({m})");
                    retried = true;
                    pass.after = None;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let issues = &v["data"]["issues"];
            for n in issues["nodes"].as_array().into_iter().flatten() {
                let Some(id) = n["id"].as_str().filter(|i| safe_id(i, 64)) else {
                    continue;
                };
                let key = ItemKey::new(format!("issue/{id}"));
                let updated = n["updatedAt"]
                    .as_str()
                    .filter(|u| parse_rfc3339(u).is_some());
                if let Some(u) = updated
                    && pass.max_seen.as_deref().is_none_or(|m| later(u, m))
                {
                    pass.max_seen = Some(u.to_string());
                }
                if !n["archivedAt"].is_null() || n["trashed"].as_bool() == Some(true) {
                    // Marked as handled, so the reconcile does not delete it twice.
                    live.insert(key.clone());
                    sink.send(Change::Delete(key)).await?;
                    continue;
                }
                live.insert(key.clone());
                if updated.is_some() && known.get(&key).is_some_and(|v| v.as_deref() == updated) {
                    continue;
                }
                let doc = self.document(n, key, viewer, cancel).await?;
                sink.send(Change::Upsert(doc)).await?;
            }
            match issues["pageInfo"]["endCursor"].as_str() {
                Some(c) if issues["pageInfo"]["hasNextPage"].as_bool() == Some(true) => {
                    pass.after = Some(c.to_string());
                    cur.pass = Some(pass.clone());
                    sink.checkpoint(cur.to_value()).await?;
                }
                _ => break,
            }
        }
        if let Some(m) = pass.max_seen.take()
            && cur.since.as_deref().is_none_or(|s| later(&m, s))
        {
            cur.since = Some(m);
        }
        cur.pass = None;
        Ok(())
    }

    async fn more_comments(&self, n: &Value, cancel: &CancellationToken) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let Some(id) = n["id"].as_str() else {
            return Ok(out);
        };
        let mut after = n["comments"]["pageInfo"]["endCursor"]
            .as_str()
            .map(str::to_string);
        let mut fetched = n["comments"]["nodes"].as_array().map_or(0, Vec::len);
        while let Some(a) = after.take() {
            if fetched >= MAX_COMMENTS {
                break;
            }
            let v = self
                .graphql(MORE_COMMENTS, json!({"id": id, "after": a}), cancel)
                .await?;
            let c = &v["data"]["issue"]["comments"];
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
        key: ItemKey,
        viewer: &Viewer,
        cancel: &CancellationToken,
    ) -> Result<Document> {
        let ident = one_line(n["identifier"].as_str().unwrap_or("?"), 40);
        let title = one_line(n["title"].as_str().unwrap_or("Untitled"), 300);
        let team = n["team"]["name"].as_str().map(|t| one_line(t, 100));
        let team_key = n["team"]["key"].as_str().map(|t| one_line(t, 20));
        let state = n["state"]["name"].as_str().map(|t| one_line(t, 60));
        let priority = n["priorityLabel"].as_str().map(|t| one_line(t, 40));
        let project = n["project"]["name"].as_str().map(|t| one_line(t, 200));
        let labels: Vec<String> = n["labels"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .take(50)
            .filter_map(|l| l["name"].as_str().map(|s| one_line(s, 100)))
            .collect();
        let creator_me = n["creator"]["id"].as_str() == Some(viewer.id.as_str());
        let assignee = (!n["assignee"].is_null()).then(|| person(&n["assignee"]));
        let creator = person(&n["creator"]);

        let mut comments = n["comments"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if n["comments"]["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
            comments.extend(self.more_comments(n, cancel).await?);
        }
        comments.truncate(MAX_COMMENTS);
        comments.sort_by(|a, b| a["createdAt"].as_str().cmp(&b["createdAt"].as_str()));
        let mut mine = usize::from(creator_me);
        let mut participants: Vec<String> = Vec::new();
        for c in &comments {
            if c["user"]["id"].as_str() == Some(viewer.id.as_str()) {
                mine += 1;
            }
            let p = person(&c["user"]);
            if !participants.contains(&p) && participants.len() < 100 {
                participants.push(p);
            }
        }

        let mut out = format!("# {ident}: {title}\n\n");
        let facts: Vec<String> = [
            team.clone().map(|t| format!("Team: {t}")),
            state.clone().map(|s| format!("State: {s}")),
            priority.clone().map(|p| format!("Priority: {p}")),
            project.clone().map(|p| format!("Project: {p}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !facts.is_empty() {
            out.push_str(&facts.join(" · "));
            out.push('\n');
        }
        out.push_str(&format!(
            "Created by {creator} on {}",
            short_date(n["createdAt"].as_str())
        ));
        if let Some(a) = &assignee {
            out.push_str(&format!(" · Assignee: {a}"));
        }
        out.push('\n');
        if !labels.is_empty() {
            out.push_str(&format!("Labels: {}\n", labels.join(", ")));
        }
        let desc = body(&n["description"]);
        if !desc.is_empty() {
            out.push_str(&format!("\n{desc}\n"));
        }
        // Top-level comments in order, each followed by its replies.
        let ids: HashSet<&str> = comments.iter().filter_map(|c| c["id"].as_str()).collect();
        let is_top = |c: &Value| c["parent"]["id"].as_str().is_none_or(|p| !ids.contains(p));
        for c in comments.iter().filter(|c| is_top(c)) {
            if out.len() >= MAX_DOC_CHARS {
                out.push_str("\n… (more comments not shown)\n");
                break;
            }
            out.push_str(&format!(
                "\n---\n\n**{}** commented on {}:\n\n{}\n",
                person(&c["user"]),
                short_date(c["createdAt"].as_str()),
                body(&c["body"])
            ));
            let cid = c["id"].as_str();
            for r in comments
                .iter()
                .filter(|r| cid.is_some() && r["parent"]["id"].as_str() == cid)
            {
                let text = body(&r["body"]).replace('\n', "\n> ");
                out.push_str(&format!(
                    "\n> **{}** replied on {}:\n>\n> {text}\n",
                    person(&r["user"]),
                    short_date(r["createdAt"].as_str())
                ));
            }
        }
        let updated = n["updatedAt"].as_str().unwrap_or_default().to_string();
        let mut doc = Document::new(
            key.as_str(),
            "text/markdown",
            Content::Text(truncate_chars(&out, MAX_DOC_CHARS)),
        );
        doc.title = Some(format!("{ident}: {title}"));
        doc.modified_at = parse_rfc3339(&updated);
        doc.version = Some(updated);
        doc.url = n["url"]
            .as_str()
            .filter(|u| u.starts_with("https://") && u.len() < 2_000)
            .map(str::to_string);
        let mut meta = json!({
            "service": "linear",
            "identifier": ident,
            "labels": labels,
            "author": creator,
            "participants": participants,
            "from_me": creator_me,
            "my_message_count": mine,
            "me": viewer.name,
        });
        for (k, v) in [
            ("team", team_key),
            ("team_name", team),
            ("state", state),
            ("priority", priority),
            ("project", project),
            ("assignee", assignee),
        ] {
            if let Some(v) = v {
                meta[k] = json!(v);
            }
        }
        doc.metadata = meta;
        Ok(doc)
    }
}

/// Whether RFC 3339 time `a` is after `b` (unparseable values never are).
fn later(a: &str, b: &str) -> bool {
    match (parse_rfc3339(a), parse_rfc3339(b)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

fn person(v: &Value) -> String {
    [v["displayName"].as_str(), v["name"].as_str()]
        .into_iter()
        .flatten()
        .map(|n| one_line(n, 100))
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn body(v: &Value) -> String {
    truncate_chars(clean(v.as_str().unwrap_or_default()).trim(), MAX_BODY_CHARS)
}

fn short_date(rfc: Option<&str>) -> String {
    rfc.and_then(parse_rfc3339)
        .map_or_else(|| "?".into(), |t| date(t.unix_timestamp()))
}
