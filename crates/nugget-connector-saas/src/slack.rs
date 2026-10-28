//! `slack`: conversations read through the Slack Web API with the user's own token.
//!
//! **Documents.** Messages are grouped into *conversation windows*: one document per
//! conversation per UTC day (`<channel>/day/<YYYY-MM-DD>`, the top-level messages of that day)
//! and one per thread (`<channel>/thread/<ts>`, the parent and every reply). Author ids are
//! resolved to names (`users.info`, cached) and mrkdwn is rendered as plain text.
//!
//! **Cursor** (JSON): `{"v":1,"user_id":"U..","channels":{"<id>":<state>}}`, where `<state>` is
//! `{"latest":"<ts>","checked_at":<unix>,"backfill":{"from":<unix>,"before":<unix>}}`:
//! - `latest`: the newest message seen. Each pass probes `conversations.history` for anything
//!   newer (one request); when there is, the days from `latest`'s day to now are rebuilt.
//! - `checked_at`: every [`RECHECK_INTERVAL_SECS`] the last `recheck_days` are re-read in full,
//!   which picks up edits, deletions and new replies to recent threads (thread documents are
//!   refetched only when the parent's reply count or latest reply changed).
//! - `backfill`: a window rebuild in progress, newest day first. `before` moves down as days
//!   complete, so an interrupted backfill resumes where it stopped.
//!
//! A conversation without state (first run, or a reset cursor) gets a reconciling rebuild of
//! the whole window: indexed days and threads that no longer exist, or fall outside the
//! window, are deleted. Conversations no longer synced are dropped using the indexed keys.
//!
//! Rate limits: HTTP 429 and `ratelimited` honour `Retry-After`; requests are sequential, which
//! keeps a pass inside the Tier 3 (≈50/min) budget of `conversations.history` for internal apps.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::api::{self, Api, Outcome, classify_common, retry_after_ms};
use crate::auth::{Credential, Token};
use crate::settings::{self, MAX_WINDOW_DAYS};
use crate::text::{
    DAY_SECS, clean, clock, date, day_start, hash_hex, now_unix, one_line, parse_date, safe_id,
    truncate_chars,
};
use crate::{Endpoints, RetryPolicy, SLACK_KIND};

const FEATURE: &str = "sync:slack";
const SERVICE: &str = "Slack";
const PAGE: &str = "200";
/// Re-read the last `recheck_days` this often (edits, deletions, thread replies).
pub(crate) const RECHECK_INTERVAL_SECS: i64 = 6 * 3600;
const MAX_MESSAGE_CHARS: usize = 20_000;
const MAX_DOC_MESSAGES: usize = 2_000;
const MAX_DOC_CHARS: usize = 400_000;
const MAX_THREAD_MESSAGES: usize = 1_000;
/// Pages of one history range; a guard against a server that never stops paging.
const MAX_HISTORY_PAGES: usize = 5_000;
const MAX_LIST_PAGES: usize = 50;
const MAX_USER_LOOKUPS_PER_PASS: usize = 500;
const MAX_USERS_CACHED: usize = 50_000;
/// Subtypes that carry no conversation content.
const SKIP_SUBTYPES: &[&str] = &[
    "channel_join",
    "channel_leave",
    "group_join",
    "group_leave",
    "tombstone",
];
/// `ok:false` errors that mean the token is bad or lacks access.
const AUTH_ERRORS: &[&str] = &[
    "not_authed",
    "invalid_auth",
    "account_inactive",
    "token_revoked",
    "no_permission",
    "missing_scope",
    "not_allowed_token_type",
    "ekm_access_denied",
    "team_access_not_granted",
];
const TRANSIENT_ERRORS: &[&str] = &[
    "internal_error",
    "fatal_error",
    "service_unavailable",
    "request_timeout",
];

fn yes() -> bool {
    true
}
fn default_max_conversations() -> u32 {
    100
}
fn default_window() -> u32 {
    90
}
fn default_recheck() -> u32 {
    3
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlackSettings {
    #[serde(default)]
    token_env: Option<String>,
    /// Channel ids (`C…`, `G…`, `D…`) or names (`general`, `#general`). Empty: every DM and
    /// channel the user is a member of, up to `max_conversations`.
    #[serde(default)]
    conversations: Option<Vec<String>>,
    #[serde(default = "yes")]
    include_dms: bool,
    #[serde(default = "yes")]
    include_channels: bool,
    #[serde(default = "default_max_conversations")]
    max_conversations: u32,
    #[serde(default = "default_window")]
    window_days: u32,
    #[serde(default = "default_recheck")]
    recheck_days: u32,
}

impl SlackSettings {
    pub(crate) fn parse(v: &Value) -> Result<Self> {
        let s: Self = settings::parse(v)?;
        settings::check_token_env(s.token_env.as_deref())?;
        settings::check_list("conversations", s.conversations.as_deref(), 1000)?;
        settings::check_range("max_conversations", s.max_conversations, 1, 1000)?;
        settings::check_range("window_days", s.window_days, 1, MAX_WINDOW_DAYS)?;
        settings::check_range("recheck_days", s.recheck_days, 0, 30)?;
        if !s.include_dms && !s.include_channels && s.configured().is_none() {
            return Err(Error::Invalid(
                "settings: nothing to sync (include_dms and include_channels are both off)".into(),
            ));
        }
        Ok(s)
    }

    fn configured(&self) -> Option<Vec<String>> {
        self.conversations
            .as_ref()
            .filter(|c| !c.is_empty())
            .map(|c| c.iter().map(|s| s.trim().to_string()).collect())
    }

    pub(crate) fn schema() -> Value {
        settings::schema(
            "the User OAuth Token (xoxp-…) of your own Slack app.",
            json!({
                "conversations": {
                    "type": "array", "items": {"type": "string", "minLength": 1},
                    "title": "Conversations",
                    "description": "Channel ids (C…, G…, D…) or names (#general). Leave empty for your DMs and the channels you are in."
                },
                "include_dms": {"type": "boolean", "default": true, "title": "Direct messages",
                    "description": "With no conversations listed: include DMs and group DMs."},
                "include_channels": {"type": "boolean", "default": true, "title": "Channels",
                    "description": "With no conversations listed: include public and private channels you are a member of."},
                "max_conversations": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100,
                    "title": "Conversation limit"},
                "window_days": settings::window_prop(90, "messages"),
                "recheck_days": {"type": "integer", "minimum": 0, "maximum": 30, "default": 3,
                    "title": "Re-check recent days",
                    "description": "Days re-read every few hours to pick up edits, deletions and thread replies."}
            }),
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Range {
    from: i64,
    before: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ChanState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(default)]
    checked_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backfill: Option<Range>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default)]
    channels: BTreeMap<String, ChanState>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            v: 1,
            user_id: None,
            channels: BTreeMap::new(),
        }
    }
}

