//! `nugget-plugin.toml`: what a plugin is and which capabilities it asks for.
//!
//! The manifest is untrusted input until its package signature has been checked, and even then
//! every field is validated (see [`Manifest::parse`]). Unknown fields are rejected, so a typo in a
//! capability cannot silently widen or narrow what the user approves.

use std::collections::BTreeSet;

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

/// File name of the manifest inside a package directory.
pub const MANIFEST_FILE: &str = "nugget-plugin.toml";
/// Largest accepted manifest.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Largest `capabilities.kv_max_bytes` a plugin may request.
pub const MAX_KV_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HOSTS: usize = 64;
const MAX_SECRETS: usize = 16;
/// Most `capabilities.file_roots` a plugin may declare.
pub const MAX_FILE_ROOTS: usize = 8;

/// What a plugin provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// Exports `nugget:plugin/connector`; runs as a source kind in the ingest pipeline.
    Connector,
    /// Exports `nugget:plugin/tools`; read-only tools for the agent.
    Tool,
}

/// How a plugin connector authenticates. Plugins have no interactive OAuth: a secret is set by
/// the user and read through `nugget:plugin/secrets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAuth {
    #[default]
    None,
    Password,
}

/// The `[connector]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSection {
    pub display_name: String,
    #[serde(default)]
    pub auth: ConnectorAuth,
    /// How often the pipeline polls, in seconds. Clamped to at least 60.
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
    /// JSON Schema of the source settings object.
    #[serde(default)]
    pub settings_schema: Option<Value>,
}

/// Declared capabilities. The host denies everything not listed here.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Hosts reachable through `nugget:plugin/http`: `example.com` (https, default port),
    /// `example.com:8443`, `*.example.com` (any subdomain, not the apex) or an explicit scheme
    /// such as `http://127.0.0.1:8080`. Plain `http` is allowed only when spelled out.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Names of the plugin's own keychain secrets, `[a-z0-9_-]`.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Maximum size of the plugin's key-value store, in bytes. `0` means no key-value store.
    #[serde(default)]
    pub kv_max_bytes: u64,
    /// Names of connector settings fields (`[a-z0-9_]`, 1 to 64 characters, at most
    /// [`MAX_FILE_ROOTS`]) that hold a path the user chose: a folder or a single file. The
    /// connector may read, through `nugget:plugin/fs`, only below those paths. Connectors only.
    ///
    /// Left out of the serialized form when empty, so approvals recorded before the field existed
    /// keep the same canonical JSON (and approval seal).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_roots: Vec<String>,
}

impl Capabilities {
    /// Whether every capability in `self` is also in `other` (so approving `other` covers
    /// `self`).
    pub fn is_subset_of(&self, other: &Capabilities) -> bool {
        let norm = |hosts: &[String]| -> BTreeSet<String> {
            hosts
                .iter()
                .map(|h| match HostRule::parse(h) {
                    Ok(rule) => rule.to_string(),
                    Err(_) => h.clone(),
                })
                .collect()
        };
        norm(&self.hosts).is_subset(&norm(&other.hosts))
            && self.secrets.iter().all(|s| other.secrets.contains(s))
            && self.kv_max_bytes <= other.kv_max_bytes
            && self.file_roots.iter().all(|r| other.file_roots.contains(r))
    }

    /// The parsed host rules. Fails on any malformed entry.
    pub fn host_rules(&self) -> Result<Vec<HostRule>> {
        self.hosts.iter().map(|h| HostRule::parse(h)).collect()
    }

    fn validate(&self) -> Result<()> {
        if self.hosts.len() > MAX_HOSTS {
            return Err(invalid(format!("more than {MAX_HOSTS} hosts")));
        }
        self.host_rules()?;
        if self.secrets.len() > MAX_SECRETS {
            return Err(invalid(format!("more than {MAX_SECRETS} secrets")));
        }
        let mut seen = BTreeSet::new();
        for s in &self.secrets {
            validate_secret_name(s)?;
            if !seen.insert(s) {
                return Err(invalid(format!("secret `{s}` listed twice")));
            }
        }
        if self.kv_max_bytes > MAX_KV_BYTES {
            return Err(invalid(format!(
                "kv_max_bytes {} exceeds the limit of {MAX_KV_BYTES}",
                self.kv_max_bytes
            )));
        }
        if self.file_roots.len() > MAX_FILE_ROOTS {
            return Err(invalid(format!("more than {MAX_FILE_ROOTS} file_roots")));
        }
        let mut seen = BTreeSet::new();
        for r in &self.file_roots {
            validate_file_root_name(r)?;
            if !seen.insert(r) {
                return Err(invalid(format!("file root `{r}` listed twice")));
            }
        }
        Ok(())
    }
}

