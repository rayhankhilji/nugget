//! Anthropic Messages API adapter (`POST {base}/v1/messages`, streaming).
//!
//! - Images: base64 `image` blocks. Tools: client tools with `eager_input_streaming` (on by
//!   default; arguments are parsed strictly when the block completes).
//! - Structured output: `output_config.format = {type: "json_schema", schema}`; unsupported
//!   constraints are stripped (validate client-side with [`crate::schema::validate`]).
//! - Thinking / redacted-thinking blocks stream out as [`ChatEvent::Opaque`] and are replayed
//!   verbatim, so multi-turn tool loops keep reasoning ("preserved thinking").
//! - Server-side refusal fallbacks (enabled via `extra_body` + `headers`) are handled: the
//!   `fallback` block is surfaced as an opaque marker and non-text content before it is not
//!   replayed.
//! - `temperature` is rejected by API version `2026-10-21` and later, so it is only sent when an
//!   older `anthropic_version` is configured.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{Map, Value, json};

use crate::config::{ANTHROPIC_DEFAULT_API_VERSION, ProviderConfig, Secret, merge_extra_body};
use crate::http::{SseDecoder, error_detail_value, parse_tool_arguments, sse_chat_stream};
use crate::provider::{Common, Prepared, Provider, auth_header};
use crate::schema;
use crate::sse::SseEvent;
use crate::types::{
    ChatEvent, ChatRequest, ChatStream, Message, Part, ProviderKind, ProviderMeta, Role,
    StopReason, is_fallback_marker,
};

/// Used when neither the request nor the config sets an output cap (the API requires one).
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;
/// First API version that rejects sampling parameters.
const SAMPLING_REMOVED_IN: &str = "2026-10-21";

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

    fn body(&self, req: &ChatRequest) -> Result<Value> {
        build_body(
            &self.common.cfg,
            &self.api_version,
            self.eager_input_streaming,
            req,
        )
    }
}

pub(crate) fn build_body(
    cfg: &ProviderConfig,
    api_version: &str,
    eager_input_streaming: bool,
    req: &ChatRequest,
) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(cfg.model));
    body.insert(
        "max_tokens".into(),
        json!(
            req.max_tokens
                .or(cfg.max_output_tokens)
                .unwrap_or(DEFAULT_MAX_TOKENS)
        ),
    );
    body.insert("stream".into(), json!(true));
    if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
        body.insert("system".into(), json!(system));
    }
    body.insert(
        "messages".into(),
        Value::Array(convert_messages(&req.messages)?),
    );
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                });
                if eager_input_streaming {
                    tool["eager_input_streaming"] = json!(true);
                }
                tool
            })
            .collect();
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
    let mut body = Value::Object(body);
    merge_extra_body(&mut body, cfg.extra_body.as_ref());
    Ok(body)
}

