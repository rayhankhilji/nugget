//! Anthropic Messages API adapter (`POST {base}/v1/messages`, streaming).
//!
//! - Tools use `eager_input_streaming` (default on); arguments are parsed when the block ends.
//! - Structured output: `output_config.format` json_schema with unsupported constraints stripped.
//! - Thinking / redacted-thinking blocks stream out as [`ChatEvent::Opaque`] and are replayed
//!   verbatim. A server-side refusal `fallback` block is surfaced as an opaque marker; non-text
//!   content before it is not replayed.
//! - `temperature` is only sent for `anthropic_version` before `2026-10-21` (rejected after).
//! - Prompt caching: a [`CacheHint`] with `system: true` puts `cache_control` on the system
//!   prompt (or the last tool without one); each [`Part::CacheBreakpoint`] on the block before
//!   it (never a thinking block). The API allows four breakpoints, so the system one and the last
//!   three message ones are kept.
//! - [`Provider::prewarm`] is `GET /v1/models`; [`Provider::prewarm_prefix`] sends the cacheable
//!   prefix with `max_tokens: 0`, `stream: false` (prefill only).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{Map, Value, json};

use crate::adapters::{
    as_u32, emit_tool_call, finish_body, misplaced, parse_event, release_deferred, stream_error,
};
use crate::config::{ANTHROPIC_DEFAULT_API_VERSION, ProviderConfig, Secret};
use crate::http::{SseDecoder, parse_tool_arguments};
use crate::provider::{Common, Prepared, Provider, auth_header};
use crate::schema;
use crate::sse::SseEvent;
#[cfg(doc)]
use crate::types::CacheHint;
use crate::types::{
    CacheTtl, ChatEvent, ChatRequest, ChatStream, Message, Part, ProviderKind, ProviderMeta, Role,
    StopReason, is_fallback_marker,
};

/// Used when neither the request nor the config sets an output cap (the API requires one).
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;
/// First API version that rejects sampling parameters.
const SAMPLING_REMOVED_IN: &str = "2026-10-21";
pub const MAX_CACHE_BREAKPOINTS: usize = 4;

pub struct AnthropicProvider {
    common: Common,
    api_version: String,
    eager_input_streaming: bool,
}

impl AnthropicProvider {
    pub(crate) fn new(
        cfg: &ProviderConfig,
        egress: Arc<dyn EgressSink>,
        key: Option<Secret>,
        prepared: Prepared,
    ) -> Result<Self> {
        let api_version = cfg
            .anthropic_version
            .clone()
            .unwrap_or_else(|| ANTHROPIC_DEFAULT_API_VERSION.to_string());
        let mut auth = auth_header("x-api-key", key, str::to_string)?;
        let version = HeaderValue::from_str(&api_version)
            .map_err(|_| Error::Invalid(format!("anthropic_version {api_version:?}")))?;
        auth.push((HeaderName::from_static("anthropic-version"), version));
        Ok(Self {
            common: Common::new(cfg, egress, auth, prepared)?,
            api_version,
            eager_input_streaming: cfg.eager_input_streaming.unwrap_or(true),
        })
    }
}

impl AnthropicProvider {
    /// Another model on the same endpoint and connection pool (Heed cloud's two tiers).
    pub(crate) fn sibling(&self, cfg: &ProviderConfig) -> Self {
        Self {
            common: self.common.sibling(cfg),
            api_version: self.api_version.clone(),
            eager_input_streaming: self.eager_input_streaming,
        }
    }
}

