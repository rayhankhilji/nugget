//! OpenAI-compatible adapter: `POST {base}/chat/completions` (streaming) and
//! `POST {base}/embeddings`. Works with Ollama, LM Studio, llama-server, vLLM, OpenRouter and
//! OpenAI itself.
//!
//! - User text-only content is sent as a plain string (widest server support); content arrays
//!   are used only when images are present.
//! - Tool calls are assembled from `delta.tool_calls` fragments by index and emitted when the
//!   choice finishes. `usage` is requested via `stream_options.include_usage` (configurable).
//!   A call without an `id` (llama-server sometimes omits it) gets `call_<nonce>_<index>`, where
//!   the nonce is per response (process-wide counter mixed with the clock); a server id repeated
//!   for another index in the same response gets a `_<index>` suffix. Other ids are kept as sent.
//! - Mid-stream `{"error": ...}` payloads (OpenRouter, vLLM, llama-server) end the stream with an
//!   error.
//! - Prompt caching: `usage.prompt_tokens_details.cached_tokens` (OpenAI, llama-server) or
//!   llama-server's `timings.cache_n` is reported in [`crate::ChatStats`].
//!   [`crate::Part::CacheBreakpoint`]s are dropped (every message is sent as before).
//! - llama-server extensions (config `llama_cpp`, on by default for on-device endpoints; checked
//!   against llama.cpp b11940's `tools/server`, which copies unknown body fields into its
//!   completion parameters): `cache_prompt: true` on every chat request; with `slot_affinity`
//!   and a [`crate::CacheHint`] key, `id_slot` = a stable hash of the key (the server takes it
//!   modulo its slot count; a busy pinned slot defers the request rather than using another).
//!   A request with tools and no `json_schema` also gets `tool_choice: "required"` and
//!   `parallel_tool_calls: false`: with `--jinja` the server compiles a grammar from the tool
//!   schemas, so the model can only emit one well-formed call (`extra_body` can override both).
//! - Embeddings: texts are sent as given. [`Provider::embed_prefixes`] reports the model's task
//!   prefixes ([`crate::embed_prefixes`], e.g. `search_query: ` / `search_document: ` for
//!   nomic-embed-text); callers add them, so an index keeps the scheme it was built with.
//! - [`Provider::prewarm`] is `GET {base}/models`. With `llama_cpp`, [`Provider::prewarm_prefix`]
//!   sends the stable prefix to `chat/completions` with `n_predict: 0` (the server evaluates the
//!   prompt into the slot's KV cache and generates at most one token), `cache_prompt: true`,
//!   `stream: false` and the output cap set to 1 for servers that ignore `n_predict`. Otherwise it
//!   only prewarms the connection. The warm-up carries the same `tool_choice` fields as the real
//!   request (they change the sampling grammar, not the prompt; harmless with `n_predict: 0`).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use serde_json::{Map, Value, json};

use crate::adapters::data_url;
use crate::config::{MaxTokensParam, ProviderConfig, Secret, embed_prefixes, merge_extra_body};
use crate::http::{SseDecoder, error_detail_value, parse_tool_arguments, sse_chat_stream};
use crate::provider::{Common, Prepared, Provider, auth_header};
use crate::schema::is_strict_compatible;
use crate::sse::SseEvent;
use crate::types::{
    ChatEvent, ChatRequest, ChatStream, FEATURE_EMBED, Message, Part, ProviderMeta, Role,
    StopReason,
};
use nugget_core::capability::Locality;

const DEFAULT_EMBED_BATCH: usize = 64;

pub struct OpenAiCompatibleProvider {
    common: Common,
    ext: LlamaExt,
}

/// llama-server request extensions in effect for one provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LlamaExt {
    pub cache_prompt: bool,
    pub slot_affinity: bool,
}