/// A parsed `nugget-plugin.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Reverse-DNS id, e.g. `dev.nugget.example.rss`. `[a-z0-9-]` labels joined by dots, at
    /// least two labels, at most 64 characters.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    pub version: semver::Version,
    pub description: String,
    /// What the plugin provides. At least one entry, no duplicates.
    pub kind: Vec<PluginKind>,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Required when `kind` includes `connector`, forbidden otherwise.
    #[serde(default)]
    pub connector: Option<ConnectorSection>,
}

impl Manifest {
    /// Parse and validate manifest bytes. This is the only way to obtain a [`Manifest`] from
    /// untrusted input.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(invalid(format!(
                "manifest is larger than {MAX_MANIFEST_BYTES} bytes"
            )));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid("manifest is not UTF-8"))?;
        let manifest: Manifest =
            toml::from_str(text).map_err(|e| invalid(format!("manifest: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn provides(&self, kind: PluginKind) -> bool {
        self.kind.contains(&kind)
    }

    fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        validate_text("name", &self.name, 1, 80)?;
        validate_text("description", &self.description, 0, 2000)?;
        if self.kind.is_empty() {
            return Err(invalid("`kind` must list at least one of connector, tool"));
        }
        if self.kind.iter().collect::<BTreeSet<_>>().len() != self.kind.len() {
            return Err(invalid("`kind` lists an entry twice"));
        }
        self.capabilities.validate()?;
        match (&self.connector, self.provides(PluginKind::Connector)) {
            (None, true) => return Err(invalid("a connector plugin needs a [connector] section")),
            (_, false) if !self.capabilities.file_roots.is_empty() => {
                return Err(invalid(
                    "capabilities.file_roots is only allowed when kind has connector",
                ));
            }
            (Some(_), false) => {
                return Err(invalid(
                    "[connector] is only allowed when kind has connector",
                ));
            }
            (Some(c), true) => {
                validate_text("connector.display_name", &c.display_name, 1, 80)?;
                if let Some(schema) = &c.settings_schema
                    && !schema.is_object()
                {
                    return Err(invalid("connector.settings_schema must be a table"));
                }
                if let Some(props) = c.settings_schema.as_ref().and_then(|s| s.get("properties")) {
                    let props = props.as_object().ok_or_else(|| {
                        invalid("connector.settings_schema.properties must be a table")
                    })?;
                    if let Some(r) = self
                        .capabilities
                        .file_roots
                        .iter()
                        .find(|r| !props.contains_key(r.as_str()))
                    {
                        return Err(invalid(format!(
                            "file root `{r}` is not a property of connector.settings_schema"
                        )));
                    }
                }
                if c.auth == ConnectorAuth::Password && self.capabilities.secrets.is_empty() {
                    return Err(invalid(
                        "connector.auth = \"password\" needs a declared secret",
                    ));
                }
            }
            (None, false) => {}
        }
        Ok(())
    }
}

/// One allowed destination for `nugget:plugin/http`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    /// `https` or `http`.
    pub scheme: String,
    /// Lowercase host name or IP literal (IPv6 without brackets).
    pub host: String,
    /// Match any subdomain of `host` (not `host` itself).
    pub wildcard: bool,
    /// Explicit port. `None` means the scheme's default port.
    pub port: Option<u16>,
}

