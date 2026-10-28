//! `notion`: pages and databases shared with the user's integration, through the Notion REST API
//! (`Notion-Version: 2022-06-28`).
//!
//! **Documents.** Each page becomes Markdown (`page/<id>`): its title, then its blocks rendered
//! recursively (headings, lists, to-dos, toggles, quotes, callouts, code, tables, ...), down to
//! `max_depth` levels and at most [`MAX_BLOCKS`] blocks / [`MAX_DOC_CHARS`] characters. Child
//! pages are their own documents and appear as links. Database rows are pages too: their
//! properties come first as `- **Name:** value` lines. A database itself (`database/<id>`) is a
//! short document with its title, description and columns.
//!
//! **Cursor** (JSON): `{"v":1,"since":"<rfc3339>","reconciled_at":<unix>}`.
//! - Incremental: `search` sorted by `last_edited_time` descending, stopping at `since` (minus
//!   a margin: Notion rounds `last_edited_time` to the minute). A page whose version (its
//!   `last_edited_time`) is already indexed is skipped without fetching its blocks. A page
//!   edited in the last few minutes is stored with a provisional version (`<time>~`), so an
//!   edit later in the same minute is still picked up by the next pass.
//! - Reconciling: with no cursor, and every [`RECONCILE_INTERVAL_SECS`], the whole listing is
//!   read; indexed pages it no longer returns (archived, trashed, unshared, beyond
//!   `max_pages`) are deleted. Archived or trashed results are deleted on any pass.
//!
//! A pass checkpoints only at its end: an interrupted pass re-lists, but pages already indexed
//! with the same version are skipped cheaply.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::http::{MeteredHttp, Response};
use nugget_connector::{
    AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory,
    Content, Document, KindInfo,
};
use nugget_core::ids::ItemKey;
use nugget_core::{Error, Result};
use reqwest::Method;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{self, Api, Outcome, classify_common, snippet};
use crate::auth::{Credential, Token};
use crate::settings;
use crate::text::{clean, now_unix, one_line, parse_rfc3339, rfc3339, safe_id, truncate_chars};
use crate::{Endpoints, NOTION_KIND, RetryPolicy};

const FEATURE: &str = "sync:notion";
const SERVICE: &str = "Notion";
pub(crate) const NOTION_VERSION: &str = "2022-06-28";
pub(crate) const RECONCILE_INTERVAL_SECS: i64 = 6 * 3600;
/// `last_edited_time` is rounded to the minute: re-read a little before `since`.
const SINCE_MARGIN_SECS: i64 = 120;
/// Pages edited this recently get a provisional version (re-read next pass).
const PROVISIONAL_SECS: i64 = 180;
pub(crate) const MAX_BLOCKS: usize = 3_000;
const MAX_BLOCK_REQUESTS: usize = 300;
pub(crate) const MAX_DOC_CHARS: usize = 500_000;
const MAX_TEXT_CHARS: usize = 50_000;
const MAX_SEARCH_PAGES: usize = 2_000;
const MAX_USERS_CACHED: usize = 10_000;

