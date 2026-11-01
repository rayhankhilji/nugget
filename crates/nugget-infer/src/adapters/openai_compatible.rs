//! OpenAI-compatible adapter: `POST {base}/chat/completions` (streaming) and
//! `POST {base}/embeddings` (Ollama, LM Studio, llama-server, vLLM, OpenRouter, OpenAI).
//!
//! - Text-only user content is sent as a plain string; content arrays only with images.
//! - Tool calls are assembled from `delta.tool_calls` fragments by index and emitted when the
//!   choice finishes. A call without an `id` gets `call_<nonce>_<index>` (nonce per response); a
//!   server id repeated for another index gets a `_<index>` suffix.
//! - Mid-stream `{"error": ...}` payloads end the stream with an error.
//! - Cache hits: `usage.prompt_tokens_details.cached_tokens` or llama-server `timings.cache_n`.
//! - llama-server extensions (`llama_cpp`, default on for on-device endpoints): `cache_prompt:
//!   true`; with `slot_affinity` and a cache key, `id_slot` = a stable hash of the key. With
//!   tools and no `json_schema`, also `tool_choice: "required"` and `parallel_tool_calls: false`
//!   (with `--jinja` the server then grammar-constrains one well-formed call).
//! - [`Provider::prewarm`] is `GET {base}/models`. With `llama_cpp`, [`Provider::prewarm_prefix`]
//!   sends the stable prefix with `n_predict: 0`, `cache_prompt: true`, `stream: false` and an
//!   output cap of 1, evaluating it into the slot's KV cache.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use serde_json::{Map, Value, json};

use crate::adapters::{
    args_string, as_u32, data_url, finish_body, misplaced, parse_event, result_text, stream_error,
};
use crate::config::{MaxTokensParam, ProviderConfig, Secret, embed_prefixes};
use crate::http::{SseDecoder, parse_tool_arguments};
use crate::provider::{Common, Prepared, Provider, auth_header};
use crate::schema::is_strict_compatible;
use crate::sse::SseEvent;
use crate::types::{
    ChatEvent, ChatRequest, ChatStream, FEATURE_EMBED, FEATURE_PROBE, Message, Part, ProviderMeta,
    Role, StopReason,
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
    let obj = body.as_object_mut().expect("body is an object");
    obj.insert("stream".into(), json!(false));
    obj.remove("stream_options");
    obj.insert("n_predict".into(), json!(0));
    obj.insert("cache_prompt".into(), json!(true));
    obj.insert(max_tokens_field(cfg).into(), json!(1));
    if let Some(Value::Array(messages)) = obj.get_mut("messages")
        && messages.last().and_then(|m| m["role"].as_str()) != Some("user")
    {
        // Many chat templates refuse a conversation without a user turn; this one sits after the
        // prefix, so it does not change the cached tokens.
        messages.push(json!({"role": "user", "content": "warmup"}));
    }
    Ok(Some(body))
}

fn max_tokens_field(cfg: &ProviderConfig) -> &'static str {
    match cfg.max_tokens_param.unwrap_or_default() {
        MaxTokensParam::MaxTokens => "max_tokens",
        MaxTokensParam::MaxCompletionTokens => "max_completion_tokens",
    }
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
        body.insert(max_tokens_field(cfg).into(), json!(max));
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
    Ok(finish_body(body, cfg))
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
                            "content": result_text(content, *is_error),
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
                        Part::ToolCall { .. } => return Err(misplaced(true)),
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
                        } => calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": args_string(arguments)},
                        })),
                        Part::Image { .. } | Part::ToolResult { .. } => {
                            return Err(misplaced(false));
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

impl SseDecoder for Decoder {
    fn on_event(&mut self, ev: SseEvent) -> Result<Vec<ChatEvent>> {
        let data = ev.data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            return Ok(self.done());
        }
        let v = parse_event(&self.provider, data)?;
        if v.get("error").is_some_and(|e| !e.is_null()) || ev.event.as_deref() == Some("error") {
            return Err(stream_error(&self.provider, &v, data));
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
        Ok(if self.finish_reason.is_some() {
            self.done()
        } else {
            Vec::new()
        })
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
        let decoder = Decoder::new(self.common.id.clone());
        self.common
            .stream("chat/completions", &body, &req, decoder)
            .await
    }

    async fn prewarm(&self) -> Result<()> {
        self.common.prewarm_get("models").await
    }

    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        if self.ext.cache_prompt
            && let Some(body) = build_prewarm_body(&self.common.cfg, self.ext, req)?
        {
            return self
                .common
                .prewarm_post("chat/completions", &body, req)
                .await;
        }
        self.prewarm().await
    }

    fn embed_prefixes(&self) -> (String, String) {
        embed_prefixes(&self.common.cfg)
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_items(texts, &[]).await
    }

    async fn embed_probe(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_for(texts, &[], FEATURE_PROBE).await
    }

    async fn embed_items(&self, texts: &[String], items: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
        self.embed_for(texts, items, FEATURE_EMBED).await
    }
}

impl OpenAiCompatibleProvider {
    /// Embed `texts` in batches; each batch's ledger row is recorded under `feature` and lists
    /// the distinct items of its texts.
    async fn embed_for(
        &self,
        texts: &[String],
        items: &[ItemRef],
        feature: &str,
    ) -> Result<Vec<Vec<f32>>> {
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
                .post_for_json("embeddings", &body, feature, &refs)
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
    fn tool_call_ids_are_generated_unique_and_deduplicated() {
        let call = |i: u32, id: &str| {
            format!(
                r#"{{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":{i},{id}"function":{{"name":"a","arguments":"{{}}"}}}}]}}}}]}}"#
            )
        };
        let finish = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let ids = |chunks: &[String]| -> Vec<String> {
            let mut all: Vec<&str> = chunks.iter().map(String::as_str).collect();
            all.push(finish);
            decode(&all)
                .unwrap()
                .into_iter()
                .filter_map(|e| match e {
                    ChatEvent::ToolCall { id, .. } => Some(id),
                    _ => None,
                })
                .collect()
        };
        // Missing or empty ids are generated, and differ across responses of one run.
        let idless = [call(0, ""), call(1, r#""id":"","#)];
        let first = ids(&idless);
        assert!(first.iter().all(|id| id.starts_with("call_")) && first[0] != first[1]);
        assert!(ids(&idless).iter().all(|id| !first.contains(id)));
        // Server ids are kept; a repeat gets an index suffix.
        let repeated = [
            call(0, r#""id":"x","#),
            call(1, r#""id":"x","#),
            call(2, r#""id":"y","#),
        ];
        assert_eq!(ids(&repeated), ["x", "x_1", "y"]);
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