/// The `max_tokens: 0` cache-warming request for `req`'s stable prefix, or `None` when nothing
/// in it is marked cacheable.
pub(crate) fn build_prewarm_body(
    cfg: &ProviderConfig,
    api_version: &str,
    eager_input_streaming: bool,
    req: &ChatRequest,
) -> Result<Option<Value>> {
    let Some(prefix) = req.cache_prefix() else {
        return Ok(None);
    };
    let has_system = prefix
        .system
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    let cache_system =
        prefix.cache.as_ref().is_some_and(|c| c.system) && (has_system || !prefix.tools.is_empty());
    if !cache_system && !prefix.has_cache_breakpoints() {
        return Ok(None);
    }
    let mut body = build_body(cfg, api_version, eager_input_streaming, &prefix)?;
    body["max_tokens"] = json!(0);
    body["stream"] = json!(false);
    let obj = body.as_object_mut().expect("body is an object");
    // Rejected together with `max_tokens: 0`.
    if let Some(oc) = obj.get_mut("output_config").and_then(Value::as_object_mut) {
        oc.remove("format");
        if oc.is_empty() {
            obj.remove("output_config");
        }
    }
    if matches!(
        obj.get("tool_choice").and_then(|t| t["type"].as_str()),
        Some("any" | "tool")
    ) {
        obj.remove("tool_choice");
    }
    let messages = obj
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::Other("anthropic body has no messages".into()))?;
    if messages.last().and_then(|m| m["role"].as_str()) != Some("user") {
        // Read during prefill but never answered; sits after every breakpoint.
        messages.push(json!({"role": "user", "content": [{"type": "text", "text": "warmup"}]}));
    }
    Ok(Some(body))
}

pub(crate) fn build_body(
    cfg: &ProviderConfig,
    api_version: &str,
    eager_input_streaming: bool,
    req: &ChatRequest,
) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(cfg.model));
    let max = req
        .max_tokens
        .or(cfg.max_output_tokens)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    body.insert("max_tokens".into(), json!(max));
    body.insert("stream".into(), json!(true));
    let cc = match req.cache.as_ref().map(|c| c.ttl).unwrap_or_default() {
        CacheTtl::Default => json!({"type": "ephemeral"}),
        CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
    };
    let cache_system = req.cache.as_ref().is_some_and(|c| c.system);
    let system = req.system.as_deref().filter(|s| !s.trim().is_empty());
    if let Some(system) = system {
        let v = if cache_system {
            json!([{"type": "text", "text": system, "cache_control": cc}])
        } else {
            json!(system)
        };
        body.insert("system".into(), v);
    }
    let mut messages = convert_messages(&req.messages, &cc)?;
    limit_message_breakpoints(
        &mut messages,
        MAX_CACHE_BREAKPOINTS - usize::from(cache_system),
    );
    body.insert("messages".into(), Value::Array(messages));
    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({"name": t.name, "description": t.description, "input_schema": t.input_schema});
                if eager_input_streaming {
                    tool["eager_input_streaming"] = json!(true);
                }
                tool
            })
            .collect();
        if cache_system
            && system.is_none()
            && let Some(last) = tools.last_mut()
        {
            last["cache_control"] = cc.clone();
        }
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(s) = &req.json_schema {
        body.insert(
            "output_config".into(),
            json!({"format": {"type": "json_schema", "schema": schema::for_anthropic_output(s)}}),
        );
    }
    if let Some(t) = req.temperature {
        if api_version < SAMPLING_REMOVED_IN {
            body.insert("temperature".into(), json!(t));
        } else {
            tracing::debug!(provider = %cfg.id, "temperature not sent: unsupported by anthropic-version {api_version}");
        }
    }
    Ok(finish_body(body, cfg))
}

/// Blocks that may not carry `cache_control`.
fn uncacheable(block: &Value) -> bool {
    matches!(
        block["type"].as_str(),
        Some("thinking" | "redacted_thinking")
    )
}

/// Put `cache_control` on the last cacheable block emitted so far: in the current message, else
/// in earlier messages.
fn mark_breakpoint(blocks: &mut [Value], out: &mut [Value], cc: &Value) {
    let earlier = out
        .iter_mut()
        .rev()
        .filter_map(|m| m["content"].as_array_mut());
    let target = std::iter::once(&mut *blocks)
        .map(|b| b.iter_mut().rev().find(|b| !uncacheable(b)))
        .chain(earlier.map(|c| c.iter_mut().rev().find(|b| !uncacheable(b))))
        .flatten()
        .next();
    if let Some(b) = target {
        b["cache_control"] = cc.clone();
    }
}

