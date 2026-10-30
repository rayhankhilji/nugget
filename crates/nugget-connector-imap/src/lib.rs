//! IMAP connector (`imap`): mail from any IMAP server (Fastmail, iCloud, Proton Bridge,
//! self-hosted, ...) over TLS only.
//!
//! ## Settings
//! See [`ImapSettings`]: `{host, port?, tls: "implicit"|"starttls", username, folders?,
//! window_days?, max_message_mb?}`. The password (usually an app password) is read from the
//! keychain under [`nugget_core::secrets::password_account`]; missing → `Error::Unauthorized`.
//!
//! ## Sync model
//! Per mailbox the cursor stores `UIDVALIDITY`, the highest UID seen and (with CONDSTORE) the
//! mailbox `HIGHESTMODSEQ`:
//! - new mail: `UID SEARCH UID <last+1>:* [SINCE <window>]`, then `UID FETCH` of metadata and
//!   `BODY.PEEK[]` (never sets `\Seen`; mailboxes are opened with `EXAMINE`, read-only);
//! - flag changes: `CHANGEDSINCE <modseq>` under CONDSTORE, otherwise a FLAGS fetch of the known
//!   UIDs compared against the stored flags hash;
//! - deletes: `VANISHED (EARLIER)` under QRESYNC, otherwise `UID SEARCH ALL` against known keys;
//! - a changed `UIDVALIDITY` deletes the mailbox's items and resyncs it.
//!
//! Item key: `<mailbox>/<uidvalidity>/<uid>`; version `modseq:<n>` (CONDSTORE) or
//! `flags:<fnv64 hex>`; content `message/rfc822` bytes.
//!
//! ## Cursor (JSON)
//! `{"v":1,"folders":{"INBOX":{"uidvalidity":7,"last_uid":1234,"highest_modseq":99}}}`

mod conn;
mod guard;
mod settings;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::{
    AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory,
    Content, Document, KindInfo,
};
use nugget_core::ids::ItemKey;
use nugget_core::secrets::password_account;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio_rustls::rustls::ClientConfig;

use crate::conn::{EgressMeter, Endpoint, IdleEvent, Imap, ListedMailbox, MessageMeta, Special};
pub use crate::settings::{ImapSettings, TlsMode};

/// Connector kind.
pub const KIND: &str = "imap";
/// Egress-ledger feature for every IMAP byte.
pub const FEATURE: &str = "sync:imap";
/// Egress-ledger feature for changes made on the user's behalf ([`ImapConnector::archive`]).
pub const ACTION_FEATURE: &str = "action:imap";

/// UIDs per metadata FETCH (and per checkpoint while fetching new mail).
const META_BATCH: usize = 200;
/// Messages per body FETCH.
const BODY_BATCH_COUNT: usize = 25;
/// Bytes per body FETCH (by RFC822.SIZE).
const BODY_BATCH_BYTES: u64 = 16 * 1024 * 1024;
/// Longest UID set put on one command line (RFC 7162 §4 suggests staying under 8 KB).
const MAX_SET_LEN: usize = 4000;
/// Largest expunge range expanded from a live `VANISHED` while idling.
const MAX_IDLE_VANISHED: u32 = 10_000;
/// Re-issue IDLE before the 30-minute server inactivity timeout (RFC 2177).
const IDLE_REFRESH: Duration = Duration::from_secs(25 * 60);
const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Reconnects within one pass after the server dropped the session (BYE, reset).
const MAX_RECONNECTS: usize = 3;
/// Pause before reconnect number `n` is `n` times this.
const RECONNECT_DELAY: Duration = Duration::from_millis(250);
/// Passes that try a message the server refuses before it is given up (logged).
const MAX_FETCH_ATTEMPTS: u32 = 5;

// ---------------------------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------------------------

/// Factory for `imap` sources.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImapFactory;

impl ImapFactory {
    pub fn new() -> Self {
        Self
    }
}

impl ConnectorFactory for ImapFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: KIND,
            display_name: "IMAP mail",
            auth: AuthKind::Password,
            settings_schema: settings::schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        ImapSettings::parse(settings).map(|_| ())
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        Ok(Arc::new(ImapConnector::new(
            ImapSettings::parse(settings)?,
            ctx,
        )))
    }
}

