//! "Sign in with ChatGPT": the user's ChatGPT plan pays for the light and smart tiers, through
//! OpenAI's programme for open-source and locally hosted apps
//! (developers.openai.com/siwc/token-sharing-open-source, read 2026-11-02).
//!
//! LOCAL MODE ONLY. OpenAI's docs cover "open-source projects, personal projects that run
//! locally"; paid or remotely hosted use needs OpenAI's approval, which Heed has not requested.
//! So these tokens live on the user's computer (OS keychain, through nuggetd's secret store),
//! requests go from this computer straight to `api.openai.com`, and nothing here is used by
//! Heed Cloud (`nugget-cloud` does not depend on this module; a test there checks it).
//!
//! The flow, as documented:
//! - **Host id**: an opaque `urn:uuid:` value per installation, chosen before the first sign-in
//!   and reused ([`new_host_id`]). Never an email or user id.
//! - **Registration**: the first authorize request uses the reserved client id
//!   [`DYNAMIC_CLIENT_ID`] with `agent_name_hint` and `ext_agent_host_id`; the callback returns
//!   the issued `client_id`, which is saved (before the code exchange) and reused, also across
//!   sign-outs. No client secret exists.
//! - **Authorize**: system browser, loopback `127.0.0.1` redirect, fresh `state`, `nonce` and
//!   PKCE S256 each time, `resource=https://api.openai.com/v1`, scopes [`SCOPES`].
//! - **Token**: `authorization_code` with the issued client id, `code_verifier`, the same
//!   `redirect_uri` and `resource`. The ID token is verified (RS256 against the JWKS, issuer,
//!   audience, nonce, expiry) and the granted scopes must include [`PLAN_SCOPE`] before any
//!   inference; without it plan usage is off and nothing is sent.
//! - **Refresh**: `refresh_token` grant; access token, expiry, scopes and the ROTATING refresh
//!   token are replaced together.
//! - **Inference**: `POST /v1/responses` with the bearer token, `store: false`, `stream: true`;
//!   models from `GET /v1/models` (entries with `visibility: "list"`).
//! - **Limits**: HTTP 429 `subscription_sharing_usage_limit_exceeded` pauses ChatGPT requests
//!   until the user resumes (no reset time is inferred) and is NOT a fallback error: the router
//!   never moves the request to another, possibly paid, provider on its own. The user is sent to
//!   [`MANAGE_USAGE_URL`]. `subscription_sharing_invalid_user` (401) asks for a new sign-in.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nugget_core::capability::{Locality, Tier};
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapters::openai_responses::OpenAiResponsesProvider;
use crate::cloud::BearerSource;
use crate::config::{ProviderConfig, Secret};
use crate::locality::{HostResolver, SystemResolver};
use crate::provider::{Prepared, Provider};
use crate::types::{ChatRequest, ChatStream, ProviderMeta};

pub const ISSUER: &str = "https://auth.openai.com";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub const REVOKE_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
pub const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";
/// The `resource` of every authorize and token request, and the inference API root.
pub const RESOURCE: &str = "https://api.openai.com/v1";
/// The reserved client id of a first sign-in (dynamic registration); never saved.
pub const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
/// Identity, a refresh token, and ChatGPT plan usage.
pub const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The scope without which no inference may happen.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
pub use crate::http::{INVALID_USER_CODE, USAGE_LIMIT_CODE};
pub const MANAGE_USAGE_URL: &str = "https://chatgpt.com/settings/usage";
/// What OpenAI shows the user as the app's name at registration.
pub const APP_NAME: &str = "Heed";

/// Provider ids of the two tiers.
pub const CHATGPT_LIGHT: &str = "chatgpt-light";
pub const CHATGPT_SMART: &str = "chatgpt-smart";
/// Tiers each serves. Triage of local items is not among them: it stays on-device.
pub const LIGHT_TIERS: &[Tier] = &[Tier::Light];
pub const SMART_TIERS: &[Tier] = &[Tier::Synthesis, Tier::Agentic];