impl Cursor {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Factory for `slack` sources.
#[derive(Debug, Clone, Default)]
pub struct SlackFactory {
    endpoints: Endpoints,
    retry: RetryPolicy,
}

impl SlackFactory {
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

impl ConnectorFactory for SlackFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: SLACK_KIND,
            display_name: "Slack",
            auth: AuthKind::Password,
            settings_schema: SlackSettings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        SlackSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let settings = SlackSettings::parse(settings)?;
        let http = api::metered(ctx.egress.clone(), FEATURE)?;
        let cred = Arc::new(Credential::new(
            SERVICE,
            ctx.secrets.clone(),
            ctx.source.as_str(),
            settings.token_env.clone(),
            None,
        ));
        Ok(Arc::new(SlackConnector {
            api: Api::new(http, cred, self.retry, SERVICE),
            base: self.endpoints.slack_api.clone(),
            settings,
            ctx,
            users: Mutex::new(HashMap::new()),
        }))
    }
}

/// One Slack workspace, as one user.
pub struct SlackConnector {
    api: Api,
    base: String,
    settings: SlackSettings,
    ctx: ConnectorContext,
    /// User id → display name, kept across passes.
    users: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for SlackConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackConnector")
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

// ---- API types ----

#[derive(Deserialize, Default)]
#[serde(default)]
struct Envelope {
    ok: bool,
    error: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Meta {
    next_cursor: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AuthTest {
    url: Option<String>,
    team: Option<String>,
    user: Option<String>,
    user_id: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Conv {
    id: String,
    name: Option<String>,
    is_im: bool,
    is_mpim: bool,
    is_private: bool,
    is_archived: bool,
    user: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ConvList {
    #[serde(deserialize_with = "lenient")]
    channels: Vec<Conv>,
    response_metadata: Option<Meta>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ConvInfo {
    channel: Option<Conv>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Msg {
    ts: String,
    user: Option<String>,
    bot_id: Option<String>,
    username: Option<String>,
    bot_profile: Option<BotProfile>,
    text: String,
    subtype: Option<String>,
    thread_ts: Option<String>,
    reply_count: u32,
    latest_reply: Option<String>,
    edited: Option<Edited>,
    files: Vec<File>,
    attachments: Vec<Attachment>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct BotProfile {
    name: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Edited {
    ts: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct File {
    name: Option<String>,
    title: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Attachment {
    fallback: Option<String>,
    title: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct History {
    #[serde(deserialize_with = "lenient")]
    messages: Vec<Msg>,
    has_more: bool,
    response_metadata: Option<Meta>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UserInfo {
    user: Option<User>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct User {
    name: Option<String>,
    real_name: Option<String>,
    profile: Option<Profile>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Profile {
    display_name: Option<String>,
    real_name: Option<String>,
}

/// A list whose malformed entries are skipped instead of failing the whole page.
fn lenient<'de, D, T>(d: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let raw: Option<Vec<Value>> = Option::deserialize(d)?;
    Ok(raw
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

fn classify(resp: Response) -> Outcome {
    if !resp.status.is_success() {
        return classify_common(resp, SERVICE);
    }
    let env: Envelope = serde_json::from_slice(&resp.body).unwrap_or_default();
    if env.ok {
        return Outcome::Done(resp);
    }
    let code = env.error.unwrap_or_default();
    let code: String = code.chars().take(80).collect();
    match code.as_str() {
        "token_expired" => Outcome::Refresh,
        "ratelimited" | "rate_limited" => Outcome::Retry(Error::RateLimited {
            retry_after_ms: retry_after_ms(&resp.headers),
        }),
        c if AUTH_ERRORS.contains(&c) => {
            Outcome::Fatal(Error::Unauthorized(format!("Slack refused the token: {c}")))
        }
        c if TRANSIENT_ERRORS.contains(&c) => {
            Outcome::Retry(Error::Network(format!("Slack error: {c}")))
        }
        _ => Outcome::Done(resp),
    }
}

/// The resolved conversation being synced.
#[derive(Debug, Clone)]
struct Conversation {
    id: String,
    /// `#general`, `DM with Ada`, `Group DM (ada, bob)`.
    display: String,
    kind: &'static str,
}

/// Indexed keys of one conversation, by time.
#[derive(Default)]
struct KnownConv {
    days: BTreeMap<i64, ItemKey>,
    /// Thread start (Unix seconds) → (ts, key, version).
    threads: BTreeMap<i64, Vec<(String, ItemKey, Option<String>)>>,
}

/// What every document of a pass shares.
struct PassInfo {
    me: Option<String>,
    me_name: Option<String>,
    team: Option<String>,
    team_url: Option<String>,
}

/// One UTC day of top-level messages being collected.
struct DayBucket {
    day: i64,
    msgs: Vec<Msg>,
    dropped: usize,
}

#[async_trait]
impl Connector for SlackConnector {
    fn kind(&self) -> &'static str {
        SLACK_KIND
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
                    tracing::warn!(source = %self.ctx.source, "unreadable slack cursor ({e}); starting over");
                    None
                }
            })
            .unwrap_or_default();
        let who: AuthTest = self
            .call("auth.test", &[], cancel)
            .await?
            .map_err(|e| Error::Unauthorized(format!("Slack auth.test failed: {e}")))?;
        let me = who.user_id.filter(|u| safe_id(u, 32));
        if cur.user_id.is_some() && cur.user_id != me {
            tracing::warn!(source = %self.ctx.source, "slack token belongs to another user; resyncing");
            cur.channels.clear();
        }
        cur.user_id = me.clone();
        let me_name = match &me {
            Some(id) => {
                self.ensure_users(std::slice::from_ref(id), &mut 0, cancel)
                    .await?;
                self.user_name(id).or(who.user.clone())
            }
            None => who.user.clone(),
        };
        let info = PassInfo {
            me,
            me_name,
            team: who.team.map(|t| one_line(&t, 200)),
            team_url: who
                .url
                .filter(|u| {
                    u.starts_with("https://") && u.len() < 300 && !u.contains(char::is_whitespace)
                })
                .map(|u| if u.ends_with('/') { u } else { format!("{u}/") }),
        };

        let convs = self.conversations(cancel).await?;
        let known = self.ctx.known.versions().await?;
        let mut by_conv: HashMap<String, KnownConv> = HashMap::new();
        for (key, version) in &known {
            index_key(&mut by_conv, key, version);
        }
        drop(known);

        // Conversations no longer synced: drop their documents.
        let wanted: HashSet<&str> = convs.iter().map(|c| c.id.as_str()).collect();
        let dropped: BTreeSet<String> = cur
            .channels
            .keys()
            .chain(by_conv.keys())
            .filter(|c| !wanted.contains(c.as_str()))
            .cloned()
            .collect();
        if !dropped.is_empty() {
            for c in &dropped {
                if let Some(k) = by_conv.remove(c) {
                    for key in k.days.into_values() {
                        sink.send(Change::Delete(key)).await?;
                    }
                    for (_, key, _) in k.threads.into_values().flatten() {
                        sink.send(Change::Delete(key)).await?;
                    }
                }
                cur.channels.remove(c);
            }
            sink.checkpoint(cur.to_value()).await?;
        }

        let mut lookups = 0usize;
        for conv in &convs {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut known = by_conv.remove(&conv.id).unwrap_or_default();
            let r = self
                .sync_conversation(
                    conv,
                    &info,
                    &mut cur,
                    &mut known,
                    &mut lookups,
                    sink,
                    cancel,
                )
                .await;
            match r {
                Ok(()) => {}
                Err(Error::NotFound(m)) => {
                    tracing::warn!(source = %self.ctx.source, channel = %conv.id, "skipping slack conversation: {m}");
                    continue;
                }
                Err(e) => return Err(e),
            }
            sink.checkpoint(cur.to_value()).await?;
        }
        Ok(cur.to_value())
    }
}

/// Record an indexed key under its conversation (unknown shapes are ignored).
fn index_key(by_conv: &mut HashMap<String, KnownConv>, key: &ItemKey, version: &Option<String>) {
    let mut parts = key.as_str().splitn(3, '/');
    let (Some(chan), Some(kind), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return;
    };
    match kind {
        "day" => {
            if let Some(d) = parse_date(rest) {
                by_conv
                    .entry(chan.to_string())
                    .or_default()
                    .days
                    .insert(d, key.clone());
            }
        }
        "thread" => {
            if let Some(secs) = ts_secs(rest) {
                by_conv
                    .entry(chan.to_string())
                    .or_default()
                    .threads
                    .entry(secs)
                    .or_default()
                    .push((rest.to_string(), key.clone(), version.clone()));
            }
        }
        _ => {}
    }
}

/// Seconds of a Slack ts (`1700000000.000200`); `None` when malformed.
fn ts_secs(ts: &str) -> Option<i64> {
    if ts.len() > 32 {
        return None;
    }
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, ""));
    if secs.is_empty()
        || !secs.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    secs.parse().ok()
}

impl SlackConnector {
    /// Call a Web API method. `Ok(Err(code))` is an `ok:false` answer the caller interprets.
    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        query: &[(&str, &str)],
        cancel: &CancellationToken,
    ) -> Result<std::result::Result<T, String>> {
        let mut url = Url::parse(&self.base)
            .map_err(|e| Error::Invalid(format!("bad Slack API base: {e}")))?;
        url.path_segments_mut()
            .map_err(|_| Error::Invalid("bad Slack API base".into()))?
            .pop_if_empty()
            .push(method);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let resp = self
            .api
            .execute(cancel, classify, |http: &MeteredHttp, token: &Token| {
                http.request(Method::GET, url.as_str())
                    .bearer_auth(token.secret())
            })
            .await?;
        let env: Envelope = api::json(&resp, method)?;
        if !env.ok {
            return Ok(Err(env.error.unwrap_or_else(|| "unknown_error".into())));
        }
        api::json(&resp, method).map(Ok)
    }

    fn user_name(&self, id: &str) -> Option<String> {
        self.users.lock().ok()?.get(id).cloned()
    }

    /// Look up names of unknown users (bounded per pass; failures fall back to the id).
    async fn ensure_users(
        &self,
        ids: &[String],
        lookups: &mut usize,
        cancel: &CancellationToken,
    ) -> Result<()> {
        for id in ids {
            if !safe_id(id, 32) || self.user_name(id).is_some() {
                continue;
            }
            if *lookups >= MAX_USER_LOOKUPS_PER_PASS {
                break;
            }
            *lookups += 1;
            let name = match self
                .call::<UserInfo>("users.info", &[("user", id)], cancel)
                .await?
            {
                Ok(info) => info.user.and_then(|u| {
                    let p = u.profile.unwrap_or_default();
                    [p.display_name, p.real_name, u.real_name, u.name]
                        .into_iter()
                        .flatten()
                        .map(|n| one_line(&n, 80))
                        .find(|n| !n.is_empty())
                }),
                Err(_) => None,
            };
            if let Ok(mut users) = self.users.lock()
                && users.len() < MAX_USERS_CACHED
            {
                users.insert(id.clone(), name.unwrap_or_else(|| id.clone()));
            }
        }
        Ok(())
    }

    /// The conversations to sync, in order, capped at `max_conversations`.
    async fn conversations(&self, cancel: &CancellationToken) -> Result<Vec<Conversation>> {
        let max = self.settings.max_conversations as usize;
        let mut out: Vec<Conv> = Vec::new();
        match self.settings.configured() {
            Some(entries) => {
                let mut by_name: Option<Vec<Conv>> = None;
                for entry in entries {
                    let name = entry.trim_start_matches('#');
                    let looks_like_id = name.len() >= 9
                        && matches!(name.as_bytes()[0], b'C' | b'G' | b'D')
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
                    let found = if looks_like_id {
                        match self
                            .call::<ConvInfo>("conversations.info", &[("channel", name)], cancel)
                            .await?
                        {
                            Ok(info) => info.channel,
                            Err(code) => {
                                tracing::warn!(source = %self.ctx.source, channel = name, "slack conversation unavailable: {code}");
                                None
                            }
                        }
                    } else {
                        if by_name.is_none() {
                            let mut all = self
                                .list(
                                    "users.conversations",
                                    "public_channel,private_channel,mpim",
                                    usize::MAX,
                                    cancel,
                                )
                                .await?;
                            all.extend(
                                self.list(
                                    "conversations.list",
                                    "public_channel",
                                    usize::MAX,
                                    cancel,
                                )
                                .await?,
                            );
                            by_name = Some(all);
                        }
                        by_name
                            .as_ref()
                            .and_then(|all| all.iter().find(|c| c.name.as_deref() == Some(name)))
                            .cloned()
                    };
                    match found {
                        Some(c) if !out.iter().any(|o| o.id == c.id) => out.push(c),
                        Some(_) => {}
                        None => {
                            tracing::warn!(source = %self.ctx.source, conversation = name, "slack conversation not found");
                        }
                    }
                    if out.len() >= max {
                        break;
                    }
                }
            }
            None => {
                let mut types = Vec::new();
                if self.settings.include_dms {
                    types.extend(["im", "mpim"]);
                }
                if self.settings.include_channels {
                    types.extend(["public_channel", "private_channel"]);
                }
                out = self
                    .list("users.conversations", &types.join(","), max, cancel)
                    .await?;
            }
        }
        out.retain(|c| safe_id(&c.id, 32) && !c.is_archived);
        out.truncate(max);
        let dm_users: Vec<String> = out
            .iter()
            .filter(|c| c.is_im)
            .filter_map(|c| c.user.clone())
            .collect();
        self.ensure_users(&dm_users, &mut 0, cancel).await?;
        Ok(out
            .into_iter()
            .map(|c| {
                let name = c.name.as_deref().map(|n| one_line(n, 120));
                let (display, kind) = if c.is_im {
                    let who = c
                        .user
                        .as_deref()
                        .map(|u| self.user_name(u).unwrap_or_else(|| u.to_string()))
                        .unwrap_or_else(|| "someone".into());
                    (format!("DM with {who}"), "im")
                } else if c.is_mpim {
                    let members = name
                        .as_deref()
                        .unwrap_or_default()
                        .trim_start_matches("mpdm-")
                        .rsplit_once('-')
                        .map_or(String::new(), |(m, _)| {
                            m.split("--").collect::<Vec<_>>().join(", ")
                        });
                    (format!("Group DM ({members})"), "mpim")
                } else if c.is_private {
                    (
                        format!("#{}", name.unwrap_or_else(|| c.id.clone())),
                        "private_channel",
                    )
                } else {
                    (
                        format!("#{}", name.unwrap_or_else(|| c.id.clone())),
                        "channel",
                    )
                };
                Conversation {
                    id: c.id,
                    display,
                    kind,
                }
            })
            .collect())
    }

    async fn list(
        &self,
        method: &str,
        types: &str,
        max: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<Conv>> {
        let mut out = Vec::new();
        let mut next: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut q = vec![
                ("types", types),
                ("exclude_archived", "true"),
                ("limit", PAGE),
            ];
            if let Some(c) = &next {
                q.push(("cursor", c));
            }
            let page: ConvList = match self.call(method, &q, cancel).await? {
                Ok(p) => p,
                Err(code) => return Err(Error::Other(format!("Slack {method}: {code}"))),
            };
            out.extend(page.channels);
            if out.len() >= max {
                break;
            }
            match page
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty())
            {
                Some(c) => next = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    async fn sync_conversation(
        &self,
        conv: &Conversation,
        info: &PassInfo,
        cur: &mut Cursor,
        known: &mut KnownConv,
        lookups: &mut usize,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let now = now_unix();
        let window_from = day_start(now - i64::from(self.settings.window_days) * DAY_SECS);
        let mut state = cur.channels.get(&conv.id).cloned().unwrap_or_default();
        if state.latest.is_none() && state.backfill.is_none() {
            // First sync of this conversation, or a reset cursor: rebuild the whole window and
            // drop what lies outside it.
            state.backfill = Some(Range {
                from: window_from,
                before: now + 1,
            });
            state.checked_at = now;
            for key in known
                .days
                .range(..window_from)
                .map(|(_, k)| k.clone())
                .collect::<Vec<_>>()
            {
                sink.send(Change::Delete(key)).await?;
            }
            for (_, key, _) in known
                .threads
                .range(..window_from)
                .flat_map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
            {
                sink.send(Change::Delete(key)).await?;
            }
            known.days = known.days.split_off(&window_from);
            known.threads = known.threads.split_off(&window_from);
            cur.channels.insert(conv.id.clone(), state.clone());
        } else {
            let recheck =
                self.settings.recheck_days > 0 && now - state.checked_at >= RECHECK_INTERVAL_SECS;
            let from = if recheck {
                Some(
                    day_start(now - i64::from(self.settings.recheck_days) * DAY_SECS)
                        .max(window_from),
                )
            } else {
                match &state.latest {
                    Some(latest) if self.has_newer(&conv.id, latest, cancel).await? => {
                        ts_secs(latest).map(day_start)
                    }
                    _ => None,
                }
            };
            if let Some(from) = from {
                self.rebuild(
                    conv,
                    info,
                    from,
                    now + 1,
                    false,
                    cur,
                    known,
                    lookups,
                    sink,
                    cancel,
                )
                .await?;
            }
            if recheck && let Some(s) = cur.channels.get_mut(&conv.id) {
                s.checked_at = now;
            }
        }
        let backfill = cur.channels.get(&conv.id).and_then(|s| s.backfill);
        if let Some(r) = backfill {
            self.rebuild(
                conv, info, r.from, r.before, true, cur, known, lookups, sink, cancel,
            )
            .await?;
            if let Some(s) = cur.channels.get_mut(&conv.id) {
                s.backfill = None;
                if s.latest.is_none() {
                    s.latest = Some(format!("{}.000000", r.from));
                }
            }
        }
        Ok(())
    }

    /// Whether the conversation has a message newer than `latest` (one small request).
    async fn has_newer(
        &self,
        chan: &str,
        latest: &str,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        match self
            .call::<History>(
                "conversations.history",
                &[
                    ("channel", chan),
                    ("oldest", latest),
                    ("limit", "1"),
                    ("inclusive", "false"),
                ],
                cancel,
            )
            .await?
        {
            Ok(h) => Ok(!h.messages.is_empty()),
            Err(code) => Err(not_found_or_other(code)),
        }
    }

    /// Rebuild the documents of every day in `[from, to)` (and the threads started in it) from
    /// a fresh read, newest first. Indexed days and threads in the range that were not seen
    /// are deleted. With `backfill`, `backfill.before` follows the progress and is checkpointed.
    #[allow(clippy::too_many_arguments)]
    async fn rebuild(
        &self,
        conv: &Conversation,
        info: &PassInfo,
        from: i64,
        to: i64,
        backfill: bool,
        cur: &mut Cursor,
        known: &mut KnownConv,
        lookups: &mut usize,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let oldest = format!("{from}.000000");
        let latest = format!("{to}.000000");
        let mut upper = to;
        let mut bucket: Option<DayBucket> = None;
        let mut page_cursor: Option<String> = None;
        let mut pages = 0usize;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            pages += 1;
            let mut q = vec![
                ("channel", conv.id.as_str()),
                ("oldest", oldest.as_str()),
                ("latest", latest.as_str()),
                ("inclusive", "true"),
                ("limit", PAGE),
            ];
            if let Some(c) = &page_cursor {
                q.push(("cursor", c));
            }
            let page: History = match self.call("conversations.history", &q, cancel).await? {
                Ok(h) => h,
                Err(code) => return Err(not_found_or_other(code)),
            };
            let mut finished_any = false;
            for msg in page.messages {
                let Some(secs) = ts_secs(&msg.ts) else {
                    continue;
                };
                if secs < from || secs > to {
                    continue;
                }
                if msg
                    .subtype
                    .as_deref()
                    .is_some_and(|s| SKIP_SUBTYPES.contains(&s))
                {
                    continue;
                }
                // The newest message seen moves `latest` forward.
                if let Some(s) = cur.channels.get_mut(&conv.id)
                    && s.latest
                        .as_deref()
                        .and_then(ts_secs_frac)
                        .is_none_or(|l| ts_secs_frac(&msg.ts).is_some_and(|m| m > l))
                {
                    s.latest = Some(msg.ts.clone());
                }
                let day = day_start(secs);
                match &mut bucket {
                    Some(b) if b.day == day => {}
                    Some(b) if b.day < day => {
                        // Out of order (hostile or clock skew): keep it with the current day.
                    }
                    _ => {
                        if let Some(done) = bucket.take() {
                            self.finish_day(
                                conv, info, done, &mut upper, known, lookups, sink, cancel,
                            )
                            .await?;
                            finished_any = true;
                        }
                        bucket = Some(DayBucket {
                            day,
                            msgs: Vec::new(),
                            dropped: 0,
                        });
                    }
                }
                if let Some(b) = &mut bucket {
                    if b.msgs.len() < MAX_DOC_MESSAGES {
                        b.msgs.push(msg);
                    } else {
                        b.dropped += 1;
                    }
                }
            }
            if backfill && finished_any {
                if let Some(s) = cur.channels.get_mut(&conv.id) {
                    s.backfill = Some(Range {
                        from,
                        before: upper,
                    });
                }
                sink.checkpoint(cur.to_value()).await?;
            }
            let next = page
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty());
            match next {
                Some(c) if page.has_more && pages < MAX_HISTORY_PAGES => page_cursor = Some(c),
                _ => break,
            }
        }
        if let Some(done) = bucket.take() {
            self.finish_day(conv, info, done, &mut upper, known, lookups, sink, cancel)
                .await?;
        }
        // Nothing left in [from, upper): whatever is indexed there is gone.
        self.delete_span(from, upper, None, known, sink).await?;
        Ok(())
    }

    /// Delete indexed days in `[lo, hi)` and threads started in it, except `keep_threads`.
    async fn delete_span(
        &self,
        lo: i64,
        hi: i64,
        keep_threads: Option<&HashSet<String>>,
        known: &mut KnownConv,
        sink: &dyn ChangeSink,
    ) -> Result<()> {
        if lo >= hi {
            return Ok(());
        }
        let days: Vec<i64> = known.days.range(lo..hi).map(|(d, _)| *d).collect();
        for d in days {
            if let Some(key) = known.days.remove(&d) {
                sink.send(Change::Delete(key)).await?;
            }
        }
        let starts: Vec<i64> = known.threads.range(lo..hi).map(|(t, _)| *t).collect();
        for t in starts {
            let Some(list) = known.threads.remove(&t) else {
                continue;
            };
            let mut kept = Vec::new();
            for (ts, key, version) in list {
                if keep_threads.is_some_and(|k| k.contains(&ts)) {
                    kept.push((ts, key, version));
                } else {
                    sink.send(Change::Delete(key)).await?;
                }
            }
            if !kept.is_empty() {
                known.threads.insert(t, kept);
            }
        }
        Ok(())
    }

    /// Emit one day's document and its threads; delete what vanished between it and `upper`.
    #[allow(clippy::too_many_arguments)]
    async fn finish_day(
        &self,
        conv: &Conversation,
        info: &PassInfo,
        mut bucket: DayBucket,
        upper: &mut i64,
        known: &mut KnownConv,
        lookups: &mut usize,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let next_day = bucket.day + DAY_SECS;
        // Days between this one and the last finished one had no messages.
        self.delete_span(next_day, *upper, None, known, sink)
            .await?;
        bucket.msgs.sort_by_key(|m| ts_secs_frac(&m.ts));
        let ids = author_and_mention_ids(&bucket.msgs);
        self.ensure_users(&ids, lookups, cancel).await?;

        let mut threads_seen: HashSet<String> = HashSet::new();
        for parent in bucket
            .msgs
            .iter()
            .filter(|m| m.reply_count > 0 && m.thread_ts.as_deref() == Some(m.ts.as_str()))
        {
            threads_seen.insert(parent.ts.clone());
            self.sync_thread(conv, info, parent, known, lookups, sink, cancel)
                .await?;
        }
        // Threads that started this day but whose parent is gone (the day itself is upserted
        // below, so it is taken out of the known set first).
        known.days.remove(&bucket.day);
        self.delete_span(bucket.day, next_day, Some(&threads_seen), known, sink)
            .await?;
        let doc = self.day_document(conv, info, &bucket);
        sink.send(Change::Upsert(doc)).await?;
        *upper = bucket.day;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn sync_thread(
        &self,
        conv: &Conversation,
        info: &PassInfo,
        parent: &Msg,
        known: &mut KnownConv,
        lookups: &mut usize,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let meta = format!(
            "{}:{}:{}",
            parent.reply_count,
            parent.latest_reply.as_deref().unwrap_or_default(),
            parent
                .edited
                .as_ref()
                .and_then(|e| e.ts.as_deref())
                .unwrap_or_default()
        );
        let key = format!("{}/thread/{}", conv.id, parent.ts);
        let unchanged = ts_secs(&parent.ts)
            .and_then(|t| known.threads.get(&t))
            .and_then(|l| l.iter().find(|(ts, _, _)| *ts == parent.ts))
            .and_then(|(_, _, v)| v.as_deref())
            .is_some_and(|v| v.split_once('#').is_some_and(|(m, _)| m == meta));
        if unchanged {
            return Ok(());
        }
        let mut msgs: Vec<Msg> = Vec::new();
        let mut next: Option<String> = None;
        let mut dropped = 0usize;
        for _ in 0..(MAX_THREAD_MESSAGES / 200 + 2) {
            let mut q = vec![
                ("channel", conv.id.as_str()),
                ("ts", parent.ts.as_str()),
                ("limit", PAGE),
            ];
            if let Some(c) = &next {
                q.push(("cursor", c));
            }
            let page: History = match self.call("conversations.replies", &q, cancel).await? {
                Ok(h) => h,
                Err(code) if code == "thread_not_found" => History::default(),
                Err(code) => return Err(not_found_or_other(code)),
            };
            for m in page.messages {
                if ts_secs(&m.ts).is_none() || msgs.iter().any(|x| x.ts == m.ts) {
                    continue;
                }
                if msgs.len() < MAX_THREAD_MESSAGES {
                    msgs.push(m);
                } else {
                    dropped += 1;
                }
            }
            match page
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty())
            {
                Some(c) if page.has_more => next = Some(c),
                _ => break,
            }
        }
        if !msgs.iter().any(|m| m.ts == parent.ts) {
            msgs.push(parent.clone());
        }
        msgs.sort_by_key(|m| ts_secs_frac(&m.ts));
        let ids = author_and_mention_ids(&msgs);
        self.ensure_users(&ids, lookups, cancel).await?;

        let users = self.users_snapshot();
        let first = render_mrkdwn(&parent.text, &users);
        let title = format!("Thread in {}: {}", conv.display, one_line(&first, 80));
        let (body, stats) = render_messages(&msgs, dropped, &users, info, true);
        let content = format!("{title}\n\n{body}");
        let secs = ts_secs(&parent.ts).unwrap_or_default();
        let mut doc = Document::new(key, "text/plain", Content::Text(content.clone()));
        doc.title = Some(title);
        doc.version = Some(format!("{meta}#{}", hash_hex(&content)));
        doc.modified_at = msgs
            .iter()
            .filter_map(|m| ts_secs(&m.ts))
            .max()
            .and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok());
        doc.url = permalink(info, &conv.id, &parent.ts);
        doc.metadata = self.metadata(conv, info, &stats, secs, Some(&parent.ts));
        sink.send(Change::Upsert(doc)).await?;
        if let Some(list) = known.threads.get_mut(&secs) {
            list.retain(|(ts, _, _)| *ts != parent.ts);
        }
        Ok(())
    }

    fn users_snapshot(&self) -> HashMap<String, String> {
        self.users.lock().map(|u| u.clone()).unwrap_or_default()
    }

    fn day_document(&self, conv: &Conversation, info: &PassInfo, bucket: &DayBucket) -> Document {
        let users = self.users_snapshot();
        let day = date(bucket.day);
        let title = format!("{} · {day}", conv.display);
        let (body, stats) = render_messages(&bucket.msgs, bucket.dropped, &users, info, false);
        let content = format!("{} — {day} (UTC)\n\n{body}", conv.display);
        let mut doc = Document::new(
            format!("{}/day/{day}", conv.id),
            "text/plain",
            Content::Text(content.clone()),
        );
        doc.title = Some(title);
        doc.version = Some(hash_hex(&content));
        doc.modified_at = bucket
            .msgs
            .iter()
            .filter_map(|m| ts_secs(&m.ts))
            .max()
            .and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok());
        doc.url = bucket
            .msgs
            .first()
            .and_then(|m| permalink(info, &conv.id, &m.ts));
        doc.metadata = self.metadata(conv, info, &stats, bucket.day, None);
        doc
    }

    fn metadata(
        &self,
        conv: &Conversation,
        info: &PassInfo,
        stats: &Stats,
        when: i64,
        thread_ts: Option<&str>,
    ) -> Value {
        let mut m = json!({
            "service": "slack",
            "channel_id": conv.id,
            "channel": conv.display,
            "channel_type": conv.kind,
            "date": date(when),
            "authors": stats.authors,
            "message_count": stats.messages,
            "from_me": stats.mine > 0,
            "my_message_count": stats.mine,
        });
        if let Some(t) = &info.team {
            m["team"] = json!(t);
        }
        if let Some(me) = &info.me_name {
            m["me"] = json!(me);
        }
        if let Some(ts) = thread_ts {
            m["thread_ts"] = json!(ts);
        }
        m
    }
}

fn not_found_or_other(code: String) -> Error {
    match code.as_str() {
        "channel_not_found" | "not_in_channel" | "is_archived" | "access_denied" => {
            Error::NotFound(format!("Slack: {code}"))
        }
        _ => Error::Other(format!("Slack error: {}", one_line(&code, 80))),
    }
}

/// (seconds, fraction) of a ts, for ordering.
fn ts_secs_frac(ts: &str) -> Option<(i64, u64)> {
    let secs = ts_secs(ts)?;
    let frac = ts.split_once('.').map_or("0", |(_, f)| f);
    let frac: u64 = frac.get(..frac.len().min(9))?.parse().unwrap_or(0);
    Some((secs, frac))
}

fn permalink(info: &PassInfo, chan: &str, ts: &str) -> Option<String> {
    let base = info.team_url.as_ref()?;
    ts_secs(ts)?;
    Some(format!("{base}archives/{chan}/p{}", ts.replace('.', "")))
}

struct Stats {
    authors: Vec<String>,
    messages: usize,
    mine: usize,
}

fn author_name(m: &Msg, users: &HashMap<String, String>) -> String {
    if let Some(u) = &m.user {
        return users.get(u).cloned().unwrap_or_else(|| one_line(u, 32));
    }
    [
        m.bot_profile.as_ref().and_then(|b| b.name.clone()),
        m.username.clone(),
    ]
    .into_iter()
    .flatten()
    .map(|n| one_line(&n, 80))
    .find(|n| !n.is_empty())
    .unwrap_or_else(|| {
        if m.bot_id.is_some() {
            "bot".into()
        } else {
            "unknown".into()
        }
    })
}

/// `[HH:MM] Name: text` lines, capped; also who wrote what.
fn render_messages(
    msgs: &[Msg],
    dropped: usize,
    users: &HashMap<String, String>,
    info: &PassInfo,
    in_thread: bool,
) -> (String, Stats) {
    let mut out = String::new();
    let mut authors: Vec<String> = Vec::new();
    let mut mine = 0usize;
    let mut omitted = dropped;
    for m in msgs {
        let name = author_name(m, users);
        if !authors.contains(&name) && authors.len() < 100 {
            authors.push(name.clone());
        }
        if m.user.is_some() && m.user == info.me {
            mine += 1;
        }
        if out.len() >= MAX_DOC_CHARS {
            omitted += 1;
            continue;
        }
        let secs = ts_secs(&m.ts).unwrap_or_default();
        let mut text = render_mrkdwn(&m.text, users);
        for a in m.attachments.iter().take(10) {
            let part = [
                a.title.as_deref(),
                a.text.as_deref().or(a.fallback.as_deref()),
            ]
            .into_iter()
            .flatten()
            .map(|s| render_mrkdwn(s, users))
            .collect::<Vec<_>>()
            .join(": ");
            if !part.trim().is_empty() {
                text.push_str(&format!("\n  > {}", truncate_chars(part.trim(), 2_000)));
            }
        }
        for f in m.files.iter().take(20) {
            if let Some(n) = f.title.as_deref().or(f.name.as_deref()) {
                text.push_str(&format!(" [file: {}]", one_line(n, 200)));
            }
        }
        let text = truncate_chars(text.trim(), MAX_MESSAGE_CHARS);
        let indent = if in_thread && m.thread_ts.as_deref() != Some(m.ts.as_str()) {
            "  "
        } else {
            ""
        };
        out.push_str(&format!("{indent}[{}] {name}: {text}", clock(secs)));
        if m.edited.is_some() {
            out.push_str(" (edited)");
        }
        if !in_thread && m.reply_count > 0 {
            out.push_str(&format!(" [thread: {} replies]", m.reply_count));
        }
        out.push('\n');
    }
    if omitted > 0 {
        out.push_str(&format!("… ({omitted} more messages not shown)\n"));
    }
    (
        out,
        Stats {
            authors,
            messages: msgs.len() + dropped,
            mine,
        },
    )
}

/// Ids of authors and `<@U…>` mentions in `msgs`.
fn author_and_mention_ids(msgs: &[Msg]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        if safe_id(id, 32) && !ids.iter().any(|i| i == id) && ids.len() < 1_000 {
            ids.push(id.to_string());
        }
    };
    for m in msgs {
        if let Some(u) = &m.user {
            push(u);
        }
        let mut rest = m.text.as_str();
        while let Some(i) = rest.find("<@") {
            rest = &rest[i + 2..];
            let end = rest.find(['>', '|']).unwrap_or(rest.len()).min(40);
            push(rest.get(..end).unwrap_or_default());
        }
    }
    ids
}

/// Slack mrkdwn → plain text: mentions and channel links resolved, links as `label (url)`,
/// entities decoded, `*bold*` / `_italic_` / `~strike~` / backtick markers dropped.
pub(crate) fn render_mrkdwn(text: &str, users: &HashMap<String, String>) -> String {
    let text = clean(text);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(open) = rest.find('<') {
        out.push_str(&strip_markers(&decode_entities(&rest[..open])));
        let after = &rest[open + 1..];
        match after.find('>') {
            Some(close) if close <= 2_000 && !after[..close].contains('<') => {
                out.push_str(&render_token(&after[..close], users));
                rest = &after[close + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(&strip_markers(&decode_entities(rest)));
    out
}

fn render_token(inner: &str, users: &HashMap<String, String>) -> String {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t, Some(strip_markers(&decode_entities(l)))),
        None => (inner, None),
    };
    if let Some(id) = target.strip_prefix('@') {
        let name = users
            .get(id)
            .cloned()
            .or(label)
            .unwrap_or_else(|| id.to_string());
        return format!("@{}", name.trim_start_matches('@'));
    }
    if let Some(id) = target.strip_prefix('#') {
        return format!("#{}", label.unwrap_or_else(|| id.to_string()));
    }
    if let Some(special) = target.strip_prefix('!') {
        return match special {
            "here" | "channel" | "everyone" => format!("@{special}"),
            s if s.starts_with("subteam^") => label.unwrap_or_else(|| "@group".into()),
            _ => label.unwrap_or_default(),
        };
    }
    let url = decode_entities(target);
    match label {
        Some(l) if !l.is_empty() && l != url && !url.starts_with("mailto:") => {
            format!("{l} ({url})")
        }
        Some(l) if !l.is_empty() => l,
        _ => url.trim_start_matches("mailto:").to_string(),
    }
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Drop formatting markers that open or close a word; keep them inside words (`snake_case`).
fn strip_markers(s: &str) -> String {
    let s = s.replace("```", "");
    let chars: Vec<char> = s.chars().collect();
    let edge = |c: Option<&char>| c.is_none_or(|c| c.is_whitespace() || c.is_ascii_punctuation());
    let mut out = String::with_capacity(s.len());
    for (i, c) in chars.iter().enumerate() {
        if matches!(c, '*' | '_' | '~' | '`') {
            let prev = if i == 0 { None } else { chars.get(i - 1) };
            let next = chars.get(i + 1);
            let opens = edge(prev) && next.is_some_and(|n| !n.is_whitespace());
            let closes = prev.is_some_and(|p| !p.is_whitespace()) && edge(next);
            if opens || closes {
                continue;
            }
        }
        out.push(*c);
    }
    out
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn mrkdwn_renders_as_plain_text() {
        let users: HashMap<String, String> = [("U1".to_string(), "Ada".to_string())].into();
        assert_eq!(
            render_mrkdwn(
                "hi <@U1>, see <#C1|general> and <https://x.io/a_b|the doc>",
                &users
            ),
            "hi @Ada, see #general and the doc (https://x.io/a_b)"
        );
        assert_eq!(
            render_mrkdwn("*bold* _it_ ~gone~ `code` snake_case 2*3", &users),
            "bold it gone code snake_case 2*3"
        );
        assert_eq!(
            render_mrkdwn("a &lt;b&gt; &amp; <!here> <@U9>", &users),
            "a <b> & @here @U9"
        );
        assert_eq!(
            render_mrkdwn("<mailto:a@b.c|a@b.c> <https://u>", &users),
            "a@b.c https://u"
        );
        assert_eq!(
            render_mrkdwn("unclosed < bracket", &users),
            "unclosed < bracket"
        );
        assert_eq!(render_mrkdwn("```let x = 1;```", &users), "let x = 1;");
    }

    #[test]
    fn ts_parsing_is_strict() {
        assert_eq!(ts_secs("1700000000.000200"), Some(1_700_000_000));
        assert_eq!(ts_secs("abc"), None);
        assert_eq!(ts_secs("1.2.3"), None);
        assert!(ts_secs_frac("1.000200") > ts_secs_frac("1.000100"));
    }
}
