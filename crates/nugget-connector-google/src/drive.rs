//! `gdrive`: Google Drive over the REST API with `drive.readonly`.
//!
//! A pass is either a **full listing** or an **incremental** pass over the Changes API:
//!
//! 1. Full listing: `changes/startPageToken` is taken first (so nothing changed during the
//!    listing is missed), then `files.list` is paged through (`trashed=false`, plus
//!    `'me' in owners` when neither "shared with me" nor shared drives are wanted; `corpora=user`,
//!    or `allDrives` with shared drives on). The cursor is checkpointed after every page.
//! 2. Incremental: `changes.list` from the saved token (`includeRemoved=true`, `spaces=drive`),
//!    checkpointed after every page, ending at `newStartPageToken`. A full listing is followed
//!    by an incremental pass from its start token in the same `sync` call. An expired token
//!    (404/410) starts a reconciling full listing ([`MAX_RESTARTS`] per pass).
//!
//! Cursor (JSON): `{"v":1,"full":bool,"reconcile":bool,"start_token":"<tok>"|null,
//! "page_token":"<tok>"|null,"changes_token":"<tok>"|null}` (false/null fields omitted).
//! `full` marks a listing in progress: `start_token` is the Changes token taken before it and
//! `page_token` the next `files.list` page. Otherwise `changes_token` is the next Changes page
//! (or `newStartPageToken`). With `reconcile` (no cursor while files are indexed, e.g. after a
//! settings change, or an expired token) every indexed file the listing does not report in
//! scope is deleted at its end, unless Drive flagged the listing `incompleteSearch`; an
//! interrupted reconciling listing restarts from its first page. Any other cursor version or
//! shape is discarded (and the pass starts over).
//!
//! Scope: a file is synced iff it is not trashed, not a folder or shortcut, and either lives in
//! a shared drive (`driveId`) with `include_shared_drives` on, or lives outside shared drives
//! and is owned by the user (`ownedByMe` not `false`) or `include_shared_with_me` is on.
//!
//! Keys are Drive file ids. Versions are `<version>|<driveId>` (empty drive id outside shared
//! drives) for indexed files, and `<version>|<driveId>|skip<max_file_mb>` for "not indexed"
//! stubs, so raising the size cap fetches them again. The drive id lets every file of a shared
//! drive the user left (or that was deleted) be removed when Drive reports only the drive.
//!
//! One bad file never stops a pass: a file whose content fails (5xx or network after the
//! in-pass retries, or a malformed answer) keeps its indexed version (it still counts as
//! listed) and is carried in the cursor as `"retry":[{"id":"<id>","n":<passes failed>},...]`
//! (bare ids from older cursors read as `n: 0`), fetched again (`files.get`) at the end of the
//! next pass, and dropped with a warning after failing in
//! [`MAX_ATTEMPTS`](crate::carry::MAX_ATTEMPTS) passes. A carried file Drive now refuses (a
//! 400 or non-rate 403, e.g. `insufficientFilePermissions`) is removed like a deleted one.
//! [`MAX_FAILURE_STREAK`] first-time failures in a row (an outage, not one file) end the pass,
//! after the failure counts are checkpointed; files that failed in earlier passes never count
//! towards it, and failures while retrying carried files only stop the retrying.
//!
//! Scopes: `drive.readonly` only. It covers listing, the Changes API, downloads and Docs /
//! Sheets / Slides exports (the Sheets and Slides read APIs accept it too); nothing is written.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::{
    AuthFlow, AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext,
    ConnectorFactory, Content, Document, KindInfo,
};
use nugget_core::ids::ItemKey;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::api::{GoogleApi, Media, OAuthSettings, PageGuard, api_url};
use crate::carry::{Carried, Pending};
use crate::drive_content::{ContentFetcher, Fetched, FileRef, is_container};
use crate::settings::DriveSettings;
use crate::text::truncate_chars;
use crate::{GoogleEndpoints, RetryPolicy};

