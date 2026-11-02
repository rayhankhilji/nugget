//! Service configuration from the environment. Secrets are read here and nowhere else.

use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    /// PEM file of the CA that signs the database's certificate (Supabase's root CA).
    pub database_ca_cert: Option<String>,
    pub supabase_url: String,
    pub supabase_anon_key: String,
    pub supabase_service_role_key: Option<String>,
    pub composio_api_key: Option<String>,
    pub composio_base: String,
    /// Toolkit slug → Composio auth config id (`COMPOSIO_AUTH_CONFIGS=gmail=ac_1,...`).
    pub composio_auth_configs: BTreeMap<String, String>,
    pub composio_webhook_secret: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub anthropic_base: String,
    pub openai_api_key: Option<String>,
    pub openai_base: String,
    pub model_fast: Option<String>,
    pub model_smart: Option<String>,
    /// Where Composio sends the browser after consent.
    pub connect_callback: String,
    /// `json` (default) or `text` log lines.
    pub log_format: String,
    /// Internal listener for `/metrics` (not exposed publicly; Fly scrapes it).
    pub metrics_bind: Option<String>,
    pub sentry_dsn: Option<String>,
    pub sentry_environment: String,
    pub shutdown_grace_secs: u64,
    pub pool: crate::db::PoolOptions,
    pub limits: crate::ratelimit::Limits,
    /// Header carrying the real client IP from the edge proxy (`fly-client-ip`); empty means
    /// use the TCP peer address.
    pub client_ip_header: Option<String>,
    pub stripe_secret_key: Option<String>,
    pub stripe_webhook_secret: Option<String>,
    pub stripe_base: String,
    /// The service's public https base URL (Stripe returns the browser to `/billing/done`).
    pub public_url: Option<String>,
    /// Scheme of the desktop app's deep links (`nugget`, or `heed` after the rename).
    pub app_scheme: String,
    /// Website origins (`https://heed.app`) allowed for CORS and billing return URLs.
    pub site_origins: Vec<String>,
    /// Per-deploy secret that salts waitlist IP hashes; without it no IP hash is stored.
    pub waitlist_salt: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print secrets.
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("supabase_url", &self.supabase_url)
            .field("composio_toolkits", &self.composio_auth_configs.keys())
            .field("composio", &self.composio_api_key.is_some())
            .field("anthropic", &self.anthropic_api_key.is_some())
            .field("openai", &self.openai_api_key.is_some())
            .field("stripe", &self.stripe_secret_key.is_some())
            .field("public_url", &self.public_url)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("missing required environment variable {0}")]
