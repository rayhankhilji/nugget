//! `onedrive`: the user's OneDrive over Microsoft Graph with `Files.Read`.
//!
//! Scopes: `Files.Read` (read the user's own files; not `Files.Read.All`, so nothing shared from
//! other drives), `offline_access` (a refresh token, so syncing continues without signing in
//! again) and `User.Read` (lets the consent screen name the account). Tokens live only in the
//! secret store; download URLs are pre-authenticated and fetched without the token.
//!
//! `/me/drive/root/delta` enumerates the drive, then reports changes. Delta items carry no
//! path, so the folder tree (id → parent id and name) is kept in the cursor and paths are
//! rebuilt from it for the include/exclude filters and metadata. Files of supported types up to
//! `max_file_mb` are downloaded through the pre-authenticated `@microsoft.graph.downloadUrl`:
//! a cross-host URL fetched **without** the bearer token, only over HTTPS and only from
//! Microsoft download hosts ([`MicrosoftEndpoints::download_hosts`]). A missing, refused or
//! expired download URL is replaced by the `Location` of `/items/{id}/content` (inspected, not
//! followed) and checked the same way.
//!
//! Cursor (JSON): `{"v":1,"delta_link":"<url>"|null,"next_link":"<url>"|null,"full":bool,
//! "reconcile":bool,"root":"<id>","folders":{"<id>":["<parent id>","<name>"],...}}`.
//! `full` marks an enumeration from scratch. With `reconcile` (cursor reset while files are
//! indexed, an expired delta token, or a folder renamed or moved while folder filters are set)
//! every indexed file the enumeration does not report in scope is deleted at its end; an
//! interrupted reconciling enumeration restarts from scratch.
//!
//! Keys are drive item ids. Versions are `<eTag>|<parent id>`: the parent lets a deleted
//! folder's indexed files be removed even when Graph reports only the folder.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::http::MeteredHttp;
use nugget_connector::{
    AuthFlow, AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext,
    ConnectorFactory, Content, Document, KindInfo,
};
use nugget_core::ids::ItemKey;
use nugget_core::{Error, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::graph::{
    DeltaPage, Get, GraphApi, backoff, is_item_failure, needs_resync, retry_after_ms, status_error,
};
use crate::oauth::{SCOPE_FILES_READ, SCOPE_OFFLINE_ACCESS, SCOPE_USER_READ};
use crate::settings::OneDriveSettings;
use crate::text::clean_line;
use crate::{MicrosoftEndpoints, RetryPolicy};

pub(crate) const KIND: &str = "onedrive";
const FEATURE: &str = "sync:onedrive";
const SCOPES: &[&str] = &[SCOPE_OFFLINE_ACCESS, SCOPE_USER_READ, SCOPE_FILES_READ];
const PAGE_SIZE: &str = "odata.maxpagesize=200";
/// Folders tracked in the cursor; beyond this, paths below untracked folders are unknown.
const MAX_FOLDERS: usize = 200_000;
/// Deepest folder nesting followed when building a path.
const MAX_DEPTH: usize = 128;
const MAX_NAME: usize = 400;
const MAX_RESTARTS: u32 = 3;
const MAX_PAGES: u32 = 100_000;

/// File extensions indexed, with the MIME type handed to the extractor.
const SUPPORTED: &[(&str, &str)] = &[
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("rst", "text/plain"),
    ("org", "text/plain"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("json", "application/json"),
    ("eml", "message/rfc822"),
    ("ics", "text/calendar"),
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
];

/// `[parent id, name]`; the root has an empty parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Folder(String, String);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delta_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_link: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    full: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reconcile: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(default)]
    folders: BTreeMap<String, Folder>,
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn fresh(reconcile: bool) -> Self {
        Self {
            v: 1,
            full: true,
            reconcile,
            ..Default::default()
        }
    }
}

/// Factory for `onedrive` sources.
#[derive(Debug, Clone, Default)]
pub struct OneDriveFactory {
    endpoints: MicrosoftEndpoints,
    retry: RetryPolicy,
}

impl OneDriveFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_endpoints(mut self, endpoints: MicrosoftEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

impl ConnectorFactory for OneDriveFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "OneDrive",
            auth: AuthKind::OAuth,
            settings_schema: OneDriveSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        OneDriveSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = OneDriveSettings::parse(settings)?;
        let api = GraphApi::new(
            FEATURE,
            SCOPES,
            &settings.common(),
            &self.endpoints,
            self.retry,
            ctx.clone(),
        )?;
        // A separate client for the pre-authenticated download URLs: it never sees the token.
        let download = MeteredHttp::new(ctx.egress.clone(), FEATURE)?
            .with_max_body(usize::try_from(settings.max_bytes()).unwrap_or(usize::MAX));
        Ok(Arc::new(OneDriveConnector {
            api,
            download,
            hosts: self
                .endpoints
                .download_hosts
                .iter()
                .map(|h| h.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
            allow_http: self.endpoints.allow_http_downloads,
            settings,
            ctx,
        }))
    }
}

/// One account's OneDrive.
pub struct OneDriveConnector {
    api: GraphApi,
    download: MeteredHttp,
    hosts: Vec<String>,
    allow_http: bool,
    settings: OneDriveSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for OneDriveConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OneDriveConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ParentRef {
    id: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileFacet {
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DriveItem {
    id: String,
    name: Option<String>,
    size: Option<i64>,
    #[serde(rename = "eTag")]
    etag: Option<String>,
    #[serde(rename = "cTag")]
    ctag: Option<String>,
    #[serde(rename = "lastModifiedDateTime")]
    last_modified: Option<String>,
    #[serde(rename = "createdDateTime")]
    created: Option<String>,
    #[serde(rename = "webUrl")]
    web_url: Option<String>,
    file: Option<FileFacet>,
    folder: Option<Value>,
    package: Option<Value>,
    root: Option<Value>,
    deleted: Option<Value>,
    #[serde(rename = "remoteItem")]
    remote_item: Option<Value>,
    #[serde(rename = "parentReference")]
    parent: Option<ParentRef>,
    #[serde(rename = "@microsoft.graph.downloadUrl")]
    download_url: Option<String>,
}

impl DriveItem {
    fn is_folder(&self) -> bool {
        self.folder.is_some() || self.package.is_some() || self.root.is_some()
    }

    fn parent_id(&self) -> Option<&str> {
        self.parent
            .as_ref()
            .and_then(|p| p.id.as_deref())
            .filter(|p| valid_item_id(p))
    }
}

/// State of one sync pass.
struct Pass {
    /// Indexed files (key → version), kept up to date with what this pass sends.
    live: HashMap<String, Option<String>>,
    /// Files reported in scope during a full enumeration.
    seen: HashSet<String>,
    /// A folder moved or was renamed while folder filters are set: re-enumerate.
    rescan: bool,
}

#[async_trait]
impl Connector for OneDriveConnector {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
    }

    async fn begin_auth(&self) -> Result<AuthFlow> {
        self.api.begin_auth().await
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.api.ensure_token().await?;
        let known = self.ctx.known.versions().await?;
        let parsed = cursor.and_then(|v| match serde_json::from_value::<Cursor>(v) {
            Ok(c) if c.full || c.delta_link.is_some() => Some(c),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(source = %self.ctx.source, "unreadable onedrive cursor ({e}); starting over");
                None
            }
        });
        let cur = match parsed {
            Some(c) => c,
            None => {
                // No cursor but indexed files: a reconciling enumeration.
                let reconcile = !known.is_empty();
                if reconcile {
                    tracing::info!(source = %self.ctx.source, "onedrive cursor reset; reconciling resync");
                }
                Cursor::fresh(reconcile)
            }
        };
        let mut pass = Pass {
            live: known
                .into_iter()
                .map(|(k, v)| (k.as_str().to_string(), v))
                .collect(),
            seen: HashSet::new(),
            rescan: false,
        };
        self.listing(cur, &mut pass, sink, cancel)
            .await
            .map(|c| c.to_value())
    }
}

impl OneDriveConnector {
    async fn listing(
        &self,
        mut cur: Cursor,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Cursor> {
        if cur.full && cur.reconcile && cur.next_link.is_some() {
            // Pages enumerated before the interruption are not remembered: start over rather
            // than take their files for missing.
            cur.next_link = None;
        }
        let mut restarts = 0u32;
        let mut pages = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            pages += 1;
            if pages > MAX_PAGES {
                return Err(Error::Invalid(
                    "onedrive: delta listing does not end".into(),
                ));
            }
            if cur.full && cur.next_link.is_none() {
                // Enumeration from scratch: the tree is rebuilt.
                cur.folders.clear();
                cur.root = None;
                cur.delta_link = None;
                pass.seen.clear();
            }
            let url = match (&cur.next_link, &cur.delta_link) {
                (Some(n), _) => self.api.link(n)?,
                (None, Some(d)) if !cur.full => self.api.link(d)?,
                _ => self.api.url(&["me", "drive", "root", "delta"], &[])?,
            };
            let get = Get {
                prefer: &[PAGE_SIZE],
                ..Default::default()
            };
            let resp = self.api.get(&url, get, cancel).await?;
            if needs_resync(&resp) {
                if restarts >= MAX_RESTARTS {
                    return Err(Error::Other("onedrive: delta token keeps expiring".into()));
                }
                tracing::info!(source = %self.ctx.source, "onedrive delta token expired; reconciling resync");
                restarts += 1;
                cur = Cursor::fresh(true);
                continue;
            }
            if resp.status.as_u16() != 200 {
                return Err(status_error("onedrive delta", &resp));
            }
            let page: DeltaPage = GraphApi::json(&resp, "onedrive delta")?;
            let next = page
                .next_link
                .filter(|l| !l.is_empty())
                .map(|l| self.api.link(&l).map(|_| l))
                .transpose()?;
            let delta = page
                .delta_link
                .filter(|l| !l.is_empty())
                .map(|l| self.api.link(&l).map(|_| l))
                .transpose()?;
            if next.is_none() && delta.is_none() {
                return Err(Error::Invalid(
                    "onedrive delta page has neither nextLink nor deltaLink".into(),
                ));
            }
            let items: Vec<DriveItem> = page
                .value
                .into_iter()
                // A non-object entry would deserialize from its array form (`["<id>"]`, an item
                // without a file facet, which reads as a delete): skip it.
                .filter(Value::is_object)
                .filter_map(|v| serde_json::from_value::<DriveItem>(v).ok())
                .filter(|i| valid_item_id(&i.id) && i.remote_item.is_none())
                .collect();
            self.apply_page(&mut cur, items, pass, sink, cancel).await?;

            if let Some(n) = next {
                cur.next_link = Some(n);
                sink.checkpoint(cur.to_value()).await?;
                continue;
            }
            if cur.full && cur.reconcile {
                let mut gone: Vec<&String> = pass
                    .live
                    .keys()
                    .filter(|k| !pass.seen.contains(*k))
                    .collect();
                gone.sort();
                for key in gone {
                    sink.send(Change::Delete(ItemKey::new(key.as_str())))
                        .await?;
                }
            }
            cur.delta_link = delta;
            cur.next_link = None;
            cur.full = false;
            cur.reconcile = false;
            if pass.rescan && restarts < MAX_RESTARTS {
                // Paths under a moved or renamed folder changed: enumerate again so the folder
                // filters are re-applied to every file.
                tracing::info!(source = %self.ctx.source, "onedrive folder moved; re-applying folder filters");
                restarts += 1;
                pass.rescan = false;
                sink.checkpoint(cur.to_value()).await?;
                cur = Cursor::fresh(true);
                continue;
            }
            return Ok(cur);
        }
    }

    async fn apply_page(
        &self,
        cur: &mut Cursor,
        items: Vec<DriveItem>,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        // Folders first, so files later in the page (or listed before their parent) resolve.
        for item in items.iter().filter(|i| i.is_folder()) {
            if item.deleted.is_some() {
                let subtree = subtree(&cur.folders, &item.id);
                let mut gone: Vec<String> = pass
                    .live
                    .iter()
                    .filter(|(_, v)| {
                        version_parent(v.as_deref()).is_some_and(|p| subtree.contains(p))
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                gone.sort();
                for key in gone {
                    pass.live.remove(&key);
                    pass.seen.remove(&key);
                    sink.send(Change::Delete(ItemKey::new(key))).await?;
                }
                for id in &subtree {
                    cur.folders.remove(id);
                }
                continue;
            }
            let name = item
                .name
                .as_deref()
                .map(|n| clean_line(n, MAX_NAME))
                .unwrap_or_default();
            let entry = if item.root.is_some() {
                cur.root = Some(item.id.clone());
                Folder(String::new(), String::new())
            } else {
                Folder(item.parent_id().unwrap_or_default().to_string(), name)
            };
            match cur.folders.get(&item.id) {
                Some(old) if *old != entry => {
                    if self.settings.has_filters() && !cur.full {
                        pass.rescan = true;
                    }
                    cur.folders.insert(item.id.clone(), entry);
                }
                Some(_) => {}
                None if cur.folders.len() < MAX_FOLDERS => {
                    cur.folders.insert(item.id.clone(), entry);
                }
                None => {
                    tracing::warn!(source = %self.ctx.source, "onedrive: too many folders to track");
                }
            }
        }
        for item in items.into_iter().filter(|i| !i.is_folder()) {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let key = item.id.clone();
            if item.deleted.is_some() || item.file.is_none() {
                pass.seen.remove(&key);
                if pass.live.remove(&key).is_some() {
                    sink.send(Change::Delete(ItemKey::new(key))).await?;
                }
                continue;
            }
            let name = item
                .name
                .as_deref()
                .map(|n| clean_line(n, MAX_NAME))
                .unwrap_or_default();
            let parent = item.parent_id().map(str::to_string);
            let dir = parent.as_deref().and_then(|p| folder_path(&cur.folders, p));
            let path = dir.as_ref().map(|d| {
                if d == "/" {
                    format!("/{name}")
                } else {
                    format!("{d}/{name}")
                }
            });
            let in_scope = path
                .as_ref()
                .is_none_or(|p| self.settings.in_scope(&p.to_lowercase()));
            if path.is_none() && self.settings.has_filters() {
                // Cannot tell whether it is in scope: leave it as it is.
                tracing::debug!(source = %self.ctx.source, "onedrive: file in an unknown folder");
                continue;
            }
            let mime = mime_for(&name);
            let size_ok = item
                .size
                .is_none_or(|s| s >= 0 && (s as u64) <= self.settings.max_bytes());
            let Some(mime) = mime.filter(|_| in_scope && size_ok) else {
                pass.seen.remove(&key);
                if pass.live.remove(&key).is_some() {
                    sink.send(Change::Delete(ItemKey::new(key))).await?;
                }
                continue;
            };
            let version = format!(
                "{}|{}",
                item.etag
                    .as_deref()
                    .or(item.ctag.as_deref())
                    .unwrap_or_default(),
                parent.as_deref().unwrap_or_default()
            );
            pass.seen.insert(key.clone());
            if pass
                .live
                .get(&key)
                .is_some_and(|v| v.as_deref() == Some(version.as_str()))
            {
                continue;
            }
            let Some(bytes) = self.download(&item, cancel).await? else {
                continue;
            };
            let doc = self.document(item, name, path, mime, version.clone(), bytes);
            pass.live.insert(key, Some(version));
            sink.send(Change::Upsert(doc)).await?;
        }
        Ok(())
    }

    fn document(
        &self,
        item: DriveItem,
        name: String,
        path: Option<String>,
        mime: &str,
        version: String,
        bytes: Vec<u8>,
    ) -> Document {
        let mut doc = Document::new(item.id.clone(), mime, Content::Bytes(bytes));
        doc.title = Some(name.clone()).filter(|n| !n.is_empty());
        doc.version = Some(version);
        doc.modified_at = item
            .last_modified
            .as_deref()
            .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok());
        doc.url = item
            .web_url
            .as_deref()
            .filter(|u| u.starts_with("https://") && u.len() <= 4096)
            .map(str::to_string);
        doc.metadata = json!({
            "item_id": item.id,
            "name": name,
            "path": path,
            "parent_id": item.parent_id(),
            "size": item.size,
            "mime_type": item.file.and_then(|f| f.mime_type).map(|m| clean_line(&m, 200)),
            "etag": item.etag,
            "ctag": item.ctag,
            "created": item.created,
            "modified": item.last_modified,
        });
        doc
    }

    /// The file's bytes, or `None` when it cannot be downloaded safely (no acceptable URL,
    /// gone, too large).
    async fn download(
        &self,
        item: &DriveItem,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<u8>>> {
        if let Some(u) = item.download_url.as_deref()
            && let Some(bytes) = self.fetch_download(u, cancel).await?
        {
            return Ok(Some(bytes));
        }
        // No usable download URL (or it expired): ask Graph for a fresh one. `/content` answers
        // with a redirect to it, which is inspected rather than followed.
        let url = self
            .api
            .url(&["me", "drive", "items", &item.id, "content"], &[])?;
        let get = Get {
            allow_redirect: true,
            raw: true,
            max_body: usize::try_from(self.settings.max_bytes()).unwrap_or(usize::MAX),
            ..Default::default()
        };
        let resp = match self.api.get(&url, get, cancel).await {
            Ok(r) => r,
            Err(Error::Invalid(m)) if m.contains("exceeds") => return Ok(None),
            // One unreadable file must not stop the pass.
            Err(e) if is_item_failure(&e) => {
                tracing::warn!(source = %self.ctx.source, "onedrive: skipping unreadable file: {e}");
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        match resp.status.as_u16() {
            300..=399 => {
                let Some(loc) = resp
                    .headers
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                else {
                    return Ok(None);
                };
                self.fetch_download(loc, cancel).await
            }
            404 | 410 => Ok(None),
            s if (200..300).contains(&s) => Ok(Some(resp.body)),
            _ => {
                let e = status_error("onedrive content", &resp);
                tracing::warn!(source = %self.ctx.source, "onedrive: skipping unreadable file: {e}");
                Ok(None)
            }
        }
    }

    /// GET a pre-authenticated download URL **without** the bearer token. `None` when the URL is
    /// refused, expired (401/403/404/410), redirects, or the file is too large.
    async fn fetch_download(
        &self,
        raw: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<u8>>> {
        let Some(url) = download_allowed(&self.hosts, self.allow_http, raw) else {
            tracing::warn!(
                source = %self.ctx.source,
                host = Url::parse(raw).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default(),
                "onedrive: refusing download URL outside the Microsoft download hosts"
            );
            return Ok(None);
        };
        let retry = self.api.retry();
        let mut attempt = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let req = self.download.request(Method::GET, url.as_str());
            let err = match self.download.send(req, &[]).await {
                Ok(resp) => match resp.status.as_u16() {
                    200..=299 => return Ok(Some(resp.body)),
                    300..=399 => {
                        tracing::warn!(source = %self.ctx.source, "onedrive: download redirected; not followed");
                        return Ok(None);
                    }
                    401 | 403 | 404 | 410 => return Ok(None),
                    429 | 503 | 509 => Error::RateLimited {
                        retry_after_ms: retry_after_ms(&resp),
                    },
                    s @ (408 | 500..=599) => {
                        // The server answered for this file: after the retries, skip it
                        // rather than stop the pass.
                        if attempt >= retry.max_retries {
                            tracing::warn!(source = %self.ctx.source, "onedrive: skipping file, download failed with HTTP {s}");
                            return Ok(None);
                        }
                        Error::Network(format!("onedrive download: HTTP {s}"))
                    }
                    s => {
                        tracing::warn!(source = %self.ctx.source, "onedrive: skipping file, download failed with HTTP {s}");
                        return Ok(None);
                    }
                },
                Err(Error::Invalid(m)) if m.contains("exceeds") => {
                    tracing::warn!(source = %self.ctx.source, "onedrive: skipping file over the size cap");
                    return Ok(None);
                }
                Err(e @ Error::Network(_)) => e,
                Err(e) => return Err(e),
            };
            if attempt >= retry.max_retries {
                return Err(err);
            }
            let delay = backoff(&retry, attempt, &err);
            attempt += 1;
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            }
        }
    }
}

/// Whether a download URL may be fetched: HTTPS (unless `allow_http`, for tests), no
/// credentials in the URL, and a host on the allowlist (the host itself or a subdomain).
fn download_allowed(hosts: &[String], allow_http: bool, raw: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    let scheme_ok = url.scheme() == "https" || (allow_http && url.scheme() == "http");
    if !scheme_ok || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    hosts
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        .then_some(url)
}

/// The parent id in a version written by [`OneDriveConnector::apply_page`].
fn version_parent(v: Option<&str>) -> Option<&str> {
    v?.rsplit_once('|')
        .map(|(_, p)| p)
        .filter(|p| !p.is_empty())
}

/// `id` and every tracked folder below it.
fn subtree(folders: &BTreeMap<String, Folder>, id: &str) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::from([id.to_string()]);
    // Repeat until no new descendant appears; bounded by the depth limit.
    for _ in 0..MAX_DEPTH {
        let before = out.len();
        for (fid, Folder(parent, _)) in folders {
            if !parent.is_empty() && out.contains(parent) {
                out.insert(fid.clone());
            }
        }
        if out.len() == before {
            break;
        }
    }
    out
}

/// `/a/b` for a tracked folder (`/` for the root); `None` when the chain is broken, too deep or
/// cyclic.
fn folder_path(folders: &BTreeMap<String, Folder>, id: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let Folder(parent, name) = folders.get(cur)?;
        if parent.is_empty() {
            parts.reverse();
            return Some(format!("/{}", parts.join("/")));
        }
        if name.is_empty() || name.contains('/') {
            return None;
        }
        parts.push(name);
        cur = parent;
    }
    None
}

fn mime_for(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    SUPPORTED.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

/// Drive item ids: alphanumerics plus `!` (personal drives), `-`, `_` and `.`.
fn valid_item_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        // Dot-segments: the URL builder drops them, so `..` would address another endpoint.
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'!' | b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod unit {
    use super::*;

    fn tree() -> BTreeMap<String, Folder> {
        let f = |p: &str, n: &str| Folder(p.into(), n.into());
        BTreeMap::from([
            ("root".to_string(), f("", "")),
            ("docs".to_string(), f("root", "Documents")),
            ("work".to_string(), f("docs", "Work")),
            ("loop1".to_string(), f("loop2", "a")),
            ("loop2".to_string(), f("loop1", "b")),
            ("orphan".to_string(), f("missing", "x")),
        ])
    }

    #[test]
    fn paths_from_the_tree() {
        let t = tree();
        assert_eq!(folder_path(&t, "root").as_deref(), Some("/"));
        assert_eq!(folder_path(&t, "work").as_deref(), Some("/Documents/Work"));
        assert_eq!(folder_path(&t, "loop1"), None);
        assert_eq!(folder_path(&t, "orphan"), None);
        assert_eq!(folder_path(&t, "nope"), None);
        let s = subtree(&t, "docs");
        assert_eq!(s, HashSet::from(["docs".to_string(), "work".to_string()]));
        // Cycles terminate.
        assert_eq!(subtree(&t, "loop1").len(), 2);
    }

    #[test]
    fn mime_and_ids() {
        assert_eq!(mime_for("Report.PDF"), Some("application/pdf"));
        assert_eq!(mime_for("notes.md"), Some("text/markdown"));
        assert_eq!(mime_for("setup.exe"), None);
        assert_eq!(mime_for("noext"), None);
        assert!(valid_item_id("D4648F06C91D9D3D!54927"));
        assert!(valid_item_id("01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K"));
        assert!(!valid_item_id("a/b"));
        assert!(!valid_item_id("..") && !valid_item_id("."));
        assert!(!valid_item_id(""));
        assert_eq!(version_parent(Some("\"{x},1\"|docs")), Some("docs"));
        assert_eq!(version_parent(Some("e|")), None);
    }

    #[test]
    fn download_hosts_are_checked() {
        let hosts: Vec<String> = crate::DOWNLOAD_HOSTS
            .iter()
            .map(|h| h.to_string())
            .collect();
        let ok = |u: &str| download_allowed(&hosts, false, u).is_some();
        assert!(ok("https://public.bn1302.files.1drv.com/y4m/file.pdf?x=1"));
        assert!(ok(
            "https://contoso-my.sharepoint.com/personal/_layouts/15/download.aspx?UniqueId=1"
        ));
        assert!(ok("https://my.microsoftpersonalcontent.com/x"));
        assert!(ok("https://SHAREPOINT.COM./x"));
        assert!(!ok("http://contoso-my.sharepoint.com/x"), "plain HTTP");
        assert!(!ok("https://sharepoint.com.evil.example/x"));
        assert!(!ok("https://evilsharepoint.com/x"));
        assert!(!ok("https://user:pw@contoso.sharepoint.com/x"));
        assert!(!ok("https://graph.microsoft.com/v1.0/me"));
        assert!(!ok("https://127.0.0.1/x"));
        assert!(!ok("file:///etc/passwd"));
        assert!(!ok("not a url"));
        let local = vec!["127.0.0.1".to_string()];
        assert!(download_allowed(&local, true, "http://127.0.0.1:8080/x").is_some());
        assert!(download_allowed(&local, false, "http://127.0.0.1:8080/x").is_none());
    }

    #[test]
    fn cursor_round_trips() {
        let mut c = Cursor::fresh(true);
        c.folders
            .insert("root".into(), Folder(String::new(), String::new()));
        let v = c.to_value();
        assert_eq!(
            v,
            json!({"v": 1, "full": true, "reconcile": true, "folders": {"root": ["", ""]}})
        );
        assert_eq!(serde_json::from_value::<Cursor>(v).unwrap(), c);
    }
}