impl LlamaExt {
    pub(crate) fn for_config(cfg: &ProviderConfig, locality: Locality) -> Self {
        let cache_prompt = cfg.llama_cpp.unwrap_or(locality == Locality::OnDevice);
        Self {
            cache_prompt,
            slot_affinity: cache_prompt && cfg.slot_affinity.unwrap_or(false),
        }
    }
}

/// Stable (FNV-1a) slot number for a cache key; llama-server reduces it modulo its slot count.
pub(crate) fn slot_for_key(key: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in key.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h & 0x7fff_ffff
}

impl OpenAiCompatibleProvider {
    pub(crate) fn new(
        cfg: &ProviderConfig,
        egress: Arc<dyn EgressSink>,
        key: Option<Secret>,
        prepared: Prepared,
    ) -> Result<Self> {
        let auth = auth_header("authorization", key, |k| format!("Bearer {k}"))?;
        let ext = LlamaExt::for_config(cfg, prepared.locality);
        Ok(Self {
            common: Common::new(cfg, egress, auth, prepared)?,
            ext,
        })
    }
}

/// The `n_predict: 0` KV-cache warm-up for `req`'s stable prefix (llama-server), or `None` if
/// there is no prefix.
pub(crate) fn build_prewarm_body(
    cfg: &ProviderConfig,
    ext: LlamaExt,
    req: &ChatRequest,
) -> Result<Option<Value>> {
    let Some(prefix) = req.cache_prefix() else {
        return Ok(None);
    };
    // The prefix drops `json_schema`, so decide the tool constraint from the real request.
    let mut body = body_with(cfg, ext, &prefix, requires_tool(ext, req))?;
    let obj = body
        .as_object_mut()
        .ok_or_else(|| Error::Other("chat body is not an object".into()))?;
    obj.insert("stream".into(), json!(false));
    obj.remove("stream_options");
    obj.insert("n_predict".into(), json!(0));
    obj.insert("cache_prompt".into(), json!(true));
    let cap = match cfg.max_tokens_param.unwrap_or_default() {
        MaxTokensParam::MaxTokens => "max_tokens",
        MaxTokensParam::MaxCompletionTokens => "max_completion_tokens",
    };
    obj.insert(cap.into(), json!(1));
    if let Some(Value::Array(messages)) = obj.get_mut("messages")
        && messages.last().and_then(|m| m["role"].as_str()) != Some("user")
    {
        // Many chat templates refuse a conversation without a user turn; this one sits after the
        // prefix, so it does not change the cached tokens.
        messages.push(json!({"role": "user", "content": "warmup"}));
    }
    Ok(Some(body))
}

/// Whether llama-server should be held to exactly one schema-valid tool call for `req`.
fn requires_tool(ext: LlamaExt, req: &ChatRequest) -> bool {
    ext.cache_prompt && !req.tools.is_empty() && req.json_schema.is_none()
}

pub(crate) fn build_body(cfg: &ProviderConfig, ext: LlamaExt, req: &ChatRequest) -> Result<Value> {
    body_with(cfg, ext, req, requires_tool(ext, req))
}