pub struct MissingVar(pub &'static str);

impl Config {
    pub fn from_env() -> Result<Self, MissingVar> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, MissingVar> {
        let opt = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let req = |k: &'static str| opt(k).ok_or(MissingVar(k));
        Ok(Self {
            bind: opt("NUGGET_CLOUD_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()),
            database_url: req("DATABASE_URL")?,
            database_ca_cert: opt("DATABASE_CA_CERT"),
            supabase_url: req("SUPABASE_URL")?.trim_end_matches('/').to_owned(),
            supabase_anon_key: req("SUPABASE_ANON_KEY")?,
            supabase_service_role_key: opt("SUPABASE_SERVICE_ROLE_KEY"),
            composio_api_key: opt("COMPOSIO_API_KEY"),
            composio_base: opt("COMPOSIO_BASE_URL")
                .unwrap_or_else(|| "https://backend.composio.dev/api/v3.1".into()),
            composio_auth_configs: parse_auth_configs(
                &opt("COMPOSIO_AUTH_CONFIGS").unwrap_or_default(),
            ),
            composio_webhook_secret: opt("COMPOSIO_WEBHOOK_SECRET"),
            anthropic_api_key: opt("ANTHROPIC_API_KEY"),
            // Deliberately not ANTHROPIC_BASE_URL: tooling sets that for other purposes.
            anthropic_base: opt("NUGGET_ANTHROPIC_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".into()),
            openai_api_key: opt("OPENAI_API_KEY"),
            openai_base: opt("NUGGET_OPENAI_BASE_URL")
                .unwrap_or_else(|| "https://api.openai.com".into()),
            model_fast: opt("NUGGET_MODEL_FAST"),
            model_smart: opt("NUGGET_MODEL_SMART"),
            connect_callback: opt("NUGGET_CONNECT_CALLBACK")
                .unwrap_or_else(|| "heed://connections/callback".into()),
            log_format: opt("NUGGET_LOG_FORMAT").unwrap_or_else(|| "json".into()),
            metrics_bind: match opt("NUGGET_METRICS_BIND").as_deref() {
                Some("off") => None,
                Some(b) => Some(b.to_owned()),
                None => Some("0.0.0.0:9091".into()),
            },
            sentry_dsn: opt("SENTRY_DSN"),
            sentry_environment: opt("SENTRY_ENVIRONMENT").unwrap_or_else(|| "production".into()),
            shutdown_grace_secs: num(&opt, "NUGGET_SHUTDOWN_GRACE_SECS", 25),
            pool: crate::db::PoolOptions {
                max_size: num(&opt, "DATABASE_POOL_MAX", 8).max(1) as usize,
                timeout_secs: num(&opt, "DATABASE_POOL_TIMEOUT_SECS", 5).max(1),
                statement_timeout_ms: num(&opt, "DATABASE_STATEMENT_TIMEOUT_MS", 15_000),
            },
            limits: {
                let d = crate::ratelimit::Limits::default();
                let n = |k, v: u32| num(&opt, k, u64::from(v)).min(u64::from(u32::MAX)) as u32;
                crate::ratelimit::Limits {
                    ip_per_min: n("NUGGET_RATE_IP_PER_MIN", d.ip_per_min),
                    user_per_min: n("NUGGET_RATE_USER_PER_MIN", d.user_per_min),
                    ask_per_min: n("NUGGET_RATE_ASK_PER_MIN", d.ask_per_min),
                    sync_every_secs: n("NUGGET_SYNC_EVERY_SECS", d.sync_every_secs),
                    action_per_min: n("NUGGET_RATE_ACTION_PER_MIN", d.action_per_min),
                    waitlist_per_hour: n("NUGGET_RATE_WAITLIST_PER_HOUR", d.waitlist_per_hour),
                    llm_per_min: n("NUGGET_RATE_LLM_PER_MIN", d.llm_per_min),
                }
            },
            client_ip_header: match get("NUGGET_CLIENT_IP_HEADER") {
                Some(v) if v.trim().is_empty() => None,
                Some(v) => Some(v.trim().to_ascii_lowercase()),
                None => Some("fly-client-ip".into()),
            },
            stripe_secret_key: opt("STRIPE_SECRET_KEY"),
            stripe_webhook_secret: opt("STRIPE_WEBHOOK_SECRET"),
            stripe_base: opt("NUGGET_STRIPE_BASE_URL")
                .unwrap_or_else(|| "https://api.stripe.com".into()),
            public_url: opt("NUGGET_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_owned()),
            site_origins: parse_origins(&opt("NUGGET_SITE_ORIGINS").unwrap_or_default()),
            waitlist_salt: opt("NUGGET_WAITLIST_SALT"),
            app_scheme: opt("NUGGET_APP_SCHEME")
                .filter(|s| s.chars().all(|c| c.is_ascii_lowercase()))
                .unwrap_or_else(|| "heed".into()),
        })
    }
}

/// `https://heed.app, https://www.heed.app/` → normalised origins (scheme://host[:port]).
/// Entries that are not plain http(s) origins are dropped.
pub fn parse_origins(s: &str) -> Vec<String> {
    s.split(',')
        .filter_map(|o| {
            let u = url::Url::parse(o.trim()).ok()?;
            let plain = matches!(u.scheme(), "https" | "http")
                && u.host_str().is_some()
                && matches!(u.path(), "" | "/")
                && u.query().is_none()
                && u.username().is_empty();
            plain.then(|| u.origin().ascii_serialization())
        })
        .collect()
}

/// A non-negative integer variable, or `default` when unset or unparsable.
fn num(opt: &impl Fn(&str) -> Option<String>, k: &str, default: u64) -> u64 {
    opt(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// `gmail=ac_1, googlecalendar=ac_2` → map. Malformed pairs are ignored.
pub fn parse_auth_configs(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            (!k.is_empty() && !v.is_empty()).then(|| (k, v.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_configs_parse() {
        let m = parse_auth_configs(" Gmail=ac_1 ,googlecalendar=ac_2,bad,=x,y=");
        assert_eq!(m.len(), 2);
        assert_eq!(m["gmail"], "ac_1");
        assert_eq!(m["googlecalendar"], "ac_2");
    }

    #[test]
    fn origins_parse() {
        assert_eq!(
            parse_origins(
                " https://heed.app/ ,http://localhost:4321,https://x.dev/path,ftp://a.b,,nonsense"
            ),
            vec![
                "https://heed.app".to_owned(),
                "http://localhost:4321".to_owned()
            ]
        );
        assert!(parse_origins("").is_empty());
    }

    #[test]
    fn required_vars_and_defaults() {
        let env = |k: &str| match k {
            "DATABASE_URL" => Some("postgres://x".into()),
            "SUPABASE_URL" => Some("https://p.supabase.co/".into()),
            "SUPABASE_ANON_KEY" => Some("anon".into()),
            "OPENAI_API_KEY" => Some("  ".into()),
            _ => None,
        };
        let c = Config::from_lookup(env).unwrap();
        assert_eq!(c.supabase_url, "https://p.supabase.co");
        assert!(c.openai_api_key.is_none(), "blank means unset");
        assert!(c.composio_base.ends_with("/api/v3.1"));
        assert!(
            !format!("{c:?}").contains("anon"),
            "debug output hides secrets"
        );
        assert_eq!(c.log_format, "json");
        assert_eq!(c.metrics_bind.as_deref(), Some("0.0.0.0:9091"));
        assert_eq!(c.pool.max_size, 8);
        assert_eq!(c.limits, crate::ratelimit::Limits::default());
        assert_eq!(c.client_ip_header.as_deref(), Some("fly-client-ip"));
        let err = Config::from_lookup(|_| None).unwrap_err();
        assert_eq!(err.0, "DATABASE_URL");
    }
}