pub(crate) const KIND: &str = "gdrive";
const FEATURE: &str = "sync:gdrive";
/// Read-only access to the user's Drive files (Sheets and Slides reads accept it too).
pub(crate) const SCOPE_DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
const PAGE_SIZE: &str = "1000";
/// Fields requested for every file, in `files.list` and `changes.list`.
const FILE_FIELDS: &str = "id,name,mimeType,size,version,modifiedTime,createdTime,trashed,parents,driveId,ownedByMe,shared,webViewLink,owners(displayName,emailAddress),md5Checksum";
const MAX_NAME: usize = 400;
const MAX_OWNERS: usize = 10;
const MAX_PARENTS: usize = 10;
const MAX_ID: usize = 256;
const MAX_TOKEN: usize = 4096;
const MAX_URL: usize = 4096;
/// Guard against a server that keeps invalidating tokens.
const MAX_RESTARTS: u32 = 3;
/// Pages requested in one pass, all listings together.
const MAX_PAGES: u32 = 100_000;
/// Consecutive per-file failures after which the pass gives up.
const MAX_FAILURE_STREAK: u32 = 8;
/// Longest list of failed files carried in the cursor.
const MAX_RETRY: usize = 1_000;
/// Largest `files.get` answer for a carried file.
const MAX_FILE_JSON: usize = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    full: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reconcile: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    changes_token: Option<String>,
    /// Files whose content failed, to fetch again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retry: Vec<Carried>,
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

    /// A cursor this version can resume from.
    fn usable(&self) -> bool {
        self.v == 1
            && (self.full || self.changes_token.is_some())
            && [&self.start_token, &self.page_token, &self.changes_token]
                .iter()
                .all(|t| t.as_deref().is_none_or(valid_token))
    }
}

/// Factory for `gdrive` sources.
#[derive(Debug, Clone, Default)]
pub struct DriveFactory {
    endpoints: GoogleEndpoints,
    retry: RetryPolicy,
}

impl DriveFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_endpoints(mut self, endpoints: GoogleEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

impl ConnectorFactory for DriveFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "Google Drive",
            auth: AuthKind::OAuth,
            settings_schema: DriveSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        DriveSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = DriveSettings::parse(settings)?;
        let api = GoogleApi::new(
            FEATURE,
            SCOPE_DRIVE_READONLY,
            OAuthSettings {
                client_id: settings.client_id.clone(),
                client_secret: settings.client_secret.clone(),
                login_hint: settings.account_hint.clone(),
            },
            &self.endpoints,
            self.retry,
            ctx.clone(),
        )?;
        let fetcher = ContentFetcher::new(&self.endpoints, settings.max_bytes());
        Ok(Arc::new(DriveConnector {
            api,
            fetcher,
            base: self.endpoints.drive_api.clone(),
            settings,
            ctx,
        }))
    }
}

/// One Google account's Drive.
pub struct DriveConnector {
    api: GoogleApi,
    fetcher: ContentFetcher,
    base: String,
    settings: DriveSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for DriveConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct StartToken {
    #[serde(rename = "startPageToken")]
    start_page_token: Option<String>,
}

#[derive(Deserialize)]
struct FileList {
    #[serde(default)]
    files: Vec<Value>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(default, rename = "incompleteSearch")]
    incomplete_search: Option<bool>,
}