/// Refresh this long before the access token expires.
pub const REFRESH_MARGIN_SECS: i64 = 300;
const EXPIRY_MARGIN_SECS: i64 = 15;

/// OpenAI's endpoints; tests point them at a fake authorization server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub issuer: String,
    pub authorize: String,
    pub token: String,
    pub revoke: String,
    pub jwks: String,
    pub api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            issuer: ISSUER.into(),
            authorize: AUTHORIZE_URL.into(),
            token: TOKEN_URL.into(),
            revoke: REVOKE_URL.into(),
            jwks: JWKS_URL.into(),
            api: RESOURCE.into(),
        }
    }
}

fn random_b64(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| Error::Other("no secure random source".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// A new opaque host id: `urn:uuid:` with a random (v4) UUID.
pub fn new_host_id() -> Result<String> {
    let mut b = [0u8; 16];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| Error::Other("no secure random source".into()))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// Whether `s` is a host id we would have made (persisted ids are checked on load).
pub fn is_host_id(s: &str) -> bool {
    let Some(u) = s.strip_prefix("urn:uuid:") else {
        return false;
    };
    u.len() == 36
        && u.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            }
        })
}

/// One authorization attempt's secrets: `state`, `nonce` and the PKCE verifier.
#[derive(Clone)]
pub struct Attempt {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect_uri: String,
}

impl std::fmt::Debug for Attempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attempt")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl Attempt {
    pub fn new(redirect_uri: String) -> Result<Self> {
        Ok(Self {
            state: random_b64(32)?,
            nonce: random_b64(32)?,
            // 64 random bytes → 86 base64url characters (43..=128 allowed).
            verifier: random_b64(64)?,
            redirect_uri,
        })
    }

    /// The S256 challenge of the verifier.
    pub fn challenge(&self) -> String {
        pkce_challenge(&self.verifier)
    }
}

pub fn pkce_challenge(verifier: &str) -> String {
    let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(d.as_ref())
}

/// The browser URL. `client_id` None: first registration (with the app name); `login_hint`
/// for a returning sign-in.
pub fn authorize_url(
    ep: &Endpoints,
    client_id: Option<&str>,
    host_id: &str,
    attempt: &Attempt,
    login_hint: Option<&str>,
) -> Result<String> {
    let mut u = url::Url::parse(&ep.authorize).map_err(|e| Error::Invalid(e.to_string()))?;
    {
        let mut q = u.query_pairs_mut();
        match client_id {
            Some(id) => {
                q.append_pair("client_id", id);
            }
            None => {
                q.append_pair("client_id", DYNAMIC_CLIENT_ID);
                q.append_pair("agent_name_hint", APP_NAME);
            }
        }
        q.append_pair("ext_agent_host_id", host_id);
        if let Some(h) = login_hint {
            q.append_pair("login_hint", h);
        }
        q.append_pair("response_type", "code");
        q.append_pair("redirect_uri", &attempt.redirect_uri);
        q.append_pair("scope", SCOPES);
        q.append_pair("resource", RESOURCE);
        q.append_pair("state", &attempt.state);
        q.append_pair("nonce", &attempt.nonce);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("code_challenge", &attempt.challenge());
    }
    Ok(u.into())
}

/// What the browser brought back to the loopback listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    Code {
        code: String,
        /// Present on a first registration: the issued client id.
        client_id: Option<String>,
    },
    /// The user declined, or the server refused (`error` from the redirect).
    Denied { error: String },
}

/// Parses the callback query, checking `state`. `Err` is a state mismatch or a malformed
/// callback (nothing is exchanged then).
pub fn parse_callback(query: &str, expected_state: &str) -> std::result::Result<Callback, String> {
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let state = get("state").unwrap_or_default();
    if !constant_eq(state.as_bytes(), expected_state.as_bytes()) {
        return Err("state_mismatch".into());
    }
    if let Some(error) = get("error") {
        return Ok(Callback::Denied { error });
    }
    let code = get("code").filter(|c| !c.is_empty()).ok_or("no code")?;
    let client_id = get("client_id").filter(|c| is_client_id(c));
    Ok(Callback::Code { code, client_id })
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A plausible issued client id (it is put into URLs and form bodies).
pub fn is_client_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s != DYNAMIC_CLIENT_ID
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

/// A token set as stored (in the OS keychain only).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    pub fn plan_usage(&self) -> bool {
        self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }
}

