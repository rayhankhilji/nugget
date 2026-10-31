//! MCP client registry: per-client bearer tokens, scopes, revocation and `last_used`.
//!
//! - A token is `ngt_` + base64url (no padding) of 32 random bytes from the OS RNG. It is shown
//!   once, at creation, and never stored: the [`ClientStore`] keeps only
//!   `HMAC-SHA256(server_key, token)` (hex), so a leaked client table cannot be replayed and the
//!   hashes cannot be brute-forced offline without the server key.
//! - Lookups go by that keyed hash, so comparing tokens leaks nothing useful through timing.
//! - Revoking deletes the record: the token stops working on the next request, on every
//!   transport, and a revoked stdio session fails its next call.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use nugget_core::ipc::{McpClientInfo, McpScopes};
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use time::OffsetDateTime;
use zeroize::{Zeroize, Zeroizing};

use crate::tools;

/// Every token starts with this.
pub const TOKEN_PREFIX: &str = "ngt_";
/// Random bytes per token.
pub const TOKEN_BYTES: usize = 32;
/// `ngt_` + 43 base64url characters.
pub const TOKEN_LEN: usize = TOKEN_PREFIX.len() + 43;
const MAX_NAME_LEN: usize = 100;

/// Key for hashing tokens. The daemon keeps it in the encrypted store or the OS keychain.
#[derive(Clone)]
pub struct ServerKey([u8; 32]);

impl Drop for ServerKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl ServerKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn generate() -> Result<Self> {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| Error::Other(format!("os rng: {e}")))?;
        Ok(Self(b))
    }

    /// For persisting the key (handle with care).
    pub fn expose_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// `HMAC-SHA256(key, token)` as lowercase hex.
    pub fn hash_token(&self, token: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.0).expect("any key length");
        mac.update(b"nugget-mcp-token-v1\0");
        mac.update(token.as_bytes());
        hex(&mac.finalize().into_bytes())
    }

    /// `HMAC-SHA256(key, text)`, first 16 hex characters: lets the audit log tell repeated
    /// arguments apart without keeping (or allowing a dictionary guess of) the text.
    pub fn hash_audit_text(&self, text: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.0).expect("any key length");
        mac.update(b"nugget-mcp-audit-v1\0");
        mac.update(text.as_bytes());
        let mut h = hex(&mac.finalize().into_bytes());
        h.truncate(16);
        h
    }
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerKey(<redacted>)")
    }
}

/// A freshly issued bearer token. Zeroed on drop; `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientToken(String);

impl Drop for ClientToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl ClientToken {
    fn generate() -> Result<Self> {
        let mut b = Zeroizing::new([0u8; TOKEN_BYTES]);
        getrandom::fill(b.as_mut()).map_err(|e| Error::Other(format!("os rng: {e}")))?;
        Ok(Self(format!(
            "{TOKEN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(*b)
        )))
    }

    /// The token text, to show the user exactly once.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ClientToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientToken(<redacted>)")
    }
}