#[derive(Deserialize)]
struct ChangeList {
    #[serde(default)]
    changes: Vec<Value>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(rename = "newStartPageToken")]
    new_start_page_token: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DriveChange {
    #[serde(rename = "changeType")]
    change_type: Option<String>,
    removed: Option<bool>,
    #[serde(rename = "fileId")]
    file_id: Option<String>,
    #[serde(rename = "driveId")]
    drive_id: Option<String>,
    file: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Owner {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DriveFile {
    id: String,
    name: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    /// A decimal string in Drive's JSON (numbers accepted too).
    size: Option<Value>,
    /// A decimal string in Drive's JSON (numbers accepted too).
    version: Option<Value>,
    #[serde(rename = "modifiedTime")]
    modified_time: Option<String>,
    #[serde(rename = "createdTime")]
    created_time: Option<String>,
    trashed: Option<bool>,
    parents: Option<Vec<String>>,
    #[serde(rename = "driveId")]
    drive_id: Option<String>,
    #[serde(rename = "ownedByMe")]
    owned_by_me: Option<bool>,
    shared: Option<bool>,
    #[serde(rename = "webViewLink")]
    web_view_link: Option<String>,
    owners: Option<Vec<Owner>>,
    #[serde(rename = "md5Checksum")]
    md5_checksum: Option<String>,
}

impl DriveFile {
    fn drive_id(&self) -> Option<&str> {
        self.drive_id.as_deref().filter(|d| valid_id(d))
    }

    fn mime(&self) -> &str {
        self.mime_type.as_deref().unwrap_or_default()
    }

    fn size(&self) -> Option<u64> {
        match self.size.as_ref()? {
            Value::String(s) => s.trim().parse().ok(),
            Value::Number(n) => n.as_u64(),
            _ => None,
        }
    }

    /// Drive's `version`, falling back to `modifiedTime`; never contains `|`.
    fn version(&self) -> String {
        let v = match &self.version {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => self.modified_time.clone().unwrap_or_default(),
        };
        clean_line(&v, 100).replace('|', "")
    }
}

/// State of one sync pass.
struct Pass {
    /// Indexed files (key → version), kept up to date with what this pass sends.
    live: HashMap<String, Option<String>>,
    /// Files reported in scope during a full listing.
    seen: HashSet<String>,
    /// Drive flagged the listing as possibly incomplete: no reconciliation deletes.
    incomplete: bool,
    pages: u32,
    /// Files whose content failed in this or an earlier pass (see the module docs).
    retry: Pending,
    /// Files whose content was fetched in this pass (not retried again in it).
    tried: HashSet<String>,
    /// First-time per-file failures in a row.
    streak: u32,
    /// The streak ended the pass: save the failure counts before failing.
    tripped: bool,
}

impl Pass {
    /// One file failed with `e`: carry it, unless first-time failures keep coming (then fail
    /// the pass).
    fn failed(&mut self, key: &str, e: Error) -> Result<()> {
        if self.retry.fail(key, "gdrive", &e) {
            self.streak += 1;
            if self.streak >= MAX_FAILURE_STREAK {
                self.tripped = true;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Checkpoint `cur` with the carried failures.
    async fn checkpoint(&self, cur: &mut Cursor, sink: &dyn ChangeSink) -> Result<()> {
        cur.retry = self.retry.list();
        sink.checkpoint(cur.to_value()).await
    }

    fn page(&mut self) -> Result<()> {
        self.pages += 1;
        if self.pages > MAX_PAGES {
            return Err(Error::Invalid("gdrive: listing does not end".into()));
        }
        Ok(())
    }

    /// Forget `key`; `true` when it was indexed (so a delete is due).
    fn forget(&mut self, key: &str) -> bool {
        self.seen.remove(key);
        self.retry.resolve(key);
        self.live.remove(key).is_some()
    }
}

/// How a listing ended.
enum Flow {
    Done,
    /// A page or change token expired (404/410): start over with a reconciling listing.
    Expired,
}

#[async_trait]
impl Connector for DriveConnector {
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
            Ok(c) if c.usable() => Some(c),
            Ok(_) => {
                tracing::warn!(source = %self.ctx.source, "unusable gdrive cursor; starting over");
                None
            }
            Err(e) => {
                tracing::warn!(source = %self.ctx.source, "unreadable gdrive cursor ({e}); starting over");
                None
            }
        });
        let mut cur = match parsed {
            Some(c) => c,
            None => {
                // No cursor but indexed files (e.g. the settings changed): reconcile.
                let reconcile = !known.is_empty();
                if reconcile {
                    tracing::info!(source = %self.ctx.source, "gdrive cursor reset; reconciling listing");
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
            incomplete: false,
            pages: 0,
            retry: Pending::new(&std::mem::take(&mut cur.retry), MAX_RETRY, "gdrive file"),
            streak: 0,
            tried: HashSet::new(),
            tripped: false,
        };
        match self.run(&mut cur, &mut pass, sink, cancel).await {
            Ok(()) => Ok(cur.to_value()),
            Err(e) => {
                if pass.tripped
                    && let Err(ce) = pass.checkpoint(&mut cur, sink).await
                {
                    tracing::warn!(source = %self.ctx.source, "gdrive: could not save failure counts: {ce}");
                }
                Err(e)
            }
        }
    }
}

impl DriveConnector {
    /// One pass from `cur` (left at the last checkpointed position on error).
    async fn run(
        &self,
        cur: &mut Cursor,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        // Only what earlier passes carried in is retried at the end of this one.
        let carried = pass.retry.take_carried();
        let mut restarts = 0u32;
        loop {
            let mut flow = Flow::Done;
            if cur.full {
                flow = self.listing(cur, pass, sink, cancel).await?;
            }
            if matches!(flow, Flow::Done) {
                flow = self.changes(cur, pass, sink, cancel).await?;
            }
            match flow {
                Flow::Done => {
                    self.retry_failed(carried, pass, sink, cancel).await?;
                    cur.retry = pass.retry.list();
                    return Ok(());
                }
                Flow::Expired if restarts < MAX_RESTARTS => {
                    tracing::info!(source = %self.ctx.source, "gdrive token expired; reconciling listing");
                    restarts += 1;
                    *cur = Cursor::fresh(!pass.live.is_empty());
                    pass.seen.clear();
                    pass.incomplete = false;
                }
                Flow::Expired => {
                    return Err(Error::Other("gdrive: page tokens keep expiring".into()));
                }
            }
        }
    }
}

impl DriveConnector {
    fn shared_drives(&self) -> bool {
        self.settings.include_shared_drives
    }

    /// Whether `file` belongs to this source (see the module docs).
    fn in_scope(&self, file: &DriveFile) -> bool {
        in_scope(
            file,
            self.settings.include_shared_with_me,
            self.settings.include_shared_drives,
        )
    }

    /// The `files.list` query for the first page (`page_token` appended by the caller).
    fn files_query(&self) -> Vec<(&'static str, &'static str)> {
        let s = &self.settings;
        let q = if !s.include_shared_with_me && !s.include_shared_drives {
            "trashed=false and 'me' in owners"
        } else {
            "trashed=false"
        };
        let mut query = vec![("pageSize", PAGE_SIZE), ("q", q)];
        if s.include_shared_drives {
            query.extend([
                ("corpora", "allDrives"),
                ("includeItemsFromAllDrives", "true"),
                ("supportsAllDrives", "true"),
            ]);
        } else {
            query.push(("corpora", "user"));
        }
        query
    }

    async fn start_token(&self, cancel: &CancellationToken) -> Result<String> {
        let url = api_url(
            &self.base,
            &["changes", "startPageToken"],
            &[("supportsAllDrives", "true")],
        )?;
        let resp = self.api.get(&url, cancel).await?;
        if resp.status.as_u16() != 200 {
            return Err(Error::Other(format!(
                "gdrive changes.getStartPageToken: HTTP {}",
                resp.status
            )));
        }
        let t: StartToken = GoogleApi::json(&resp, "gdrive changes.getStartPageToken")?;
        t.start_page_token
            .filter(|t| valid_token(t))
            .ok_or_else(|| Error::Invalid("gdrive: no usable startPageToken".into()))
    }

    /// The full listing; on success `cur` is switched to incremental mode at its start token.
    async fn listing(
        &self,
        cur: &mut Cursor,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Flow> {
        if cur.reconcile && cur.page_token.is_some() {
            // Pages listed before the interruption are not remembered: start over rather than
            // take their files for missing.
            cur.page_token = None;
        }
        if cur.start_token.is_none() {
            cur.start_token = Some(self.start_token(cancel).await?);
            cur.page_token = None;
            pass.checkpoint(cur, sink).await?;
        }
        if cur.page_token.is_none() {
            pass.seen.clear();
            pass.incomplete = false;
        }
        let mut guard = PageGuard::default();
        if let Some(t) = &cur.page_token {
            guard.next(t, "gdrive files.list")?;
        }
        let fields = format!("nextPageToken,incompleteSearch,files({FILE_FIELDS})");
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            pass.page()?;
            let mut query: Vec<(&str, &str)> = self.files_query();
            query.push(("fields", &fields));
            if let Some(p) = &cur.page_token {
                query.push(("pageToken", p));
            }
            let url = api_url(&self.base, &["files"], &query)?;
            let resp = self.api.get(&url, cancel).await?;
            match resp.status.as_u16() {
                200 => {}
                404 | 410 => return Ok(Flow::Expired),
                s => return Err(Error::Other(format!("gdrive files.list: HTTP {s}"))),
            }
            let page: FileList = GoogleApi::json(&resp, "gdrive files.list")?;
            if page.incomplete_search == Some(true) {
                pass.incomplete = true;
            }
            for v in page.files {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                match serde_json::from_value::<DriveFile>(v) {
                    Ok(f) => self.apply_file(f, pass, sink, cancel).await?,
                    Err(e) => {
                        tracing::debug!(source = %self.ctx.source, "gdrive: unreadable file entry ({e})")
                    }
                }
            }
            if let Some(t) = page.next_page_token.filter(|t| !t.is_empty()) {
                if !valid_token(&t) {
                    return Err(Error::Invalid("gdrive files.list: bad page token".into()));
                }
                guard.next(&t, "gdrive files.list")?;
                cur.page_token = Some(t);
                pass.checkpoint(cur, sink).await?;
                continue;
            }
            if cur.reconcile {
                if pass.incomplete {
                    tracing::warn!(source = %self.ctx.source, "gdrive: Drive reported an incomplete listing; keeping unlisted files");
                } else {
                    let mut gone: Vec<String> = pass
                        .live
                        .keys()
                        .filter(|k| !pass.seen.contains(*k))
                        .cloned()
                        .collect();
                    gone.sort();
                    for key in gone {
                        pass.live.remove(&key);
                        sink.send(Change::Delete(ItemKey::new(key))).await?;
                    }
                }
            }
            *cur = Cursor {
                v: 1,
                changes_token: cur.start_token.take(),
                ..Default::default()
            };
            pass.checkpoint(cur, sink).await?;
            return Ok(Flow::Done);
        }
    }

    /// Incremental changes from `cur.changes_token` up to the next start token.
    async fn changes(
        &self,
        cur: &mut Cursor,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Flow> {
        let mut guard = PageGuard::default();
        let fields = format!(
            "nextPageToken,newStartPageToken,changes(changeType,removed,fileId,driveId,file({FILE_FIELDS}))"
        );
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Some(token) = cur.changes_token.clone() else {
                // Not resumable (should not happen): list everything again.
                return Ok(Flow::Expired);
            };
            guard.next(&token, "gdrive changes.list")?;
            pass.page()?;
            let mut query: Vec<(&str, &str)> = vec![
                ("pageToken", &token),
                ("pageSize", PAGE_SIZE),
                ("includeRemoved", "true"),
                ("spaces", "drive"),
                ("fields", &fields),
            ];
            if self.shared_drives() {
                query.extend([
                    ("includeItemsFromAllDrives", "true"),
                    ("supportsAllDrives", "true"),
                ]);
            }
            let url = api_url(&self.base, &["changes"], &query)?;
            let resp = self.api.get(&url, cancel).await?;
            match resp.status.as_u16() {
                200 => {}
                404 | 410 => return Ok(Flow::Expired),
                s => return Err(Error::Other(format!("gdrive changes.list: HTTP {s}"))),
            }
            let page: ChangeList = GoogleApi::json(&resp, "gdrive changes.list")?;
            let next = page
                .next_page_token
                .filter(|t| !t.is_empty())
                .filter(|t| valid_token(t));
            let new_start = page
                .new_start_page_token
                .filter(|t| !t.is_empty())
                .filter(|t| valid_token(t));
            let Some(following) = next.clone().or_else(|| new_start.clone()) else {
                return Err(Error::Invalid(
                    "gdrive changes page has neither nextPageToken nor newStartPageToken".into(),
                ));
            };
            for v in page.changes {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                match serde_json::from_value::<DriveChange>(v) {
                    Ok(c) => self.apply_change(c, pass, sink, cancel).await?,
                    Err(e) => {
                        tracing::debug!(source = %self.ctx.source, "gdrive: unreadable change entry ({e})")
                    }
                }
            }
            cur.changes_token = Some(following);
            pass.checkpoint(cur, sink).await?;
            if next.is_none() {
                return Ok(Flow::Done);
            }
        }
    }

    /// Fetch the files carried from earlier failures again (`files.get`, then as if listed).
    /// Every outcome is per file; a run of failures stops the retrying, never the pass.
    async fn retry_failed(
        &self,
        carried: Vec<String>,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        pass.streak = 0;
        for id in carried {
            if pass.tried.contains(&id) || !pass.retry.is_pending(&id) {
                // Fetched (or removed) earlier in this pass.
                continue;
            }
            if !valid_id(&id) {
                pass.retry.resolve(&id);
                continue;
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let r = self.retry_one(&id, pass, sink, cancel).await;
            match r {
                Ok(()) => {}
                Err(_) if pass.tripped => {
                    // An outage: leave the rest for the next pass.
                    pass.tripped = false;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn retry_one(
        &self,
        id: &str,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let url = api_url(
            &self.base,
            &["files", id],
            &[("fields", FILE_FIELDS), ("supportsAllDrives", "true")],
        )?;
        let resp = match self
            .api
            .get_media(&url, "application/json", MAX_FILE_JSON, cancel)
            .await
        {
            Ok(Media::Body(r)) => r,
            Ok(Media::TooLarge) => {
                return pass.failed(id, Error::Invalid("files.get answer too large".into()));
            }
            Ok(Media::Refused { status, reason, .. }) => {
                // No longer readable by this user (permissions changed, app not authorized):
                // gone from this source.
                tracing::warn!(source = %self.ctx.source, file = %id, "gdrive files.get refused (HTTP {status} {reason}); removing it");
                if pass.forget(id) {
                    sink.send(Change::Delete(ItemKey::new(id))).await?;
                }
                return Ok(());
            }
            Err(e @ (Error::Network(_) | Error::Invalid(_) | Error::Other(_))) => {
                return pass.failed(id, e);
            }
            Err(e) => return Err(e),
        };
        match resp.status.as_u16() {
            200 => match serde_json::from_slice::<DriveFile>(&resp.body) {
                Ok(f) if f.id == id => self.apply_file(f, pass, sink, cancel).await,
                _ => pass.failed(id, Error::Invalid("unreadable files.get answer".into())),
            },
            404 | 410 => {
                if pass.forget(id) {
                    sink.send(Change::Delete(ItemKey::new(id))).await?;
                }
                Ok(())
            }
            s => pass.failed(id, Error::Other(format!("files.get: HTTP {s}"))),
        }
    }

    async fn apply_change(
        &self,
        change: DriveChange,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let removed = change.removed == Some(true);
        if change.change_type.as_deref() == Some("drive") {
            // A shared drive: only its removal (deleted, or the user left) matters here.
            let Some(drive) = change.drive_id.as_deref().filter(|d| valid_id(d)) else {
                return Ok(());
            };
            if removed {
                let mut gone: Vec<String> = pass
                    .live
                    .iter()
                    .filter(|(_, v)| version_drive(v.as_deref()) == Some(drive))
                    .map(|(k, _)| k.clone())
                    .collect();
                gone.sort();
                for key in gone {
                    pass.forget(&key);
                    sink.send(Change::Delete(ItemKey::new(key))).await?;
                }
            }
            return Ok(());
        }
        let file = change
            .file
            .filter(|_| !removed)
            .and_then(|v| serde_json::from_value::<DriveFile>(v).ok());
        match file {
            Some(f) => self.apply_file(f, pass, sink, cancel).await,
            None => {
                // Removed (deleted for good, or no longer visible to the user).
                if let Some(id) = change.file_id.as_deref().filter(|i| valid_id(i))
                    && pass.forget(id)
                {
                    sink.send(Change::Delete(ItemKey::new(id))).await?;
                }
                Ok(())
            }
        }
    }

    async fn apply_file(
        &self,
        file: DriveFile,
        pass: &mut Pass,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if !valid_id(&file.id) {
            tracing::debug!(source = %self.ctx.source, "gdrive: skipping a file with a malformed id");
            return Ok(());
        }
        let key = file.id.clone();
        if is_container(file.mime()) || !self.in_scope(&file) {
            if pass.forget(&key) {
                sink.send(Change::Delete(ItemKey::new(key))).await?;
            }
            return Ok(());
        }
        let base = format!("{}|{}", file.version(), file.drive_id().unwrap_or_default());
        let stub = format!("{base}|skip{}", self.settings.max_file_mb);
        pass.seen.insert(key.clone());
        if pass
            .live
            .get(&key)
            .is_some_and(|v| v.as_deref() == Some(base.as_str()) || v.as_deref() == Some(&stub))
        {
            pass.retry.resolve(&key);
            return Ok(());
        }
        let name = clean_line(file.name.as_deref().unwrap_or_default(), MAX_NAME);
        let mime = clean_line(file.mime(), 200);
        pass.tried.insert(key.clone());
        let fetched = match self
            .fetcher
            .fetch(
                &self.api,
                FileRef {
                    id: &file.id,
                    name: &name,
                    mime_type: &mime,
                    size: file.size(),
                    shared_drive: file.drive_id().is_some(),
                },
                cancel,
            )
            .await
        {
            Ok(f) => {
                pass.streak = 0;
                pass.retry.resolve(&key);
                f
            }
            // This file only (a 5xx or network failure after the retries, a malformed answer):
            // keep its indexed version and try again next pass.
            Err(e @ (Error::Network(_) | Error::Invalid(_) | Error::Other(_))) => {
                return pass.failed(&key, e);
            }
            Err(e) => return Err(e),
        };
        let doc = match fetched {
            Fetched::Ready { mime, content, via } => {
                let mut doc = Document::new(key.clone(), mime, content);
                doc.version = Some(base);
                doc.metadata = metadata(&file, &name, Some(via), None);
                doc
            }
            Fetched::Skipped(reason) => {
                let reason = clean_line(&reason, 300);
                let shown = if name.is_empty() { "(untitled)" } else { &name };
                let mut doc = Document::new(
                    key.clone(),
                    "text/plain",
                    Content::Text(format!("{shown}\n\nNot indexed: {reason}")),
                );
                doc.version = Some(stub);
                doc.metadata = metadata(&file, &name, None, Some(&reason));
                doc
            }
            Fetched::Gone => {
                if pass.forget(&key) {
                    sink.send(Change::Delete(ItemKey::new(key))).await?;
                }
                return Ok(());
            }
        };
        let mut doc = doc;
        doc.title = Some(name).filter(|n| !n.is_empty());
        doc.modified_at = file
            .modified_time
            .as_deref()
            .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok());
        doc.url = file
            .web_view_link
            .as_deref()
            .filter(|u| u.starts_with("https://") && u.len() <= MAX_URL)
            .filter(|u| !u.chars().any(|c| c.is_control() || c.is_whitespace()))
            .map(str::to_string);
        pass.live.insert(key, doc.version.clone());
        sink.send(Change::Upsert(doc)).await
    }
}

/// Document metadata for `file` (every string from Drive is cleaned and bounded).
fn metadata(file: &DriveFile, name: &str, via: Option<&str>, skipped: Option<&str>) -> Value {
    let parents: Vec<&str> = file
        .parents
        .iter()
        .flatten()
        .map(String::as_str)
        .filter(|p| valid_id(p))
        .take(MAX_PARENTS)
        .collect();
    let owners: Vec<Value> = file
        .owners
        .iter()
        .flatten()
        .take(MAX_OWNERS)
        .map(|o| {
            json!({
                "name": o.display_name.as_deref().map(|n| clean_line(n, 200)),
                "email": o.email_address.as_deref().map(|e| clean_line(e, 320)),
            })
        })
        .collect();
    let time = |t: Option<&str>| t.map(|t| clean_line(t, 64));
    let mut meta = json!({
        "file_id": file.id,
        "name": name,
        "mime_type": clean_line(file.mime(), 200),
        "size": file.size(),
        "parents": parents,
        "drive_id": file.drive_id(),
        "owned_by_me": file.owned_by_me,
        "shared": file.shared,
        "owners": owners,
        "created": time(file.created_time.as_deref()),
        "modified": time(file.modified_time.as_deref()),
        "md5_checksum": file.md5_checksum.as_deref().map(|m| clean_line(m, 64)),
        "via": via,
    });
    if let Some(reason) = skipped {
        meta["skipped"] = json!(reason);
    }
    meta
}

/// The scope rule (see the module docs).
fn in_scope(file: &DriveFile, shared_with_me: bool, shared_drives: bool) -> bool {
    if file.trashed == Some(true) || is_container(file.mime()) {
        return false;
    }
    match file.drive_id() {
        Some(_) => shared_drives,
        None => file.owned_by_me != Some(false) || shared_with_me,
    }
}

/// The shared drive id in a version written by [`DriveConnector::apply_file`].
fn version_drive(v: Option<&str>) -> Option<&str> {
    v?.split('|').nth(1).filter(|d| !d.is_empty())
}

/// Drive file and shared drive ids: alphanumerics, `-` and `_`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Page tokens are opaque but go into the cursor and URLs: bounded, printable ASCII.
fn valid_token(t: &str) -> bool {
    !t.is_empty() && t.len() <= MAX_TOKEN && t.bytes().all(|b| b.is_ascii_graphic())
}

/// One line of untrusted text: control and bidi-override characters become spaces, trimmed,
/// at most `max` characters.
fn clean_line(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    truncate_chars(cleaned.trim(), max)
}

/// A `files.list` / `changes.list` entry as [`DriveConnector::apply_file`] reads it, for
/// [`crate::drive_content::fuzz_export`].
pub(crate) fn fuzz_file_entry(body: &[u8]) -> Vec<(&'static str, String, usize)> {
    let Ok(file) = serde_json::from_slice::<DriveFile>(body) else {
        return Vec::new();
    };
    let mut out = vec![("version", file.version(), 400)];
    if valid_id(&file.id) {
        let name = clean_line(file.name.as_deref().unwrap_or_default(), MAX_NAME);
        let meta = metadata(&file, &name, Some("download"), Some("reason"));
        out.push(("line", name, MAX_NAME * 4));
        out.push(("json", meta.to_string(), 256 * 1024));
    }
    out
}

#[cfg(test)]
mod unit {
    use super::*;

    fn file(v: Value) -> DriveFile {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn ids_and_tokens() {
        assert!(valid_id("1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms"));
        assert!(valid_id("0AF-x_y"));
        assert!(!valid_id(""));
        assert!(!valid_id(".."));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("a b"));
        assert!(!valid_id("a?b"));
        assert!(!valid_id(&"a".repeat(MAX_ID + 1)));
        assert!(valid_token("~!@4:1234"));
        assert!(!valid_token(""));
        assert!(!valid_token("a b"));
        assert!(!valid_token(&"a".repeat(MAX_TOKEN + 1)));
    }

    #[test]
    fn scope_rule() {
        let mine = file(json!({"id": "a", "mimeType": "text/plain", "ownedByMe": true}));
        let theirs = file(json!({"id": "b", "mimeType": "text/plain", "ownedByMe": false}));
        let shared = file(json!({"id": "c", "mimeType": "text/plain", "driveId": "0AD"}));
        let trashed = file(json!({"id": "d", "ownedByMe": true, "trashed": true}));
        let folder = file(
            json!({"id": "e", "ownedByMe": true, "mimeType": "application/vnd.google-apps.folder"}),
        );
        let unknown_owner = file(json!({"id": "f", "mimeType": "text/plain"}));
        assert!(in_scope(&mine, false, false));
        assert!(in_scope(&unknown_owner, false, false));
        assert!(!in_scope(&theirs, false, false));
        assert!(in_scope(&theirs, true, false));
        assert!(!in_scope(&theirs, false, true));
        assert!(!in_scope(&shared, true, false));
        assert!(in_scope(&shared, false, true));
        assert!(!in_scope(&trashed, true, true));
        assert!(!in_scope(&folder, true, true));
    }

    #[test]
    fn versions() {
        let f = file(json!({"id": "a", "version": "42", "size": "1024"}));
        assert_eq!(f.version(), "42");
        assert_eq!(f.size(), Some(1024));
        let f = file(json!({"id": "a", "version": 7, "size": 5}));
        assert_eq!(f.version(), "7");
        assert_eq!(f.size(), Some(5));
        let f = file(json!({"id": "a", "modifiedTime": "2025-01-01T00:00:00Z", "size": "x"}));
        assert_eq!(f.version(), "2025-01-01T00:00:00Z");
        assert_eq!(f.size(), None);
        assert_eq!(file(json!({"id": "a", "version": "1|2"})).version(), "12");
        assert_eq!(version_drive(Some("42|0AD")), Some("0AD"));
        assert_eq!(version_drive(Some("42|0AD|skip25")), Some("0AD"));
        assert_eq!(version_drive(Some("42|")), None);
        assert_eq!(version_drive(Some("42||skip25")), None);
        assert_eq!(version_drive(Some("42")), None);
        assert_eq!(version_drive(None), None);
    }

    #[test]
    fn cursor_round_trips() {
        let mut c = Cursor::fresh(true);
        c.start_token = Some("100".into());
        c.page_token = Some("p2".into());
        let v = c.to_value();
        assert_eq!(
            v,
            json!({"v": 1, "full": true, "reconcile": true, "start_token": "100", "page_token": "p2"})
        );
        let back: Cursor = serde_json::from_value(v).unwrap();
        assert_eq!(back, c);
        assert!(back.usable());
        let inc = Cursor {
            v: 1,
            changes_token: Some("200".into()),
            ..Default::default()
        };
        assert_eq!(inc.to_value(), json!({"v": 1, "changes_token": "200"}));
        assert!(inc.usable());
        assert!(
            !Cursor {
                v: 1,
                ..Default::default()
            }
            .usable()
        );
        assert!(
            !Cursor {
                v: 2,
                ..inc.clone()
            }
            .usable()
        );
        assert!(
            !Cursor {
                changes_token: Some("a b".into()),
                ..inc
            }
            .usable()
        );
    }

    #[test]
    fn cleans_untrusted_text() {
        assert_eq!(clean_line(" a\u{0}b\nc\u{202E}d ", 20), "a b c d");
        assert_eq!(clean_line("abcdef", 3), "abc");
    }

    #[test]
    fn metadata_is_bounded() {
        let owners: Vec<Value> = (0..50)
            .map(|i| json!({"displayName": format!("n{i}\n"), "emailAddress": "e@x.org"}))
            .collect();
        let f = file(json!({
            "id": "a", "name": "Doc", "mimeType": "application/pdf", "size": "10",
            "parents": ["p1", "bad/id"], "owners": owners, "ownedByMe": true,
        }));
        let m = metadata(&f, "Doc", Some("download"), Some("Too big."));
        assert_eq!(m["owners"].as_array().unwrap().len(), MAX_OWNERS);
        assert_eq!(m["owners"][0]["name"], "n0");
        assert_eq!(m["parents"], json!(["p1"]));
        assert_eq!(m["size"], 10);
        assert_eq!(m["via"], "download");
        assert_eq!(m["skipped"], "Too big.");
    }
}