// ---------------------------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    #[serde(default)]
    folders: BTreeMap<String, FolderCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FolderCursor {
    uidvalidity: u32,
    last_uid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    highest_modseq: Option<u64>,
    /// Messages the server refused to deliver, as `(uid, failed attempts)`: retried on later
    /// passes (they are below `last_uid`) until [`MAX_FETCH_ATTEMPTS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retry: Vec<(u32, u32)>,
}

impl Cursor {
    fn load(v: Option<Value>) -> Self {
        match v.map(serde_json::from_value::<Cursor>) {
            Some(Ok(c)) if c.v == 1 => c,
            Some(_) => {
                tracing::warn!("ignoring unreadable IMAP cursor; resyncing");
                Self::fresh()
            }
            None => Self::fresh(),
        }
    }

    fn fresh() -> Self {
        Self {
            v: 1,
            folders: BTreeMap::new(),
        }
    }

    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// `mailbox/uidvalidity/uid` (the mailbox name may itself contain `/`).
fn item_key(folder: &str, uidvalidity: u32, uid: u32) -> String {
    format!("{folder}/{uidvalidity}/{uid}")
}

fn parse_key(k: &str) -> Option<(&str, u32, u32)> {
    let mut it = k.rsplitn(3, '/');
    let uid = it.next()?.parse().ok()?;
    let uv = it.next()?.parse().ok()?;
    let folder = it.next()?;
    Some((folder, uv, uid))
}

/// Known items grouped: mailbox → uidvalidity → uid → version.
type KnownTree = HashMap<String, HashMap<u32, BTreeMap<u32, Option<String>>>>;

// ---------------------------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------------------------

/// One configured IMAP account.
pub struct ImapConnector {
    settings: ImapSettings,
    ctx: ConnectorContext,
    tls: OnceLock<Arc<ClientConfig>>,
    io_timeout: Duration,
    idle_refresh: Duration,
    idle_supported: AtomicBool,
}

impl std::fmt::Debug for ImapConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConnector")
            .field("host", &self.settings.host)
            .field("source", &self.ctx.source)
            .finish_non_exhaustive()
    }
}

/// TLS client config: aws-lc-rs provider and the OS verifier, as for reqwest.
fn platform_tls_config() -> Result<Arc<ClientConfig>> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Other(format!("TLS setup: {e}")))?
        .with_platform_verifier()
        .map_err(|e| Error::Other(format!("TLS verifier: {e}")))?
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

impl ImapConnector {
    pub fn new(settings: ImapSettings, ctx: ConnectorContext) -> Self {
        Self {
            settings,
            ctx,
            tls: OnceLock::new(),
            io_timeout: IO_TIMEOUT,
            idle_refresh: IDLE_REFRESH,
            idle_supported: AtomicBool::new(false),
        }
    }

