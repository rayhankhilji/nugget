//! `gmail`: Gmail over the REST API with `gmail.readonly`.
//!
//! Cursor (JSON, tagged by `phase`):
//! - `{"phase":"backfill","history_id":"<id>","after":<unix secs>,"page_token":"<tok>"|null,"resync":bool,"reconcile":bool}`
//!   while the newest-first backfill runs (`history_id` is the mailbox history id taken *before*
//!   listing, so nothing that arrives during the backfill is missed);
//! - `{"phase":"incremental","history_id":"<id>","verify":["<id>",...]}` afterwards (`verify`,
//!   omitted when empty, holds indexed messages a resync did not list and that are still to be
//!   checked, [`MAX_VERIFY`] per pass).
//!
//! A pass without a cursor while items are already indexed (a settings update resets the
//! cursor) is a *reconciling* backfill: indexed messages the listing does not return are
//! deleted when they are older than the (possibly new) window, and checked otherwise, so mail
//! that is now excluded (spam, trash), out of the window or another account's goes away. A
//! plain resync (expired history id) checks only messages inside the window: older ones are
//! expected to be missing from the listing and stay. A resync interrupted part-way restarts its listing
//! (cheap: only unknown messages are fetched), so no message is wrongly taken as missing.
//!
//! Versions are `<historyId>@<internalDate ms>`, so the window test needs no extra request.
//!
//! One bad message never stops a pass: a malformed or mismatched `messages.get` answer is
//! skipped, and a message whose fetch keeps failing (5xx or network after the in-pass
//! retries) is carried in the cursor as `"refetch":["<id>",...]` (either phase, omitted when
//! empty) and fetched again on the next pass. Only [`MAX_FAILURE_STREAK`] failures in a row
//! (Google is down, not one message) end the pass with the error. Messages larger than
//! [`MAX_MESSAGE_BYTES`] are skipped.
//!
//! Scopes: `gmail.readonly` (read messages and history; the default), or `gmail.modify` only
//! when the source opts into `allow_modify` (archiving from Nugget). Nothing else is requested.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
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

use crate::api::{GoogleApi, OAuthSettings, PageGuard, api_url};
use crate::settings::GmailSettings;
use crate::text::mail_headers;
use crate::{GoogleEndpoints, RetryPolicy};

pub(crate) const KIND: &str = "gmail";
const FEATURE: &str = "sync:gmail";
/// Egress-ledger feature for changes made on the user's behalf ([`GmailConnector::archive`]).
pub const ACTION_FEATURE: &str = "action:gmail";
/// Messages per `messages.list` page.
const LIST_PAGE: &str = "100";
/// History records per `history.list` page.
const HISTORY_PAGE: &str = "500";
/// Concurrent `messages.get` calls (Gmail allows ~50/s per user; stay well below).
const FETCH_CONCURRENCY: usize = 4;
/// At most this many indexed-but-unlisted messages are checked per pass; the rest wait in the
/// cursor for the next pass.
const MAX_VERIFY: usize = 500;
/// Longest list of messages carried in the cursor for later checks.
const MAX_CARRY: usize = 20_000;
/// Slack on the window boundary when deciding whether an unlisted message should have been
/// listed (Gmail's `after:` and `internalDate` may disagree by a time zone).
const WINDOW_SLACK_SECS: i64 = 86_400;
/// Labels that never get indexed (unless `include_spam_trash`, for the last two).
const DRAFT: &str = "DRAFT";
const HIDDEN: &[&str] = &["SPAM", "TRASH"];
/// Label changes too frequent and low-value to justify re-fetching a message.
const IGNORED_LABEL_CHANGES: &[&str] = &["UNREAD"];
/// Largest `messages.get?format=raw` response downloaded; larger messages are skipped.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Consecutive per-message failures after which the pass gives up (an outage, not one item).
const MAX_FAILURE_STREAK: u32 = 8;

