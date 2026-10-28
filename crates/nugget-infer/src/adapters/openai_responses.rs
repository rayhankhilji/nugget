//! OpenAI Responses API adapter (`POST {base}/responses`, streaming).
//!
//! - Stateless by default (`store: false`) with `include: ["reasoning.encrypted_content"]`, so
//!   reasoning items stream out as [`ChatEvent::Opaque`] and can be replayed without OpenAI
//!   keeping the conversation.
//! - Function tools and `text.format` JSON schemas use `strict: true` only when the schema meets
//!   strict-mode rules (all properties required, `additionalProperties: false`).
//! - Embeddings are not part of this API; configure an `openai_compatible` provider against the
//!   same base URL for `/embeddings`.

use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use serde_json::{Map, Value, json};

use crate::adapters::data_url;
use crate::config::{ProviderConfig, Secret, merge_extra_body};
use crate::http::{SseDecoder, error_detail_value, parse_tool_arguments, sse_chat_stream};
use crate::provider::{Common, Prepared, Provider, auth_header};
use crate::schema::is_strict_compatible;
use crate::sse::SseEvent;
use crate::types::{
    ChatEvent, ChatRequest, ChatStream, Message, Part, ProviderKind, ProviderMeta, Role, StopReason,
};

pub struct OpenAiResponsesProvider {
    common: Common,
    store: bool,
}

impl OpenAiResponsesProvider {
    pub(crate) fn new(
        cfg: &ProviderConfig,
        egress: Arc<dyn EgressSink>,
        key: Option<Secret>,
        prepared: Prepared,
    ) -> Result<Self> {
        let auth = auth_header("authorization", key, |k| format!("Bearer {k}"))?;
        Ok(Self {
            common: Common::new(cfg, egress, auth, prepared)?,
            store: cfg.store.unwrap_or(false),
        })
    }
}

pub(crate) fn build_body(cfg: &ProviderConfig, store: bool, req: &ChatRequest) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(cfg.model));
    body.insert("stream".into(), json!(true));
    body.insert("store".into(), json!(store));
    if !store {
        body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    }
    if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
        body.insert("instructions".into(), json!(system));
    }
    body.insert("input".into(), Value::Array(convert_input(&req.messages)?));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                    "strict": is_strict_compatible(&t.input_schema),
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(schema) = &req.json_schema {
        body.insert(
            "text".into(),
            json!({"format": {
                "type": "json_schema",
                "name": "output",
                "schema": schema,
                "strict": is_strict_compatible(schema),
            }}),
        );
    }
    if let Some(max) = req.max_tokens.or(cfg.max_output_tokens) {
        body.insert("max_output_tokens".into(), json!(max));
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    let mut body = Value::Object(body);
    merge_extra_body(&mut body, cfg.extra_body.as_ref());
    Ok(body)
}

fn convert_input(messages: &[Message]) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    for m in messages {
        match m.role {
            Role::User => {
                let mut content = Vec::new();
                for p in &m.content {
                    match p {
                        Part::ToolResult {
                            call_id,
                            content: out,
                            is_error,
                        } => items.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": if *is_error { format!("[error] {out}") } else { out.clone() },
                        })),
                        Part::Text { text } if !text.is_empty() => {
                            content.push(json!({"type": "input_text", "text": text}));
                        }
                        Part::Image {
                            media_type,
                            data_base64,
                        } => content.push(json!({
                            "type": "input_image",
                            "image_url": data_url(media_type, data_base64),
                        })),
                        Part::ToolCall { .. } => {
                            return Err(Error::Invalid(
                                "tool calls belong in assistant messages".into(),
                            ));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    items.push(json!({"role": "user", "content": content}));
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                let flush = |text: &mut String, items: &mut Vec<Value>| {
                    if !text.is_empty() {
                        items.push(json!({"role": "assistant", "content": std::mem::take(text)}));
                    }
                };
                for p in &m.content {
                    match p {
                        Part::Text { text: t } => text.push_str(t),
                        Part::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            flush(&mut text, &mut items);
                            let args = match arguments {
                                Value::String(raw) => raw.clone(),
                                other => other.to_string(),
                            };
                            items.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": args,
                            }));
                        }
                        Part::Opaque {
                            kind: ProviderKind::OpenaiResponses,
                            data,
                        } => {
                            flush(&mut text, &mut items);
                            items.push(data.clone());
                        }
                        Part::Image { .. } | Part::ToolResult { .. } => {
                            return Err(Error::Invalid(
                                "images and tool results belong in user messages".into(),
                            ));
                        }
                        Part::Opaque { .. } => {}
                    }
                }
                flush(&mut text, &mut items);
            }
        }
    }
    Ok(items)
}

pub(crate) struct Decoder {
    provider: ProviderId,
    store: bool,
    saw_tool_call: bool,
    saw_refusal: bool,
    deferred: Vec<ChatEvent>,
}

