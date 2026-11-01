//! Wire-protocol adapters and the helpers they share.

pub mod anthropic;
pub mod openai_compatible;
pub mod openai_responses;

use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use serde_json::{Map, Value};

use crate::config::{ProviderConfig, merge_extra_body};
use crate::http::error_detail_value;
use crate::types::{ChatEvent, StopReason};

/// `data:` URL for an inline image.
pub(crate) fn data_url(media_type: &str, data_base64: &str) -> String {
    format!("data:{media_type};base64,{data_base64}")
}

/// A part sent in the wrong role's message.
pub(crate) fn misplaced(in_user: bool) -> Error {
    Error::Invalid(if in_user {
        "tool calls belong in assistant messages".into()
    } else {
        "images and tool results belong in user messages".into()
    })
}

/// Tool-call arguments as the JSON string the OpenAI APIs expect (raw text kept as sent).
pub(crate) fn args_string(arguments: &Value) -> String {
    match arguments {
        Value::String(raw) => raw.clone(),
        other => other.to_string(),
    }
}

/// Tool-result text for APIs without an error flag.
pub(crate) fn result_text(content: &str, is_error: bool) -> String {
    if is_error {
        format!("[error] {content}")
    } else {
        content.to_string()
    }
}

pub(crate) fn as_u32(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32)
}

/// The finished request body: `extra_body` merged over ours.
pub(crate) fn finish_body(body: Map<String, Value>, cfg: &ProviderConfig) -> Value {
    let mut body = Value::Object(body);
    merge_extra_body(&mut body, cfg.extra_body.as_ref());
    body
}

/// Parse one stream event's JSON data.
pub(crate) fn parse_event(provider: &ProviderId, data: &str) -> Result<Value> {
    serde_json::from_str(data)
        .map_err(|e| Error::Provider(format!("{provider}: malformed stream event: {e}")))
}

/// An in-stream error event.
pub(crate) fn stream_error(provider: &ProviderId, v: &Value, data: &str) -> Error {
    let detail = error_detail_value(v).unwrap_or_else(|| data.to_string());
    Error::Provider(format!("{provider}: stream error: {detail}"))
}

/// Emit a tool call now, or hold it back when its arguments are not valid JSON (they may be
/// truncated; see [`release_deferred`]).
pub(crate) fn emit_tool_call(
    ev: ChatEvent,
    deferred: &mut Vec<ChatEvent>,
    out: &mut Vec<ChatEvent>,
) {
    if matches!(
        &ev,
        ChatEvent::ToolCall {
            arguments: Value::String(_),
            ..
        }
    ) {
        deferred.push(ev);
    } else {
        out.push(ev);
    }
}

/// Emit held-back tool calls, unless output was cut by the token limit (then they are dropped).
pub(crate) fn release_deferred(
    provider: &ProviderId,
    stop: &StopReason,
    deferred: &mut Vec<ChatEvent>,
    out: &mut Vec<ChatEvent>,
) {
    if *stop != StopReason::MaxTokens {
        out.append(deferred);
    } else if !deferred.is_empty() {
        tracing::warn!(%provider, "dropping tool call truncated by the output limit");
        deferred.clear();
    }
}