/// Gmail's `raw` is base64url; padding varies.
const B64URL: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Cursor {
    Backfill {
        history_id: String,
        after: i64,
        #[serde(default)]
        page_token: Option<String>,
        #[serde(default)]
        resync: bool,
        /// Check every indexed message the listing misses, not only those inside the window.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reconcile: bool,
        /// Messages whose fetch failed, to fetch again.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        refetch: Vec<String>,
    },
    Incremental {
        history_id: String,
        /// Unlisted messages still to be checked after a resync, most likely deleted first.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        verify: Vec<String>,
        /// Messages whose fetch failed, to fetch again.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        refetch: Vec<String>,
    },
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Factory for `gmail` sources.
#[derive(Debug, Clone, Default)]
pub struct GmailFactory {
    endpoints: GoogleEndpoints,
    retry: RetryPolicy,
    /// Test override of [`MAX_MESSAGE_BYTES`].
    max_message_bytes: Option<usize>,
}

impl GmailFactory {
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

    #[cfg(test)]
    pub(crate) fn with_max_message_bytes(mut self, max: usize) -> Self {
        self.max_message_bytes = Some(max);
        self
    }
}

impl ConnectorFactory for GmailFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "Gmail",
            auth: AuthKind::OAuth,
            settings_schema: GmailSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        GmailSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        Ok(Arc::new(self.connector(settings, ctx)?))
    }
}

impl GmailFactory {
    /// Like [`ConnectorFactory::build`], typed (for [`GmailConnector::archive`]).
    pub fn connector(&self, settings: &Value, ctx: ConnectorContext) -> Result<GmailConnector> {
        let settings = GmailSettings::parse(settings)?;
        let scope = if settings.allow_modify {
            nugget_oauth::google::SCOPE_GMAIL_MODIFY
        } else {
            nugget_oauth::google::SCOPE_GMAIL_READONLY
        };
        let oauth = || OAuthSettings {
            client_id: settings.client_id.clone(),
            client_secret: settings.client_secret.clone(),
            login_hint: settings.account_hint.clone(),
        };
        let api = GoogleApi::new(
            FEATURE,
            scope,
            oauth(),
            &self.endpoints,
            self.retry,
            ctx.clone(),
        )?;
        let actions = if settings.allow_modify {
            Some(GoogleApi::new(
                ACTION_FEATURE,
                scope,
                oauth(),
                &self.endpoints,
                self.retry,
                ctx.clone(),
            )?)
        } else {
            None
        };
        Ok(GmailConnector {
            api,
            actions,
            base: self.endpoints.gmail_api.clone(),
            max_message_bytes: self.max_message_bytes.unwrap_or(MAX_MESSAGE_BYTES),
            settings,
            ctx,
        })
    }
}

/// One Gmail account.
pub struct GmailConnector {
    api: GoogleApi,
    /// Same account, metered as [`ACTION_FEATURE`]; only with `allow_modify`.
    actions: Option<GoogleApi>,
    base: String,
    max_message_bytes: usize,
    settings: GmailSettings,
    ctx: ConnectorContext,
}

impl std::fmt::Debug for GmailConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmailConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Profile {
    #[serde(rename = "historyId")]
    history_id: String,
}

#[derive(Deserialize)]
struct MessageList {
    #[serde(default)]
    messages: Vec<MessageRef>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct MessageRef {
    id: String,
    #[serde(default, rename = "labelIds")]
    label_ids: Vec<String>,
}

#[derive(Deserialize)]
struct Message {
    id: String,
    #[serde(default, rename = "threadId")]
    thread_id: Option<String>,
    #[serde(default, rename = "labelIds")]
    label_ids: Vec<String>,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default, rename = "historyId")]
    history_id: Option<String>,
    #[serde(default, rename = "internalDate")]
    internal_date: Option<String>,
    #[serde(default, rename = "sizeEstimate")]
    size_estimate: Option<u64>,
    #[serde(default)]
    raw: Option<String>,
}

#[derive(Deserialize)]
struct HistoryList {
    #[serde(default)]
    history: Vec<HistoryRecord>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(rename = "historyId")]
    history_id: Option<String>,
}

#[derive(Deserialize)]
struct HistoryRecord {
    id: String,
    #[serde(default, rename = "messagesAdded")]
    messages_added: Vec<HistoryMessage>,
    #[serde(default, rename = "messagesDeleted")]
    messages_deleted: Vec<HistoryMessage>,
    #[serde(default, rename = "labelsAdded")]
    labels_added: Vec<HistoryLabel>,
    #[serde(default, rename = "labelsRemoved")]
    labels_removed: Vec<HistoryLabel>,
}