fn body_with(
    cfg: &ProviderConfig,
    ext: LlamaExt,
    req: &ChatRequest,
    require_tool: bool,
) -> Result<Value> {
    let mut body = Map::new();
    body.insert("model".into(), json!(cfg.model));
    body.insert("stream".into(), json!(true));
    if cfg.stream_usage.unwrap_or(true) {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    let mut messages = Vec::new();
    if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(convert_messages(&req.messages)?);
    body.insert("messages".into(), Value::Array(messages));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if require_tool {
            body.insert("tool_choice".into(), json!("required"));
            body.insert("parallel_tool_calls".into(), json!(false));
        }
    }
    if let Some(schema) = &req.json_schema {
        body.insert(
            "response_format".into(),
            json!({"type": "json_schema", "json_schema": {
                "name": "output",
                "schema": schema,
                "strict": is_strict_compatible(schema),
            }}),
        );
    }
    if let Some(max) = req.max_tokens.or(cfg.max_output_tokens) {
        let field = match cfg.max_tokens_param.unwrap_or_default() {
            MaxTokensParam::MaxTokens => "max_tokens",
            MaxTokensParam::MaxCompletionTokens => "max_completion_tokens",
        };
        body.insert(field.into(), json!(max));
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if ext.cache_prompt {
        body.insert("cache_prompt".into(), json!(true));
    }
    if ext.slot_affinity
        && let Some(key) = req.cache.as_ref().and_then(|c| c.key.as_deref())
    {
        body.insert("id_slot".into(), json!(slot_for_key(key)));
    }
    let mut body = Value::Object(body);
    merge_extra_body(&mut body, cfg.extra_body.as_ref());
    Ok(body)
}

fn convert_messages(messages: &[Message]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::User => {
                let mut parts = Vec::new();
                let mut has_image = false;
                for p in &m.content {
                    match p {
                        Part::ToolResult {
                            call_id,
                            content,
                            is_error,
                        } => out.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": if *is_error { format!("[error] {content}") } else { content.clone() },
                        })),
                        Part::Text { text } if !text.is_empty() => {
                            parts.push(json!({"type": "text", "text": text}));
                        }
                        Part::Image {
                            media_type,
                            data_base64,
                        } => {
                            has_image = true;
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": data_url(media_type, data_base64)},
                            }));
                        }
                        Part::ToolCall { .. } => {
                            return Err(Error::Invalid(
                                "tool calls belong in assistant messages".into(),
                            ));
                        }
                        _ => {}
                    }
                }
                if parts.is_empty() {
                    continue;
                }
                let content = if has_image {
                    Value::Array(parts)
                } else {
                    Value::String(
                        parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                };
                out.push(json!({"role": "user", "content": content}));
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut calls = Vec::new();
                for p in &m.content {
                    match p {
                        Part::Text { text: t } => text.push_str(t),
                        Part::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            let args = match arguments {
                                Value::String(raw) => raw.clone(),
                                other => other.to_string(),
                            };
                            calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {"name": name, "arguments": args},
                            }));
                        }
                        Part::Image { .. } | Part::ToolResult { .. } => {
                            return Err(Error::Invalid(
                                "images and tool results belong in user messages".into(),
                            ));
                        }
                        Part::Opaque { .. } | Part::CacheBreakpoint => {}
                    }
                }
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                let mut msg = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                });
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                out.push(msg);
            }
        }
    }
    Ok(out)
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
    /// Some servers send arguments as a JSON object rather than a string.
    object: Option<Value>,
}

/// Per-response nonce for generated tool-call ids: a process-wide counter mixed with the clock,
/// so ids differ across responses of one run and are unlikely to repeat across runs.
fn call_nonce() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    // splitmix64 finaliser over the mix.
    let mut z = t ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

pub(crate) struct Decoder {
    provider: ProviderId,
    nonce: u64,
    calls: BTreeMap<u64, PartialCall>,
    finish_reason: Option<String>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cached_tokens: Option<u32>,
    flushed: bool,
    finished: bool,
}

impl Decoder {
    pub(crate) fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            nonce: call_nonce(),
            calls: BTreeMap::new(),
            finish_reason: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            flushed: false,
            finished: false,
        }
    }

    /// Emit assembled tool calls (once), unless truncated by the length limit.
    fn flush_calls(&mut self, out: &mut Vec<ChatEvent>) {
        if self.flushed {
            return;
        }
        self.flushed = true;
        let truncated = self.finish_reason.as_deref() == Some("length");
        let mut seen = HashSet::new();
        for (idx, mut c) in std::mem::take(&mut self.calls) {
            if c.id.is_empty() {
                c.id = format!("call_{:016x}_{idx}", self.nonce);
            }
            while !seen.insert(c.id.clone()) {
                c.id = format!("{}_{idx}", c.id);
            }
            let arguments = match c.object {
                Some(obj) => obj,
                None => parse_tool_arguments(&self.provider, &c.name, &c.arguments),
            };
            if truncated && arguments.is_string() {
                tracing::warn!(provider = %self.provider, "dropping tool call truncated by max_tokens");
                continue;
            }
            out.push(ChatEvent::ToolCall {
                id: c.id,
                name: c.name,
                arguments,
            });
        }
    }

    fn done(&mut self) -> Vec<ChatEvent> {
        let mut out = Vec::new();
        if std::mem::replace(&mut self.finished, true) {
            return out;
        }
        self.flush_calls(&mut out);
        let stop = match self.finish_reason.as_deref() {
            Some(r) => StopReason::from_wire(r),
            None => StopReason::EndTurn,
        };
        out.push(ChatEvent::Done {
            stop_reason: stop,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        });
        out
    }
}