/// The account from a verified ID token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// Why a token request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The refresh token is no longer usable: clear the tokens and sign in again (keeping the
    /// client id).
    Reauth(String),
    /// Our client id is wrong (`invalid_client`).
    BadClient(String),
    /// Network or server trouble: keep everything, try again later.
    Transient(String),
    /// The response was not a usable token set.
    Malformed(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reauth(m) => write!(f, "sign in again: {m}"),
            Self::BadClient(m) => write!(f, "client registration rejected: {m}"),
            Self::Transient(m) => write!(f, "temporarily unavailable: {m}"),
            Self::Malformed(m) => write!(f, "unexpected token response: {m}"),
        }
    }
}

const REAUTH_CODES: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// The token response → tokens and the raw ID token (when one came).
fn parse_token_response(
    status: u16,
    body: &str,
    now: i64,
    previous_refresh: Option<&str>,
) -> std::result::Result<(Tokens, Option<String>), TokenError> {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let code = v["error"]
            .as_str()
            .or_else(|| v["error"]["code"].as_str())
            .unwrap_or("")
            .to_owned();
        return Err(if REAUTH_CODES.contains(&code.as_str()) {
            TokenError::Reauth(code)
        } else if code == "invalid_client" {
            TokenError::BadClient(code)
        } else if status >= 500 || status == 429 {
            TokenError::Transient(format!("HTTP {status}"))
        } else {
            TokenError::Malformed(format!("HTTP {status} {code}"))
        });
    }
    let access = v["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| TokenError::Malformed("no access_token".into()))?;
    let expires_in = v["expires_in"]
        .as_i64()
        .unwrap_or(3600)
        .clamp(60, 7 * 86_400);
    let scopes = v["scope"]
        .as_str()
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    Ok((
        Tokens {
            access_token: access.to_owned(),
            // A rotating refresh token replaces the old one; keep the old only if none came.
            refresh_token: v["refresh_token"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| previous_refresh.map(str::to_owned)),
            expires_at: now + expires_in,
            scopes,
        },
        v["id_token"].as_str().map(str::to_owned),
    ))
}

async fn post_form(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> std::result::Result<(u16, String), TokenError> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let res = http
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| TokenError::Transient(e.without_url().to_string()))?;
    let status = res.status().as_u16();
    let body = res
        .text()
        .await
        .map_err(|e| TokenError::Transient(e.without_url().to_string()))?;
    Ok((status, body))
}