#[derive(Deserialize)]
struct HistoryMessage {
    message: MessageRef,
}

#[derive(Deserialize)]
struct HistoryLabel {
    message: MessageRef,
    #[serde(default, rename = "labelIds")]
    label_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Fetch,
    Delete,
}

/// Per-message actions in first-seen order; the last action for a message wins.
#[derive(Default)]
struct Plan {
    order: Vec<String>,
    actions: HashMap<String, Action>,
}

impl Plan {
    fn set(&mut self, id: &str, action: Action) {
        if self.actions.insert(id.to_string(), action).is_none() {
            self.order.push(id.to_string());
        }
    }
}

/// Per-message failures of one pass (see the module docs).
#[derive(Default)]
struct Failures {
    /// Messages to fetch again (carried ones not yet retried, plus this pass's failures).
    refetch: Vec<String>,
    /// What the cursor carried in; fetched again at the end of this pass.
    carried: Vec<String>,
    /// Messages fetched in this pass (not fetched twice).
    tried: HashSet<String>,
    /// Failures in a row.
    streak: u32,
}

impl Failures {
    fn new(carried: Vec<String>) -> Self {
        Self {
            refetch: carried.clone(),
            carried,
            ..Default::default()
        }
    }

    fn carry(&mut self, id: &str) {
        if self.refetch.len() < MAX_CARRY && !self.refetch.iter().any(|r| r == id) {
            self.refetch.push(id.to_string());
        }
    }
}

/// What became of one message fetch.
enum Fetched {
    Doc(Document),
    /// Gone, or not indexed (draft, spam, trash, too large).
    NotIndexed,
    /// This message failed; the pass goes on.
    Failed,
}

enum HistoryOutcome {
    /// Caught up; the history id to resume from.
    Done(String),
    Expired,
}

#[async_trait]
impl Connector for GmailConnector {
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
        let parsed = cursor.and_then(|v| match serde_json::from_value::<Cursor>(v) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(source = %self.ctx.source, "unreadable gmail cursor ({e}); starting over");
                None
            }
        });
        let mut cur = match parsed {
            Some(c) => c,
            None => {
                // No cursor but indexed mail: a reconciling pass (see the module docs).
                let reconcile = !self.ctx.known.versions().await?.is_empty();
                if reconcile {
                    tracing::info!(source = %self.ctx.source, "gmail cursor reset; reconciling resync");
                }
                let c = self
                    .start_backfill(reconcile, reconcile, Vec::new(), cancel)
                    .await?;
                sink.checkpoint(c.to_value()).await?;
                c
            }
        };
        loop {
            match cur {
                Cursor::Backfill { .. } => {
                    return Ok(self.backfill(cur, sink, cancel).await?.to_value());
                }
                Cursor::Incremental {
                    ref history_id,
                    ref verify,
                    ref refetch,
                } => {
                    let mut fails = Failures::new(refetch.clone());
                    match self
                        .history(history_id, verify, &mut fails, sink, cancel)
                        .await?
                    {
                        HistoryOutcome::Done(history_id) => {
                            let verify = self.verify_carried(verify.clone(), sink, cancel).await?;
                            let refetch = self.refetch(fails, sink, cancel).await?;
                            return Ok(Cursor::Incremental {
                                history_id,
                                verify,
                                refetch,
                            }
                            .to_value());
                        }
                        HistoryOutcome::Expired => {
                            tracing::info!(source = %self.ctx.source, "gmail history id expired; resyncing");
                            cur = self
                                .start_backfill(true, false, fails.refetch, cancel)
                                .await?;
                            sink.checkpoint(cur.to_value()).await?;
                        }
                    }
                }
            }
        }
    }
}

impl GmailConnector {
    /// Whether the source allows changing labels (`allow_modify`), so [`archive`](Self::archive)
    /// can work.
    pub fn can_modify(&self) -> bool {
        self.actions.is_some()
    }

