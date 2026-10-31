//! `outlook`: Outlook / Exchange Online mail over Microsoft Graph with `Mail.Read`.
//!
//! Scopes: `Mail.Read` (read messages and their MIME; no send or write), `offline_access` (a
//! refresh token, so syncing continues without signing in again) and `User.Read` (lets the
//! consent screen name the account). Tokens live only in the secret store.
//!
//! Each selected folder (default: `inbox` and `sentitems`) is synced with its own message delta
//! query. Keys are `<folder>/<immutable message id>`, so a message moved between two synced
//! folders becomes a delete under one and an upsert under the other, whatever order the folders
//! are processed in.
//!
//! Cursor (JSON): `{"v":1,"folders":{"<folder>":<state>}}` where `<state>` is
//! - `{"delta_link":"<url>"}` once the folder is in incremental mode,
//! - `{"after":<unix secs>,"next_link":"<url>"|null,"resync":bool,"reconcile":bool}` while the
//!   windowed full listing (`receivedDateTime ge <after>`) runs, or
//! - `{"delta_link":"<url>","next_link":"<url>"}` while an incremental round is part-way,
//!
//! plus `"verify":["<key>",...]` (omitted when empty): indexed messages a resync did not list and
//! that are still to be checked, [`MAX_VERIFY`] per pass. At the top level,
//! `"retry":[{"key":"<key>","attempts":n},...]` (omitted when empty) lists messages whose MIME
//! Graph could not serve (5xx after retries, 403 `ErrorAccessDenied`, 400, 409/423); they are
//! fetched again at the end of later passes, deleted if gone, and given up after
//! [`MAX_ITEM_ATTEMPTS`](crate::graph::MAX_ITEM_ATTEMPTS) tries. Several such failures in a row
//! mean an outage and end the pass instead.
//!
//! Folders that are no longer synced are found from the indexed keys, not only from the cursor,
//! so a reset cursor still drops them. A folder with indexed messages but no cursor state (a
//! settings update resets the cursor) gets a *reconciling* full listing: indexed messages it
//! does not return are deleted when they are older than the (possibly new) window, and checked
//! otherwise. A plain resync (410 / `syncStateNotFound`) checks only messages inside the window;
//! older ones are expected to be missing from the listing and stay. A resync interrupted part-way
//! restarts its listing, so no message is wrongly taken as missing.
//!
//! Versions are `r<receivedDateTime unix secs>`: sent and received mail does not change, so a
//! delta entry for a read-flag or category change does not re-download the MIME, and the window
//! test needs no request.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
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

use crate::graph::{
    DeltaPage, Get, GraphApi, ItemGet, RetryItem, Skips, is_removed, needs_resync, status_error,
};
use crate::oauth::{SCOPE_MAIL_READ, SCOPE_OFFLINE_ACCESS, SCOPE_USER_READ};
use crate::settings::{OutlookSettings, valid_folder_id};
use crate::text::clean_line;
use crate::{MicrosoftEndpoints, RetryPolicy};

pub(crate) const KIND: &str = "outlook";
const FEATURE: &str = "sync:outlook";
const SCOPES: &[&str] = &[SCOPE_OFFLINE_ACCESS, SCOPE_USER_READ, SCOPE_MAIL_READ];
/// Messages per delta page.
const PAGE_SIZE: &str = "odata.maxpagesize=50";
/// Ids that survive moves between folders.
const IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";
const SELECT: &str = "id,receivedDateTime,lastModifiedDateTime,subject,from,conversationId,\
                      internetMessageId,isDraft,webLink";