/// Exchanges the authorization code.
pub async fn exchange_code(
    http: &reqwest::Client,
    ep: &Endpoints,
    client_id: &str,
    code: &str,
    attempt: &Attempt,
    now: i64,
) -> std::result::Result<(Tokens, Option<String>), TokenError> {
    let (status, body) = post_form(
        http,
        &ep.token,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", &attempt.verifier),
            ("redirect_uri", &attempt.redirect_uri),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    parse_token_response(status, &body, now, None)
}

/// Refreshes; the rotating refresh token in the answer replaces the old one.
pub async fn refresh(
    http: &reqwest::Client,
    ep: &Endpoints,
    client_id: &str,
    refresh_token: &str,
    now: i64,
) -> std::result::Result<Tokens, TokenError> {
    let (status, body) = post_form(
        http,
        &ep.token,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    parse_token_response(status, &body, now, Some(refresh_token)).map(|(t, _)| t)
}

/// Revokes a token (best effort at sign-out).
pub async fn revoke(http: &reqwest::Client, ep: &Endpoints, client_id: &str, token: &str) {
    let _ = post_form(
        http,
        &ep.revoke,
        &[("client_id", client_id), ("token", token)],
    )
    .await;
}

pub async fn fetch_jwks(http: &reqwest::Client, ep: &Endpoints) -> Result<Value> {
    let res = http
        .get(&ep.jwks)
        .send()
        .await
        .map_err(|e| Error::Network(e.without_url().to_string()))?;
    if !res.status().is_success() {
        return Err(Error::Provider(format!("jwks: HTTP {}", res.status())));
    }
    let text = res
        .text()
        .await
        .map_err(|e| Error::Network(e.without_url().to_string()))?;
    serde_json::from_str(&text).map_err(|e| Error::Provider(format!("jwks: {e}")))
}

/// Verifies an ID token: RS256 signature by a key in `jwks`, issuer, audience (`client_id`),
/// nonce and expiry. Returns the account.
pub fn verify_id_token(
    token: &str,
    jwks: &Value,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> std::result::Result<Account, String> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("not a JWT".into());
    };
    let dec = |x: &str| {
        URL_SAFE_NO_PAD
            .decode(x)
            .map_err(|_| "bad base64".to_string())
    };
    let header: Value = serde_json::from_slice(&dec(h)?).map_err(|_| "bad header")?;
    if header["alg"] != "RS256" {
        return Err("unsupported alg".into());
    }
    let kid = header["kid"].as_str();
    let key = jwks["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|k| k["kty"] == "RSA" && (kid.is_none() || k["kid"].as_str() == kid))
        .ok_or("no matching key")?;
    let n = dec(key["n"].as_str().ok_or("key without n")?)?;
    let e = dec(key["e"].as_str().ok_or("key without e")?)?;
    let sig = dec(s)?;
    let signed = format!("{h}.{p}");
    aws_lc_rs::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
            signed.as_bytes(),
            &sig,
        )
        .map_err(|_| "bad signature".to_string())?;
    let c: Value = serde_json::from_slice(&dec(p)?).map_err(|_| "bad claims")?;
    if c["iss"].as_str() != Some(issuer) {
        return Err("wrong issuer".into());
    }
    let aud_ok = match &c["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(a) => a.iter().any(|x| x == client_id),
        _ => false,
    };
    if !aud_ok {
        return Err("wrong audience".into());
    }
    if c["nonce"].as_str().map(str::as_bytes) != Some(nonce.as_bytes()) {
        return Err("wrong nonce".into());
    }
    let exp = c["exp"].as_i64().ok_or("no exp")?;
    if exp + 60 < now {
        return Err("expired".into());
    }
    Ok(Account {
        sub: c["sub"].as_str().ok_or("no sub")?.to_owned(),
        email: c["email"].as_str().map(str::to_owned),
        name: c["name"].as_str().map(str::to_owned),
    })
}

/// From `GET /v1/models`: the listed slugs, light and smart picks. The smart tier takes the
/// first listed model; the light tier the first listed slug with `mini` or `nano` in it, else
/// the same model. (OpenAI's docs do not rank models; this keeps the light tier cheap where
/// the plan offers a small model.)
pub fn pick_models(models: &Value) -> (Vec<String>, Option<String>, Option<String>) {
    let list = models["data"].as_array().or(models["models"].as_array());
    let slugs: Vec<String> = list
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"].as_str().is_none_or(|v| v == "list"))
        .filter_map(|m| {
            m["slug"]
                .as_str()
                .or(m["id"].as_str())
                .filter(|s| {
                    s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
                })
                .map(str::to_owned)
        })
        .collect();
    let smart = slugs.first().cloned();
    let light = slugs
        .iter()
        .find(|s| s.contains("mini") || s.contains("nano"))
        .cloned()
        .or_else(|| smart.clone());
    (slugs, light, smart)
}

// ---- session and providers ------------------------------------------------------------------

#[derive(Default)]
struct SessionState {
    tokens: Option<Tokens>,
    /// The usage limit was reached: no ChatGPT request is sent until the user resumes.
    paused: bool,
    /// The server rejected the token: refresh before the next request.
    rejected: bool,
    /// The account must sign in again (invalid user, unusable refresh token).
    reauth: bool,
}

type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

/// The signed-in ChatGPT tokens, read at every request. Refreshing is the owner's job
/// ([`ChatgptSession::needs_refresh`]); an expired or rejected token is not sent.
pub struct ChatgptSession {
    state: RwLock<SessionState>,
    now: Clock,
}

impl Default for ChatgptSession {
    fn default() -> Self {
        Self::new()
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

impl ChatgptSession {
    pub fn new() -> Self {
        Self::with_clock(Box::new(unix_now))
    }

    pub fn with_clock(now: Clock) -> Self {
        Self {
            state: RwLock::new(SessionState::default()),
            now,
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, SessionState> {
        self.state.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, SessionState> {
        self.state.write().unwrap_or_else(|p| p.into_inner())
    }

    /// New tokens (after sign-in or refresh): clears the rejection and the re-auth flag.
    pub fn set(&self, tokens: Tokens) {
        let mut s = self.write();
        s.tokens = Some(tokens);
        s.rejected = false;
        s.reauth = false;
    }

    pub fn clear(&self) {
        *self.write() = SessionState::default();
    }

    pub fn tokens(&self) -> Option<Tokens> {
        self.read().tokens.clone()
    }

    pub fn plan_usage(&self) -> bool {
        self.read().tokens.as_ref().is_some_and(Tokens::plan_usage)
    }

    pub fn paused(&self) -> bool {
        self.read().paused
    }

    pub fn pause(&self) {
        self.write().paused = true;
    }

    pub fn resume(&self) {
        self.write().paused = false;
    }

    pub fn needs_reauth(&self) -> bool {
        self.read().reauth
    }

    pub fn require_reauth(&self) {
        let mut s = self.write();
        s.reauth = true;
        if let Some(t) = s.tokens.as_mut() {
            t.access_token.clear();
            t.refresh_token = None;
        }
    }

    /// Whether the access token should be refreshed now (rejected, or expiring soon).
    pub fn needs_refresh(&self) -> bool {
        let s = self.read();
        match &s.tokens {
            Some(t) if !s.reauth && t.refresh_token.is_some() => {
                s.rejected || t.expires_at - REFRESH_MARGIN_SECS <= (self.now)()
            }
            _ => false,
        }
    }
}

impl BearerSource for ChatgptSession {
    fn bearer(&self) -> Option<Secret> {
        let s = self.read();
        let t = s.tokens.as_ref()?;
        let usable = !s.rejected
            && !s.reauth
            && !s.paused
            && t.plan_usage()
            && !t.access_token.is_empty()
            && t.expires_at - EXPIRY_MARGIN_SECS > (self.now)();
        usable.then(|| Secret::new(t.access_token.clone()))
    }

    fn rejected(&self) {
        self.write().rejected = true;
    }
}

/// The error a paused or limited ChatGPT provider answers with. Not a fallback error, so the
/// request is not moved to another (possibly paid) provider without the user choosing.
pub fn usage_limit_error(provider: &ProviderId) -> Error {
    Error::Unauthorized(format!(
        "{provider}: {USAGE_LIMIT_CODE}: your ChatGPT plan's usage limit was reached; see {MANAGE_USAGE_URL}"
    ))
}

pub fn is_usage_limit(e: &Error) -> bool {
    matches!(e, Error::Unauthorized(m) if m.contains(USAGE_LIMIT_CODE))
}

pub fn is_invalid_user(e: &Error) -> bool {
    matches!(e, Error::Unauthorized(m) if m.contains(INVALID_USER_CODE))
}

/// A ChatGPT provider: the OpenAI Responses adapter with the session's token, gated on plan
/// usage and the pause.
pub struct ChatgptProvider {
    inner: Arc<dyn Provider>,
    session: Arc<ChatgptSession>,
}

impl ChatgptProvider {
    fn gate(&self) -> Result<()> {
        if self.session.paused() {
            return Err(usage_limit_error(self.inner.id()));
        }
        if self.session.needs_reauth() {
            return Err(Error::Unauthorized(format!(
                "{}: {INVALID_USER_CODE}: sign in to ChatGPT again",
                self.inner.id()
            )));
        }
        if !self.session.plan_usage() {
            return Err(Error::Unauthorized(format!(
                "{}: ChatGPT plan usage is not enabled ({PLAN_SCOPE} not granted)",
                self.inner.id()
            )));
        }
        Ok(())
    }

    /// Pauses on the plan's limit (one clear message for the user) and asks for a new sign-in
    /// when the account is no longer valid.
    fn observe<T>(&self, r: Result<T>) -> Result<T> {
        match r {
            Err(e) if is_usage_limit(&e) => {
                self.session.pause();
                Err(usage_limit_error(self.inner.id()))
            }
            Err(e) if is_invalid_user(&e) => {
                self.session.require_reauth();
                Err(e)
            }
            other => other,
        }
    }
}

#[async_trait]
impl Provider for ChatgptProvider {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }
    fn meta(&self) -> ProviderMeta {
        self.inner.meta()
    }
    fn available(&self) -> bool {
        // Paused stays "available": the request must fail with the limit error, not be routed
        // past this provider to another one without the user's say.
        self.session.paused() || self.session.bearer().is_some() || self.session.needs_refresh()
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        self.gate()?;
        let r = self.inner.chat(req).await;
        self.observe(r)
    }
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // Embeddings stay local: plan usage covers Responses requests only.
        Err(Error::Unsupported(
            "ChatGPT plan usage covers Responses only".into(),
        ))
    }
    async fn embed_items(&self, texts: &[String], _items: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts).await
    }
    async fn prewarm(&self) -> Result<()> {
        if self.gate().is_err() {
            return Ok(());
        }
        self.inner.prewarm().await
    }
    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        // Sending a prefix uses plan usage for nothing the user asked: only open the connection.
        let _ = req;
        self.prewarm().await
    }
}

/// The two providers over one session.
pub struct ChatgptProviders {
    pub light: Arc<dyn Provider>,
    pub smart: Arc<dyn Provider>,
}

fn provider_config(id: &str, model: &str, api: &str) -> Result<ProviderConfig> {
    let cfg: ProviderConfig = toml::from_str(&format!(
        "id = {id:?}\nkind = \"openai_responses\"\nbase_url = {api:?}\nmodel = {model:?}\n\
         store = false\nmax_retries = 0\n"
    ))
    .map_err(|e| Error::Invalid(format!("chatgpt provider config: {e}")))?;
    Ok(cfg)
}

/// Builds the light and smart providers (blocking: resolves the API host once).
pub fn build_chatgpt_providers(
    session: Arc<ChatgptSession>,
    egress: Arc<dyn EgressSink>,
    api: &str,
    light_model: &str,
    smart_model: &str,
) -> Result<ChatgptProviders> {
    build_with(
        session,
        egress,
        api,
        light_model,
        smart_model,
        &SystemResolver,
    )
}

pub(crate) fn build_with(
    session: Arc<ChatgptSession>,
    egress: Arc<dyn EgressSink>,
    api: &str,
    light_model: &str,
    smart_model: &str,
    resolver: &dyn HostResolver,
) -> Result<ChatgptProviders> {
    let make = |id: &str, model: &str| -> Result<Arc<dyn Provider>> {
        let cfg = provider_config(id, model, api)?;
        cfg.validate()?;
        let (locality, resolved) = cfg.resolve_network(resolver)?;
        if api == RESOURCE && locality != Locality::Cloud {
            return Err(Error::Integrity(
                "api.openai.com must be a cloud endpoint".into(),
            ));
        }
        let prepared = Prepared {
            locality,
            resolved,
            headers: Vec::new(),
            bearer: Some(session.clone() as Arc<dyn BearerSource>),
        };
        let inner = OpenAiResponsesProvider::new(&cfg, egress.clone(), None, prepared)?;
        Ok(Arc::new(ChatgptProvider {
            inner: Arc::new(inner),
            session: session.clone(),
        }))
    };
    Ok(ChatgptProviders {
        light: make(CHATGPT_LIGHT, light_model)?,
        smart: make(CHATGPT_SMART, smart_model)?,
    })
}

#[cfg(test)]
mod tests;