impl HostRule {
    /// Parse a manifest host entry.
    pub fn parse(entry: &str) -> Result<Self> {
        let bad = |why: &str| invalid(format!("host `{entry}`: {why}"));
        let entry = entry.trim();
        let (scheme, rest) = match entry.split_once("://") {
            Some((s, r)) => (s.to_ascii_lowercase(), r),
            None => ("https".to_string(), entry),
        };
        if scheme != "https" && scheme != "http" {
            return Err(bad("scheme must be https or http"));
        }
        if rest.is_empty() || rest.contains(['/', '@', '?', '#', '\\', ' ']) {
            return Err(bad(
                "must be a bare host[:port] without path or credentials",
            ));
        }
        let (wildcard, rest) = match rest.strip_prefix("*.") {
            Some(r) => (true, r),
            None => (false, rest),
        };
        if rest.contains('*') {
            return Err(bad("`*` is only allowed as a leading `*.` label"));
        }
        let url = Url::parse(&format!("{scheme}://{rest}/")).map_err(|e| bad(&e.to_string()))?;
        let host = match url.host() {
            // `matches` ignores a URL's trailing dot, so the rule must too or it never matches.
            // `matches` ignores one trailing dot on a URL's host, so the rule drops it too. A
            // name that is not a plain domain without it (`2..` would read as an IPv4 address,
            // `a..b` has an empty label) is refused rather than silently changing kind.
            Some(url::Host::Domain(d)) => {
                let d = d.strip_suffix('.').unwrap_or(d);
                if d.is_empty() || d.split('.').any(str::is_empty) {
                    return Err(bad("empty label in host name"));
                }
                match Url::parse(&format!("{scheme}://{d}/"))
                    .ok()
                    .and_then(|u| u.host().map(|h| matches!(h, url::Host::Domain(_))))
                {
                    Some(true) => d.to_string(),
                    _ => return Err(bad("host name is not a domain without its trailing dot")),
                }
            }
            Some(url::Host::Ipv4(ip)) if !wildcard => ip.to_string(),
            Some(url::Host::Ipv6(ip)) if !wildcard => ip.to_string(),
            Some(_) => return Err(bad("a wildcard needs a domain name")),
            None => return Err(bad("missing host")),
        };
        if wildcard && host.split('.').filter(|l| !l.is_empty()).count() < 2 {
            return Err(bad("a wildcard must cover a registered domain, not a TLD"));
        }
        Ok(Self {
            scheme,
            host,
            wildcard,
            port: url.port(),
        })
    }

    /// Whether `url` is a destination this rule allows.
    pub fn matches(&self, url: &Url) -> bool {
        if url.scheme() != self.scheme || !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        let host = match url.host() {
            Some(url::Host::Domain(d)) => d.to_ascii_lowercase(),
            Some(url::Host::Ipv4(ip)) => ip.to_string(),
            Some(url::Host::Ipv6(ip)) => ip.to_string(),
            None => return false,
        };
        let host = host.trim_end_matches('.');
        let host_ok = if self.wildcard {
            host.len() > self.host.len()
                && host.ends_with(&self.host)
                && host.as_bytes()[host.len() - self.host.len() - 1] == b'.'
        } else {
            host == self.host
        };
        // `Url::port()` is `None` when the port is the scheme default.
        host_ok && url.port() == self.port
    }
}

impl std::fmt::Display for HostRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://", self.scheme)?;
        if self.wildcard {
            f.write_str("*.")?;
        }
        if self.host.contains(':') {
            write!(f, "[{}]", self.host)?;
        } else {
            f.write_str(&self.host)?;
        }
        if let Some(p) = self.port {
            write!(f, ":{p}")?;
        }
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<()> {
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    if id.is_empty() || id.len() > 64 || id.split('.').count() < 2 || !id.split('.').all(label_ok) {
        return Err(invalid(format!(
            "plugin id `{id}` must be reverse-DNS: lowercase [a-z0-9-] labels joined by dots"
        )));
    }
    Ok(())
}

/// Secret names: `[a-z0-9_-]`, 1 to 64 characters.
pub(crate) fn validate_secret_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(invalid(format!(
            "secret name `{name}` must be 1-64 chars of [a-z0-9_-]"
        )));
    }
    Ok(())
}

/// File root names: `[a-z0-9_]`, 1 to 64 characters.
fn validate_file_root_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(invalid(format!(
            "file root `{name}` must be 1-64 chars of [a-z0-9_]"
        )));
    }
    Ok(())
}

fn validate_text(field: &str, s: &str, min: usize, max: usize) -> Result<()> {
    let n = s.chars().count();
    if n < min || n > max {
        return Err(invalid(format!("{field} must be {min}-{max} characters")));
    }
    if s.chars().any(|c| c.is_control() && c != '\n') {
        return Err(invalid(format!("{field} contains control characters")));
    }
    Ok(())
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}