/// Largest MIME message downloaded; bigger ones are skipped.
const MAX_MIME: usize = 35 * 1024 * 1024;
/// Concurrent MIME downloads (Graph allows 4 concurrent requests per mailbox).
const FETCH_CONCURRENCY: usize = 4;
/// Unlisted messages checked per pass; the rest wait in the cursor.
const MAX_VERIFY: usize = 500;
/// Longest list of messages carried in the cursor for later checks.
const MAX_CARRY: usize = 20_000;
/// Slack on the window boundary when deciding whether an unlisted message should have been
/// listed.
const WINDOW_SLACK_SECS: i64 = 86_400;
/// Guard against a server that keeps invalidating delta tokens.
const MAX_RESTARTS: u32 = 3;
/// Guard against a server that never ends a listing.
const MAX_PAGES: u32 = 100_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct FolderState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delta_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_link: Option<String>,
    /// Set while a windowed full listing runs: the window start (Unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    resync: bool,
    /// With `resync`: also delete indexed messages older than the window.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reconcile: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verify: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default)]
    folders: BTreeMap<String, FolderState>,
    /// Messages whose MIME could not be fetched, tried again on later passes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retry: Vec<RetryItem>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            v: 1,
            folders: BTreeMap::new(),
            retry: Vec::new(),
        }
    }
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The cursor with the items still to retry.
    fn with(&mut self, skips: &Skips) -> Value {
        self.retry = skips.list.clone();
        self.to_value()
    }
}

/// Factory for `outlook` sources.
#[derive(Debug, Clone, Default)]
pub struct OutlookFactory {
    endpoints: MicrosoftEndpoints,
    retry: RetryPolicy,
}

impl OutlookFactory {
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

impl ConnectorFactory for OutlookFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "Outlook mail",
            auth: AuthKind::OAuth,
            settings_schema: OutlookSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        OutlookSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = OutlookSettings::parse(settings)?;
        let api = GraphApi::new(
            FEATURE,
            SCOPES,
            &settings.common(),
            &self.endpoints,
            self.retry,
            ctx.clone(),
        )?;
        Ok(Arc::new(OutlookConnector { api, settings, ctx }))
    }
}

/// One Microsoft 365 / Outlook.com mailbox.
pub struct OutlookConnector {
    api: GraphApi,
    settings: OutlookSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for OutlookConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlookConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
    id: String,
    #[serde(rename = "receivedDateTime")]
    received: Option<String>,
    #[serde(rename = "lastModifiedDateTime")]
    last_modified: Option<String>,
    subject: Option<String>,
    from: Option<Recipient>,
    #[serde(rename = "conversationId")]
    conversation_id: Option<String>,
    #[serde(rename = "internetMessageId")]
    internet_message_id: Option<String>,
    #[serde(rename = "isDraft")]
    is_draft: Option<bool>,
    #[serde(rename = "webLink")]
    web_link: Option<String>,
    #[serde(rename = "parentFolderId")]
    parent_folder_id: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Recipient {
    #[serde(rename = "emailAddress")]
    email: EmailAddress,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EmailAddress {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FolderInfo {
    id: String,
}

/// Result of fetching one message.
enum Fetched {
    Change(Option<Change>),
    /// Graph could not serve it now: retry on a later pass.
    Failed(String),
}

/// What to do for one delta entry, in page order.
enum Step {
    Delete(String),
    Fetch(String, Box<Message>),
}

#[async_trait]
impl Connector for OutlookConnector {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(60)
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
        let mut cur = cursor
            .and_then(|v| match serde_json::from_value::<Cursor>(v) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(source = %self.ctx.source, "unreadable outlook cursor ({e}); starting over");
                    None
                }
            })
            .unwrap_or_default();
        let folders = self.settings.folder_ids();
        let mut skips = Skips::new(std::mem::take(&mut cur.retry));

        // Folders no longer synced: drop their messages. They are found from the indexed keys
        // as well as the cursor, which a settings update resets.
        let known = self.ctx.known.versions().await?;
        let known_folders: BTreeSet<&str> = known
            .keys()
            .filter_map(|k| k.as_str().split_once('/').map(|(f, _)| f))
            .collect();
        let wanted: HashSet<&str> = folders.iter().map(String::as_str).collect();
        let dropped: BTreeSet<String> = cur
            .folders
            .keys()
            .map(String::as_str)
            .chain(known_folders.iter().copied())
            .filter(|f| !wanted.contains(f))
            .map(str::to_string)
            .collect();
        if !dropped.is_empty() {
            for folder in &dropped {
                let prefix = format!("{folder}/");
                for key in known.keys().filter(|k| k.as_str().starts_with(&prefix)) {
                    sink.send(Change::Delete(key.clone())).await?;
                }
                cur.folders.remove(folder);
            }
            sink.checkpoint(cur.with(&skips)).await?;
        }
        // Indexed mail but no cursor state: a reconciling full listing.
        for folder in &folders {
            if !cur.folders.contains_key(folder) && known_folders.contains(folder.as_str()) {
                tracing::info!(source = %self.ctx.source, folder = %folder, "outlook cursor reset; reconciling resync");
                cur.folders.insert(
                    folder.clone(),
                    FolderState {
                        resync: true,
                        reconcile: true,
                        ..Default::default()
                    },
                );
            }
        }
        drop(known);

        for folder in &folders {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.sync_folder(folder, &mut cur, &mut skips, sink, cancel)
                .await?;
            sink.checkpoint(cur.with(&skips)).await?;
        }
        self.retry_skipped(&wanted, &mut skips, sink, cancel)
            .await?;
        Ok(cur.with(&skips))
    }
}

