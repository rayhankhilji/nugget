//! Supabase Auth over PKCE: the authorize URL (Google; Apple is not in M1), the magic link, the token
//! exchange and refresh, logout, and the `heed://auth/callback` deep link.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{CloudConfig, CloudError, Result};

/// Where Supabase sends the browser back (allow-listed in the project's URL configuration).
pub const REDIRECT_URL: &str = "heed://auth/callback";

/// Deep-link schemes a callback may arrive on: `heed` (the new name) and `nugget` (until every
/// installed build and the Supabase allow-list have moved). Any app can claim a scheme on
/// macOS, Windows and Linux, so the scheme is not a secret: PKCE (the verifier never leaves
/// this process) is what makes an intercepted or forged code useless.
pub const CALLBACK_SCHEMES: &[&str] = &["heed", "nugget"];

const MAX_CALLBACK_LEN: usize = 2048;
const MAX_CODE_LEN: usize = 512;
const MAX_ERROR_LEN: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Google,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "google" => Ok(Self::Google),
            // Not in M1: needs an Apple Developer account first.
            "apple" => Err(CloudError::Invalid(
                "Sign in with Apple isn't available yet".into(),
            )),
            other => Err(CloudError::Invalid(format!(
                "unknown sign-in provider `{other}`"
            ))),
        }
    }
}

/// `{supabase_url}/auth/v1/authorize?provider=..&redirect_to=..&code_challenge=..&code_challenge_method=s256`
pub fn authorize_url(cfg: &CloudConfig, provider: Provider, challenge: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("{}/auth/v1/authorize", cfg.supabase_url))
        .map_err(|e| CloudError::NotConfigured(format!("supabase url: {e}")))?;
    url.query_pairs_mut()
        .append_pair("provider", provider.as_str())
        .append_pair("redirect_to", REDIRECT_URL)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "s256");
    Ok(url.into())
}