    /// Archive messages (by item key, the Gmail message id): `messages.modify` removing the
    /// `INBOX` label. Nothing is deleted. Messages that no longer exist are skipped. Needs
    /// `allow_modify` (else `Unsupported`) and a token granted `gmail.modify` (else Google
    /// answers 403, reported as `Unauthorized`: reconnect the account). Returns how many
    /// messages were archived.
    pub async fn archive(&self, ids: &[String], cancel: &CancellationToken) -> Result<usize> {
        let api = self.actions.as_ref().ok_or_else(|| {
            Error::Unsupported("archiving is off for this Gmail source (allow_modify)".into())
        })?;
        if let Some(bad) = ids.iter().find(|id| !valid_id(id)) {
            return Err(Error::Invalid(format!("not a Gmail message id: {bad}")));
        }
        let body = json!({"removeLabelIds": ["INBOX"]});
        let mut done = 0;
        for id in ids {
            let url = self.url(&["messages", id, "modify"], &[])?;
            let resp = api.post_json(&url, &body, cancel).await?;
            match resp.status.as_u16() {
                200..=299 => done += 1,
                404 => tracing::debug!(source = %self.ctx.source, "gmail archive: message gone"),
                s => return Err(Error::Other(format!("gmail messages.modify: HTTP {s}"))),
            }
        }
        Ok(done)
    }

    fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<url::Url> {
        let mut all = vec!["users", "me"];
        all.extend_from_slice(segments);
        api_url(&self.base, &all, query)
    }

    async fn start_backfill(
        &self,
        resync: bool,
        reconcile: bool,
        refetch: Vec<String>,
        cancel: &CancellationToken,
    ) -> Result<Cursor> {
        let resp = self.api.get(&self.url(&["profile"], &[])?, cancel).await?;
        if resp.status.as_u16() != 200 {
            return Err(Error::Other(format!("gmail profile: HTTP {}", resp.status)));
        }
        let profile: Profile = GoogleApi::json(&resp, "gmail profile")?;
        let after = OffsetDateTime::now_utc().unix_timestamp()
            - i64::from(self.settings.window_days) * 86_400;
        Ok(Cursor::Backfill {
            history_id: profile.history_id,
            after,
            page_token: None,
            resync,
            reconcile,
            refetch,
        })
    }

    /// Newest-first listing of the window; checkpoints after every page.
    async fn backfill(
        &self,
        cur: Cursor,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Cursor> {
        let Cursor::Backfill {
            history_id,
            after,
            mut page_token,
            resync,
            reconcile,
            refetch,
        } = cur
        else {
            return Ok(cur);
        };
        let mut fails = Failures::new(refetch);
        if resync && page_token.is_some() {
            // Pages listed before the interruption are not remembered: list again from the
            // start rather than take their messages for missing.
            page_token = None;
        }
        let known: HashMap<ItemKey, Option<String>> = self.ctx.known.versions().await?;
        let mut seen: HashSet<String> = HashSet::new();
        let q = format!("after:{after}");
        let mut pages = PageGuard::default();
        if let Some(t) = &page_token {
            pages.next(t, "gmail messages.list")?;
        }
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut query = vec![("maxResults", LIST_PAGE), ("q", q.as_str())];
            if self.settings.include_spam_trash {
                query.push(("includeSpamTrash", "true"));
            }
            if let Some(t) = &page_token {
                query.push(("pageToken", t));
            }
            let resp = self
                .api
                .get(&self.url(&["messages"], &query)?, cancel)
                .await?;
            if resp.status.as_u16() != 200 {
                return Err(Error::Other(format!(
                    "gmail messages.list: HTTP {}",
                    resp.status
                )));
            }
            let list: MessageList = GoogleApi::json(&resp, "gmail messages.list")?;
            let ids: Vec<String> = list
                .messages
                .into_iter()
                .map(|m| m.id)
                .filter(|id| valid_id(id))
                .collect();
            seen.extend(ids.iter().cloned());
            let todo: Vec<String> = ids
                .iter()
                .filter(|id| !known.contains_key(&ItemKey::new(id.as_str())))
                .cloned()
                .collect();
            let mut fetched = futures::stream::iter(todo)
                .map(|id: String| async move {
                    let r = self.fetch(&id, cancel).await;
                    (id, r)
                })
                .buffered(FETCH_CONCURRENCY);
            while let Some((id, r)) = fetched.next().await {
                if let Fetched::Doc(doc) = self.settle(&id, r, &mut fails)? {
                    sink.send(Change::Upsert(doc)).await?;
                }
            }
            drop(fetched);
            match list.next_page_token.filter(|t| !t.is_empty()) {
                Some(t) => {
                    pages.next(&t, "gmail messages.list")?;
                    page_token = Some(t);
                    let c = Cursor::Backfill {
                        history_id: history_id.clone(),
                        after,
                        page_token: page_token.clone(),
                        resync,
                        reconcile,
                        refetch: fails.refetch.clone(),
                    };
                    sink.checkpoint(c.to_value()).await?;
                }
                None => break,
            }
        }
        let mut verify = Vec::new();
        if resync {
            let missing = missing_candidates(&known, &seen, after);
            if reconcile {
                // Out of scope after a settings change (e.g. a shorter window): drop them.
                for id in missing.out_of_window {
                    sink.send(Change::Delete(ItemKey::new(id))).await?;
                }
            }
            verify = self.verify_batch(missing.check, sink, cancel).await?;
        }
        let refetch = self.refetch(fails, sink, cancel).await?;
        Ok(Cursor::Incremental {
            history_id,
            verify,
            refetch,
        })
    }

