//! Audit entries for MCP calls.
//!
//! Every `tools/call` and `resources/read` that reaches a tool produces one [`AuditEntry`]. Calls
//! refused before they run (rate limited, revoked) are audited coalesced: at most one entry per
//! client and outcome per minute, carrying `refused_since_last_entry`. Over HTTP a rate-limited
//! request is refused before its body is read and recorded as [`ACTION_REFUSED`]; a request with
//! a missing, wrong or revoked token names no client and is not audited (401). Entries hold
//! argument summaries and item references, never returned content or free text the client sent
//! (see [`summarize_args`]). MCP is loopback/stdio only, so entries say `"locality": "local"` and
//! nothing reaches the egress ledger.

use std::sync::Arc;

use nugget_core::ids::ItemRef;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::auth::ServerKey;
use crate::render::inline;

/// One audit-log entry. Field for field the same as `nugget_store::AuditEntry` (which this crate
/// does not depend on), so the daemon converts it with a struct literal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// `mcp:<client id>`.
    pub actor: String,
    /// [`ACTION_TOOL`] or [`ACTION_RESOURCE`].
    pub action: String,
    /// Tool name or resource URI.
    pub target: String,
    pub detail: Value,
}

/// Receives one entry per call. Must not block for long (it runs on the request path).
pub type AuditSink = Arc<dyn Fn(AuditEntry) + Send + Sync>;

/// `action` of tool-call entries.
pub const ACTION_TOOL: &str = "mcp.tool_call";
/// `action` of resource-read entries.
pub const ACTION_RESOURCE: &str = "mcp.resource_read";
/// `action` of an HTTP request refused at the gate before its method was read (rate limited);
/// the target is `"http"`. Coalesced like every refusal entry.
pub const ACTION_REFUSED: &str = "mcp.request_refused";

const MAX_ARG_CHARS: usize = 80;
/// String arguments that name what a call is scoped to (an enum value, a source, an item key,
/// a timestamp), kept clipped. Every other string is free text and kept as length and hash only.
const SCOPE_ARGS: &[&str] = &["kind", "source", "key", "since"];
const MAX_AUDITED_ITEMS: usize = 50;

/// How a client is connected (local transports only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// `nuggetd mcp --client <id>` (or an in-process byte stream).
    Stdio,
    /// Streamable HTTP on 127.0.0.1.
    LoopbackHttp,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::LoopbackHttp => "loopback_http",
        }
    }
}

/// Outcome recorded in an audit entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    Ok,
    /// Not allowed by the client's scopes.
    Denied,
    /// Bad arguments or not found.
    Rejected,
    Error,
    /// Stopped before it finished: the client sent `notifications/cancelled`, or the session
    /// ended (end of input, disconnect, shutdown).
    Cancelled,
    /// Refused by the per-client rate limit before it ran (coalesced, see
    /// `server::REFUSAL_AUDIT_WINDOW`).
    RateLimited,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Rejected => "rejected",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::RateLimited => "rate_limited",
        }
    }
}

/// Everything an audit entry is built from.
#[derive(Debug, Clone)]
pub struct CallRecord<'a> {
    pub client_id: &'a str,
    pub client_name: &'a str,
    pub transport: Transport,
    pub action: &'static str,
    /// Tool name, or the resource URI for reads.
    pub target: &'a str,
    pub args: Option<&'a Map<String, Value>>,
    pub outcome: Outcome,
    pub results: usize,
    pub items: &'a [ItemRef],
}

impl CallRecord<'_> {
    /// `key` hashes free-text arguments; without one only their length is kept.
    pub fn to_entry(&self, key: Option<&ServerKey>) -> AuditEntry {
        let items: Vec<String> = self
            .items
            .iter()
            .take(MAX_AUDITED_ITEMS)
            .map(|i| format!("{}/{}", i.source, i.key))
            .collect();
        AuditEntry {
            actor: format!("mcp:{}", self.client_id),
            action: self.action.to_owned(),
            target: inline(self.target, 200),
            detail: json!({
                "client_id": self.client_id,
                "client_name": self.client_name,
                "transport": self.transport.as_str(),
                "locality": "local",
                "args": self.args.map(|a| summarize_args(a, key)).unwrap_or(Value::Null),
                "outcome": self.outcome.as_str(),
                "results": self.results,
                "items": items,
                "items_truncated": self.items.len() > MAX_AUDITED_ITEMS,
            }),
        }
    }
}

/// Argument summary: scope strings ([`SCOPE_ARGS`]) clipped to 80 characters, any other string
/// (a query, a search text, an id) replaced by `{"chars": n, "hmac": "<16 hex>"}` (keyed with
/// `key`; no `hmac` without one), numbers and booleans kept, anything nested replaced by its type.
pub fn summarize_args(args: &Map<String, Value>, key: Option<&ServerKey>) -> Value {
    let mut out = Map::new();
    for (k, v) in args.iter().take(16) {
        let s = match v {
            Value::String(s) if SCOPE_ARGS.contains(&k.as_str()) => {
                Value::String(inline(s, MAX_ARG_CHARS))
            }
            Value::String(s) => {
                let mut o = Map::new();
                o.insert("chars".into(), s.chars().count().into());
                if let Some(key) = key {
                    o.insert("hmac".into(), key.hash_audit_text(s).into());
                }
                Value::Object(o)
            }
            Value::Number(_) | Value::Bool(_) | Value::Null => v.clone(),
            Value::Array(_) => Value::String("<array>".into()),
            Value::Object(_) => Value::String("<object>".into()),
        };
        out.insert(inline(k, 40), s);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_args_keep_no_free_text() {
        let args = json!({"query": "GG8CANARY Acme renewal", "limit": 5, "kind": "person"});
        let args = args.as_object().unwrap();
        let key = ServerKey::from_bytes([7; 32]);
        let s = summarize_args(args, Some(&key));
        assert!(!s.to_string().contains("GG8CANARY"), "{s}");
        assert_eq!(s["query"]["chars"], 22);
        let h = s["query"]["hmac"].as_str().unwrap();
        assert_eq!(h.len(), 16);
        // Same text, same key: same hash; another key: another hash.
        assert_eq!(summarize_args(args, Some(&key))["query"]["hmac"], h);
        let other = ServerKey::from_bytes([8; 32]);
        assert_ne!(summarize_args(args, Some(&other))["query"]["hmac"], h);
        assert_eq!(s["limit"], 5);
        assert_eq!(s["kind"], "person");
        assert_eq!(summarize_args(args, None)["query"], json!({"chars": 22}));
    }
}