/// Keep at most `max` message breakpoints, the latest ones.
fn limit_message_breakpoints(messages: &mut [Value], max: usize) {
    let mut seen = 0;
    let blocks = messages
        .iter_mut()
        .rev()
        .filter_map(|m| m["content"].as_array_mut())
        .flat_map(|c| c.iter_mut().rev());
    for obj in blocks.filter_map(Value::as_object_mut) {
        if obj.contains_key("cache_control") {
            seen += 1;
            if seen > max {
                obj.remove("cache_control");
            }
        }
    }
}

fn convert_messages(messages: &[Message], cc: &Value) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let mut blocks: Vec<Value> = Vec::new();
        let user = m.role == Role::User;
        // tool_result blocks must lead the user turn.
        if user {
            for p in &m.content {
                if let Part::ToolResult {
                    call_id,
                    content,
                    is_error,
                } = p
                {
                    let mut b =
                        json!({"type": "tool_result", "tool_use_id": call_id, "content": content});
                    if *is_error {
                        b["is_error"] = json!(true);
                    }
                    blocks.push(b);
                }
            }
        }
        // Assistant content before a fallback marker that is not text belongs to the declined
        // attempt.
        let cut = m.content.iter().rposition(
            |p| matches!(p, Part::Opaque { kind, data } if is_fallback_marker(*kind, data)),
        );
        for (i, p) in m.content.iter().enumerate() {
            let replay = cut.is_none_or(|c| i >= c);
            match p {
                Part::Text { text } if !text.is_empty() => {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                Part::Image {
                    media_type,
                    data_base64,
                } if user => blocks.push(json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": data_base64},
                })),
                Part::ToolCall {
                    id,
                    name,
                    arguments,
                } if !user && replay => {
                    let input = if arguments.is_object() {
                        arguments.clone()
                    } else {
                        json!({})
                    };
                    blocks
                        .push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
                }
                Part::Opaque {
                    kind: ProviderKind::Anthropic,
                    data,
                } if !user && replay && !is_fallback_marker(ProviderKind::Anthropic, data) => {
                    blocks.push(data.clone());
                }
                Part::ToolCall { .. } if user => return Err(misplaced(true)),
                Part::Image { .. } | Part::ToolResult { .. } if !user => {
                    return Err(misplaced(false));
                }
                Part::CacheBreakpoint => mark_breakpoint(&mut blocks, &mut out, cc),
                _ => {}
            }
        }
        if !blocks.is_empty() {
            let role = if user { "user" } else { "assistant" };
            out.push(json!({"role": role, "content": blocks}));
        }
    }
    Ok(out)
}

enum Block {
    Text,
    Tool {
        id: String,
        name: String,
        json: String,
        initial: Value,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    Verbatim(Value),
    Ignored,
}

pub(crate) struct Decoder {
    provider: ProviderId,
    blocks: HashMap<u64, Block>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cache_read: Option<u32>,
    cache_write: Option<u32>,
    stop_reason: Option<String>,
    /// Tool calls whose arguments were not valid JSON (see [`emit_tool_call`]).
    deferred: Vec<ChatEvent>,
    finished: bool,
}

impl Decoder {
    pub(crate) fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            blocks: HashMap::new(),
            input_tokens: None,
            output_tokens: None,
            cache_read: None,
            cache_write: None,
            stop_reason: None,
            deferred: Vec::new(),
            finished: false,
        }
    }

    fn done(&mut self) -> Vec<ChatEvent> {
        let mut out = Vec::new();
        if std::mem::replace(&mut self.finished, true) {
            return out;
        }
        let stop = StopReason::from_wire(self.stop_reason.as_deref().unwrap_or("end_turn"));
        release_deferred(&self.provider, &stop, &mut self.deferred, &mut out);
        out.push(ChatEvent::Done {
            stop_reason: stop,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        });
        out
    }

    fn usage(&mut self, usage: &Value) {
        let n = |k: &str| as_u32(usage.get(k));
        let parts = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ];
        if let Some(i) = parts
            .iter()
            .filter_map(|k| usage.get(*k)?.as_u64())
            .reduce(|a, b| a + b)
        {
            self.input_tokens = Some(i.min(u32::MAX as u64) as u32);
        }
        self.output_tokens = n("output_tokens").or(self.output_tokens);
        self.cache_read = n("cache_read_input_tokens").or(self.cache_read);
        self.cache_write = n("cache_creation_input_tokens").or(self.cache_write);
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn text(t: &str, out: &mut Vec<ChatEvent>) {
    if !t.is_empty() {
        out.push(ChatEvent::TextDelta(t.to_string()));
    }
}