fn convert_messages(messages: &[Message]) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let mut blocks: Vec<Value> = Vec::new();
        match m.role {
            Role::User => {
                // tool_result blocks must lead the user turn.
                for p in &m.content {
                    if let Part::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } = p
                    {
                        let mut b = json!({"type": "tool_result", "tool_use_id": call_id, "content": content});
                        if *is_error {
                            b["is_error"] = json!(true);
                        }
                        blocks.push(b);
                    }
                }
                for p in &m.content {
                    match p {
                        Part::Text { text } if !text.is_empty() => {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                        Part::Image {
                            media_type,
                            data_base64,
                        } => blocks.push(json!({
                            "type": "image",
                            "source": {"type": "base64", "media_type": media_type, "data": data_base64},
                        })),
                        Part::ToolCall { .. } => {
                            return Err(Error::Invalid(
                                "tool calls belong in assistant messages".into(),
                            ));
                        }
                        _ => {}
                    }
                }
            }
            Role::Assistant => {
                let cut = m.content.iter().rposition(
                    |p| matches!(p, Part::Opaque { kind, data } if is_fallback_marker(*kind, data)),
                );
                for (i, p) in m.content.iter().enumerate() {
                    let before_fallback = cut.is_some_and(|c| i < c);
                    match p {
                        Part::Text { text } if !text.is_empty() => {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                        Part::ToolCall {
                            id,
                            name,
                            arguments,
                        } if !before_fallback => {
                            let input = if arguments.is_object() {
                                arguments.clone()
                            } else {
                                json!({})
                            };
                            blocks.push(
                                json!({"type": "tool_use", "id": id, "name": name, "input": input}),
                            );
                        }
                        Part::Opaque {
                            kind: ProviderKind::Anthropic,
                            data,
                        } if !before_fallback
                            && !is_fallback_marker(ProviderKind::Anthropic, data) =>
                        {
                            blocks.push(data.clone());
                        }
                        Part::Image { .. } | Part::ToolResult { .. } => {
                            return Err(Error::Invalid(
                                "images and tool results belong in user messages".into(),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        if blocks.is_empty() {
            continue;
        }
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        out.push(json!({"role": role, "content": blocks}));
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
    stop_reason: Option<String>,
    /// Tool calls whose arguments were not valid JSON; emitted at the end unless the output was
    /// truncated by `max_tokens` (then they are incomplete and dropped).
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
            stop_reason: None,
            deferred: Vec::new(),
            finished: false,
        }
    }

    fn done(&mut self) -> Vec<ChatEvent> {
        if std::mem::replace(&mut self.finished, true) {
            return Vec::new();
        }
        let stop = StopReason::from_wire(self.stop_reason.as_deref().unwrap_or("end_turn"));
        let mut out = Vec::new();
        if stop == StopReason::MaxTokens {
            if !self.deferred.is_empty() {
                tracing::warn!(provider = %self.provider, "dropping tool call truncated by max_tokens");
            }
        } else {
            out.append(&mut self.deferred);
        }
        out.push(ChatEvent::Done {
            stop_reason: stop,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        });
        out
    }

    fn usage(&mut self, usage: &Value) {
        let n = |k: &str| usage.get(k).and_then(Value::as_u64);
        let input = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .iter()
        .filter_map(|k| n(k))
        .reduce(|a, b| a + b);
        if let Some(i) = input {
            self.input_tokens = Some(i.min(u32::MAX as u64) as u32);
        }
        if let Some(o) = n("output_tokens") {
            self.output_tokens = Some(o.min(u32::MAX as u64) as u32);
        }
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

impl SseDecoder for Decoder {
    fn on_event(&mut self, ev: SseEvent) -> Result<Vec<ChatEvent>> {
        if ev.data.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(&ev.data).map_err(|e| {
            Error::Provider(format!("{}: malformed stream event: {e}", self.provider))
        })?;
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
                        let t = str_of(&cb, "text");
                        if !t.is_empty() {
                            out.push(ChatEvent::TextDelta(t.to_string()));
                        }
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
                        out.push(ChatEvent::Opaque {
                            kind: ProviderKind::Anthropic,
                            data: cb,
                        });
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
                    ("text_delta", _) => {
                        let t = str_of(&d, "text");
                        if !t.is_empty() {
                            out.push(ChatEvent::TextDelta(t.to_string()));
                        }
                    }
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
                    let ev = ChatEvent::ToolCall {
                        id,
                        name,
                        arguments,
                    };
                    if matches!(
                        &ev,
                        ChatEvent::ToolCall {
                            arguments: Value::String(_),
                            ..
                        }
                    ) {
                        self.deferred.push(ev);
                    } else {
                        out.push(ev);
                    }
                }
                Some(Block::Thinking {
                    thinking,
                    signature,
                }) => out.push(ChatEvent::Opaque {
                    kind: ProviderKind::Anthropic,
                    data: json!({"type": "thinking", "thinking": thinking, "signature": signature}),
                }),
                Some(Block::Verbatim(data)) => out.push(ChatEvent::Opaque {
                    kind: ProviderKind::Anthropic,
                    data,
                }),
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
            "error" => {
                let detail = error_detail_value(&v).unwrap_or_else(|| ev.data.clone());
                return Err(Error::Provider(format!(
                    "{}: stream error: {detail}",
                    self.provider
                )));
            }
            _ => {} // ping and future event types
        }
        Ok(out)
    }

    fn on_eof(&mut self) -> Result<Vec<ChatEvent>> {
        // Tolerate a missing `message_stop` once the stop reason is known.
        if self.stop_reason.is_some() {
            Ok(self.done())
        } else {
            Ok(Vec::new())
        }
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
        let body = self.body(&req)?;
        let (resp, meter) = self
            .common
            .http
            .post_json("v1/messages", &body, &req.feature, &req.items, true)
            .await?;
        Ok(sse_chat_stream(
            self.common.id.clone(),
            resp,
            meter,
            Decoder::new(self.common.id.clone()),
        ))
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
        let req = ChatRequest::new(vec![
            Message::user(vec![
                Part::text("hi"),
                Part::image("image/png", "AAAA"),
                Part::tool_result("t0", "ok"),
            ]),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Opaque {
                        kind: ProviderKind::Anthropic,
                        data: json!({"type": "thinking", "thinking": "", "signature": "s"}),
                    },
                    Part::Opaque {
                        kind: ProviderKind::OpenaiResponses,
                        data: json!({"type": "reasoning"}),
                    },
                    Part::text("calling"),
                    Part::ToolCall {
                        id: "t1".into(),
                        name: "f".into(),
                        arguments: json!({"x": 1}),
                    },
                ],
            ),
        ])
        .with_system("sys")
        .with_tools(vec![ToolSpec {
            name: "f".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        }])
        .with_json_schema(
            json!({"type": "object", "properties": {"n": {"type": "integer", "minimum": 1}}}),
        );
        let mut req = req;
        req.temperature = Some(0.2);
        let b = build_body(&cfg(), "2026-10-21", true, &req).unwrap();
        assert_eq!(b["model"], "test-model");
        assert_eq!(b["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(b["stream"], true);
        assert_eq!(b["system"], "sys");
        assert!(b.get("temperature").is_none());
        let user = &b["messages"][0]["content"];
        assert_eq!(user[0]["type"], "tool_result");
        assert_eq!(user[1], json!({"type": "text", "text": "hi"}));
        assert_eq!(user[2]["source"]["type"], "base64");
        let asst = &b["messages"][1]["content"];
        assert_eq!(asst.as_array().unwrap().len(), 3);
        assert_eq!(asst[0]["type"], "thinking");
        assert_eq!(asst[2]["type"], "tool_use");
        assert_eq!(b["tools"][0]["eager_input_streaming"], true);
        let fmt = &b["output_config"]["format"];
        assert_eq!(fmt["type"], "json_schema");
        assert!(fmt["schema"]["properties"]["n"].get("minimum").is_none());
        assert_eq!(fmt["schema"]["additionalProperties"], false);

        let b = build_body(&cfg(), "2023-06-01", false, &req).unwrap();
        assert!((b["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert!(b["tools"][0].get("eager_input_streaming").is_none());
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
    fn decodes_thinking_text_and_tools() {
        let out = decode(&[
            ("message_start", json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_read_input_tokens": 5, "output_tokens": 1}}})),
            ("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}})),
            ("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}})),
            ("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "SIG"}})),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            ("ping", json!({"type": "ping"})),
            ("content_block_start", json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}})),
            ("content_block_delta", json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Hi"}})),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 1})),
            ("content_block_start", json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "get", "input": {}}})),
            ("content_block_delta", json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"q\": \"pa"}})),
            ("content_block_delta", json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "ris\"}"}})),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 2})),
            ("content_block_start", json!({"type": "content_block_start", "index": 3, "content_block": {"type": "tool_use", "id": "toolu_2", "name": "noargs", "input": {}}})),
            ("content_block_stop", json!({"type": "content_block_stop", "index": 3})),
            ("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 42}})),
            ("message_stop", json!({"type": "message_stop"})),
        ])
        .unwrap();
        assert_eq!(
            out,
            vec![
                ChatEvent::Opaque {
                    kind: ProviderKind::Anthropic,
                    data: json!({"type": "thinking", "thinking": "hmm", "signature": "SIG"})
                },
                ChatEvent::TextDelta("Hi".into()),
                ChatEvent::ToolCall {
                    id: "toolu_1".into(),
                    name: "get".into(),
                    arguments: json!({"q": "paris"})
                },
                ChatEvent::ToolCall {
                    id: "toolu_2".into(),
                    name: "noargs".into(),
                    arguments: json!({})
                },
                ChatEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    input_tokens: Some(15),
                    output_tokens: Some(42)
                },
            ]
        );
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
    fn error_event() {
        let err = decode(&[(
            "error",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        )])
        .unwrap_err();
        assert!(
            err.to_string().contains("overloaded_error: Overloaded"),
            "{err}"
        );
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