    /// Sort one message's fetch result: errors that concern only this message are logged and
    /// the pass goes on (transient ones carried for a later pass); rate limits, auth failures,
    /// cancellation and a run of [`MAX_FAILURE_STREAK`] failures end the pass.
    fn settle(
        &self,
        id: &str,
        r: Result<Option<Document>>,
        fails: &mut Failures,
    ) -> Result<Fetched> {
        fails.tried.insert(id.to_string());
        fails.refetch.retain(|r| r != id);
        let e = match r {
            Ok(Some(doc)) => {
                fails.streak = 0;
                return Ok(Fetched::Doc(doc));
            }
            Ok(None) => {
                fails.streak = 0;
                return Ok(Fetched::NotIndexed);
            }
            Err(e) => e,
        };
        let transient = match &e {
            Error::Network(_) => true,
            Error::Invalid(_) | Error::Other(_) => false,
            _ => return Err(e),
        };
        fails.streak += 1;
        if fails.streak >= MAX_FAILURE_STREAK {
            return Err(e);
        }
        if transient {
            tracing::warn!(source = %self.ctx.source, message = id, "gmail: fetch failed, retrying next pass: {e}");
            fails.carry(id);
        } else {
            tracing::warn!(source = %self.ctx.source, message = id, "gmail: skipping a bad message: {e}");
        }
        Ok(Fetched::Failed)
    }