    /// Test hook: trust a specific TLS config (e.g. a test CA) instead of the OS store.
    #[cfg(test)]
    pub(crate) fn with_tls_config(self, cfg: Arc<ClientConfig>) -> Self {
        let _ = self.tls.set(cfg);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_timeouts(mut self, io: Duration, idle_refresh: Duration) -> Self {
        self.io_timeout = io;
        self.idle_refresh = idle_refresh;
        self
    }

    fn endpoint(&self) -> Result<Endpoint> {
        let tls_config = match self.tls.get() {
            Some(c) => c.clone(),
            None => {
                let c = platform_tls_config()?;
                self.tls.get_or_init(|| c).clone()
            }
        };
        Ok(Endpoint {
            host: self.settings.host.trim().to_string(),
            port: self.settings.port(),
            tls: self.settings.tls,
            username: self.settings.username.clone(),
            tls_config,
            io_timeout: self.io_timeout,
            max_literal: u64::from(self.settings.max_message_mb) * 1024 * 1024
                + conn::LITERAL_SLACK,
        })
    }

    fn password(&self) -> Result<String> {
        let account = password_account(self.ctx.source.as_str());
        match self.ctx.secrets.get(&account)? {
            Some(p) if !p.is_empty() => Ok(p),
            _ => Err(Error::Unauthorized(format!(
                "no IMAP password stored for source `{}`",
                self.ctx.source
            ))),
        }
    }

    fn meter(&self) -> EgressMeter {
        EgressMeter::new(
            self.ctx.egress.clone(),
            format!("{}:{}", self.settings.host.trim(), self.settings.port()),
            FEATURE,
        )
    }

    async fn connect(&self, meter: &EgressMeter, cancel: &CancellationToken) -> Result<Imap> {
        let password = self.password()?;
        let ep = self.endpoint()?;
        let imap = Imap::connect(&ep, &password, meter.counters(), cancel).await?;
        self.idle_supported
            .store(imap.has("IDLE"), Ordering::Relaxed);
        Ok(imap)
    }

    async fn known_tree(&self) -> Result<KnownTree> {
        let mut tree: KnownTree = HashMap::new();
        for (key, version) in self.ctx.known.versions().await? {
            if let Some((folder, uv, uid)) = parse_key(key.as_str()) {
                tree.entry(folder.to_string())
                    .or_default()
                    .entry(uv)
                    .or_default()
                    .insert(uid, version);
            }
        }
        Ok(tree)
    }

    /// Which mailboxes to sync, as wire names.
    async fn resolve_folders(&self, imap: &mut Imap) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let push = |name: String, out: &mut Vec<String>| {
            if !out.contains(&name) {
                out.push(name);
            }
        };
        match &self.settings.folders {
            None => {
                push("INBOX".into(), &mut out);
                let list = imap.list_all().await?;
                if let Some(sent) = find_sent(&list) {
                    push(sent, &mut out);
                }
            }
            Some(folders) => {
                let list = if folders.iter().any(|f| f == "*") {
                    imap.list_all().await?
                } else {
                    Vec::new()
                };
                for f in folders {
                    if f == "*" {
                        for mb in &list {
                            if !mb.noselect && !is_excluded(mb) {
                                push(normalize_inbox(&mb.name), &mut out);
                            }
                        }
                    } else {
                        push(normalize_inbox(&conn::encode_mutf7(f)), &mut out);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Archive messages by item key (`<mailbox>/<uidvalidity>/<uid>`): `UID MOVE` them to the
    /// archive mailbox (`\Archive` special-use, else `\All`, else `Archive`). Needs the
    /// server's `MOVE` capability (RFC 6851); without it nothing changes and the result is
    /// `Unsupported` (no COPY + EXPUNGE fallback). Keys whose mailbox changed its UIDVALIDITY
    /// since indexing, or that are already in the archive, are skipped. Traffic is recorded with
    /// feature [`ACTION_FEATURE`]. Returns how many messages were moved.
    pub async fn archive(&self, keys: &[ItemKey], cancel: &CancellationToken) -> Result<usize> {
        let mut by_folder: BTreeMap<&str, (u32, Vec<u32>)> = BTreeMap::new();
        for k in keys {
            let (folder, uv, uid) = parse_key(k.as_str())
                .ok_or_else(|| Error::Invalid(format!("not an IMAP item key: {k}")))?;
            let e = by_folder.entry(folder).or_insert((uv, Vec::new()));
            if e.0 == uv {
                e.1.push(uid);
            }
        }
        if by_folder.is_empty() {
            return Ok(0);
        }
        let mut meter = EgressMeter::new(
            self.ctx.egress.clone(),
            format!("{}:{}", self.settings.host.trim(), self.settings.port()),
            ACTION_FEATURE,
        );
        let mut imap = self.connect(&meter, cancel).await?;
        let result = async {
            if !imap.has("MOVE") {
                return Err(Error::Unsupported(
                    "this IMAP server cannot move messages (no MOVE capability)".into(),
                ));
            }
            let dest = imap.archive_mailbox().await?.ok_or_else(|| {
                Error::Unsupported("this IMAP account has no archive mailbox".into())
            })?;
            let mut moved = 0;
            for (folder, (uv, mut uids)) in by_folder {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if folder == dest {
                    continue;
                }
                let Some(sel) = imap.select(folder).await? else {
                    continue;
                };
                if sel.uidvalidity != Some(uv) {
                    tracing::warn!(folder, "UIDVALIDITY changed; not archiving stale messages");
                    continue;
                }
                uids.sort_unstable();
                uids.dedup();
                for set in conn::uid_sets(&uids, MAX_SET_LEN) {
                    imap.uid_move(&set, &dest).await?;
                }
                moved += uids.len();
            }
            Ok(moved)
        }
        .await;
        imap.logout().await;
        meter.flush();
        result
    }

    async fn run_sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let mut cursor = Cursor::load(cursor);
        let mut meter = self.meter();
        let mut imap = self.connect(&meter, cancel).await?;
        let known = self.known_tree().await?;
        let folders = self.resolve_folders(&mut imap).await?;

        // Mailboxes dropped from the selection: forget their items.
        let selected: HashSet<&str> = folders.iter().map(String::as_str).collect();
        for (folder, by_uv) in &known {
            if selected.contains(folder.as_str()) {
                continue;
            }
            for (uv, uids) in by_uv {
                for uid in uids.keys() {
                    send_delete(sink, folder, *uv, *uid).await?;
                }
            }
        }
        cursor.folders.retain(|f, _| selected.contains(f.as_str()));

        let mut run = Run {
            c: self,
            imap,
            sink,
            cancel,
            refused: Vec::new(),
        };
        let mut known = known;
        let mut reconnects = 0;
        let mut i = 0;
        let empty = HashMap::new();
        while let Some(folder) = folders.get(i) {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let known_f = known.get(folder).unwrap_or(&empty);
            match run
                .sync_folder(folder, known_f, &mut cursor, &mut meter)
                .await
            {
                Ok(()) => i += 1,
                // The server dropped the session (BYE, reset): reconnect and redo this mailbox
                // from the progress committed so far, against what was applied meanwhile.
                Err(e) if is_disconnect(&e) && reconnects < MAX_RECONNECTS => {
                    reconnects += 1;
                    tracing::warn!(folder, error = %e, reconnects, "IMAP session dropped; reconnecting");
                    meter.flush();
                    tokio::select! {
                        _ = tokio::time::sleep(RECONNECT_DELAY * reconnects as u32) => {}
                        _ = cancel.cancelled() => return Err(Error::Cancelled),
                    }
                    run.imap = self.connect(&meter, cancel).await?;
                    known = self.known_tree().await?;
                }
                Err(e) => return Err(e),
            }
            meter.flush();
        }
        run.imap.logout().await;
        meter.flush();
        Ok(cursor.to_value())
    }

    async fn run_watch(&self, sink: Arc<dyn ChangeSink>, cancel: CancellationToken) -> Result<()> {
        let mut meter = self.meter();
        let mut imap = self.connect(&meter, &cancel).await?;
        if !imap.has("IDLE") {
            imap.logout().await;
            return Err(Error::Unsupported(
                "IMAP server does not support IDLE".into(),
            ));
        }
        // Watch INBOX when it is selected, otherwise the first selected mailbox. Never a mailbox
        // outside the selection: its items would be deleted again by the next sync.
        let folders = self.resolve_folders(&mut imap).await?;
        let Some(folder) = watch_folder(&folders) else {
            imap.logout().await;
            return Ok(()); // nothing to watch: the pipeline falls back to polling
        };
        let folder = folder.as_str();
        let sel = imap
            .examine(folder)
            .await?
            .ok_or_else(|| Error::NotFound(format!("mailbox {folder}")))?;
        let uv = sel
            .uidvalidity
            .ok_or_else(|| Error::Network(format!("EXAMINE {folder} returned no UIDVALIDITY")))?;
        let mut last = match sel.uidnext {
            Some(n) => n.saturating_sub(1),
            None => imap.uid_search("ALL").await?.last().copied().unwrap_or(0),
        };
        let mut run = Run {
            c: self,
            imap,
            sink: &*sink,
            cancel: &cancel,
            refused: Vec::new(),
        };
        let no_known = BTreeMap::new();
        loop {
            let id = run.imap.idle_start().await?;
            let deadline = tokio::time::Instant::now() + self.idle_refresh;
            let mut new_mail = false;
            let mut vanished = Vec::new();
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let ev = tokio::select! {
                    _ = cancel.cancelled() => None,
                    ev = run.imap.idle_next(remaining) => Some(ev),
                };
                match ev {
                    None => {
                        let _ = run.imap.idle_done(&id).await;
                        run.imap.logout().await;
                        return Ok(());
                    }
                    Some(ev) => match ev? {
                        None => break,
                        Some(IdleEvent::Exists(_)) => {
                            new_mail = true;
                            break;
                        }
                        Some(IdleEvent::Vanished(r)) => {
                            vanished.extend(r);
                            break;
                        }
                        Some(_) => {}
                    },
                }
            }
            run.imap.idle_done(&id).await?;
            if new_mail {
                let mut uids = run.imap.uid_search(&format!("UID {}:*", last + 1)).await?;
                uids.retain(|u| *u > last);
                for chunk in uids.chunks(META_BATCH) {
                    run.process(folder, uv, chunk, &no_known).await?;
                }
                last = last.max(uids.last().copied().unwrap_or(0));
            }
            for r in vanished {
                let (lo, hi) = (*r.start(), (*r.end()).min(last));
                if hi >= lo && hi - lo < MAX_IDLE_VANISHED {
                    for uid in lo..=hi {
                        send_delete(&*sink, folder, uv, uid).await?;
                    }
                }
            }
            meter.flush();
        }
    }
}

#[async_trait]
impl Connector for ImapConnector {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.run_sync(cursor, sink, cancel).await
    }

    fn supports_watch(&self) -> bool {
        self.idle_supported.load(Ordering::Relaxed)
    }

    async fn watch(&self, sink: Arc<dyn ChangeSink>, cancel: CancellationToken) -> Result<()> {
        self.run_watch(sink, cancel).await
    }
}

/// Whether `e` means the server dropped the session (worth a reconnect), as opposed to a
/// timeout, a protocol violation or a refusal.
fn is_disconnect(e: &Error) -> bool {
    matches!(e, Error::Network(m) if m.contains("closed the session")
        || m.contains("closed the connection")
        || m.contains("connection lost")
        || m.starts_with("imap i/o"))
}

async fn send_delete(sink: &dyn ChangeSink, folder: &str, uv: u32, uid: u32) -> Result<()> {
    sink.send(Change::Delete(ItemKey::new(item_key(folder, uv, uid))))
        .await
}

// ---------------------------------------------------------------------------------------------
// One sync pass over a session
// ---------------------------------------------------------------------------------------------

struct Run<'a> {
    c: &'a ImapConnector,
    imap: Imap,
    sink: &'a dyn ChangeSink,
    cancel: &'a CancellationToken,
    /// UIDs the server refused to deliver during the current mailbox.
    refused: Vec<u32>,
}

impl Run<'_> {
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    async fn sync_folder(
        &mut self,
        folder: &str,
        known_f: &HashMap<u32, BTreeMap<u32, Option<String>>>,
        cursor: &mut Cursor,
        meter: &mut EgressMeter,
    ) -> Result<()> {
        let Some(sel) = self.imap.examine(folder).await? else {
            // Missing or inaccessible: keep its items and cursor; retried next pass.
            return Ok(());
        };
        let uv = sel
            .uidvalidity
            .ok_or_else(|| Error::Network(format!("EXAMINE {folder} returned no UIDVALIDITY")))?;

        // Items from an older UIDVALIDITY are gone for good.
        for (kuv, uids) in known_f {
            if *kuv != uv {
                for uid in uids.keys() {
                    send_delete(self.sink, folder, *kuv, *uid).await?;
                }
            }
        }
        let empty = BTreeMap::new();
        let known = known_f.get(&uv).unwrap_or(&empty);
        let prev = cursor
            .folders
            .get(folder)
            .cloned()
            .filter(|s| s.uidvalidity == uv);
        self.refused.clear();
        let use_modseq = self.imap.condstore && sel.highest_modseq.is_some();

        let mut refetch: Vec<u32> = Vec::new();
        let mut gone: Vec<u32> = Vec::new();
        match &prev {
            Some(st) if use_modseq && st.highest_modseq.is_some() => {
                let since = st.highest_modseq.unwrap_or(0);
                if sel.highest_modseq == Some(since) && st.retry.is_empty() {
                    // Nothing changed in this mailbox (RFC 7162: any change bumps HIGHESTMODSEQ).
                    return Ok(());
                }
                if st.last_uid > 0 {
                    let ch = self.imap.changed_since(st.last_uid, since).await?;
                    for m in &ch.changed {
                        if let Some(v) = known.get(&m.uid)
                            && v.as_deref() != Some(version_of(m, true).as_str())
                        {
                            refetch.push(m.uid);
                        }
                    }
                    if self.imap.qresync {
                        gone.extend(
                            known
                                .keys()
                                .filter(|u| ch.vanished.iter().any(|r| r.contains(u))),
                        );
                    } else {
                        gone = self.missing(known).await?;
                    }
                }
            }
            _ => {
                if !known.is_empty() {
                    gone = self.missing(known).await?;
                    // Without a usable mod-sequence baseline, compare flags of what we hold.
                    // (On a first pass the version check in `process` covers the window.)
                    if prev.is_some() {
                        let alive: Vec<u32> = known
                            .keys()
                            .copied()
                            .filter(|u| !gone.contains(u))
                            .collect();
                        for set in conn::uid_sets(&alive, MAX_SET_LEN) {
                            for m in self.imap.fetch_flags(&set).await? {
                                if let Some(v) = known.get(&m.uid)
                                    && v.as_deref() != Some(version_of(&m, use_modseq).as_str())
                                {
                                    refetch.push(m.uid);
                                }
                            }
                        }
                    }
                }
            }
        }
        for uid in &gone {
            send_delete(self.sink, folder, uv, *uid).await?;
        }
        refetch.sort_unstable();
        refetch.dedup();
        for chunk in refetch.chunks(META_BATCH) {
            self.process(folder, uv, chunk, &empty).await?;
        }

        // Messages the server refused on earlier passes: try again (those gone are dropped).
        let mut retry: Vec<(u32, u32)> = Vec::new();
        let earlier: Vec<(u32, u32)> = prev.as_ref().map(|s| s.retry.clone()).unwrap_or_default();
        if !earlier.is_empty() {
            let mut uids: Vec<u32> = earlier.iter().map(|(u, _)| *u).collect();
            uids.sort_unstable();
            uids.dedup();
            for chunk in uids.chunks(META_BATCH) {
                self.process(folder, uv, chunk, known).await?;
            }
            for (uid, attempts) in earlier {
                if self.refused.contains(&uid) {
                    if attempts + 1 >= MAX_FETCH_ATTEMPTS {
                        tracing::warn!(
                            folder,
                            uid,
                            attempts = attempts + 1,
                            "giving up on a message the server keeps refusing"
                        );
                    } else {
                        retry.push((uid, attempts + 1));
                    }
                }
            }
            self.refused.clear();
        }

        // New mail.
        let last = prev.as_ref().map_or(0, |s| s.last_uid);
        let mut criteria = if last == 0 {
            String::new()
        } else {
            format!("UID {}:*", last.saturating_add(1))
        };
        if self.c.settings.window_days > 0 {
            if !criteria.is_empty() {
                criteria.push(' ');
            }
            criteria.push_str("SINCE ");
            criteria.push_str(&imap_date(
                OffsetDateTime::now_utc()
                    - time::Duration::days(self.c.settings.window_days.into()),
            ));
        }
        if criteria.is_empty() {
            criteria.push_str("ALL");
        }
        let mut new = self.imap.uid_search(&criteria).await?;
        new.retain(|u| *u > last);

        let mut st = prev.unwrap_or(FolderCursor {
            uidvalidity: uv,
            last_uid: 0,
            highest_modseq: None,
            retry: Vec::new(),
        });
        // Refused new messages join the retry list as they happen, so a crash cannot lose them.
        let fresh_retry = |refused: &[u32], retry: &[(u32, u32)]| {
            let mut r = retry.to_vec();
            r.extend(refused.iter().map(|u| (*u, 1)));
            r
        };
        for chunk in new.chunks(META_BATCH) {
            self.process(folder, uv, chunk, known).await?;
            st.last_uid = st.last_uid.max(chunk.last().copied().unwrap_or(0));
            st.retry = fresh_retry(&self.refused, &retry);
            cursor.folders.insert(folder.to_string(), st.clone());
            self.sink.checkpoint(cursor.to_value()).await?;
            meter.flush();
        }
        if let Some(next) = sel.uidnext {
            st.last_uid = st.last_uid.max(next.saturating_sub(1));
        }
        st.highest_modseq = if use_modseq { sel.highest_modseq } else { None };
        st.retry = fresh_retry(&self.refused, &retry);
        cursor.folders.insert(folder.to_string(), st);
        self.sink.checkpoint(cursor.to_value()).await
    }