impl OutlookConnector {
    fn window_start(&self) -> i64 {
        OffsetDateTime::now_utc().unix_timestamp() - i64::from(self.settings.window_days) * 86_400
    }

    fn initial_url(&self, folder: &str, after: i64) -> Result<url::Url> {
        let since = OffsetDateTime::from_unix_timestamp(after)
            .ok()
            .and_then(|t| t.format(&Rfc3339).ok())
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".into());
        let filter = format!("receivedDateTime ge {since}");
        self.api.url(
            &["me", "mailFolders", folder, "messages", "delta"],
            &[("$select", SELECT), ("$filter", &filter)],
        )
    }

    async fn sync_folder(
        &self,
        folder: &str,
        cur: &mut Cursor,
        skips: &mut Skips,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut state = cur.folders.get(folder).cloned().unwrap_or_default();
        if state.resync && state.after.is_some() && state.next_link.is_some() {
            // Pages listed before the interruption are not remembered: list again from the
            // start (same window) rather than take their messages for missing.
            state.next_link = None;
        }
        let known = self.ctx.known.versions().await?;
        // Keys listed (in scope) during a full listing, with the version sent or found.
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut restarts = 0u32;
        let mut pages = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            pages += 1;
            if pages > MAX_PAGES {
                return Err(Error::Invalid(format!(
                    "outlook folder `{folder}`: delta listing does not end"
                )));
            }
            if state.after.is_none() && state.delta_link.is_none() && state.next_link.is_none() {
                state.after = Some(self.window_start());
            }
            let full = state.after.is_some();
            let url = match (&state.next_link, &state.delta_link) {
                (Some(n), _) => self.api.link(n)?,
                (None, Some(d)) if !full => self.api.link(d)?,
                _ => self.initial_url(folder, state.after.unwrap_or(0))?,
            };
            let get = Get {
                prefer: &[PAGE_SIZE, IMMUTABLE_IDS],
                ..Default::default()
            };
            let resp = self.api.get(&url, get, cancel).await?;
            if needs_resync(&resp) {
                if restarts >= MAX_RESTARTS {
                    return Err(Error::Other(format!(
                        "outlook folder `{folder}`: delta token keeps expiring"
                    )));
                }
                tracing::info!(source = %self.ctx.source, folder, "outlook delta token expired; resyncing");
                restarts += 1;
                state = FolderState {
                    resync: true,
                    reconcile: state.reconcile,
                    ..Default::default()
                };
                seen.clear();
                continue;
            }
            match resp.status.as_u16() {
                200 => {}
                404 => {
                    return Err(Error::NotFound(format!("mail folder `{folder}` not found")));
                }
                _ => return Err(status_error("outlook messages delta", &resp)),
            }
            let page: DeltaPage = GraphApi::json(&resp, "outlook messages delta")?;
            // Links are checked before anything is applied or stored.
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
                    "outlook delta page has neither nextLink nor deltaLink".into(),
                ));
            }
            self.apply_page(folder, page.value, &known, &mut seen, skips, sink, cancel)
                .await?;

            if let Some(n) = next {
                state.next_link = Some(n);
                cur.folders.insert(folder.to_string(), state.clone());
                sink.checkpoint(cur.with(skips)).await?;
                continue;
            }
            // Last page: check unlisted messages (a fresh resync's, or those carried over).
            let mut verify = std::mem::take(&mut state.verify);
            if full && state.resync {
                let known = self.ctx.known.versions().await?;
                let after = state.after.unwrap_or(i64::MIN);
                let missing = missing_candidates(folder, &known, &seen, after);
                if state.reconcile {
                    // Out of scope after a settings change (e.g. a shorter window): drop them.
                    for key in missing.out_of_window {
                        sink.send(Change::Delete(ItemKey::new(key))).await?;
                    }
                }
                verify = missing.check;
            } else if !verify.is_empty() {
                let known = self.ctx.known.versions().await?;
                verify.retain(|k| known.contains_key(&ItemKey::new(k.as_str())));
            }
            let verify = self.verify_batch(folder, verify, sink, cancel).await?;
            cur.folders.insert(
                folder.to_string(),
                FolderState {
                    delta_link: delta,
                    verify,
                    ..Default::default()
                },
            );
            return Ok(());
        }
    }

    // The pass state is threaded through explicitly (cursor-held skips, the listing's seen set).
    #[allow(clippy::too_many_arguments)]
    async fn apply_page(
        &self,
        folder: &str,
        items: Vec<Value>,
        known: &HashMap<ItemKey, Option<String>>,
        seen: &mut HashMap<String, String>,
        skips: &mut Skips,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut steps = Vec::new();
        for item in items {
            // A non-object entry would deserialize from its array form: skip it.
            if !item.is_object() {
                continue;
            }
            let removed = is_removed(&item);
            let Ok(msg) = serde_json::from_value::<Message>(item) else {
                tracing::debug!(source = %self.ctx.source, "skipping malformed message entry");
                continue;
            };
            if !valid_message_id(&msg.id) {
                continue;
            }
            let key = format!("{folder}/{}", msg.id);
            if removed || msg.is_draft == Some(true) {
                skips.resolved(&key);
                // Indexed before this pass, or upserted earlier in it.
                let listed = seen.remove(&key).is_some();
                if listed || known.contains_key(&ItemKey::new(key.as_str())) {
                    steps.push(Step::Delete(key));
                }
                continue;
            }
            let Some(version) = msg.received.as_deref().and_then(parse_rfc3339).map(version) else {
                tracing::debug!(source = %self.ctx.source, "skipping message without a date");
                continue;
            };
            let unchanged = seen.get(&key) == Some(&version)
                || known
                    .get(&ItemKey::new(key.as_str()))
                    .is_some_and(|v| v.as_deref() == Some(version.as_str()));
            seen.insert(key.clone(), version);
            if unchanged {
                skips.resolved(&key);
            } else {
                steps.push(Step::Fetch(key, Box::new(msg)));
            }
        }
        self.run_steps(steps, skips, sink, cancel).await
    }

    /// Apply deletes and fetch messages (a few at a time), in order. A message that cannot be
    /// read is kept in `skips` for a later pass.
    async fn run_steps(
        &self,
        steps: Vec<Step>,
        skips: &mut Skips,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut results = futures::stream::iter(steps)
            .map(|step| async move {
                match step {
                    Step::Delete(key) => {
                        let change = Change::Delete(ItemKey::new(key.as_str()));
                        Ok((key, false, Fetched::Change(Some(change))))
                    }
                    Step::Fetch(key, msg) => {
                        let out = self.fetch(&key, *msg, cancel).await?;
                        Ok::<_, Error>((key, true, out))
                    }
                }
            })
            .buffered(FETCH_CONCURRENCY);
        while let Some(r) = results.next().await {
            let (key, fetch, out) = r?;
            match out {
                Fetched::Change(change) => {
                    if fetch {
                        skips.fetched(&key);
                    } else {
                        skips.resolved(&key);
                    }
                    if let Some(change) = change {
                        sink.send(change).await?;
                    }
                }
                Fetched::Failed(reason) => {
                    skips.failed(self.ctx.source.as_str(), &key, &reason)?;
                }
            }
        }
        Ok(())
    }

    /// Fetch again the messages earlier passes could not read. One gone meanwhile is deleted.
    async fn retry_skipped(
        &self,
        wanted: &HashSet<&str>,
        skips: &mut Skips,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut steps = Vec::new();
        for key in skips.pending() {
            let Some((folder, id)) = key.split_once('/') else {
                skips.resolved(&key);
                continue;
            };
            if !wanted.contains(folder) || !valid_message_id(id) {
                skips.resolved(&key);
                continue;
            }
            let url = self.api.url(
                &["me", "messages", id],
                &[("$select", &format!("{SELECT},parentFolderId"))],
            )?;
            let get = Get {
                prefer: &[IMMUTABLE_IDS],
                ..Default::default()
            };
            match self.api.get_item(&url, get, cancel).await? {
                ItemGet::Done(resp) if matches!(resp.status.as_u16(), 404 | 410) => {
                    steps.push(Step::Delete(key));
                }
                ItemGet::Done(resp) if resp.status.as_u16() == 200 => {
                    match GraphApi::json::<Message>(&resp, "outlook messages.get") {
                        Ok(msg) if msg.is_draft == Some(true) => steps.push(Step::Delete(key)),
                        Ok(msg) if msg.received.as_deref().and_then(parse_rfc3339).is_some() => {
                            steps.push(Step::Fetch(key, Box::new(msg)));
                        }
                        _ => skips.failed(self.ctx.source.as_str(), &key, "unreadable message")?,
                    }
                }
                ItemGet::Done(resp) => {
                    let e = status_error("outlook messages.get", &resp);
                    skips.failed(self.ctx.source.as_str(), &key, &e.to_string())?;
                }
                ItemGet::TooLarge => skips.resolved(&key),
                ItemGet::Failed(reason) => skips.failed(self.ctx.source.as_str(), &key, &reason)?,
            }
        }
        self.run_steps(steps, skips, sink, cancel).await
    }

    /// `$value` → a `message/rfc822` document. A message gone meanwhile becomes a delete; an
    /// oversized or empty one is skipped; one Graph cannot serve now is [`Fetched::Failed`].
    async fn fetch(&self, key: &str, msg: Message, cancel: &CancellationToken) -> Result<Fetched> {
        let url = self.api.url(&["me", "messages", &msg.id, "$value"], &[])?;
        let get = Get {
            prefer: &[IMMUTABLE_IDS],
            max_body: MAX_MIME,
            raw: true,
            ..Default::default()
        };
        let resp = match self.api.get_item(&url, get, cancel).await? {
            ItemGet::Done(r) => r,
            ItemGet::TooLarge => {
                tracing::warn!(source = %self.ctx.source, "skipping oversized message");
                return Ok(Fetched::Change(None));
            }
            ItemGet::Failed(reason) => return Ok(Fetched::Failed(reason)),
        };
        match resp.status.as_u16() {
            200 => {}
            404 | 410 => return Ok(Fetched::Change(Some(Change::Delete(ItemKey::new(key))))),
            _ => {
                let e = status_error("outlook message $value", &resp);
                return Ok(Fetched::Failed(e.to_string()));
            }
        }
        if resp.body.is_empty() {
            tracing::warn!(source = %self.ctx.source, "skipping message with an empty MIME body");
            return Ok(Fetched::Change(None));
        }
        Ok(Fetched::Change(Some(Change::Upsert(document(
            key.to_string(),
            msg,
            resp.body,
        )))))
    }

    /// Check the first [`MAX_VERIFY`] keys of `missing` (in order) and delete the messages no
    /// longer in the folder; return the rest (capped at [`MAX_CARRY`]) for the next pass.
    async fn verify_batch(
        &self,
        folder: &str,
        mut missing: Vec<String>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>> {
        let prefix = format!("{folder}/");
        missing.retain(|k| k.starts_with(&prefix));
        if missing.is_empty() {
            return Ok(missing);
        }
        let rest = if missing.len() > MAX_VERIFY {
            let mut rest = missing.split_off(MAX_VERIFY);
            tracing::info!(
                source = %self.ctx.source,
                folder,
                "outlook: checking {MAX_VERIFY} unlisted messages now, {} on later passes",
                rest.len()
            );
            rest.truncate(MAX_CARRY);
            rest
        } else {
            Vec::new()
        };
        let folder_id = self.folder_id(folder, cancel).await?;
        let plen = prefix.len();
        let folder_id = folder_id.as_str();
        let mut checks = futures::stream::iter(missing)
            .map(|key: String| async move {
                let keep = self.still_in_folder(&key[plen..], folder_id, cancel).await;
                (key, keep)
            })
            .buffered(FETCH_CONCURRENCY);
        while let Some((key, keep)) = checks.next().await {
            if !keep? {
                sink.send(Change::Delete(ItemKey::new(key))).await?;
            }
        }
        Ok(rest)
    }

    /// The real id of a folder given by well-known name or id.
    async fn folder_id(&self, folder: &str, cancel: &CancellationToken) -> Result<String> {
        let url = self
            .api
            .url(&["me", "mailFolders", folder], &[("$select", "id")])?;
        let get = Get {
            prefer: &[IMMUTABLE_IDS],
            ..Default::default()
        };
        let resp = self.api.get(&url, get, cancel).await?;
        if resp.status.as_u16() != 200 {
            return Err(status_error("outlook mailFolders", &resp));
        }
        let info: FolderInfo = GraphApi::json(&resp, "outlook mailFolders")?;
        Ok(info.id)
    }

    async fn still_in_folder(
        &self,
        id: &str,
        folder_id: &str,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        if !valid_message_id(id) {
            return Ok(false);
        }
        let url = self.api.url(
            &["me", "messages", id],
            &[("$select", "id,parentFolderId,isDraft")],
        )?;
        let get = Get {
            prefer: &[IMMUTABLE_IDS],
            ..Default::default()
        };
        let resp = self.api.get(&url, get, cancel).await?;
        match resp.status.as_u16() {
            404 | 410 => Ok(false),
            200 => {
                let m: Message = GraphApi::json(&resp, "outlook messages.get")?;
                Ok(m.is_draft != Some(true)
                    && m.parent_folder_id.as_deref().is_none_or(|p| p == folder_id))
            }
            _ => Err(status_error("outlook messages.get", &resp)),
        }
    }
}

fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

fn version(received: OffsetDateTime) -> String {
    format!("r{}", received.unix_timestamp())
}

/// The received time in a version written by [`version`].
fn version_secs(v: Option<&str>) -> Option<i64> {
    v?.strip_prefix('r')?.parse().ok()
}

fn document(key: String, msg: Message, bytes: Vec<u8>) -> Document {
    let received = msg.received.as_deref().and_then(parse_rfc3339);
    let subject = msg
        .subject
        .as_deref()
        .map(|s| clean_line(s, 500))
        .filter(|s| !s.is_empty());
    let folder = key.split_once('/').map(|(f, _)| f.to_string());
    let from = msg.from.as_ref().map(|r| {
        json!({
            "name": r.email.name.as_deref().map(|n| clean_line(n, 300)),
            "address": r.email.address.as_deref().map(|a| clean_line(a, 320)),
        })
    });
    let mut doc = Document::new(key, "message/rfc822", Content::Bytes(bytes));
    doc.title = subject.clone();
    doc.version = received.map(version);
    doc.modified_at = received;
    doc.url = msg
        .web_link
        .filter(|l| l.starts_with("https://") && l.len() <= 4096);
    doc.metadata = json!({
        "folder": folder,
        "message_id": msg.id,
        "internet_message_id": msg.internet_message_id.as_deref().map(|s| clean_line(s, 1000)),
        "conversation_id": msg.conversation_id.as_deref().map(|s| clean_line(s, 1000)),
        "from": from,
        "subject": subject,
        "received": msg.received,
        "last_modified": msg.last_modified,
    });
    doc
}