/// What `/auth/v1/token` answers (both grants).
#[derive(Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    #[serde(default)]
    pub expires_at: Option<i64>,
    pub user: AuthUser,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("expires_in", &self.expires_in)
            .field("expires_at", &self.expires_at)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthUser {
    pub id: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// The Supabase Auth endpoints the desktop uses.
#[derive(Clone)]
pub struct SupabaseAuth {
    cfg: CloudConfig,
    http: reqwest::Client,
}

impl SupabaseAuth {
    pub fn new(cfg: CloudConfig, http: reqwest::Client) -> Self {
        Self { cfg, http }
    }

    pub fn config(&self) -> &CloudConfig {
        &self.cfg
    }

    /// `POST {supabase_url}/auth/v1/{path}?{query}` with the project's anon key.
    fn post(&self, path: &str, query: &[(&str, &str)]) -> Result<reqwest::RequestBuilder> {
        let mut url = reqwest::Url::parse(&format!("{}/auth/v1/{path}", self.cfg.supabase_url))
            .map_err(|e| CloudError::NotConfigured(format!("supabase url: {e}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(self
            .http
            .post(url)
            .header("apikey", &self.cfg.supabase_anon_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json"))
    }

    /// `POST /auth/v1/otp?redirect_to=…`: email a magic link that returns to the deep link.
    pub async fn send_magic_link(&self, email: &str, challenge: &str) -> Result<()> {
        let body = json!({
            "email": email,
            "create_user": true,
            "code_challenge": challenge,
            "code_challenge_method": "s256",
        });
        let resp = self
            .post("otp", &[("redirect_to", REDIRECT_URL)])?
            .body(body.to_string())
            .send()
            .await
            .map_err(CloudError::network)?;
        check(resp).await.map(drop)
    }

    /// `POST /auth/v1/token?grant_type=pkce`.
    pub async fn exchange(&self, auth_code: &str, verifier: &str) -> Result<TokenResponse> {
        let body = json!({ "auth_code": auth_code, "code_verifier": verifier });
        self.token("pkce", body).await
    }

    /// `POST /auth/v1/token?grant_type=refresh_token`. A refused refresh token (4xx) is
    /// [`CloudError::NotSignedIn`]; network and server errors are passed on, so a transient
    /// failure does not sign the user out.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse> {
        let body = json!({ "refresh_token": refresh_token });
        match self.token("refresh_token", body).await {
            Err(CloudError::SignInFailed(msg)) => {
                tracing::info!(reason = %msg, "the cloud refresh token was refused");
                Err(CloudError::NotSignedIn)
            }
            other => other,
        }
    }

    async fn token(&self, grant: &str, body: Value) -> Result<TokenResponse> {
        let resp = self
            .post("token", &[("grant_type", grant)])?
            .body(body.to_string())
            .send()
            .await
            .map_err(CloudError::network)?;
        let resp = check(resp).await?;
        resp.json::<TokenResponse>()
            .await
            .map_err(|e| CloudError::Protocol(format!("token response: {}", e.without_url())))
    }

    /// `POST /auth/v1/logout` with the access token: revokes the session's refresh tokens.
    pub async fn logout(&self, access_token: &str) -> Result<()> {
        let resp = self
            .post("logout", &[])?
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(CloudError::network)?;
        check(resp).await.map(drop)
    }
}

/// Supabase Auth errors: 4xx is a refusal ([`CloudError::SignInFailed`] with Supabase's message),
/// anything else unexpected is an [`CloudError::Api`] error.
async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
    let message = text("error_description")
        .or_else(|| text("msg"))
        .or_else(|| text("message"))
        .or_else(|| text("error"))
        .unwrap_or_else(|| status.to_string());
    if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(CloudError::SignInFailed(message));
    }
    Err(CloudError::Api {
        status: status.as_u16(),
        code: text("error_code").unwrap_or_else(|| format!("http_{}", status.as_u16())),
        message,
    })
}

/// The deep link Supabase redirects to: a code to exchange, or an error.
#[derive(Clone, PartialEq, Eq)]
pub enum Callback {
    Code(String),
    Error {
        error: String,
        description: Option<String>,
    },
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(_) => f.write_str("Callback::Code(<redacted>)"),
            Self::Error { error, description } => f
                .debug_struct("Callback::Error")
                .field("error", error)
                .field("description", description)
                .finish(),
        }
    }
}

impl Callback {
    /// The user-facing reason of an error callback.
    pub fn error_message(&self) -> Option<String> {
        match self {
            Self::Code(_) => None,
            Self::Error { error, description } => {
                Some(description.clone().unwrap_or_else(|| error.clone()))
            }
        }
    }
}