fn yes() -> bool {
    true
}
fn default_max_pages() -> u32 {
    5_000
}
fn default_depth() -> u32 {
    6
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotionSettings {
    #[serde(default)]
    token_env: Option<String>,
    /// Index databases and their rows.
    #[serde(default = "yes")]
    include_databases: bool,
    /// Most recently edited pages kept in the index.
    #[serde(default = "default_max_pages")]
    max_pages: u32,
    /// Deepest block nesting rendered.
    #[serde(default = "default_depth")]
    max_depth: u32,
}

impl NotionSettings {
    pub(crate) fn parse(v: &Value) -> Result<Self> {
        let s: Self = settings::parse(v)?;
        settings::check_token_env(s.token_env.as_deref())?;
        settings::check_range("max_pages", s.max_pages, 1, 100_000)?;
        settings::check_range("max_depth", s.max_depth, 1, 12)?;
        Ok(s)
    }

    pub(crate) fn schema() -> Value {
        settings::schema(
            "the Internal Integration Secret (ntn_… / secret_…) of your Notion integration.",
            json!({
                "include_databases": {"type": "boolean", "default": true, "title": "Databases",
                    "description": "Index databases shared with the integration, and their rows."},
                "max_pages": {"type": "integer", "minimum": 1, "maximum": 100000, "default": 5000,
                    "title": "Page limit", "description": "The most recently edited pages kept in the index."},
                "max_depth": {"type": "integer", "minimum": 1, "maximum": 12, "default": 6,
                    "title": "Block depth", "description": "How deeply nested blocks are rendered."}
            }),
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    since: Option<String>,
    #[serde(default)]
    reconciled_at: i64,
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Factory for `notion` sources.
#[derive(Debug, Clone, Default)]
pub struct NotionFactory {
    endpoints: Endpoints,
    retry: RetryPolicy,
}

impl NotionFactory {
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

impl ConnectorFactory for NotionFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: NOTION_KIND,
            display_name: "Notion",
            auth: AuthKind::Password,
            settings_schema: NotionSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        NotionSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = NotionSettings::parse(settings)?;
        let http = api::metered(ctx.egress.clone(), FEATURE)?;
        let cred = Arc::new(Credential::new(
            SERVICE,
            ctx.secrets.clone(),
            ctx.source.as_str(),
            settings.token_env.clone(),
            None,
        ));
        Ok(Arc::new(NotionConnector {
            api: Api::new(http, cred, self.retry, SERVICE),
            base: self.endpoints.notion_api.trim_end_matches('/').to_string(),
            settings,
            ctx,
            users: Mutex::new(HashMap::new()),
        }))
    }
}

/// One Notion integration's shared pages and databases.
pub struct NotionConnector {
    api: Api,
    base: String,
    settings: NotionSettings,
    ctx: ConnectorContext,
    /// User id → name (`None`: unknown or not readable), kept across passes.
    users: Mutex<HashMap<String, Option<String>>>,
}

impl std::fmt::Debug for NotionConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotionConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

fn classify(resp: Response) -> Outcome {
    match resp.status.as_u16() {
        // Not found / not shared: the caller decides (usually: the page is gone).
        403 | 404 => Outcome::Done(resp),
        _ => classify_common(resp, SERVICE),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchPage {
    results: Vec<Value>,
    has_more: bool,
    next_cursor: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ChildrenPage {
    results: Vec<Value>,
    has_more: bool,
    next_cursor: Option<String>,
}

struct PassInfo {
    me: Option<String>,
    me_name: Option<String>,
}

#[async_trait]
impl Connector for NotionConnector {
    fn kind(&self) -> &'static str {
        NOTION_KIND
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
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
                    tracing::warn!(source = %self.ctx.source, "unreadable notion cursor ({e}); starting over");
                    None
                }
            })
            .unwrap_or_default();
        cur.v = 1;
        let info = self.whoami(cancel).await?;
        let now = now_unix();
        let since = cur
            .since
            .as_deref()
            .and_then(parse_rfc3339)
            .map(|t| t.unix_timestamp());
        let reconcile = since.is_none() || now - cur.reconciled_at >= RECONCILE_INTERVAL_SECS;
        let stop_before = since.map(|s| s - SINCE_MARGIN_SECS);
        let known = self.ctx.known.versions().await?;

        let mut live: HashSet<ItemKey> = HashSet::new();
        let mut max_seen = since;
        let mut kept = 0usize;
        let mut start: Option<String> = None;
        'listing: for _ in 0..MAX_SEARCH_PAGES {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut body = json!({
                "sort": {"direction": "descending", "timestamp": "last_edited_time"},
                "page_size": 100
            });
            if let Some(s) = &start {
                body["start_cursor"] = json!(s);
            }
            let resp = self.post("search", &body, cancel).await?;
            if !resp.status.is_success() {
                return Err(status_error(&resp, "search"));
            }
            let page: SearchPage = api::json(&resp, "notion search")?;
            for obj in page.results {
                let Some(r) = Obj::parse(&obj) else { continue };
                if !reconcile && stop_before.is_some_and(|s| r.edited < s) {
                    break 'listing;
                }
                if r.gone {
                    live.insert(r.key.clone());
                    sink.send(Change::Delete(r.key)).await?;
                    continue;
                }
                if !self.settings.include_databases && (r.is_database || r.in_database) {
                    continue;
                }
                if kept >= self.settings.max_pages as usize {
                    if reconcile {
                        continue;
                    }
                    break 'listing;
                }
                kept += 1;
                live.insert(r.key.clone());
                max_seen = Some(max_seen.map_or(r.edited, |m| m.max(r.edited)));
                let version = if now - r.edited < PROVISIONAL_SECS {
                    format!("{}~", r.edited_raw)
                } else {
                    r.edited_raw.clone()
                };
                if known
                    .get(&r.key)
                    .is_some_and(|v| v.as_deref() == Some(version.as_str()))
                {
                    continue;
                }
                let doc = if r.is_database {
                    Some(
                        self.database_document(&obj, &r, &info, version, cancel)
                            .await?,
                    )
                } else {
                    self.page_document(&obj, &r, &info, version, cancel).await?
                };
                match doc {
                    Some(d) => sink.send(Change::Upsert(d)).await?,
                    None => {
                        live.insert(r.key.clone());
                        sink.send(Change::Delete(r.key)).await?;
                    }
                }
            }
            match page.next_cursor.filter(|c| page.has_more && !c.is_empty()) {
                Some(c) => start = Some(c),
                None => break,
            }
        }
        if reconcile {
            for key in known.keys().filter(|k| !live.contains(*k)) {
                sink.send(Change::Delete(key.clone())).await?;
            }
            cur.reconciled_at = now;
        }
        if let Some(m) = max_seen {
            cur.since = rfc3339(m);
        }
        Ok(cur.to_value())
    }
}

