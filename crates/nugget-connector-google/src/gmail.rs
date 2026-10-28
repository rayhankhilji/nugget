//! `gmail`: Gmail over the REST API with `gmail.readonly`.
//!
//! Cursor (JSON, tagged by `phase`):
//! - `{"phase":"backfill","history_id":"<id>","after":<unix secs>,"page_token":"<tok>"|null,"resync":bool}`
//!   while the newest-first backfill runs (`history_id` is the mailbox history id taken *before*
//!   listing, so nothing that arrives during the backfill is missed);
//! - `{"phase":"incremental","history_id":"<id>"}` afterwards.

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

use crate::api::{GoogleApi, OAuthSettings, api_url};
use crate::settings::GmailSettings;
use crate::text::mail_headers;
use crate::{GoogleEndpoints, RetryPolicy};

pub(crate) const KIND: &str = "gmail";
const FEATURE: &str = "sync:gmail";
/// Messages per `messages.list` page.
const LIST_PAGE: &str = "100";
/// History records per `history.list` page.
const HISTORY_PAGE: &str = "500";
/// Concurrent `messages.get` calls (Gmail allows ~50/s per user; stay well below).
const FETCH_CONCURRENCY: usize = 4;
/// On resync, at most this many indexed-but-unlisted messages are checked for deletion.
const MAX_VERIFY: usize = 500;
/// Labels that never get indexed (unless `include_spam_trash`, for the last two).
const DRAFT: &str = "DRAFT";
const HIDDEN: &[&str] = &["SPAM", "TRASH"];
/// Label changes too frequent and low-value to justify re-fetching a message.
const IGNORED_LABEL_CHANGES: &[&str] = &["UNREAD"];

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
    },
    Incremental {
        history_id: String,
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
        let settings = GmailSettings::parse(settings)?;
        let api = GoogleApi::new(
            FEATURE,
            nugget_oauth::google::SCOPE_GMAIL_READONLY,
            OAuthSettings {
                client_id: settings.client_id.clone(),
                client_secret: settings.client_secret.clone(),
                login_hint: settings.account_hint.clone(),
            },
            &self.endpoints,
            self.retry,
            ctx.clone(),
        )?;
        Ok(Arc::new(GmailConnector {
            api,
            base: self.endpoints.gmail_api.clone(),
            settings,
            ctx,
        }))
    }
}

/// One Gmail account.
pub struct GmailConnector {
    api: GoogleApi,
    base: String,
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

enum HistoryOutcome {
    Done(Cursor),
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
                let c = self.start_backfill(false, cancel).await?;
                sink.checkpoint(c.to_value()).await?;
                c
            }
        };
        loop {
            match cur {
                Cursor::Backfill { .. } => {
                    return Ok(self.backfill(cur, sink, cancel).await?.to_value());
                }
                Cursor::Incremental { ref history_id } => {
                    match self.history(history_id, sink, cancel).await? {
                        HistoryOutcome::Done(c) => return Ok(c.to_value()),
                        HistoryOutcome::Expired => {
                            tracing::info!(source = %self.ctx.source, "gmail history id expired; resyncing");
                            cur = self.start_backfill(true, cancel).await?;
                            sink.checkpoint(cur.to_value()).await?;
                        }
                    }
                }
            }
        }
    }
}

impl GmailConnector {
    fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<url::Url> {
        let mut all = vec!["users", "me"];
        all.extend_from_slice(segments);
        api_url(&self.base, &all, query)
    }

    async fn start_backfill(&self, resync: bool, cancel: &CancellationToken) -> Result<Cursor> {
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
        } = cur
        else {
            return Ok(cur);
        };
        let known: HashSet<ItemKey> = self.ctx.known.versions().await?.into_keys().collect();
        let mut seen: HashSet<String> = HashSet::new();
        let q = format!("after:{after}");
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
                .filter(|id| !known.contains(&ItemKey::new(id.as_str())))
                .cloned()
                .collect();
            let mut fetched = futures::stream::iter(todo)
                .map(|id: String| async move { self.fetch(&id, cancel).await })
                .buffered(FETCH_CONCURRENCY);
            while let Some(doc) = fetched.next().await {
                if let Some(doc) = doc? {
                    sink.send(Change::Upsert(doc)).await?;
                }
            }
            drop(fetched);
            match list.next_page_token.filter(|t| !t.is_empty()) {
                Some(t) => {
                    page_token = Some(t);
                    let c = Cursor::Backfill {
                        history_id: history_id.clone(),
                        after,
                        page_token: page_token.clone(),
                        resync,
                    };
                    sink.checkpoint(c.to_value()).await?;
                }
                None => break,
            }
        }
        if resync {
            let missing: Vec<String> = known
                .iter()
                .map(|k| k.as_str().to_string())
                .filter(|k| !seen.contains(k))
                .collect();
            self.verify_missing(missing, sink, cancel).await?;
        }
        Ok(Cursor::Incremental { history_id })
    }

    /// After a resync: indexed messages the listing did not return are either older than the
    /// window or gone. Check a bounded number of them and delete the gone ones.
    async fn verify_missing(
        &self,
        mut missing: Vec<String>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if missing.len() > MAX_VERIFY {
            tracing::info!(
                source = %self.ctx.source,
                "gmail resync: checking {MAX_VERIFY} of {} unlisted messages",
                missing.len()
            );
            missing.sort();
            missing.truncate(MAX_VERIFY);
        }
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
        Ok(())
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
        let resp = match self.api.get(&url, cancel).await {
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
        let version = msg.history_id.clone().or_else(|| msg.internal_date.clone());
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
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<HistoryOutcome> {
        let mut page_token: Option<String> = None;
        let mut last_record = start.to_string();
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
            self.apply(plan, sink, cancel).await?;
            match page.next_page_token.filter(|t| !t.is_empty()) {
                Some(t) => {
                    page_token = Some(t);
                    let c = Cursor::Incremental {
                        history_id: last_record.clone(),
                    };
                    sink.checkpoint(c.to_value()).await?;
                }
                None => {
                    let history_id = page
                        .history_id
                        .filter(|h| !h.is_empty())
                        .unwrap_or(last_record);
                    return Ok(HistoryOutcome::Done(Cursor::Incremental { history_id }));
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
                match action {
                    Action::Delete => Ok((id, None)),
                    Action::Fetch => self.fetch(&id, cancel).await.map(|d| (id, d)),
                }
            })
            .buffered(FETCH_CONCURRENCY);
        while let Some(r) = results.next().await {
            let change = match r? {
                (_, Some(doc)) => Change::Upsert(doc),
                // Deleted, gone since, or moved to spam/trash/drafts: not in the index.
                (id, None) => Change::Delete(ItemKey::new(id)),
            };
            sink.send(change).await?;
        }
        Ok(())
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
        };
        let v = c.to_value();
        assert_eq!(v["phase"], "backfill");
        assert_eq!(serde_json::from_value::<Cursor>(v).unwrap(), c);
        let v = Cursor::Incremental {
            history_id: "10".into(),
        }
        .to_value();
        assert_eq!(v, json!({"phase": "incremental", "history_id": "10"}));
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