/// Parse and strictly validate `heed://auth/callback?code=…` or
/// `heed://auth/callback?error=…&error_description=…` (Supabase may add `error_code`); the
/// legacy `nugget://` scheme is accepted the same way.
/// Anything else is refused: other hosts or paths, credentials, ports, fragments, unknown or
/// repeated parameters, over-long values and control characters.
pub fn parse_callback(raw: &str) -> Result<Callback> {
    let bad = |why: &str| CloudError::Invalid(format!("not a sign-in link: {why}"));
    if raw.len() > MAX_CALLBACK_LEN {
        return Err(bad("too long"));
    }
    let url = reqwest::Url::parse(raw).map_err(|_| bad("not a link"))?;
    if !CALLBACK_SCHEMES.contains(&url.scheme()) || url.host_str() != Some("auth") {
        return Err(bad("wrong place"));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err(bad("unexpected parts"));
    }
    if !matches!(url.path(), "/callback" | "/callback/") {
        return Err(bad("wrong path"));
    }
    let mut params = BTreeMap::new();
    for (k, v) in url.query_pairs() {
        if !matches!(
            k.as_ref(),
            "code" | "error" | "error_code" | "error_description"
        ) {
            return Err(bad("unknown parameter"));
        }
        if params.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(bad("repeated parameter"));
        }
    }
    let one_line = |v: &str| {
        let ok = !v.trim().is_empty()
            && v.chars().count() <= MAX_ERROR_LEN
            && !v.chars().any(char::is_control);
        ok.then(|| v.trim().to_owned())
    };
    match (params.get("code"), params.get("error")) {
        (Some(code), None) => {
            let ok = !code.is_empty()
                && code.len() <= MAX_CODE_LEN
                && code
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c));
            if !ok || params.len() != 1 {
                return Err(bad("invalid code"));
            }
            Ok(Callback::Code(code.clone()))
        }
        (None, Some(error)) => {
            let error = one_line(error).ok_or_else(|| bad("invalid error"))?;
            let description = match params.get("error_description") {
                Some(d) => Some(one_line(d).ok_or_else(|| bad("invalid error"))?),
                None => None,
            };
            if params
                .get("error_code")
                .is_some_and(|c| one_line(c).is_none())
            {
                return Err(bad("invalid error"));
            }
            Ok(Callback::Error { error, description })
        }
        _ => Err(bad("needs a code or an error")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) fn cfg(base: &str) -> CloudConfig {
        CloudConfig {
            supabase_url: base.to_owned(),
            supabase_anon_key: "anon-key".into(),
            api_base: base.to_owned(),
        }
    }

    #[test]
    fn authorize_url_is_exact() {
        let url = authorize_url(&cfg("https://abc.supabase.co"), Provider::Google, "CHAL").unwrap();
        assert_eq!(
            url,
            "https://abc.supabase.co/auth/v1/authorize?provider=google&redirect_to=heed%3A%2F%2Fauth%2Fcallback&code_challenge=CHAL&code_challenge_method=s256"
        );
        assert!(
            matches!(Provider::parse("apple"), Err(CloudError::Invalid(m)) if m.contains("isn't available yet"))
        );
        assert_eq!(Provider::parse("google").unwrap(), Provider::Google);
        assert!(Provider::parse("github").is_err());
    }

    #[test]
    fn callbacks_parse_strictly_on_both_schemes() {
        assert_eq!(
            parse_callback("heed://auth/callback?code=abc").unwrap(),
            Callback::Code("abc".into())
        );
        assert_eq!(
            parse_callback("HEED://auth/callback?code=abc").unwrap(),
            Callback::Code("abc".into()),
            "schemes are case-insensitive"
        );
    }

    #[test]
    fn callbacks_parse_strictly() {
        assert_eq!(
            parse_callback("heed://auth/callback?code=0b5c-9f1e_A.~").unwrap(),
            Callback::Code("0b5c-9f1e_A.~".into())
        );
        let err = parse_callback(
            "heed://auth/callback?error=access_denied&error_code=otp_expired&error_description=Email+link+is+invalid+or+has+expired",
        )
        .unwrap();
        assert_eq!(
            err.error_message().as_deref(),
            Some("Email link is invalid or has expired")
        );
        assert_eq!(
            parse_callback("heed://auth/callback?error=server_error").unwrap(),
            Callback::Error {
                error: "server_error".into(),
                description: None
            }
        );
        for bad in [
            "heed://auth/callback",
            "heed://auth/callback?code=",
            "heed://auth/callback?code=a&code=b",
            "heed://auth/callback?code=a&error=b",
            "heed://auth/callback?code=a&next=/x",
            "heed://auth/callback?code=a%0Ab",
            "heed://auth/callback?code=a%20b",
            "heed://auth/callback?code=a#frag",
            "heed://auth/other?code=a",
            "heed://auth/callback/x?code=a",
            "heed://evil/callback?code=a",
            "heed://u:p@auth/callback?code=a",
            "heed://auth:1/callback?code=a",
            "https://auth/callback?code=a",
            "heedx://auth/callback?code=a",
            "heed://auth/callback?error=%0A",
            "heed://auth/callback?error=x&error_description=a%07b",
            "nugget://auth/callback?error=%0A",
            "nugget://auth/callback?error=x&error_description=a%07b",
        ] {
            assert!(parse_callback(bad).is_err(), "{bad}");
        }
        let long = format!("heed://auth/callback?code={}", "a".repeat(MAX_CODE_LEN + 1));
        assert!(parse_callback(&long).is_err());
        // The code never shows in debug output.
        let dbg = format!(
            "{:?}",
            parse_callback("heed://auth/callback?code=secret").unwrap()
        );
        assert!(!dbg.contains("secret"), "{dbg}");
    }

    /// A Supabase project still allow-listing the pre-rename `nugget://auth/callback` works.
    #[test]
    fn legacy_nugget_callbacks_are_accepted() {
        assert_eq!(
            parse_callback("nugget://auth/callback?code=abc").unwrap(),
            Callback::Code("abc".into())
        );
        assert!(parse_callback("nugget://auth/other?code=abc").is_err());
        assert!(parse_callback("heedx://auth/callback?code=abc").is_err());
    }

    pub(crate) fn token_body(access: &str, refresh: &str, expires_at: i64) -> Value {
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "expires_in": 3600,
            "expires_at": expires_at,
            "token_type": "bearer",
            "user": { "id": "u-1", "email": "me@example.com" }
        })
    }

    #[tokio::test]
    async fn magic_link_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/otp"))
            .and(query_param("redirect_to", REDIRECT_URL))
            .and(header("apikey", "anon-key"))
            .and(header("content-type", "application/json"))
            .and(body_json(json!({
                "email": "me@example.com",
                "create_user": true,
                "code_challenge": "CHAL",
                "code_challenge_method": "s256",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let auth = SupabaseAuth::new(cfg(&server.uri()), crate::test_http());
        auth.send_magic_link("me@example.com", "CHAL")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn exchange_refresh_and_logout() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .and(query_param("grant_type", "pkce"))
            .and(header("apikey", "anon-key"))
            .and(body_json(
                json!({ "auth_code": "CODE", "code_verifier": "VER" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_body("at", "rt", 99)))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .and(query_param("grant_type", "refresh_token"))
            .and(body_json(json!({ "refresh_token": "rt" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_body("at2", "rt2", 199)))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .and(query_param("grant_type", "refresh_token"))
            .and(body_json(json!({ "refresh_token": "revoked" })))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "code": 400, "error_code": "refresh_token_not_found", "msg": "Invalid Refresh Token"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/logout"))
            .and(header("authorization", "Bearer at2"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let auth = SupabaseAuth::new(cfg(&server.uri()), crate::test_http());
        let t = auth.exchange("CODE", "VER").await.unwrap();
        assert_eq!(
            (t.access_token.as_str(), t.refresh_token.as_str()),
            ("at", "rt")
        );
        assert_eq!(t.user.email.as_deref(), Some("me@example.com"));
        assert_eq!(t.expires_at, Some(99));
        assert!(!format!("{t:?}").contains("rt"));
        let t = auth.refresh("rt").await.unwrap();
        assert_eq!(t.access_token, "at2");
        assert!(matches!(
            auth.refresh("revoked").await,
            Err(CloudError::NotSignedIn)
        ));
        // A wrong code is a refusal with Supabase's message.
        match auth.exchange("WRONG", "VER").await {
            Err(CloudError::SignInFailed(_)) => {}
            other => panic!("{other:?}"),
        }
        auth.logout("at2").await.unwrap();
    }

    #[tokio::test]
    async fn server_errors_keep_the_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let auth = SupabaseAuth::new(cfg(&server.uri()), crate::test_http());
        assert!(matches!(
            auth.refresh("rt").await,
            Err(CloudError::Api { status: 503, .. })
        ));
    }
}
