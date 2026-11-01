//! OpenAI Responses API adapter (`POST {base}/responses`, streaming).
//!
//! - Stateless by default (`store: false`) with `include: ["reasoning.encrypted_content"]`, so
//!   reasoning items stream out as [`ChatEvent::Opaque`] and replay without server state.
//! - Tools and `text.format` schemas use `strict: true` only when the schema allows it.
//! - No embeddings: use an `openai_compatible` provider for `/embeddings`.
//! - With a [`crate::CacheHint`], `prompt_cache_key` is the hint's `key`, else
//!   `nugget:{feature}`. [`Provider::prewarm_prefix`] only prewarms the connection.

use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use serde_json::{Map, Value, json};

use crate::adapters::{
    args_string, as_u32, data_url, emit_tool_call, finish_body, misplaced, parse_event,
    release_deferred, result_text, stream_error,
};
use crate::config::{ProviderConfig, Secret};
use crate::http::{SseDecoder, parse_tool_arguments};
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
    if let Some(hint) = &req.cache {
        let key = hint
            .key
            .clone()
            .unwrap_or_else(|| format!("nugget:{}", req.feature));
        body.insert("prompt_cache_key".into(), json!(key));
    }
    Ok(finish_body(body, cfg))
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
                            "output": result_text(out, *is_error),
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
                        Part::ToolCall { .. } => return Err(misplaced(true)),
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
                            items.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": args_string(arguments),
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
                            return Err(misplaced(false));
                        }
                        Part::Opaque { .. } | Part::CacheBreakpoint => {}
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
    cached_tokens: Option<u32>,
}

impl Decoder {
    pub(crate) fn new(provider: ProviderId, store: bool) -> Self {
        Self {
            provider,
            store,
            saw_tool_call: false,
            saw_refusal: false,
            deferred: Vec::new(),
            cached_tokens: None,
        }
    }

    fn done(&mut self, response: &Value, stop: StopReason) -> Vec<ChatEvent> {
        let mut out = Vec::new();
        release_deferred(&self.provider, &stop, &mut self.deferred, &mut out);
        let n = |k: &str| as_u32(response.pointer(&format!("/usage/{k}")));
        self.cached_tokens = n("input_tokens_details/cached_tokens");
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
        let v = parse_event(&self.provider, &ev.data)?;
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
                        emit_tool_call(call, &mut self.deferred, &mut out);
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
            "error" => return Err(stream_error(&self.provider, &v, &ev.data)),
            _ => {}
        }
        Ok(out)
    }

    fn on_eof(&mut self) -> Result<Vec<ChatEvent>> {
        Ok(Vec::new())
    }

    fn cache_usage(&self) -> (Option<u32>, Option<u32>) {
        (self.cached_tokens, None)
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
        let decoder = Decoder::new(self.common.id.clone(), self.store);
        self.common.stream("responses", &body, &req, decoder).await
    }

    async fn prewarm(&self) -> Result<()> {
        self.common.prewarm_get("models").await
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

    #[test]
    fn body_options() {
        let cfg =
            ProviderConfig::from_toml_str("id='o'\nkind='openai_responses'\nmodel='m'").unwrap();
        let req = ChatRequest::new(vec![Message::user_text("x")])
            .with_json_schema(json!({"type": "object", "properties": {"x": {"type": "string"}}}))
            .with_max_tokens(100);
        let b = build_body(&cfg, false, &req).unwrap();
        assert_eq!(b["max_output_tokens"], 100);
        assert_eq!(
            b["text"]["format"]["strict"], false,
            "schema is not strict-compatible"
        );
        assert!(
            build_body(&cfg, true, &req)
                .unwrap()
                .get("include")
                .is_none()
        );
    }

    #[test]
    fn decodes_stop_reasons_failures_and_unreplayable_reasoning() {
        let decode = |events: &[Value]| -> Result<Vec<ChatEvent>> {
            let mut d = Decoder::new(ProviderId::new("o"), false);
            let mut out = Vec::new();
            for v in events {
                out.extend(d.on_event(SseEvent {
                    event: None,
                    data: v.to_string(),
                    id: None,
                })?);
            }
            Ok(out)
        };
        let stop = |events: &[Value]| match decode(events).unwrap().pop() {
            Some(ChatEvent::Done { stop_reason, .. }) => stop_reason,
            other => panic!("{other:?}"),
        };
        // Reasoning that is neither stored nor encrypted cannot be replayed: dropped.
        let out = decode(&[
            json!({"type": "response.output_item.done", "item": {"type": "reasoning", "id": "rs_2"}}),
            json!({"type": "response.completed", "response": {}}),
        ])
        .unwrap();
        assert_eq!(out.len(), 1);
        let incomplete = json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}}});
        assert_eq!(stop(&[incomplete]), StopReason::MaxTokens);
        let refusal = json!({"type": "response.refusal.delta", "delta": "I can't"});
        assert_eq!(
            stop(&[
                refusal,
                json!({"type": "response.completed", "response": {}})
            ]),
            StopReason::Refusal
        );
        let e = decode(&[
            json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"}),
        ]);
        assert!(e.unwrap_err().to_string().contains("slow down"));
    }
}
