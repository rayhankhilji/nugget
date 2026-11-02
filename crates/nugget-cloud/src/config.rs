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
                .unwrap_or_else(|| "nugget://connections/callback".into()),
        })
    }
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
        let err = Config::from_lookup(|_| None).unwrap_err();
        assert_eq!(err.0, "DATABASE_URL");
    }
}