fn as_u32(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32)
}

impl SseDecoder for Decoder {
    fn on_event(&mut self, ev: SseEvent) -> Result<Vec<ChatEvent>> {
        let data = ev.data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            return Ok(self.done());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| {
            Error::Provider(format!("{}: malformed stream chunk: {e}", self.provider))
        })?;
        if v.get("error").is_some_and(|e| !e.is_null()) || ev.event.as_deref() == Some("error") {
            let detail = error_detail_value(&v).unwrap_or_else(|| data.to_string());
            return Err(Error::Provider(format!(
                "{}: stream error: {detail}",
                self.provider
            )));
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.input_tokens = as_u32(u.get("prompt_tokens")).or(self.input_tokens);
            self.output_tokens = as_u32(u.get("completion_tokens")).or(self.output_tokens);
            self.cached_tokens =
                as_u32(u.pointer("/prompt_tokens_details/cached_tokens")).or(self.cached_tokens);
        }
        if self.cached_tokens.is_none() {
            self.cached_tokens = as_u32(v.pointer("/timings/cache_n"));
        }
        let mut out = Vec::new();
        let choices = v.get("choices").and_then(Value::as_array);
        // Only the first choice is used (n = 1).
        if let Some(choice) = choices.and_then(|c| {
            c.iter()
                .find(|ch| ch.get("index").and_then(Value::as_u64).unwrap_or(0) == 0)
        }) {
            if let Some(delta) = choice.get("delta") {
                if let Some(t) = delta.get("content").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    out.push(ChatEvent::TextDelta(t.to_string()));
                }
                if let Some(t) = delta.get("refusal").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    self.finish_reason = Some("refusal".into());
                    out.push(ChatEvent::TextDelta(t.to_string()));
                }
                for (pos, tc) in delta
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let idx = tc
                        .get("index")
                        .and_then(Value::as_u64)
                        .unwrap_or(pos as u64);
                    let entry = self.calls.entry(idx).or_default();
                    if let Some(id) = tc.get("id").and_then(Value::as_str)
                        && !id.is_empty()
                    {
                        entry.id = id.to_string();
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(n) = f.get("name").and_then(Value::as_str) {
                            entry.name.push_str(n);
                        }
                        match f.get("arguments") {
                            Some(Value::String(a)) => entry.arguments.push_str(a),
                            Some(obj @ Value::Object(_)) => entry.object = Some(obj.clone()),
                            _ => {}
                        }
                    }
                }
            }
            if let Some(r) = choice.get("finish_reason").and_then(Value::as_str)
                && self.finish_reason.as_deref() != Some("refusal")
            {
                self.finish_reason = Some(r.to_string());
            }
        }
        if self.finish_reason.is_some() {
            // Emit tool calls as soon as the choice is finished; `Done` waits for the usage
            // chunk and `[DONE]`.
            self.flush_calls(&mut out);
        }
        Ok(out)
    }

    fn cache_usage(&self) -> (Option<u32>, Option<u32>) {
        (self.cached_tokens, None)
    }

    fn on_eof(&mut self) -> Result<Vec<ChatEvent>> {
        // Some servers omit `[DONE]`; a finished choice is enough.
        if self.finish_reason.is_some() {
            Ok(self.done())
        } else {
            Ok(Vec::new())
        }
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn id(&self) -> &ProviderId {
        &self.common.id
    }

    fn meta(&self) -> ProviderMeta {
        self.common.meta.clone()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let body = build_body(&self.common.cfg, self.ext, &req)?;
        let (resp, meter, clock) = self
            .common
            .http
            .post_json("chat/completions", &body, &req.feature, &req.items, true)
            .await?;
        Ok(sse_chat_stream(
            self.common.id.clone(),
            resp,
            meter,
            clock,
            Decoder::new(self.common.id.clone()),
        ))
    }

    async fn prewarm(&self) -> Result<()> {
        self.common.prewarm_get("models").await
    }

    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        if !self.ext.cache_prompt {
            return self.prewarm().await;
        }
        let Some(body) = build_prewarm_body(&self.common.cfg, self.ext, req)? else {
            return self.prewarm().await;
        };
        self.common
            .http
            .post_for_json("chat/completions", &body, &req.feature, &req.items)
            .await?;
        Ok(())
    }

    fn embed_prefixes(&self) -> (String, String) {
        embed_prefixes(&self.common.cfg)
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_items(texts, &[]).await
    }

    /// Each batch's ledger row lists the distinct items of its texts.
    async fn embed_items(&self, texts: &[String], items: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
        let batch = self
            .common
            .cfg
            .embed_batch_size
            .unwrap_or(DEFAULT_EMBED_BATCH)
            .max(1);
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for (n, chunk) in texts.chunks(batch).enumerate() {
            let body = json!({
                "model": self.common.cfg.model,
                "input": chunk,
                "encoding_format": "float",
            });
            let mut refs: Vec<ItemRef> = Vec::new();
            for item in items.iter().skip(n * batch).take(chunk.len()) {
                if !refs.contains(item) {
                    refs.push(item.clone());
                }
            }
            let v = self
                .common
                .http
                .post_for_json("embeddings", &body, FEATURE_EMBED, &refs)
                .await?;
            out.extend(parse_embeddings(&self.common.id, &v, chunk.len())?);
        }
        if let Some(dim) = out.first().map(Vec::len)
            && out.iter().any(|e| e.len() != dim)
        {
            return Err(Error::Provider(format!(
                "{}: embeddings have inconsistent dimensions",
                self.common.id
            )));
        }
        Ok(out)
    }
}