    /// Known UIDs no longer on the server (`UID SEARCH ALL`).
    async fn missing(&mut self, known: &BTreeMap<u32, Option<String>>) -> Result<Vec<u32>> {
        let all: HashSet<u32> = self.imap.uid_search("ALL").await?.into_iter().collect();
        Ok(known.keys().copied().filter(|u| !all.contains(u)).collect())
    }

    /// Fetch and emit the given UIDs, skipping those whose version is already stored.
    async fn process(
        &mut self,
        folder: &str,
        uv: u32,
        uids: &[u32],
        known: &BTreeMap<u32, Option<String>>,
    ) -> Result<()> {
        self.check_cancel()?;
        let wanted: HashSet<u32> = uids.iter().copied().collect();
        let condstore = self.imap.condstore;
        let mut metas: Vec<MessageMeta> = Vec::new();
        for set in conn::uid_sets(uids, MAX_SET_LEN) {
            metas.extend(self.imap.fetch_meta(&set).await?);
        }
        metas.retain(|m| wanted.contains(&m.uid));
        metas.retain(|m| {
            known.get(&m.uid).and_then(|v| v.as_deref()) != Some(version_of(m, condstore).as_str())
        });
        metas.sort_by_key(|m| m.uid);
        metas.dedup_by_key(|m| m.uid);

        let cap = u64::from(self.c.settings.max_message_mb) * 1024 * 1024;
        let (big, small): (Vec<&MessageMeta>, Vec<&MessageMeta>) = metas
            .iter()
            .partition(|m| m.size.is_some_and(|s| u64::from(s) > cap));

        let mut group: Vec<&MessageMeta> = Vec::new();
        let mut bytes = 0u64;
        for m in small {
            let size = u64::from(m.size.unwrap_or(0));
            if !group.is_empty()
                && (group.len() >= BODY_BATCH_COUNT || bytes + size > BODY_BATCH_BYTES)
            {
                self.emit(folder, uv, &group, false).await?;
                group.clear();
                bytes = 0;
            }
            bytes += size;
            group.push(m);
        }
        if !group.is_empty() {
            self.emit(folder, uv, &group, false).await?;
        }
        for chunk in big.chunks(BODY_BATCH_COUNT) {
            self.emit(folder, uv, chunk, true).await?;
        }
        Ok(())
    }

