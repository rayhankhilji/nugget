//! Audit entries for MCP calls and the egress rule.
//!
//! Every `tools/call` and `resources/read` that reaches a tool produces one [`AuditEntry`] through
//! the server's audit callback. Calls refused before they run (rate limited, revoked client) are
//! audited too, coalesced to at most one entry per client and outcome per minute, carrying the
//! number of refusals coalesced since the previous one (`refused_since_last_entry`). Over HTTP a
//! rate-limited request is refused before its body (and so its method) is read, and is recorded
//! as [`ACTION_REFUSED`]; an HTTP request with a missing, wrong or revoked token names no client
//! and is not audited (it is answered 401). The daemon appends entries to the store's
//! hash-chained audit log. An entry names the
//! client, the tool, a **summary** of the arguments (short strings clipped, numbers as is), the
//! outcome and which items were returned (references only). It never contains returned content.
//!
//! Egress: a call from a remote client would also record an `EgressEvent` listing the returned
//! items. In M2 MCP is loopback/stdio only ([`Transport`] has no remote variant), so entries say
//! `"locality": "local"` and nothing is recorded in the egress ledger.

use std::sync::Arc;

use nugget_core::ids::ItemRef;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
const MAX_AUDITED_ITEMS: usize = 50;

/// How a client is connected. M2 has no remote transport.
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

    /// Whether results leave the device (and must be recorded in the egress ledger). Always
    /// `false` in M2.
    pub fn is_remote(self) -> bool {
        false
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
    pub fn to_entry(&self) -> AuditEntry {
        let items: Vec<String> = self
            .items
            .iter()
            .take(MAX_AUDITED_ITEMS)
            .map(|i| format!("{}/{}", i.source, i.key))
            .collect();
        AuditEntry {
            actor: format!("mcp:{}", self.client_id),
            action: self.action.to_owned(),
            target: clip(self.target, 200),
            detail: json!({
                "client_id": self.client_id,
                "client_name": self.client_name,
                "transport": self.transport.as_str(),
                "locality": if self.transport.is_remote() { "remote" } else { "local" },
                "args": self.args.map(summarize_args).unwrap_or(Value::Null),
                "outcome": self.outcome.as_str(),
                "results": self.results,
                "items": items,
                "items_truncated": self.items.len() > MAX_AUDITED_ITEMS,
            }),
        }
    }
}

/// Argument summary: strings clipped to 80 characters, numbers and booleans kept, anything
/// nested replaced by its type.
pub fn summarize_args(args: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (k, v) in args.iter().take(16) {
        let s = match v {
            Value::String(s) => Value::String(clip(s, MAX_ARG_CHARS)),
            Value::Number(_) | Value::Bool(_) | Value::Null => v.clone(),
            Value::Array(_) => Value::String("<array>".into()),
            Value::Object(_) => Value::String("<object>".into()),
        };
        out.insert(clip(k, 40), s);
    }
    Value::Object(out)
}

fn clip(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max + 1)
        .collect();
    if cleaned.chars().count() > max {
        let mut t: String = cleaned.chars().take(max).collect();
        t.push('…');
        t
    } else {
        cleaned
    }
}