    /// Fetch the messages carried from earlier failures; return those that failed again.
    async fn refetch(
        &self,
        mut fails: Failures,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>> {
        // Only what earlier passes carried in: this pass's own failures wait for the next one.
        let todo: Vec<String> = std::mem::take(&mut fails.carried)
            .into_iter()
            .filter(|id| valid_id(id) && !fails.tried.contains(id))
            .collect();
        let mut fetched = futures::stream::iter(todo)
            .map(|id: String| async move {
                let r = self.fetch(&id, cancel).await;
                (id, r)
            })
            .buffered(FETCH_CONCURRENCY);
        while let Some((id, r)) = fetched.next().await {
            match self.settle(&id, r, &mut fails)? {
                Fetched::Doc(doc) => sink.send(Change::Upsert(doc)).await?,
                Fetched::NotIndexed => sink.send(Change::Delete(ItemKey::new(id))).await?,
                Fetched::Failed => {}
            }
        }
        Ok(fails.refetch)
    }

    /// Continue the checks a resync left in the cursor, skipping messages no longer indexed.
    async fn verify_carried(
        &self,
        carried: Vec<String>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>> {
        if carried.is_empty() {
            return Ok(carried);
        }
        let known = self.ctx.known.versions().await?;
        let still: Vec<String> = carried
            .into_iter()
            .filter(|id| known.contains_key(&ItemKey::new(id.as_str())))
            .collect();
        self.verify_batch(still, sink, cancel).await
    }

    /// Check the first [`MAX_VERIFY`] of `missing` (in order) and delete the gone ones; return
    /// the rest (capped at [`MAX_CARRY`]) for the next pass.
    async fn verify_batch(
        &self,
        mut missing: Vec<String>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>> {
        let rest = if missing.len() > MAX_VERIFY {
            let mut rest = missing.split_off(MAX_VERIFY);
            tracing::info!(
                source = %self.ctx.source,
                "gmail: checking {MAX_VERIFY} unlisted messages now, {} on later passes",
                rest.len()
            );
            rest.truncate(MAX_CARRY);
            rest
        } else {
            Vec::new()
        };
        let mut checks = futures::stream::iter(missing)
            .map(|id: String| async move {
                let keep = self.still_indexable(&id, cancel).await;
                (id, keep)
            })
            .buffered(FETCH_CONCURRENCY);
        while let Some((id, keep)) = checks.next().await {
            if !keep? {
                sink.send(Change::Delete(ItemKey::new(id))).await?;
            }
        }
        Ok(rest)
    }

    async fn still_indexable(&self, id: &str, cancel: &CancellationToken) -> Result<bool> {
        if !valid_id(id) {
            return Ok(false);
        }
        let resp = self
            .api
            .get(
                &self.url(&["messages", id], &[("format", "minimal")])?,
                cancel,
            )
            .await?;
        match resp.status.as_u16() {
            404 | 410 => Ok(false),
            200 => {
                let m: MessageRef = GoogleApi::json(&resp, "gmail messages.get")?;
                Ok(self.indexable(&m.label_ids))
            }
            s => Err(Error::Other(format!("gmail messages.get: HTTP {s}"))),
        }
    }

    fn indexable(&self, labels: &[String]) -> bool {
        !labels.iter().any(|l| l == DRAFT)
            && (self.settings.include_spam_trash
                || !labels.iter().any(|l| HIDDEN.contains(&l.as_str())))
    }

    /// `messages.get?format=raw` → a document. `None` when the message is gone or not indexed
    /// (draft, spam, trash) or too large to download.
    async fn fetch(&self, id: &str, cancel: &CancellationToken) -> Result<Option<Document>> {
        let url = self.url(&["messages", id], &[("format", "raw")])?;
        let resp = match self
            .api
            .get_capped(&url, self.max_message_bytes, cancel)
            .await
        {
            Ok(r) => r,
            Err(Error::Invalid(m)) if m.contains("exceeds") => {
                tracing::warn!(source = %self.ctx.source, message = id, "skipping oversized message");
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        match resp.status.as_u16() {
            200 => {}
            404 | 410 => return Ok(None),
            s => return Err(Error::Other(format!("gmail messages.get: HTTP {s}"))),
        }
        let msg: Message = GoogleApi::json(&resp, "gmail messages.get")?;
        if msg.id != id {
            // The item key comes from the response: never let it name another message.
            return Err(Error::Invalid(format!(
                "gmail messages.get for {id} returned a different message"
            )));
        }
        if !self.indexable(&msg.label_ids) {
            return Ok(None);
        }
        let Some(raw) = msg.raw.as_deref() else {
            return Err(Error::Invalid(format!(
                "gmail message {id} has no raw body"
            )));
        };
        let bytes = match B64URL.decode(raw.trim()) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(source = %self.ctx.source, message = id, "undecodable raw message: {e}");
                return Ok(None);
            }
        };
        Ok(Some(self.document(msg, bytes)))
    }

    fn document(&self, msg: Message, bytes: Vec<u8>) -> Document {
        let (from, subject) = mail_headers(&bytes);
        let internal_ms = msg
            .internal_date
            .as_deref()
            .and_then(|d| d.parse::<i64>().ok());
        let modified_at = internal_ms.and_then(|ms| {
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
        });
        let version = match (&msg.history_id, internal_ms) {
            (Some(h), Some(ms)) => Some(format!("{h}@{ms}")),
            (Some(h), None) => Some(h.clone()),
            (None, _) => msg.internal_date.clone(),
        };
        let mut doc = Document::new(msg.id.clone(), "message/rfc822", Content::Bytes(bytes));
        doc.title = subject.clone();
        doc.version = version;
        doc.modified_at = modified_at;
        doc.url = Some(self.web_url(&msg.id));
        doc.metadata = json!({
            "labels": msg.label_ids,
            "thread_id": msg.thread_id,
            "from": from,
            "subject": subject,
            "snippet": msg.snippet.map(|s| crate::text::truncate_chars(&s, 500)),
            "history_id": msg.history_id,
            "internal_date": internal_ms,
            "size_estimate": msg.size_estimate,
        });
        doc
    }

    fn web_url(&self, id: &str) -> String {
        format!("https://mail.google.com/mail/u/0/#all/{id}")
    }

    /// Apply `history.list` deltas since `start`.
    async fn history(
        &self,
        start: &str,
        verify: &[String],
        fails: &mut Failures,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<HistoryOutcome> {
        let mut page_token: Option<String> = None;
        let mut last_record = start.to_string();
        let mut pages = PageGuard::default();
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut query = vec![
                ("startHistoryId", start),
                ("maxResults", HISTORY_PAGE),
                ("historyTypes", "messageAdded"),
                ("historyTypes", "messageDeleted"),
                ("historyTypes", "labelAdded"),
                ("historyTypes", "labelRemoved"),
            ];
            if let Some(t) = &page_token {
                query.push(("pageToken", t));
            }
            let resp = self
                .api
                .get(&self.url(&["history"], &query)?, cancel)
                .await?;
            match resp.status.as_u16() {
                200 => {}
                404 => return Ok(HistoryOutcome::Expired),
                s => return Err(Error::Other(format!("gmail history.list: HTTP {s}"))),
            }
            let page: HistoryList = GoogleApi::json(&resp, "gmail history.list")?;
            let plan = self.plan(&page.history);
            if let Some(r) = page.history.last() {
                last_record = r.id.clone();
            }
            self.apply(plan, fails, sink, cancel).await?;
            match page.next_page_token.filter(|t| !t.is_empty()) {
                Some(t) => {
                    pages.next(&t, "gmail history.list")?;
                    page_token = Some(t);
                    let c = Cursor::Incremental {
                        history_id: last_record.clone(),
                        verify: verify.to_vec(),
                        refetch: fails.refetch.clone(),
                    };
                    sink.checkpoint(c.to_value()).await?;
                }
                None => {
                    let history_id = page
                        .history_id
                        .filter(|h| !h.is_empty())
                        .unwrap_or(last_record);
                    return Ok(HistoryOutcome::Done(history_id));
                }
            }
        }
    }

    fn plan(&self, records: &[HistoryRecord]) -> Plan {
        let mut plan = Plan::default();
        let hides = |labels: &[String]| {
            !self.settings.include_spam_trash && labels.iter().any(|l| HIDDEN.contains(&l.as_str()))
        };
        let trivial = |labels: &[String]| {
            labels
                .iter()
                .all(|l| IGNORED_LABEL_CHANGES.contains(&l.as_str()))
        };
        for r in records {
            for m in &r.messages_added {
                if self.indexable(&m.message.label_ids) {
                    plan.set(&m.message.id, Action::Fetch);
                }
            }
            for m in &r.messages_deleted {
                plan.set(&m.message.id, Action::Delete);
            }
            for l in &r.labels_added {
                if hides(&l.label_ids) {
                    plan.set(&l.message.id, Action::Delete);
                } else if !trivial(&l.label_ids) && self.indexable(&l.message.label_ids) {
                    plan.set(&l.message.id, Action::Fetch);
                }
            }
            for l in &r.labels_removed {
                if (hides(&l.label_ids) || !trivial(&l.label_ids))
                    && self.indexable(&l.message.label_ids)
                {
                    plan.set(&l.message.id, Action::Fetch);
                }
            }
        }
        plan
    }

    async fn apply(
        &self,
        plan: Plan,
        fails: &mut Failures,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let steps: Vec<(String, Action)> = plan
            .order
            .into_iter()
            .filter(|id| valid_id(id))
            .map(|id| {
                let a = plan.actions[&id];
                (id, a)
            })
            .collect();
        let mut results = futures::stream::iter(steps)
            .map(|(id, action): (String, Action)| async move {
                let r = match action {
                    Action::Delete => Ok(None),
                    Action::Fetch => self.fetch(&id, cancel).await,
                };
                (id, r)
            })
            .buffered(FETCH_CONCURRENCY);
        while let Some((id, r)) = results.next().await {
            let change = match self.settle(&id, r, fails)? {
                Fetched::Doc(doc) => Change::Upsert(doc),
                // Deleted, gone since, or moved to spam/trash/drafts: not in the index.
                Fetched::NotIndexed => Change::Delete(ItemKey::new(id)),
                // Failed: the indexed version (if any) stays until the refetch.
                Fetched::Failed => continue,
            };
            sink.send(change).await?;
        }
        Ok(())
    }
}

/// The internal date (ms) in a version written by [`GmailConnector::document`].
fn version_date_ms(version: Option<&str>) -> Option<i64> {
    version?.split_once('@')?.1.parse().ok()
}

/// Indexed messages a resync listing did not return.
struct Missing {
    /// Inside the window (or of unknown date): deleted, excluded or another account's, unless
    /// a check says otherwise. Newest first by id (Gmail ids grow over time).
    check: Vec<String>,
    /// Known to be older than the window: expected to be missing from the listing.
    out_of_window: Vec<String>,
}

fn missing_candidates(
    known: &HashMap<ItemKey, Option<String>>,
    seen: &HashSet<String>,
    after: i64,
) -> Missing {
    let mut check = Vec::new();
    let mut out_of_window = Vec::new();
    for (k, v) in known {
        if seen.contains(k.as_str()) {
            continue;
        }
        let in_window = version_date_ms(v.as_deref())
            .is_none_or(|ms| ms.div_euclid(1000) >= after - WINDOW_SLACK_SECS);
        if in_window {
            check.push(k.as_str().to_string());
        } else {
            out_of_window.push(k.as_str().to_string());
        }
    }
    check.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| b.cmp(a)));
    out_of_window.sort();
    Missing {
        check,
        out_of_window,
    }
}

/// Gmail ids are short alphanumerics; anything else is not used in a URL path.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        let c = Cursor::Backfill {
            history_id: "9".into(),
            after: 1,
            page_token: Some("p".into()),
            resync: false,
            reconcile: true,
            refetch: Vec::new(),
        };
        let v = c.to_value();
        assert_eq!(v["phase"], "backfill");
        assert_eq!(serde_json::from_value::<Cursor>(v).unwrap(), c);
        let v = Cursor::Incremental {
            history_id: "10".into(),
            verify: Vec::new(),
            refetch: Vec::new(),
        }
        .to_value();
        assert_eq!(v, json!({"phase": "incremental", "history_id": "10"}));
        // Older cursors (no `verify`, no `reconcile`) still load.
        let old = json!({"phase": "backfill", "history_id": "1", "after": 5, "resync": true});
        assert!(matches!(
            serde_json::from_value::<Cursor>(old).unwrap(),
            Cursor::Backfill {
                reconcile: false,
                resync: true,
                ..
            }
        ));
    }

    #[test]
    fn missing_candidates_skip_old_mail_and_go_newest_first() {
        let after = 1_700_000_000;
        let in_ms = (after + 100) * 1000;
        let old_ms = (after - 10 * 86_400) * 1000;
        let known: HashMap<ItemKey, Option<String>> = [
            ("18a0000000000001", Some(format!("5@{in_ms}"))),
            ("18b0000000000002", Some(format!("6@{in_ms}"))),
            ("17f0000000000003", Some(format!("7@{old_ms}"))),
            ("190000000000004", Some("8".to_string())), // legacy version: date unknown
            ("18c0000000000005", Some(format!("9@{in_ms}"))),
        ]
        .into_iter()
        .map(|(k, v)| (ItemKey::new(k), v))
        .collect();
        let seen: HashSet<String> = ["18c0000000000005".to_string()].into();
        let m = missing_candidates(&known, &seen, after);
        assert_eq!(
            m.check,
            vec!["18b0000000000002", "18a0000000000001", "190000000000004"]
        );
        assert_eq!(m.out_of_window, vec!["17f0000000000003"]);
    }

    #[test]
    fn ids_are_validated() {
        assert!(valid_id("18c2f0a1b2c3d4e5"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"a".repeat(65)));
    }

    #[test]
    fn raw_decoding_accepts_padded_and_unpadded() {
        assert_eq!(B64URL.decode("aGk_").unwrap(), b"hi?");
        assert_eq!(B64URL.decode("aGk=").unwrap(), b"hi");
        assert_eq!(B64URL.decode("aGk").unwrap(), b"hi");
    }
}