    async fn emit(
        &mut self,
        folder: &str,
        uv: u32,
        group: &[&MessageMeta],
        header_only: bool,
    ) -> Result<()> {
        self.check_cancel()?;
        let uids: Vec<u32> = group.iter().map(|m| m.uid).collect();
        let cap = usize::try_from(self.c.settings.max_message_mb)
            .unwrap_or(512)
            .saturating_mul(1024 * 1024);
        let max_total = BODY_BATCH_BYTES as usize + cap;
        let mut bodies = HashMap::new();
        for set in conn::uid_sets(&uids, MAX_SET_LEN) {
            match self.imap.fetch_bodies(&set, header_only, max_total).await {
                Ok(b) => bodies.extend(b),
                // The server cannot deliver some message of the group: fetch the rest one by one
                // and skip only those it refuses, so one broken message never stops the sync.
                Err(Error::Provider(m)) => {
                    tracing::warn!(folder, error = %m, "IMAP FETCH refused; retrying one by one");
                    bodies.clear();
                    let mut refused = Vec::new();
                    for uid in &uids {
                        match self
                            .imap
                            .fetch_bodies(&uid.to_string(), header_only, max_total)
                            .await
                        {
                            Ok(b) => bodies.extend(b),
                            Err(Error::Provider(m)) => {
                                tracing::warn!(folder, uid, error = %m, "skipping a message the server cannot deliver; retried next pass");
                                refused.push(*uid);
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    // Not one message of several delivered: the server (not a message) is
                    // broken right now. Fail the pass rather than skip them all.
                    if uids.len() > 1 && refused.len() == uids.len() {
                        return Err(Error::Provider(format!(
                            "IMAP server refused to deliver all {} messages of a batch: {m}",
                            uids.len()
                        )));
                    }
                    self.refused.extend(refused);
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        let condstore = self.imap.condstore;
        for m in group {
            // Expunged between the metadata and body fetch: the next pass reconciles.
            let Some(body) = bodies.remove(&m.uid) else {
                continue;
            };
            let doc = self.c.document(folder, uv, m, body, header_only, condstore);
            self.sink.send(Change::Upsert(doc)).await?;
        }
        Ok(())
    }
}

impl ImapConnector {
    fn document(
        &self,
        folder: &str,
        uv: u32,
        m: &MessageMeta,
        body: Vec<u8>,
        truncated: bool,
        condstore: bool,
    ) -> Document {
        let mut meta = serde_json::Map::new();
        meta.insert("folder".into(), json!(folder));
        meta.insert("uid".into(), json!(m.uid));
        meta.insert("uidvalidity".into(), json!(uv));
        meta.insert("flags".into(), json!(m.flags));
        if let Some(id) = header_value(&body, "message-id") {
            meta.insert("message_id".into(), json!(id));
        }
        if let Some(s) = m.size {
            meta.insert("size".into(), json!(s));
        }
        if truncated {
            meta.insert("truncated".into(), json!(true));
        }
        let mut doc = Document::new(
            item_key(folder, uv, m.uid),
            "message/rfc822",
            Content::Bytes(body),
        );
        doc.version = Some(version_of(m, condstore));
        doc.modified_at = m.internal_date;
        doc.url = Some(format!(
            "imap://{}@{}:{}/{};UIDVALIDITY={uv}/;UID={}",
            pct(&self.settings.username),
            self.settings.host.trim(),
            self.settings.port(),
            pct_path(folder),
            m.uid
        ));
        doc.metadata = Value::Object(meta);
        doc
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// `modseq:<n>` under CONDSTORE, otherwise a stable hash of the sorted flags.
fn version_of(m: &MessageMeta, condstore: bool) -> String {
    match m.modseq {
        Some(ms) if condstore => format!("modseq:{ms}"),
        _ => {
            let mut flags: Vec<String> = m
                .flags
                .iter()
                .filter(|f| !f.eq_ignore_ascii_case("\\Recent"))
                .map(|f| f.to_ascii_lowercase())
                .collect();
            flags.sort();
            format!("flags:{:016x}", fnv1a64(flags.join(" ").as_bytes()))
        }
    }
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `d-Mon-yyyy` for SEARCH SINCE.
fn imap_date(t: OffsetDateTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}-{}-{}",
        t.day(),
        MONTHS[usize::from(u8::from(t.month())) - 1],
        t.year()
    )
}

/// The mailbox IDLE watches: INBOX if selected, else the first selected mailbox.
fn watch_folder(folders: &[String]) -> Option<&String> {
    folders
        .iter()
        .find(|f| f.as_str() == "INBOX")
        .or_else(|| folders.first())
}

fn normalize_inbox(name: &str) -> String {
    if name.eq_ignore_ascii_case("INBOX") {
        "INBOX".into()
    } else {
        name.to_string()
    }
}

fn leaf(mb: &ListedMailbox) -> &str {
    match mb.delimiter.as_deref() {
        Some(d) if !d.is_empty() => mb.name.rsplit(d).next().unwrap_or(&mb.name),
        _ => &mb.name,
    }
}

/// Mailboxes `"*"` leaves out: Trash and Junk, and virtual views (`\All`, `\Flagged`,
/// `\Important`) whose messages already live in a real mailbox and would be indexed twice.
fn is_excluded(mb: &ListedMailbox) -> bool {
    if mb.virtual_view || matches!(mb.special, Some(Special::Trash | Special::Junk)) {
        return true;
    }
    let l = leaf(mb).to_ascii_lowercase();
    matches!(
        l.as_str(),
        "trash"
            | "spam"
            | "junk"
            | "junk e-mail"
            | "junk email"
            | "deleted items"
            | "deleted messages"
            | "bin"
    )
}

fn find_sent(list: &[ListedMailbox]) -> Option<String> {
    if let Some(mb) = list
        .iter()
        .find(|m| !m.noselect && m.special == Some(Special::Sent))
    {
        return Some(mb.name.clone());
    }
    list.iter()
        .filter(|m| !m.noselect)
        .find(|m| {
            matches!(
                leaf(m).to_ascii_lowercase().as_str(),
                "sent" | "sent items" | "sent messages" | "sent mail"
            )
        })
        .map(|m| m.name.clone())
}

/// First value of header `name` (case-insensitive) in an RFC 822 message, unfolded.
fn header_value(msg: &[u8], name: &str) -> Option<String> {
    let end = find(msg, b"\r\n\r\n")
        .or_else(|| find(msg, b"\n\n"))
        .unwrap_or(msg.len().min(256 * 1024));
    let head = String::from_utf8_lossy(&msg[..end]);
    let mut lines = head
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .peekable();
    while let Some(line) = lines.next() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if !k.trim().eq_ignore_ascii_case(name) {
            continue;
        }
        let mut value = v.trim().to_string();
        while let Some(next) = lines.peek() {
            if next.starts_with([' ', '\t']) {
                value.push(' ');
                value.push_str(next.trim());
                lines.next();
            } else {
                break;
            }
        }
        let mut value = value.trim().to_string();
        if value.len() > 998 {
            let mut end = 998;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            value.truncate(end);
        }
        return (!value.is_empty()).then_some(value);
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_path(s: &str) -> String {
    s.split('/').map(pct).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests;