impl Decoder {
    pub(crate) fn new(provider: ProviderId, store: bool) -> Self {
        Self {
            provider,
            store,
            saw_tool_call: false,
            saw_refusal: false,
            deferred: Vec::new(),
        }
    }

    fn done(&mut self, response: &Value, stop: StopReason) -> Vec<ChatEvent> {
        let mut out = Vec::new();
        if stop == StopReason::MaxTokens {
            if !self.deferred.is_empty() {
                tracing::warn!(provider = %self.provider, "dropping tool call truncated by max_output_tokens");
            }
        } else {
            out.append(&mut self.deferred);
        }
        let n = |k: &str| {
            response
                .pointer(&format!("/usage/{k}"))
                .and_then(Value::as_u64)
                .map(|v| v.min(u32::MAX as u64) as u32)
        };
        out.push(ChatEvent::Done {
            stop_reason: stop,
            input_tokens: n("input_tokens"),
            output_tokens: n("output_tokens"),
        });
        out
    }
}

impl SseDecoder for Decoder {
    fn on_event(&mut self, ev: SseEvent) -> Result<Vec<ChatEvent>> {
        if ev.data.trim().is_empty() || ev.data.trim() == "[DONE]" {
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
        let mut out = Vec::new();
        let delta = || v.get("delta").and_then(Value::as_str).unwrap_or_default();
        match ty {
            "response.output_text.delta" => {
                if !delta().is_empty() {
                    out.push(ChatEvent::TextDelta(delta().to_string()));
                }
            }
            "response.refusal.delta" => {
                self.saw_refusal = true;
                if !delta().is_empty() {
                    out.push(ChatEvent::TextDelta(delta().to_string()));
                }
            }
            "response.output_item.done" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        self.saw_tool_call = true;
                        let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or_default();
                        let name = s("name").to_string();
                        let arguments = parse_tool_arguments(&self.provider, &name, s("arguments"));
                        let call = ChatEvent::ToolCall {
                            id: s("call_id").to_string(),
                            name,
                            arguments,
                        };
                        if matches!(
                            &call,
                            ChatEvent::ToolCall {
                                arguments: Value::String(_),
                                ..
                            }
                        ) {
                            self.deferred.push(call);
                        } else {
                            out.push(call);
                        }
                    }
                    Some("reasoning") => {
                        // Replayable only if OpenAI stored it or returned it encrypted.
                        let has_encrypted = item
                            .get("encrypted_content")
                            .is_some_and(|c| c.as_str().is_some_and(|s| !s.is_empty()));
                        if self.store || has_encrypted {
                            out.push(ChatEvent::Opaque {
                                kind: ProviderKind::OpenaiResponses,
                                data: item,
                            });
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" => {
                let r = v.get("response").cloned().unwrap_or(Value::Null);
                let stop = if self.saw_refusal {
                    StopReason::Refusal
                } else if self.saw_tool_call {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                out.extend(self.done(&r, stop));
            }
            "response.incomplete" => {
                let r = v.get("response").cloned().unwrap_or(Value::Null);
                let reason = r
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("incomplete");
                out.extend(self.done(&r, StopReason::from_wire(reason)));
            }
            "response.failed" => {
                let detail = v
                    .pointer("/response/error")
                    .and_then(error_detail_value_or_self)
                    .unwrap_or_else(|| "response failed".into());
                return Err(Error::Provider(format!("{}: {detail}", self.provider)));
            }
            "error" => {
                let detail = error_detail_value(&v).unwrap_or_else(|| ev.data.clone());
                return Err(Error::Provider(format!(
                    "{}: stream error: {detail}",
                    self.provider
                )));
            }
            _ => {}
        }
        Ok(out)
    }

    fn on_eof(&mut self) -> Result<Vec<ChatEvent>> {
        Ok(Vec::new())
    }
}

fn error_detail_value_or_self(v: &Value) -> Option<String> {
    let message = v.get("message").and_then(Value::as_str)?;
    Some(match v.get("code").and_then(Value::as_str) {
        Some(code) => format!("{code}: {message}"),
        None => message.to_string(),
    })
}

#[async_trait]
impl Provider for OpenAiResponsesProvider {
    fn id(&self) -> &ProviderId {
        &self.common.id
    }

    fn meta(&self) -> ProviderMeta {
        self.common.meta.clone()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let body = build_body(&self.common.cfg, self.store, &req)?;
        let (resp, meter) = self
            .common
            .http
            .post_json("responses", &body, &req.feature, &req.items, true)
            .await?;
        Ok(sse_chat_stream(
            self.common.id.clone(),
            resp,
            meter,
            Decoder::new(self.common.id.clone(), self.store),
        ))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported(format!(
            "{}: the Responses API has no embeddings; configure an openai_compatible provider for /embeddings",
            self.common.id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolSpec;

    fn cfg() -> ProviderConfig {
        ProviderConfig::from_toml_str("id='o'\nkind='openai_responses'\nmodel='test-model'")
            .unwrap()
    }

    fn decode(store: bool, events: &[Value]) -> Result<Vec<ChatEvent>> {
        let mut d = Decoder::new(ProviderId::new("o"), store);
        let mut out = Vec::new();
        for data in events {
            out.extend(d.on_event(SseEvent {
                event: data["type"].as_str().map(str::to_string),
                data: data.to_string(),
                id: None,
            })?);
        }
        Ok(out)
    }

    #[test]
    fn body_shape() {
        let req = ChatRequest::new(vec![
            Message::user(vec![Part::text("look"), Part::image("image/png", "AAAA")]),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Opaque {
                        kind: ProviderKind::OpenaiResponses,
                        data: json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "e"}),
                    },
                    Part::text("ok"),
                    Part::ToolCall {
                        id: "call_1".into(),
                        name: "f".into(),
                        arguments: json!({"a": 1}),
                    },
                ],
            ),
            Message::user(vec![Part::tool_result("call_1", "42")]),
        ])
        .with_system("be brief")
        .with_tools(vec![ToolSpec {
            name: "f".into(),
            description: "d".into(),
            input_schema: json!({"type": "object", "properties": {"a": {"type": "integer"}}, "required": ["a"], "additionalProperties": false}),
        }])
        .with_json_schema(json!({"type": "object", "properties": {"x": {"type": "string"}}}))
        .with_max_tokens(100);
        let b = build_body(&cfg(), false, &req).unwrap();
        assert_eq!(b["store"], false);
        assert_eq!(b["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(b["instructions"], "be brief");
        assert_eq!(b["max_output_tokens"], 100);
        let input = b["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(
            input[0]["content"][1]["image_url"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[2], json!({"role": "assistant", "content": "ok"}));
        assert_eq!(input[3]["type"], "function_call");
        assert_eq!(input[3]["arguments"], "{\"a\":1}");
        assert_eq!(
            input[4],
            json!({"type": "function_call_output", "call_id": "call_1", "output": "42"})
        );
        assert_eq!(b["tools"][0]["strict"], true);
        assert_eq!(b["tools"][0]["parameters"]["type"], "object");
        assert_eq!(b["text"]["format"]["type"], "json_schema");
        assert_eq!(b["text"]["format"]["strict"], false);
        let b = build_body(&cfg(), true, &req).unwrap();
        assert!(b.get("include").is_none());
    }

    #[test]
    fn decodes_text_tool_and_reasoning() {
        let out = decode(
            false,
            &[
                json!({"type": "response.created", "response": {"id": "resp_1"}}),
                json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC"}}),
                json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs_2", "summary": []}}),
                json!({"type": "response.output_text.delta", "delta": "Hel"}),
                json!({"type": "response.output_text.delta", "delta": "lo"}),
                json!({"type": "response.function_call_arguments.delta", "delta": "{\"a\""}),
                json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "call_id": "call_9", "name": "f", "arguments": "{\"a\": 2}"}}),
                json!({"type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 7, "output_tokens": 9}}}),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                ChatEvent::Opaque {
                    kind: ProviderKind::OpenaiResponses,
                    data: json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC"})
                },
                ChatEvent::TextDelta("Hel".into()),
                ChatEvent::TextDelta("lo".into()),
                ChatEvent::ToolCall {
                    id: "call_9".into(),
                    name: "f".into(),
                    arguments: json!({"a": 2})
                },
                ChatEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    input_tokens: Some(7),
                    output_tokens: Some(9)
                },
            ]
        );
    }

    #[test]
    fn incomplete_refusal_and_failures() {
        let out = decode(
            false,
            &[json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}, "usage": {"input_tokens": 1, "output_tokens": 2}}})],
        )
        .unwrap();
        assert!(matches!(
            out[0],
            ChatEvent::Done {
                stop_reason: StopReason::MaxTokens,
                ..
            }
        ));
        let out = decode(
            false,
            &[
                json!({"type": "response.refusal.delta", "delta": "I can't"}),
                json!({"type": "response.completed", "response": {}}),
            ],
        )
        .unwrap();
        assert!(matches!(
            out[1],
            ChatEvent::Done {
                stop_reason: StopReason::Refusal,
                ..
            }
        ));
        let e = decode(
            false,
            &[json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom"}}})],
        )
        .unwrap_err();
        assert!(e.to_string().contains("server_error: boom"), "{e}");
        let e = decode(
            false,
            &[json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"})],
        )
        .unwrap_err();
        assert!(e.to_string().contains("slow down"), "{e}");
    }
}