/// The fields of a search result the pass needs.
struct Obj {
    key: ItemKey,
    id: String,
    is_database: bool,
    in_database: bool,
    gone: bool,
    edited: i64,
    edited_raw: String,
}

impl Obj {
    fn parse(v: &Value) -> Option<Self> {
        let object = v["object"].as_str()?;
        let is_database = match object {
            "page" => false,
            "database" => true,
            _ => return None,
        };
        let id = v["id"].as_str().filter(|i| safe_id(i, 64))?.to_string();
        let edited_raw = v["last_edited_time"].as_str()?.to_string();
        let edited = parse_rfc3339(&edited_raw)?.unix_timestamp();
        let key = ItemKey::new(format!("{object}/{id}"));
        Some(Self {
            key,
            id,
            is_database,
            in_database: v["parent"]["type"].as_str() == Some("database_id"),
            gone: v["archived"].as_bool() == Some(true) || v["in_trash"].as_bool() == Some(true),
            edited,
            edited_raw,
        })
    }
}

fn status_error(resp: &Response, what: &str) -> Error {
    match resp.status.as_u16() {
        403 => Error::Unauthorized(format!("Notion denied {what}: {}", snippet(&resp.body))),
        404 => Error::NotFound(format!("Notion {what}: not found")),
        s => Error::Other(format!("Notion {what}: HTTP {s}")),
    }
}

impl NotionConnector {
    fn request(
        &self,
        http: &MeteredHttp,
        token: &Token,
        method: Method,
        url: &str,
    ) -> reqwest::RequestBuilder {
        http.request(method, url)
            .bearer_auth(token.secret())
            .header("Notion-Version", NOTION_VERSION)
    }