/// Cheap syntactic check before hashing: prefix, length and alphabet.
pub fn looks_like_token(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token.starts_with(TOKEN_PREFIX)
        && token[TOKEN_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a [`ClientStore`] persists per client: the public info and the keyed token hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRecord {
    pub info: McpClientInfo,
    /// Lowercase hex `HMAC-SHA256(server_key, token)`.
    pub token_hash: String,
}

/// Persistence for client records. The daemon backs it with the encrypted store.
pub trait ClientStore: Send + Sync {
    /// Insert or replace by `record.info.id`.
    fn put(&self, record: &ClientRecord) -> Result<()>;
    fn get(&self, id: &str) -> Result<Option<ClientRecord>>;
    fn find_by_token_hash(&self, token_hash: &str) -> Result<Option<ClientRecord>>;
    fn list(&self) -> Result<Vec<ClientRecord>>;
    /// `true` if a record was removed.
    fn remove(&self, id: &str) -> Result<bool>;
    fn set_last_used(&self, id: &str, at: OffsetDateTime) -> Result<()>;
}

/// In-memory [`ClientStore`] (tests, ephemeral servers).
#[derive(Debug, Default)]
pub struct MemClientStore {
    records: Mutex<BTreeMap<String, ClientRecord>>,
}

impl MemClientStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything stored, for inspection in tests.
    pub fn records(&self) -> Vec<ClientRecord> {
        self.lock().values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, ClientRecord>> {
        self.records.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl ClientStore for MemClientStore {
    fn put(&self, record: &ClientRecord) -> Result<()> {
        self.lock().insert(record.info.id.clone(), record.clone());
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<ClientRecord>> {
        Ok(self.lock().get(id).cloned())
    }

    fn find_by_token_hash(&self, token_hash: &str) -> Result<Option<ClientRecord>> {
        Ok(self
            .lock()
            .values()
            .find(|r| r.token_hash == token_hash)
            .cloned())
    }

    fn list(&self) -> Result<Vec<ClientRecord>> {
        Ok(self.records())
    }

    fn remove(&self, id: &str) -> Result<bool> {
        Ok(self.lock().remove(id).is_some())
    }

    fn set_last_used(&self, id: &str, at: OffsetDateTime) -> Result<()> {
        if let Some(r) = self.lock().get_mut(id) {
            r.info.last_used = Some(at);
        }
        Ok(())
    }
}

/// Creates, authenticates, lists and revokes MCP clients.
pub struct ClientRegistry {
    store: Arc<dyn ClientStore>,
    key: ServerKey,
    /// `last_used` is written at most this often per client.
    touch_interval: Duration,
}

impl std::fmt::Debug for ClientRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientRegistry").finish_non_exhaustive()
    }
}

impl ClientRegistry {
    pub fn new(store: Arc<dyn ClientStore>, key: ServerKey) -> Self {
        Self {
            store,
            key,
            touch_interval: Duration::from_secs(60),
        }
    }

    pub(crate) fn key(&self) -> &ServerKey {
        &self.key
    }

    /// Change how often `last_used` is persisted (default 60 s; zero = every request).
    pub fn with_touch_interval(mut self, interval: Duration) -> Self {
        self.touch_interval = interval;
        self
    }

    /// Register a client. Returns its info and the bearer token, which is not recoverable later.
    pub fn create(&self, name: &str, scopes: McpScopes) -> Result<(McpClientInfo, ClientToken)> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > MAX_NAME_LEN
            || name.chars().any(char::is_control)
        {
            return Err(Error::Invalid(format!(
                "client name must be 1-{MAX_NAME_LEN} printable characters"
            )));
        }
        validate_scopes(&scopes)?;
        let token = ClientToken::generate()?;
        let info = McpClientInfo {
            id: new_client_id()?,
            name: name.to_owned(),
            scopes,
            created_at: now(),
            last_used: None,
        };
        self.store.put(&ClientRecord {
            info: info.clone(),
            token_hash: self.key.hash_token(token.expose()),
        })?;
        Ok((info, token))
    }

    pub fn list(&self) -> Result<Vec<McpClientInfo>> {
        let mut v: Vec<_> = self.store.list()?.into_iter().map(|r| r.info).collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(v)
    }

    pub fn get(&self, id: &str) -> Result<Option<McpClientInfo>> {
        Ok(self.store.get(id)?.map(|r| r.info))
    }

    /// Replace a client's scopes; its token stays valid.
    pub fn set_scopes(&self, id: &str, scopes: McpScopes) -> Result<McpClientInfo> {
        validate_scopes(&scopes)?;
        let mut rec = self
            .store
            .get(id)?
            .ok_or_else(|| Error::NotFound(format!("mcp client {id}")))?;
        rec.info.scopes = scopes;
        self.store.put(&rec)?;
        Ok(rec.info)
    }

    /// Revoke a client. `false` if it did not exist.
    pub fn revoke(&self, id: &str) -> Result<bool> {
        self.store.remove(id)
    }

    /// The client a bearer token belongs to, recording `last_used`. `None` for unknown, revoked
    /// or malformed tokens.
    pub fn authenticate(&self, token: &str) -> Result<Option<McpClientInfo>> {
        if !looks_like_token(token) {
            return Ok(None);
        }
        let Some(rec) = self.store.find_by_token_hash(&self.key.hash_token(token))? else {
            return Ok(None);
        };
        Ok(Some(self.touch(rec.info)?))
    }

    /// [`authenticate`](Self::authenticate), additionally requiring the token to belong to
    /// `client_id` (the `nuggetd mcp --client <id>` bridge).
    pub fn authenticate_client(
        &self,
        client_id: &str,
        token: &str,
    ) -> Result<Option<McpClientInfo>> {
        Ok(self.authenticate(token)?.filter(|c| c.id == client_id))
    }

    /// Record use of an already authenticated client (throttled).
    pub fn touch(&self, mut info: McpClientInfo) -> Result<McpClientInfo> {
        let now = now();
        let stale = info
            .last_used
            .is_none_or(|t| (now - t) >= self.touch_interval);
        if stale {
            self.store.set_last_used(&info.id, now)?;
            info.last_used = Some(now);
        }
        Ok(info)
    }
}

fn validate_scopes(scopes: &McpScopes) -> Result<()> {
    if let Some(unknown) = scopes
        .tools
        .iter()
        .find(|t| !tools::ALL_TOOLS.contains(&t.as_str()))
    {
        return Err(Error::Invalid(format!(
            "unknown tool in scopes: {unknown} (known: {})",
            tools::ALL_TOOLS.join(", ")
        )));
    }
    if let Some(sources) = &scopes.sources
        && sources.iter().any(|s| s.as_str().trim().is_empty())
    {
        return Err(Error::Invalid("empty source id in scopes".into()));
    }
    Ok(())
}

fn new_client_id() -> Result<String> {
    let mut b = [0u8; 10];
    getrandom::fill(&mut b).map_err(|e| Error::Other(format!("os rng: {e}")))?;
    Ok(format!("mcp_{}", hex(&b)))
}

fn now() -> OffsetDateTime {
    let t = OffsetDateTime::now_utc();
    // Whole seconds: RFC 3339 round-trips stay equal.
    t.replace_nanosecond(0).unwrap_or(t)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}