fn parse_embeddings(provider: &ProviderId, v: &Value, expected: usize) -> Result<Vec<Vec<f32>>> {
    let bad = |m: &str| Error::Provider(format!("{provider}: embeddings response {m}"));
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("has no data array"))?;
    let mut slots: Vec<Option<Vec<f32>>> = vec![None; expected];
    for (pos, item) in data.iter().enumerate() {
        let idx = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .unwrap_or(pos);
        let emb = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("item has no embedding array (base64 encoding is not supported)"))?;
        let vec: Vec<f32> = emb
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32))
            .collect::<Option<_>>()
            .ok_or_else(|| bad("has a non-numeric value"))?;
        if vec.is_empty() {
            return Err(bad("has an empty vector"));
        }
        let slot = slots
            .get_mut(idx)
            .ok_or_else(|| bad("index out of range"))?;
        *slot = Some(vec);
    }
    slots
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad(&format!("is missing vectors (expected {expected})")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolSpec;

    fn cfg() -> ProviderConfig {
        ProviderConfig::from_toml_str(
            "id='l'\nkind='openai_compatible'\nmodel='test-model'\nbase_url='http://localhost:11434/v1'",
        )
        .unwrap()
    }

    fn decode(chunks: &[&str]) -> Result<Vec<ChatEvent>> {
        let mut d = Decoder::new(ProviderId::new("l"));
        let mut out = Vec::new();
        for c in chunks {
            out.extend(d.on_event(SseEvent {
                event: None,
                data: c.to_string(),
                id: None,
            })?);
        }
        out.extend(d.on_eof()?);
        Ok(out)
    }

    #[test]
    fn body_shape() {
        let req = ChatRequest::new(vec![
            Message::user_text("hi"),
            Message::new(
                Role::Assistant,
                vec![Part::ToolCall {
                    id: "c1".into(),
                    name: "f".into(),
                    arguments: json!({"a": 1}),
                }],
            ),
            Message::user(vec![
                Part::tool_result("c1", "done"),
                Part::text("and this?"),
                Part::image("image/jpeg", "BBBB"),
            ]),
        ])
        .with_system("sys")
        .with_tools(vec![ToolSpec {
            name: "f".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        }])
        .with_json_schema(json!({"type": "object"}))
        .with_max_tokens(50);
        let b = build_body(&cfg(), LlamaExt::default(), &req).unwrap();
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({"role": "system", "content": "sys"}));
        assert_eq!(m[1], json!({"role": "user", "content": "hi"}));
        assert_eq!(m[2]["content"], Value::Null);
        assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(
            m[3],
            json!({"role": "tool", "tool_call_id": "c1", "content": "done"})
        );
        assert_eq!(
            m[4]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,BBBB"
        );
        assert_eq!(b["max_tokens"], 50);
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["tools"][0]["function"]["name"], "f");
        assert_eq!(b["response_format"]["json_schema"]["strict"], false);

        let mut c = cfg();
        c.max_tokens_param = Some(MaxTokensParam::MaxCompletionTokens);
        c.stream_usage = Some(false);
        let b = build_body(&c, LlamaExt::default(), &req).unwrap();
        assert_eq!(b["max_completion_tokens"], 50);
        assert!(b.get("max_tokens").is_none());
        assert!(b.get("stream_options").is_none());
    }

    #[test]
    fn decodes_fragmented_tool_calls() {
        let out = decode(&[
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_","arguments":""}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"weather","arguments":"{\"city\":"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"noop","arguments":{"x":true}}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Oslo\"}"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":22}}"#,
            "[DONE]",
        ])
        .unwrap();
        assert_eq!(
            out,
            vec![
                ChatEvent::ToolCall {
                    id: "call_a".into(),
                    name: "get_weather".into(),
                    arguments: json!({"city": "Oslo"})
                },
                ChatEvent::ToolCall {
                    id: "call_b".into(),
                    name: "noop".into(),
                    arguments: json!({"x": true})
                },
                ChatEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    input_tokens: Some(11),
                    output_tokens: Some(22)
                },
            ]
        );
    }

    fn call_ids(out: &[ChatEvent]) -> Vec<String> {
        out.iter()
            .filter_map(|e| match e {
                ChatEvent::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    const TWO_IDLESS: [&str; 3] = [
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"a","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"","function":{"name":"b","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];

    #[test]
    fn generates_ids_for_idless_tool_calls() {
        let first = call_ids(&decode(&TWO_IDLESS).unwrap());
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|id| id.starts_with("call_")));
        assert_ne!(first[0], first[1]);
        // A second response of the same run gets different ids.
        let second = call_ids(&decode(&TWO_IDLESS).unwrap());
        assert!(
            second.iter().all(|id| !first.contains(id)),
            "{first:?} {second:?}"
        );
    }

    #[test]
    fn keeps_server_ids_and_dedups_repeats() {
        let ids = call_ids(
            &decode(&[
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"x","function":{"name":"a","arguments":"{}"}}]}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"x","function":{"name":"b","arguments":"{}"}}]}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":2,"id":"y","function":{"name":"c","arguments":"{}"}}]}}]}"#,
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ])
            .unwrap(),
        );
        assert_eq!(ids, ["x", "x_1", "y"]);
    }

    #[test]
    fn llama_ext_requires_one_tool_call() {
        let tools = vec![ToolSpec {
            name: "f".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        }];
        let llama = LlamaExt::for_config(&cfg(), Locality::OnDevice);
        let with_tools = ChatRequest::new(vec![Message::user_text("hi")]).with_tools(tools.clone());
        let b = build_body(&cfg(), llama, &with_tools).unwrap();
        assert_eq!(b["tool_choice"], "required");
        assert_eq!(b["parallel_tool_calls"], false);

        let constrained =
            |b: &Value| b.get("tool_choice").is_some() || b.get("parallel_tool_calls").is_some();
        let no_tools = ChatRequest::new(vec![Message::user_text("hi")]);
        assert!(!constrained(&build_body(&cfg(), llama, &no_tools).unwrap()));
        let schema = with_tools
            .clone()
            .with_json_schema(json!({"type": "object"}));
        assert!(!constrained(&build_body(&cfg(), llama, &schema).unwrap()));
        assert!(!constrained(
            &build_body(&cfg(), LlamaExt::default(), &with_tools).unwrap()
        ));

        // The warm-up mirrors the real request, including for a `json_schema` request whose
        // prefix has dropped the schema.
        let pw = build_prewarm_body(&cfg(), llama, &with_tools)
            .unwrap()
            .unwrap();
        assert_eq!(pw["tool_choice"], "required");
        let pw = build_prewarm_body(&cfg(), llama, &schema).unwrap().unwrap();
        assert!(!constrained(&pw));

        // `extra_body` still wins.
        let mut c = cfg();
        c.extra_body = Some(json!({"tool_choice": "auto"}));
        assert_eq!(
            build_body(&c, llama, &with_tools).unwrap()["tool_choice"],
            "auto"
        );
    }

    #[test]
    fn eof_without_done_marker_after_finish() {
        let out = decode(&[
            r#"{"choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"length"}]}"#,
        ])
        .unwrap();
        assert_eq!(out[0], ChatEvent::TextDelta("hi".into()));
        assert!(matches!(
            out[1],
            ChatEvent::Done {
                stop_reason: StopReason::MaxTokens,
                ..
            }
        ));
        assert!(
            decode(&[r#"{"choices":[{"index":0,"delta":{"content":"hi"}}]}"#])
                .unwrap()
                .iter()
                .all(|e| !matches!(e, ChatEvent::Done { .. }))
        );
    }

    #[test]
    fn slot_hash_is_stable_and_non_negative() {
        assert_eq!(slot_for_key("draft"), slot_for_key("draft"));
        assert_ne!(slot_for_key("draft"), slot_for_key("chat"));
        assert!(slot_for_key("x") <= i32::MAX as u32);
        let local = LlamaExt::for_config(&cfg(), Locality::OnDevice);
        assert!(local.cache_prompt && !local.slot_affinity);
        assert_eq!(
            LlamaExt::for_config(&cfg(), Locality::LocalNetwork),
            LlamaExt::default()
        );
    }

    #[test]
    fn mid_stream_error() {
        let e = decode(&[r#"{"error":{"message":"upstream overloaded","code":502}}"#]).unwrap_err();
        assert!(e.to_string().contains("502: upstream overloaded"), "{e}");
    }

    #[test]
    fn embeddings_parsing() {
        let p = ProviderId::new("l");
        let v = json!({"data": [
            {"index": 1, "embedding": [0.5, 0.25]},
            {"index": 0, "embedding": [1.0, 2.0]}
        ]});
        assert_eq!(
            parse_embeddings(&p, &v, 2).unwrap(),
            vec![vec![1.0, 2.0], vec![0.5, 0.25]]
        );
        assert!(parse_embeddings(&p, &v, 3).is_err());
        assert!(parse_embeddings(&p, &json!({"data": [{"embedding": "AAAA"}]}), 1).is_err());
        assert!(parse_embeddings(&p, &json!({}), 1).is_err());
    }
}