fn opaque(data: Value) -> ChatEvent {
    ChatEvent::Opaque {
        kind: ProviderKind::Anthropic,
        data,
    }
}

impl SseDecoder for Decoder {
    fn on_event(&mut self, ev: SseEvent) -> Result<Vec<ChatEvent>> {
        if ev.data.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v = parse_event(&self.provider, &ev.data)?;
        let ty = v
            .get("type")
            .and_then(Value::as_str)
            .or(ev.event.as_deref())
            .unwrap_or_default();
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
        let mut out = Vec::new();
        match ty {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.usage(u);
                }
            }
            "content_block_start" => {
                let cb = v.get("content_block").cloned().unwrap_or(Value::Null);
                let block = match str_of(&cb, "type") {
                    "text" => {
                        text(str_of(&cb, "text"), &mut out);
                        Block::Text
                    }
                    "tool_use" => Block::Tool {
                        id: str_of(&cb, "id").to_string(),
                        name: str_of(&cb, "name").to_string(),
                        json: String::new(),
                        initial: cb.get("input").cloned().unwrap_or_else(|| json!({})),
                    },
                    "thinking" => Block::Thinking {
                        thinking: str_of(&cb, "thinking").to_string(),
                        signature: str_of(&cb, "signature").to_string(),
                    },
                    "redacted_thinking" => Block::Verbatim(cb),
                    "fallback" => {
                        out.push(opaque(cb));
                        // Content before this point belonged to a declined attempt.
                        self.deferred.clear();
                        Block::Ignored
                    }
                    _ => Block::Ignored,
                };
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                match (str_of(&d, "type"), self.blocks.get_mut(&index)) {
                    ("text_delta", _) => text(str_of(&d, "text"), &mut out),
                    ("input_json_delta", Some(Block::Tool { json, .. })) => {
                        json.push_str(str_of(&d, "partial_json"));
                    }
                    ("thinking_delta", Some(Block::Thinking { thinking, .. })) => {
                        thinking.push_str(str_of(&d, "thinking"));
                    }
                    ("signature_delta", Some(Block::Thinking { signature, .. })) => {
                        signature.push_str(str_of(&d, "signature"));
                    }
                    _ => {}
                }
            }
            "content_block_stop" => match self.blocks.remove(&index) {
                Some(Block::Tool {
                    id,
                    name,
                    json,
                    initial,
                }) => {
                    let arguments = if json.trim().is_empty() {
                        initial
                    } else {
                        parse_tool_arguments(&self.provider, &name, &json)
                    };
                    let call = ChatEvent::ToolCall {
                        id,
                        name,
                        arguments,
                    };
                    emit_tool_call(call, &mut self.deferred, &mut out);
                }
                Some(Block::Thinking {
                    thinking,
                    signature,
                }) => out.push(opaque(
                    json!({"type": "thinking", "thinking": thinking, "signature": signature}),
                )),
                Some(Block::Verbatim(data)) => out.push(opaque(data)),
                _ => {}
            },
            "message_delta" => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(r.to_string());
                }
                if let Some(u) = v.get("usage") {
                    self.usage(u);
                }
            }
            "message_stop" => out.extend(self.done()),
            "error" => return Err(stream_error(&self.provider, &v, &ev.data)),
            _ => {} // ping and future event types
        }
        Ok(out)
    }

    fn cache_usage(&self) -> (Option<u32>, Option<u32>) {
        (self.cache_read, self.cache_write)
    }

    fn on_eof(&mut self) -> Result<Vec<ChatEvent>> {
        // Tolerate a missing `message_stop` once the stop reason is known.
        Ok(if self.stop_reason.is_some() {
            self.done()
        } else {
            Vec::new()
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &ProviderId {
        &self.common.id
    }

    fn meta(&self) -> ProviderMeta {
        self.common.meta.clone()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let body = build_body(
            &self.common.cfg,
            &self.api_version,
            self.eager_input_streaming,
            &req,
        )?;
        let decoder = Decoder::new(self.common.id.clone());
        self.common
            .stream("v1/messages", &body, &req, decoder)
            .await
    }

    async fn prewarm(&self) -> Result<()> {
        self.common.prewarm_get("v1/models").await
    }

    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        let cfg = &self.common.cfg;
        match build_prewarm_body(cfg, &self.api_version, self.eager_input_streaming, req)? {
            Some(body) => self.common.prewarm_post("v1/messages", &body, req).await,
            None => self.prewarm().await,
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported(format!(
            "{}: the Anthropic API has no embeddings endpoint",
            self.common.id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolSpec;

    fn cfg() -> ProviderConfig {
        ProviderConfig::from_toml_str("id='a'\nkind='anthropic'\nmodel='test-model'").unwrap()
    }

    fn decode(events: &[(&str, Value)]) -> Result<Vec<ChatEvent>> {
        let mut d = Decoder::new(ProviderId::new("a"));
        let mut out = Vec::new();
        for (name, data) in events {
            out.extend(d.on_event(SseEvent {
                event: Some(name.to_string()),
                data: data.to_string(),
                id: None,
            })?);
        }
        Ok(out)
    }

    #[test]
    fn body_shape() {
        let opaque = |kind, data| Part::Opaque { kind, data };
        let mut req = ChatRequest::new(vec![
            Message::user(vec![Part::text("hi"), Part::tool_result("t0", "ok")]),
            Message::new(
                Role::Assistant,
                vec![
                    opaque(
                        ProviderKind::Anthropic,
                        json!({"type": "thinking", "signature": "s"}),
                    ),
                    opaque(ProviderKind::OpenaiResponses, json!({"type": "reasoning"})),
                    Part::text("calling"),
                ],
            ),
        ])
        .with_tools(vec![ToolSpec {
            name: "f".into(),
            description: "d".into(),
            input_schema: json!({}),
        }])
        .with_json_schema(
            json!({"type": "object", "properties": {"n": {"type": "integer", "minimum": 1}}}),
        );
        req.temperature = Some(0.2);
        let b = build_body(&cfg(), "2026-10-21", true, &req).unwrap();
        assert_eq!(
            b["messages"][0]["content"][0]["type"], "tool_result",
            "tool results lead"
        );
        let asst = b["messages"][1]["content"].as_array().unwrap();
        assert_eq!(
            (asst.len(), &asst[0]["type"]),
            (2, &json!("thinking")),
            "other kinds' state dropped"
        );
        let schema = &b["output_config"]["format"]["schema"];
        assert!(schema["properties"]["n"].get("minimum").is_none());
        assert_eq!(schema["additionalProperties"], false);

        // Older API versions take a temperature; eager streaming can be turned off.
        let b = build_body(&cfg(), "2023-06-01", false, &req).unwrap();
        assert!((b["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert!(b["tools"][0].get("eager_input_streaming").is_none());
    }

    #[test]
    fn cache_breakpoints_are_capped_and_skip_thinking() {
        use crate::types::CacheHint;
        let mut content = Vec::new();
        for i in 0..5 {
            content.push(Part::text(format!("t{i}")));
            content.push(Part::CacheBreakpoint);
        }
        let req = ChatRequest::new(vec![
            Message::user(content),
            Message::new(
                Role::Assistant,
                vec![
                    Part::text("before"),
                    Part::Opaque {
                        kind: ProviderKind::Anthropic,
                        data: json!({"type": "thinking", "thinking": "", "signature": "s"}),
                    },
                    Part::CacheBreakpoint,
                ],
            ),
        ])
        .with_system("sys")
        .with_cache(CacheHint::default());
        let b = build_body(&cfg(), "2026-10-21", true, &req).unwrap();
        let marked = |v: &Value| v.get("cache_control").is_some();
        assert!(marked(&b["system"][0]));
        let user = b["messages"][0]["content"].as_array().unwrap();
        let asst = b["messages"][1]["content"].as_array().unwrap();
        // System + the latest three message breakpoints = the API's limit of four.
        assert_eq!(
            user.iter().map(marked).collect::<Vec<_>>(),
            vec![false, false, false, true, true]
        );
        assert!(marked(&asst[0]), "skips back past the thinking block");
        assert!(!marked(&asst[1]));
        let total = 1 + user.iter().chain(asst).filter(|v| marked(v)).count();
        assert_eq!(total, MAX_CACHE_BREAKPOINTS);
    }

    #[test]
    fn replay_drops_declined_partial_before_fallback() {
        let req = ChatRequest::new(vec![Message::new(
            Role::Assistant,
            vec![
                Part::Opaque {
                    kind: ProviderKind::Anthropic,
                    data: json!({"type": "thinking", "thinking": "", "signature": "s"}),
                },
                Part::text("partial"),
                Part::Opaque {
                    kind: ProviderKind::Anthropic,
                    data: json!({"type": "fallback"}),
                },
                Part::text(" rest"),
            ],
        )]);
        let b = build_body(&cfg(), "2026-10-21", true, &req).unwrap();
        let content = b["messages"][0]["content"].as_array().unwrap().clone();
        assert_eq!(
            content,
            vec![
                json!({"type": "text", "text": "partial"}),
                json!({"type": "text", "text": " rest"})
            ]
        );
    }

    #[test]
    fn extra_body_merges() {
        let mut c = cfg();
        c.extra_body = Some(json!({"fallbacks": "default", "max_tokens": 5}));
        let b = build_body(&c, "2026-10-21", true, &ChatRequest::default()).unwrap();
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["max_tokens"], 5);
    }

    #[test]
    fn truncated_tool_call_is_dropped_at_max_tokens() {
        let out = decode(&[
            ("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "f", "input": {}}})),
            ("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"a\": \"tru"}})),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            ("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}, "usage": {"output_tokens": 5}})),
            ("message_stop", json!({"type": "message_stop"})),
        ])
        .unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            ChatEvent::Done {
                stop_reason: StopReason::MaxTokens,
                ..
            }
        ));
    }

    #[test]
    fn invalid_tool_json_without_truncation_is_passed_raw() {
        let out = decode(&[
            ("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "f", "input": {}}})),
            ("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{bad"}})),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            ("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}})),
            ("message_stop", json!({"type": "message_stop"})),
        ])
        .unwrap();
        assert_eq!(
            out[0],
            ChatEvent::ToolCall {
                id: "t".into(),
                name: "f".into(),
                arguments: Value::String("{bad".into())
            }
        );
    }

    #[test]
    fn refusal_and_fallback_marker() {
        let out = decode(&[
            ("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "fallback", "from": {"model": "x"}, "to": {"model": "y"}}})),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            ("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}})),
            ("message_stop", json!({"type": "message_stop"})),
        ])
        .unwrap();
        assert!(matches!(&out[0], ChatEvent::Opaque { data, .. } if data["type"] == "fallback"));
        assert!(matches!(
            out[1],
            ChatEvent::Done {
                stop_reason: StopReason::Refusal,
                ..
            }
        ));
    }

    #[test]
    fn eof_handling() {
        let mut d = Decoder::new(ProviderId::new("a"));
        assert!(d.on_eof().unwrap().is_empty());
        d.on_event(SseEvent {
            event: None,
            data: json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}})
                .to_string(),
            id: None,
        })
        .unwrap();
        assert_eq!(d.on_eof().unwrap().len(), 1);
    }
}