/// Indexed messages of one folder a resync listing did not return.
struct Missing {
    /// Inside the window (or of unknown date): gone unless a check says otherwise.
    check: Vec<String>,
    /// Known to be older than the window: expected to be missing from the listing.
    out_of_window: Vec<String>,
}

fn missing_candidates(
    folder: &str,
    known: &HashMap<ItemKey, Option<String>>,
    seen: &HashMap<String, String>,
    after: i64,
) -> Missing {
    let prefix = format!("{folder}/");
    let mut check = Vec::new();
    let mut out_of_window = Vec::new();
    for (k, v) in known {
        if !k.as_str().starts_with(&prefix) || seen.contains_key(k.as_str()) {
            continue;
        }
        let in_window =
            version_secs(v.as_deref()).is_none_or(|s| s >= after.saturating_sub(WINDOW_SLACK_SECS));
        if in_window {
            check.push(k.as_str().to_string());
        } else {
            out_of_window.push(k.as_str().to_string());
        }
    }
    check.sort();
    out_of_window.sort();
    Missing {
        check,
        out_of_window,
    }
}

/// Graph message ids are URL-safe base64 (`-`, `_`, `=`), sometimes with `+` or `/` in
/// non-immutable form; ids with `/` are refused since they cannot be keyed or put in a path.
fn valid_message_id(id: &str) -> bool {
    valid_folder_id(id)
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn cursor_shape() {
        let mut c = Cursor::default();
        c.folders.insert(
            "inbox".into(),
            FolderState {
                delta_link: Some("https://graph.microsoft.com/v1.0/x".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            c.to_value(),
            json!({"v": 1, "folders": {"inbox": {"delta_link": "https://graph.microsoft.com/v1.0/x"}}})
        );
        let back: Cursor = serde_json::from_value(c.to_value()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn versions_carry_the_received_time() {
        let t = parse_rfc3339("2024-05-01T10:00:00Z").unwrap();
        assert_eq!(version(t), "r1714557600");
        assert_eq!(version_secs(Some("r1714557600")), Some(1_714_557_600));
        assert_eq!(version_secs(Some("x")), None);
        assert_eq!(version_secs(None), None);
    }

    #[test]
    fn missing_candidates_split_by_window() {
        let after = 1_700_000_000;
        let known: HashMap<ItemKey, Option<String>> = [
            ("inbox/a", Some(format!("r{}", after + 10))),
            ("inbox/old", Some(format!("r{}", after - 10 * 86_400))),
            ("inbox/seen", Some(format!("r{}", after + 10))),
            ("inbox/legacy", None),
            ("sentitems/x", Some(format!("r{}", after + 10))),
        ]
        .into_iter()
        .map(|(k, v)| (ItemKey::new(k), v))
        .collect();
        let seen: HashMap<String, String> = [("inbox/seen".to_string(), "r1".to_string())].into();
        let m = missing_candidates("inbox", &known, &seen, after);
        assert_eq!(m.check, vec!["inbox/a", "inbox/legacy"]);
        assert_eq!(m.out_of_window, vec!["inbox/old"]);
    }

    #[test]
    fn ids_are_validated() {
        assert!(valid_message_id("AAMkAGI2THVSAAA="));
        assert!(valid_message_id("AQMkADAw-_AAA"));
        assert!(!valid_message_id("../x"));
        assert!(!valid_message_id("a/b"));
        assert!(!valid_message_id("..") && !valid_message_id("."));
        assert!(!valid_message_id(""));
    }
}