    async fn get(
        &self,
        path: &str,
        query: &[(&str, &str)],
        cancel: &CancellationToken,
    ) -> Result<Response> {
        let mut url = url::Url::parse(&format!("{}/{path}", self.base))
            .map_err(|e| Error::Invalid(format!("bad Notion URL: {e}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        self.api
            .execute(cancel, classify, |http: &MeteredHttp, token: &Token| {
                self.request(http, token, Method::GET, url.as_str())
            })
            .await
    }

    async fn post(&self, path: &str, body: &Value, cancel: &CancellationToken) -> Result<Response> {
        let url = format!("{}/{path}", self.base);
        let body = body.to_string();
        self.api
            .execute(cancel, classify, |http: &MeteredHttp, token: &Token| {
                self.request(http, token, Method::POST, &url)
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
            })
            .await
    }

    /// The integration's owner (for `from_me`). A workspace-owned integration has none.
    async fn whoami(&self, cancel: &CancellationToken) -> Result<PassInfo> {
        let resp = self.get("users/me", &[], cancel).await?;
        if !resp.status.is_success() {
            return Err(match resp.status.as_u16() {
                403 | 404 => Error::Unauthorized("Notion rejected the integration token".into()),
                _ => status_error(&resp, "users/me"),
            });
        }
        let me: Value = api::json(&resp, "notion users/me")?;
        let owner = &me["bot"]["owner"]["user"];
        let id = owner["id"]
            .as_str()
            .filter(|i| safe_id(i, 64))
            .map(str::to_string);
        let name = owner["name"].as_str().map(|n| one_line(n, 100));
        if let (Some(id), Ok(mut users)) = (&id, self.users.lock()) {
            users.insert(id.clone(), name.clone());
        }
        Ok(PassInfo {
            me: id,
            me_name: name,
        })
    }

    /// A user's name, looked up once (`None` when the integration may not read users).
    async fn user_name(&self, v: &Value, cancel: &CancellationToken) -> Result<Option<String>> {
        if let Some(n) = v["name"].as_str() {
            return Ok(Some(one_line(n, 100)));
        }
        let Some(id) = v["id"].as_str().filter(|i| safe_id(i, 64)) else {
            return Ok(None);
        };
        if let Some(cached) = self.users.lock().ok().and_then(|u| u.get(id).cloned()) {
            return Ok(cached);
        }
        let resp = self.get(&format!("users/{id}"), &[], cancel).await?;
        let name = if resp.status.is_success() {
            serde_json::from_slice::<Value>(&resp.body)
                .ok()
                .and_then(|u| u["name"].as_str().map(|n| one_line(n, 100)))
        } else {
            None
        };
        if let Ok(mut users) = self.users.lock()
            && users.len() < MAX_USERS_CACHED
        {
            users.insert(id.to_string(), name.clone());
        }
        Ok(name)
    }

    async fn metadata(
        &self,
        obj: &Value,
        r: &Obj,
        info: &PassInfo,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let created_by = self.user_name(&obj["created_by"], cancel).await?;
        let edited_by = self.user_name(&obj["last_edited_by"], cancel).await?;
        let creator_id = obj["created_by"]["id"].as_str();
        let editor_id = obj["last_edited_by"]["id"].as_str();
        let mine = |id: Option<&str>| id.is_some() && id == info.me.as_deref();
        let mut m = json!({
            "service": "notion",
            "object": if r.is_database { "database" } else { "page" },
            "id": r.id,
            "parent_type": obj["parent"]["type"].as_str().unwrap_or("unknown"),
            "from_me": mine(creator_id),
            "last_edited_by_me": mine(editor_id),
        });
        if let Some(db) = obj["parent"]["database_id"]
            .as_str()
            .filter(|i| safe_id(i, 64))
        {
            m["database_id"] = json!(db);
        }
        if let Some(n) = created_by {
            m["author"] = json!(n);
        }
        if let Some(n) = edited_by {
            m["last_edited_by"] = json!(n);
        }
        if let Some(t) = obj["created_time"].as_str().filter(|t| t.len() < 64) {
            m["created_time"] = json!(t);
        }
        if let Some(me) = &info.me_name {
            m["me"] = json!(me);
        }
        Ok(m)
    }

    async fn database_document(
        &self,
        obj: &Value,
        r: &Obj,
        info: &PassInfo,
        version: String,
        cancel: &CancellationToken,
    ) -> Result<Document> {
        let title =
            non_empty(rich_text(&obj["title"])).unwrap_or_else(|| "Untitled database".into());
        let mut out = format!("# {title}\n\n");
        if let Some(d) = non_empty(rich_text(&obj["description"])) {
            out.push_str(&d);
            out.push_str("\n\n");
        }
        if let Some(props) = obj["properties"].as_object() {
            out.push_str("Columns:\n");
            for (name, p) in props.iter().take(200) {
                let ty = p["type"].as_str().unwrap_or("unknown");
                let mut line = format!("- **{}** ({})", one_line(name, 200), one_line(ty, 40));
                let options = p[ty]["options"].as_array().map(|o| {
                    o.iter()
                        .filter_map(|x| x["name"].as_str())
                        .take(50)
                        .map(|n| one_line(n, 100))
                        .collect::<Vec<_>>()
                });
                if let Some(o) = options.filter(|o| !o.is_empty()) {
                    line.push_str(&format!(": {}", o.join(", ")));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
        let mut doc = Document::new(
            r.key.as_str(),
            "text/markdown",
            Content::Text(truncate_chars(&out, MAX_DOC_CHARS)),
        );
        doc.title = Some(title);
        doc.version = Some(version);
        doc.modified_at = time::OffsetDateTime::from_unix_timestamp(r.edited).ok();
        doc.url = https_url(&obj["url"]);
        doc.metadata = self.metadata(obj, r, info, cancel).await?;
        Ok(doc)
    }

    /// The page as Markdown; `None` when it disappeared before its blocks were read.
    async fn page_document(
        &self,
        obj: &Value,
        r: &Obj,
        info: &PassInfo,
        version: String,
        cancel: &CancellationToken,
    ) -> Result<Option<Document>> {
        let Some(tree) = self.fetch_tree(&r.id, cancel).await? else {
            return Ok(None);
        };
        let mut title = None;
        let mut props_md = String::new();
        let mut labels: Vec<String> = Vec::new();
        if let Some(props) = obj["properties"].as_object() {
            for (name, p) in props.iter().take(200) {
                if p["type"].as_str() == Some("title") {
                    title = non_empty(rich_text(&p["title"]));
                    continue;
                }
                if !r.in_database {
                    continue;
                }
                if let Some(v) = property_text(p).filter(|v| !v.is_empty()) {
                    props_md.push_str(&format!(
                        "- **{}:** {}\n",
                        one_line(name, 200),
                        truncate_chars(&v, 2_000)
                    ));
                }
                if matches!(
                    p["type"].as_str(),
                    Some("select" | "multi_select" | "status")
                ) {
                    labels.extend(
                        property_text(p)
                            .into_iter()
                            .flat_map(|v| v.split(", ").map(str::to_string).collect::<Vec<_>>()),
                    );
                }
            }
        }
        let title = title.unwrap_or_else(|| "Untitled".into());
        let mut out = format!("# {title}\n\n");
        if !props_md.is_empty() {
            out.push_str(&props_md);
            out.push('\n');
        }
        let mut renderer = Renderer {
            tree: &tree.children,
            out,
            max_depth: self.settings.max_depth as usize,
        };
        renderer.blocks(&r.id, 0, 0);
        let mut out = renderer.out;
        if tree.truncated {
            out.push_str("\n… (some nested content was not included)\n");
        }
        let mut doc = Document::new(
            r.key.as_str(),
            "text/markdown",
            Content::Text(truncate_chars(&out, MAX_DOC_CHARS)),
        );
        doc.title = Some(title);
        doc.version = Some(version);
        doc.modified_at = time::OffsetDateTime::from_unix_timestamp(r.edited).ok();
        doc.url = https_url(&obj["url"]);
        let mut meta = self.metadata(obj, r, info, cancel).await?;
        if !labels.is_empty() {
            labels.truncate(50);
            meta["labels"] = json!(labels);
        }
        doc.metadata = meta;
        Ok(Some(doc))
    }

    /// The block tree under `root`, breadth first, within the depth and size budgets. `None`
    /// when the page itself is gone (404 / no access).
    async fn fetch_tree(&self, root: &str, cancel: &CancellationToken) -> Result<Option<Tree>> {
        let mut tree = Tree::default();
        let mut queue: VecDeque<(String, usize)> = VecDeque::from([(root.to_string(), 0)]);
        let mut requests = 0usize;
        let mut blocks = 0usize;
        let max_depth = self.settings.max_depth as usize;
        while let Some((id, depth)) = queue.pop_front() {
            let mut children = Vec::new();
            let mut start: Option<String> = None;
            loop {
                if requests >= MAX_BLOCK_REQUESTS || blocks >= MAX_BLOCKS {
                    tree.truncated = true;
                    break;
                }
                requests += 1;
                let mut q = vec![("page_size", "100")];
                if let Some(s) = &start {
                    q.push(("start_cursor", s));
                }
                let resp = self
                    .get(&format!("blocks/{id}/children"), &q, cancel)
                    .await?;
                if !resp.status.is_success() {
                    if id == root && matches!(resp.status.as_u16(), 403 | 404) {
                        return Ok(None);
                    }
                    if matches!(resp.status.as_u16(), 403 | 404) {
                        break;
                    }
                    return Err(status_error(&resp, "blocks.children"));
                }
                let page: ChildrenPage = match serde_json::from_slice(&resp.body) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(source = %self.ctx.source, "skipping malformed notion blocks: {e}");
                        tree.truncated = true;
                        break;
                    }
                };
                for b in page.results {
                    if blocks >= MAX_BLOCKS {
                        tree.truncated = true;
                        break;
                    }
                    blocks += 1;
                    let has_children = b["has_children"].as_bool() == Some(true);
                    let ty = b["type"].as_str().unwrap_or_default();
                    if has_children && !matches!(ty, "child_page" | "child_database") {
                        match b["id"].as_str().filter(|i| safe_id(i, 64)) {
                            Some(cid) if depth + 1 < max_depth => {
                                queue.push_back((cid.to_string(), depth + 1));
                            }
                            Some(_) => tree.truncated = true,
                            None => {}
                        }
                    }
                    children.push(b);
                }
                match page.next_cursor.filter(|c| page.has_more && !c.is_empty()) {
                    Some(c) => start = Some(c),
                    None => break,
                }
            }
            tree.children.insert(id, children);
        }
        Ok(Some(tree))
    }
}

#[derive(Default)]
struct Tree {
    children: HashMap<String, Vec<Value>>,
    truncated: bool,
}

struct Renderer<'a> {
    tree: &'a HashMap<String, Vec<Value>>,
    out: String,
    max_depth: usize,
}

impl Renderer<'_> {
    /// Render the children of `parent`. `indent` is the list nesting level.
    fn blocks(&mut self, parent: &str, depth: usize, indent: usize) {
        if depth >= self.max_depth || self.out.len() >= MAX_DOC_CHARS {
            return;
        }
        let Some(children) = self.tree.get(parent) else {
            return;
        };
        let mut number = 0usize;
        let mut in_list = false;
        for b in children {
            if self.out.len() >= MAX_DOC_CHARS {
                return;
            }
            let ty = b["type"].as_str().unwrap_or_default();
            number = if ty == "numbered_list_item" {
                number + 1
            } else {
                0
            };
            let is_list = matches!(
                ty,
                "bulleted_list_item" | "numbered_list_item" | "to_do" | "toggle"
            );
            if in_list && !is_list && indent == 0 && !self.out.ends_with("\n\n") {
                self.out.push('\n');
            }
            in_list = is_list;
            let body = &b[ty];
            let text = truncate_chars(&rich_text(&body["rich_text"]), MAX_TEXT_CHARS);
            let pad = "  ".repeat(indent);
            let id = b["id"].as_str().unwrap_or_default();
            let mut nested_indent = indent;
            match ty {
                "paragraph" => self.line(&pad, &text, true),
                "heading_1" | "heading_2" | "heading_3" => {
                    let level = &ty[8..];
                    let hashes = "#".repeat(level.parse::<usize>().unwrap_or(1) + 1);
                    self.out
                        .push_str(&format!("{hashes} {}\n\n", one_line(&text, 500)));
                }
                "bulleted_list_item" | "toggle" => {
                    self.out.push_str(&format!("{pad}- {text}\n"));
                    nested_indent += 1;
                }
                "numbered_list_item" => {
                    self.out.push_str(&format!("{pad}{number}. {text}\n"));
                    nested_indent += 1;
                }
                "to_do" => {
                    let mark = if body["checked"].as_bool() == Some(true) {
                        "x"
                    } else {
                        " "
                    };
                    self.out.push_str(&format!("{pad}- [{mark}] {text}\n"));
                    nested_indent += 1;
                }
                "quote" => self.line(&pad, &quote(&text), true),
                "callout" => {
                    let icon = body["icon"]["emoji"].as_str().map(|e| one_line(e, 8));
                    let t = match icon {
                        Some(i) => format!("{i} {text}"),
                        None => text.clone(),
                    };
                    self.line(&pad, &quote(&t), true);
                }
                "code" => {
                    let lang = body["language"]
                        .as_str()
                        .map(|l| one_line(l, 30))
                        .unwrap_or_default();
                    self.out.push_str(&format!(
                        "```{lang}\n{}\n```\n\n",
                        text.replace("```", "ˋˋˋ")
                    ));
                }
                "divider" => self.out.push_str("---\n\n"),
                "equation" => {
                    let e = body["expression"]
                        .as_str()
                        .map(|e| truncate_chars(&clean(e), 5_000))
                        .unwrap_or_default();
                    self.out.push_str(&format!("$${e}$$\n\n"));
                }
                "child_page" => {
                    let t = body["title"]
                        .as_str()
                        .map(|t| one_line(t, 300))
                        .unwrap_or_default();
                    self.out.push_str(&format!("{pad}[Page: {t}]\n\n"));
                }
                "child_database" => {
                    let t = body["title"]
                        .as_str()
                        .map(|t| one_line(t, 300))
                        .unwrap_or_default();
                    self.out.push_str(&format!("{pad}[Database: {t}]\n\n"));
                }
                "bookmark" | "embed" | "link_preview" => {
                    if let Some(u) = https_url(&body["url"]) {
                        let caption = rich_text(&body["caption"]);
                        let label = if caption.is_empty() {
                            u.clone()
                        } else {
                            format!("[{caption}]({u})")
                        };
                        self.out.push_str(&format!("{pad}{label}\n\n"));
                    }
                }
                "image" | "video" | "file" | "pdf" | "audio" => {
                    let caption = rich_text(&body["caption"]);
                    let name = body["name"].as_str().map(|n| one_line(n, 200));
                    let label = non_empty(caption).or(name).unwrap_or_default();
                    // Notion-hosted file URLs are short-lived signed links: not kept.
                    let ext = if body["type"].as_str() == Some("external") {
                        https_url(&body["external"]["url"])
                    } else {
                        None
                    };
                    let mut s = format!("{pad}[{ty}: {label}]");
                    if let Some(u) = ext {
                        s.push_str(&format!(" ({u})"));
                    }
                    self.out.push_str(&s);
                    self.out.push_str("\n\n");
                }
                "table" => {
                    self.table(id, body["has_column_header"].as_bool() == Some(true));
                    continue;
                }
                "link_to_page" => self.out.push_str(&format!("{pad}[Link to page]\n\n")),
                // Containers: their children carry the content.
                "column_list" | "column" | "synced_block" | "template" => {}
                _ => {
                    if !text.is_empty() {
                        self.line(&pad, &text, true);
                    }
                }
            }
            if b["has_children"].as_bool() == Some(true) && !id.is_empty() {
                self.blocks(id, depth + 1, nested_indent);
            }
        }
        if indent == 0 && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn line(&mut self, pad: &str, text: &str, blank_after: bool) {
        if text.is_empty() {
            return;
        }
        for l in text.split('\n') {
            self.out.push_str(pad);
            self.out.push_str(l);
            self.out.push('\n');
        }
        if blank_after {
            self.out.push('\n');
        }
    }

    fn table(&mut self, id: &str, header: bool) {
        let Some(rows) = self.tree.get(id) else {
            return;
        };
        let mut first = true;
        for row in rows.iter().take(1_000) {
            let Some(cells) = row["table_row"]["cells"].as_array() else {
                continue;
            };
            let cells: Vec<String> = cells
                .iter()
                .take(50)
                .map(|c| one_line(&rich_text(c), 500).replace('|', "\\|"))
                .collect();
            self.out.push_str(&format!("| {} |\n", cells.join(" | ")));
            if first && header {
                self.out
                    .push_str(&format!("|{}\n", " --- |".repeat(cells.len().max(1))));
            }
            first = false;
        }
        self.out.push('\n');
    }
}

fn quote(text: &str) -> String {
    text.split('\n')
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn https_url(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|u| {
            u.starts_with("https://") && u.len() <= 2_000 && !u.contains(char::is_whitespace)
        })
        .map(str::to_string)
}

/// Notion rich text (an array of text objects) as Markdown.
pub(crate) fn rich_text(v: &Value) -> String {
    let Some(items) = v.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for item in items.iter().take(2_000) {
        let Some(t) = item["plain_text"].as_str() else {
            continue;
        };
        let t = clean(t);
        if t.trim().is_empty() {
            out.push_str(&t);
            continue;
        }
        let a = &item["annotations"];
        let on = |k: &str| a[k].as_bool() == Some(true);
        let mut s = t;
        if on("code") {
            s = format!("`{s}`");
        }
        if on("bold") {
            s = format!("**{s}**");
        }
        if on("italic") {
            s = format!("*{s}*");
        }
        if on("strikethrough") {
            s = format!("~~{s}~~");
        }
        if let Some(h) = https_url(&item["href"]) {
            s = format!("[{s}]({h})");
        }
        out.push_str(&s);
        if out.len() > MAX_TEXT_CHARS * 2 {
            break;
        }
    }
    out
}

/// A database property's value as one line of text.
pub(crate) fn property_text(p: &Value) -> Option<String> {
    let ty = p["type"].as_str()?;
    let v = &p[ty];
    let names = |arr: &Value| -> String {
        arr.as_array()
            .map(|a| {
                a.iter()
                    .take(100)
                    .filter_map(|x| x["name"].as_str().map(|n| one_line(n, 200)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let date = |d: &Value| -> Option<String> {
        let start = d["start"].as_str()?;
        Some(match d["end"].as_str() {
            Some(end) => format!("{} → {}", one_line(start, 40), one_line(end, 40)),
            None => one_line(start, 40),
        })
    };
    let s = match ty {
        "title" | "rich_text" => rich_text(v),
        "number" => v.as_f64().map(|n| n.to_string())?,
        "select" | "status" => v["name"].as_str().map(|n| one_line(n, 200))?,
        "multi_select" => names(v),
        "people" => names(v),
        "date" => date(v)?,
        "checkbox" => {
            if v.as_bool()? {
                "yes".into()
            } else {
                "no".into()
            }
        }
        "url" | "email" | "phone_number" => v.as_str().map(|s| one_line(s, 500))?,
        "created_time" | "last_edited_time" => v.as_str().map(|s| one_line(s, 40))?,
        "created_by" | "last_edited_by" => v["name"].as_str().map(|n| one_line(n, 200))?,
        "files" => names(v),
        "relation" => {
            let n = v.as_array().map_or(0, Vec::len);
            if n == 0 {
                return None;
            }
            format!("{n} linked")
        }
        "unique_id" => {
            let n = v["number"].as_i64()?;
            match v["prefix"].as_str() {
                Some(pfx) => format!("{}-{n}", one_line(pfx, 20)),
                None => n.to_string(),
            }
        }
        "formula" => {
            let fty = v["type"].as_str()?;
            match fty {
                "string" => v["string"].as_str().map(|s| one_line(s, 500))?,
                "number" => v["number"].as_f64().map(|n| n.to_string())?,
                "boolean" => v["boolean"].as_bool().map(|b| b.to_string())?,
                "date" => date(&v["date"])?,
                _ => return None,
            }
        }
        "rollup" => {
            let rty = v["type"].as_str()?;
            match rty {
                "number" => v["number"].as_f64().map(|n| n.to_string())?,
                "date" => date(&v["date"])?,
                "array" => {
                    let parts: Vec<String> = v["array"]
                        .as_array()?
                        .iter()
                        .take(50)
                        .filter_map(property_text)
                        .collect();
                    parts.join(", ")
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    let s = one_line(&s, 2_000);
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn rich_text_annotations_and_links() {
        let v = json!([
            {"plain_text": "bold", "annotations": {"bold": true}},
            {"plain_text": " and "},
            {"plain_text": "link", "href": "https://x.io", "annotations": {"italic": true}},
            {"plain_text": " js:", "href": "javascript:alert(1)"}
        ]);
        assert_eq!(rich_text(&v), "**bold** and [*link*](https://x.io) js:");
        assert_eq!(rich_text(&json!("nope")), "");
    }

    #[test]
    fn properties_render_as_text() {
        assert_eq!(
            property_text(&json!({"type": "select", "select": {"name": "Done"}})).unwrap(),
            "Done"
        );
        assert_eq!(
            property_text(
                &json!({"type": "multi_select", "multi_select": [{"name": "a"}, {"name": "b"}]})
            )
            .unwrap(),
            "a, b"
        );
        assert_eq!(
            property_text(&json!({"type": "checkbox", "checkbox": true})).unwrap(),
            "yes"
        );
        assert_eq!(
            property_text(&json!({"type": "date", "date": {"start": "2026-01-01", "end": null}}))
                .unwrap(),
            "2026-01-01"
        );
        assert_eq!(
            property_text(
                &json!({"type": "formula", "formula": {"type": "number", "number": 3.0}})
            )
            .unwrap(),
            "3"
        );
        assert_eq!(
            property_text(&json!({"type": "unique_id", "unique_id": {"prefix": "T", "number": 7}}))
                .unwrap(),
            "T-7"
        );
        assert!(property_text(&json!({"type": "relation", "relation": []})).is_none());
        assert!(property_text(&json!({"type": 5})).is_none());
    }
}
