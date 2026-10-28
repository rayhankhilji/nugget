//! Provider-neutral request and response types.

use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use nugget_core::capability::{Capabilities, Locality};
use nugget_core::ids::ItemRef;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Anthropic Messages API (`POST {base}/v1/messages`).
    Anthropic,
    /// OpenAI Responses API (`POST {base}/responses`).
    OpenaiResponses,
    /// OpenAI-compatible Chat Completions + Embeddings (`{base}/chat/completions`,
    /// `{base}/embeddings`): Ollama, LM Studio, llama-server, vLLM, OpenRouter, ...
    OpenaiCompatible,
}

impl ProviderKind {
    /// The config / IPC spelling (`anthropic`, `openai_responses`, `openai_compatible`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenaiResponses => "openai_responses",
            ProviderKind::OpenaiCompatible => "openai_compatible",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Static facts about a configured provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMeta {
    pub kind: ProviderKind,
    /// Model id exactly as configured (opaque to this crate).
    pub model: String,
    pub locality: Locality,
    /// Context window in tokens, if declared in config.
    pub context_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// One piece of message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// Inline image. `media_type` is e.g. `image/png`; `data_base64` is standard base64 without
    /// a `data:` prefix or line breaks.
    Image {
        media_type: String,
        data_base64: String,
    },
    /// A tool call the assistant made (echo it back in the assistant turn).
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// The result of executing a tool call (send it in the following user turn).
    ToolResult {
        call_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// Provider-private state that must be replayed verbatim to the same kind of provider
    /// (Anthropic thinking blocks, OpenAI reasoning items). Dropped when sent to another kind.
    Opaque {
        kind: ProviderKind,
        data: Value,
    },
    /// Prompt-cache boundary: everything before this marker (tools, system, earlier messages and
    /// the parts before it in this message) is a stable prefix worth caching. Carries no content.
    ///
    /// - Anthropic: `cache_control` on the content block just before the marker (at most four
    ///   breakpoints per request; see [`crate::adapters::anthropic`]).
    /// - OpenAI Responses / OpenAI-compatible: dropped (their caches are automatic prefix
    ///   matches); [`ChatRequest::cache_prefix`] still uses it to know what to prewarm.
    CacheBreakpoint,
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Part::Text { text: text.into() }
    }

    pub fn image(media_type: impl Into<String>, data_base64: impl Into<String>) -> Self {
        Part::Image {
            media_type: media_type.into(),
            data_base64: data_base64.into(),
        }
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Part::ToolResult {
            call_id: call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Part>,
}

impl Message {
    pub fn new(role: Role, content: Vec<Part>) -> Self {
        Self { role, content }
    }

    pub fn user(content: Vec<Part>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![Part::text(text)])
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![Part::text(text)])
    }
}

/// A client-side tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub input_schema: Value,
}

/// Default egress-ledger feature tag for chat requests.
pub const FEATURE_CHAT: &str = "chat";
/// Egress-ledger feature tag for embedding requests.
pub const FEATURE_EMBED: &str = "embed";
/// Egress-ledger feature tag for capability probes.
pub const FEATURE_PROBE: &str = "probe";
/// Egress-ledger feature tag for [`crate::Provider::prewarm`] connection warm-ups (which carry
/// no content).
pub const FEATURE_PREWARM: &str = "prewarm";

/// How long a provider should keep a cached prefix (Anthropic `cache_control.ttl`; ignored by
/// providers without a choice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTtl {
    /// The provider default (Anthropic: 5 minutes, refreshed by every hit).
    #[default]
    Default,
    /// One hour (Anthropic `ttl: "1h"`; writes cost more, see the Anthropic caching docs).
    OneHour,
}

/// Prompt-caching hints for a [`ChatRequest`]. Mark finer boundaries in the messages with
/// [`Part::CacheBreakpoint`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHint {
    /// Cache the tools + system prompt prefix (Anthropic: `cache_control` on the last system
    /// block, or the last tool when there is no system prompt). Default true.
    #[serde(default = "default_true")]
    pub system: bool,
    /// A stable routing key for requests that share a prefix, e.g. `draft:email` or a session
    /// id. Never put user content here: it is sent as-is. Used as OpenAI's `prompt_cache_key`
    /// and, when the provider enables `slot_affinity`, to pick a llama-server slot (`id_slot`).
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub ttl: CacheTtl,
}

fn default_true() -> bool {
    true
}

impl Default for CacheHint {
    fn default() -> Self {
        Self {
            system: true,
            key: None,
            ttl: CacheTtl::Default,
        }
    }
}

impl CacheHint {
    /// Cache the system prefix, routed by `key`.
    pub fn keyed(key: impl Into<String>) -> Self {
        Self {
            key: Some(key.into()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    /// Constrain the final text to this JSON Schema (structured output).
    #[serde(default)]
    pub json_schema: Option<Value>,
    /// Output token cap. `None` uses the provider's configured default.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Sampling temperature. Some APIs/models reject it (see the adapter docs).
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Feature tag recorded in the egress ledger (`chat`, `draft`, `probe`, ...).
    #[serde(default = "default_feature")]
    pub feature: String,
    /// Items whose content is included in this request (egress-ledger provenance).
    #[serde(default)]
    pub items: Vec<ItemRef>,
    /// Prompt-caching hints. `None` sends no explicit cache markers for the system prompt
    /// ([`Part::CacheBreakpoint`]s in messages still apply).
    #[serde(default)]
    pub cache: Option<CacheHint>,
}

fn default_feature() -> String {
    FEATURE_CHAT.to_string()
}

impl Default for ChatRequest {
    fn default() -> Self {
        Self {
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            json_schema: None,
            max_tokens: None,
            temperature: None,
            feature: default_feature(),
            items: Vec::new(),
            cache: None,
        }
    }
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            ..Self::default()
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_json_schema(mut self, schema: Value) -> Self {
        self.json_schema = Some(schema);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_feature(mut self, feature: impl Into<String>) -> Self {
        self.feature = feature.into();
        self
    }

    pub fn with_cache(mut self, cache: CacheHint) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Does any message carry a [`Part::CacheBreakpoint`]?
    pub fn has_cache_breakpoints(&self) -> bool {
        self.messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|p| matches!(p, Part::CacheBreakpoint))
    }

    /// The stable prefix of this request, for prewarming a provider's prompt cache before the
    /// final context is known: tools, system prompt and the messages up to (and including the
    /// marker of) the last [`Part::CacheBreakpoint`]. Without a breakpoint only tools and system
    /// remain. `None` if there is nothing to cache (no system prompt, tools or breakpoint).
    ///
    /// `json_schema`, `max_tokens` and `temperature` are dropped; `feature`, `items` and `cache`
    /// are kept so the egress ledger and routing key stay the same as the real request's.
    pub fn cache_prefix(&self) -> Option<ChatRequest> {
        let mut messages = Vec::new();
        if let Some((mi, pi)) = self.messages.iter().enumerate().rev().find_map(|(mi, m)| {
            m.content
                .iter()
                .rposition(|p| matches!(p, Part::CacheBreakpoint))
                .map(|pi| (mi, pi))
        }) {
            messages.extend(self.messages[..mi].iter().cloned());
            let m = &self.messages[mi];
            messages.push(Message::new(m.role, m.content[..=pi].to_vec()));
        }
        let has_system = self.system.as_deref().is_some_and(|s| !s.trim().is_empty());
        if messages.is_empty() && !has_system && self.tools.is_empty() {
            return None;
        }
        Some(ChatRequest {
            system: self.system.clone(),
            messages,
            tools: self.tools.clone(),
            json_schema: None,
            max_tokens: None,
            temperature: None,
            feature: self.feature.clone(),
            items: self.items.clone(),
            cache: self.cache.clone(),
        })
    }

    /// Capabilities a provider must have to serve this request.
    pub fn required_capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            vision: self
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|p| matches!(p, Part::Image { .. })),
            tools: !self.tools.is_empty(),
            json_schema: self.json_schema.is_some(),
            embeddings: false,
            context_tokens: None,
        }
    }
}

/// Why generation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    /// The model or a provider safety system declined. Discard partial output.
    Refusal,
    /// Output was filtered by the provider.
    ContentFilter,
    /// Anything else, verbatim from the provider.
    Other(String),
}

impl StopReason {
    /// Map the stop/finish reasons used by Anthropic and OpenAI-style APIs.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "end_turn" | "stop" | "completed" => StopReason::EndTurn,
            "max_tokens" | "length" | "max_output_tokens" | "model_context_window_exceeded" => {
                StopReason::MaxTokens
            }
            "stop_sequence" => StopReason::StopSequence,
            "tool_use" | "tool_calls" | "function_call" => StopReason::ToolUse,
            "refusal" => StopReason::Refusal,
            "content_filter" => StopReason::ContentFilter,
            other => StopReason::Other(other.to_string()),
        }
    }
}

/// One streamed event. A successful stream ends with exactly one `Done`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ChatEvent {
    TextDelta(String),
    /// A complete tool call. `arguments` is the parsed JSON; if the model emitted invalid JSON
    /// it is a JSON string holding the raw text, so validate before executing (see
    /// [`crate::schema::validate`]). Execute tool calls only after `Done`.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// Provider-private replay state; append it to the assistant message as [`Part::Opaque`]
    /// in stream order ([`ChatResponse::into_message`] does this).
    Opaque {
        kind: ProviderKind,
        data: Value,
    },
    /// Usage detail and timings, sent by the built-in adapters immediately before `Done`. A
    /// separate event (rather than new `Done` fields) so existing `Done` patterns keep compiling;
    /// consumers that do not care can ignore it.
    Stats(ChatStats),
    Done {
        stop_reason: StopReason,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    },
}

/// Token usage including prompt-cache detail, plus request timings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatStats {
    /// Same as `Done::input_tokens`: every prompt token, cached or not.
    #[serde(default)]
    pub input_tokens: Option<u32>,
    #[serde(default)]
    pub output_tokens: Option<u32>,
    /// Prompt tokens served from the provider's cache (included in `input_tokens`): Anthropic
    /// `cache_read_input_tokens`, OpenAI `cached_tokens`, llama-server `cached_tokens` /
    /// `timings.cache_n`. `None` if the provider did not say.
    #[serde(default)]
    pub cached_input_tokens: Option<u32>,
    /// Prompt tokens written to the cache by this request (Anthropic
    /// `cache_creation_input_tokens`; included in `input_tokens`).
    #[serde(default)]
    pub cache_write_tokens: Option<u32>,
    #[serde(default)]
    pub timings: Option<Timings>,
}

/// Wall-clock timings of one chat request, each measured from when the provider started sending
/// it (including retries). For a successful stream they are monotonic:
/// `headers <= first_byte <= first_token <= total`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timings {
    /// Time spent opening a new connection (DNS, TCP, TLS) for this request. `None` when a
    /// pooled connection was reused (or the client could not tell).
    #[serde(default)]
    pub connect: Option<Duration>,
    /// Until the response status and headers arrived (time to first byte of the response).
    pub headers: Duration,
    /// Until the first body bytes arrived.
    #[serde(default)]
    pub first_byte: Option<Duration>,
    /// Until the first text delta or tool call was decoded.
    #[serde(default)]
    pub first_token: Option<Duration>,
    /// Until the stream completed (`Done`).
    pub total: Duration,
    /// HTTP attempts made (1 = no retries).
    pub attempts: u32,
}

/// The streamed reply to a [`ChatRequest`].
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>;

/// A tool call collected from a stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// A fully collected reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Assistant content in stream order (text runs merged), ready to echo back.
    pub parts: Vec<Part>,
    pub stop_reason: StopReason,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Cache detail and timings, when the provider sent [`ChatEvent::Stats`].
    #[serde(default)]
    pub stats: Option<ChatStats>,
}

impl ChatResponse {
    /// Drain a stream into a response. Fails if the stream errors or ends without `Done`.
    pub async fn collect(mut stream: ChatStream) -> Result<Self> {
        let mut parts: Vec<Part> = Vec::new();
        let mut stats = None;
        while let Some(event) = stream.next().await {
            match event? {
                ChatEvent::Stats(s) => stats = Some(s),
                ChatEvent::TextDelta(text) => {
                    if let Some(Part::Text { text: last }) = parts.last_mut() {
                        last.push_str(&text);
                    } else {
                        parts.push(Part::Text { text });
                    }
                }
                ChatEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => parts.push(Part::ToolCall {
                    id,
                    name,
                    arguments,
                }),
                ChatEvent::Opaque { kind, data } => {
                    if is_fallback_marker(kind, &data) {
                        // Anthropic server-side fallback: content before the marker that is not
                        // plain text belongs to the declined attempt and must not be replayed.
                        parts.retain(|p| matches!(p, Part::Text { .. }));
                    }
                    parts.push(Part::Opaque { kind, data });
                }
                ChatEvent::Done {
                    stop_reason,
                    input_tokens,
                    output_tokens,
                } => {
                    return Ok(Self {
                        parts,
                        stop_reason,
                        input_tokens,
                        output_tokens,
                        stats,
                    });
                }
            }
        }
        Err(Error::Provider("stream ended without a Done event".into()))
    }

    /// All text, concatenated.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// The assistant message to append to the conversation.
    pub fn into_message(self) -> Message {
        Message::new(Role::Assistant, self.parts)
    }
}

pub(crate) fn is_fallback_marker(kind: ProviderKind, data: &Value) -> bool {
    kind == ProviderKind::Anthropic && data.get("type").and_then(Value::as_str) == Some("fallback")
}

/// Does `have` satisfy every capability set in `required`?
pub fn satisfies(have: &Capabilities, required: &Capabilities) -> bool {
    (!required.streaming || have.streaming)
        && (!required.vision || have.vision)
        && (!required.tools || have.tools)
        && (!required.json_schema || have.json_schema)
        && (!required.embeddings || have.embeddings)
        && match required.context_tokens {
            None => true,
            Some(need) => have.context_tokens.is_some_and(|c| c >= need),
        }
}

/// True if `required` asks for nothing beyond plain streaming chat.
pub(crate) fn requires_nothing_probed(required: &Capabilities) -> bool {
    !required.vision
        && !required.tools
        && !required.json_schema
        && !required.embeddings
        && required.context_tokens.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use serde_json::json;

    fn boxed(events: Vec<Result<ChatEvent>>) -> ChatStream {
        Box::pin(stream::iter(events))
    }

    #[tokio::test]
    async fn collect_merges_text_and_keeps_order() {
        let s = boxed(vec![
            Ok(ChatEvent::Opaque {
                kind: ProviderKind::Anthropic,
                data: json!({"type": "thinking", "thinking": "", "signature": "sig"}),
            }),
            Ok(ChatEvent::TextDelta("Hel".into())),
            Ok(ChatEvent::TextDelta("lo".into())),
            Ok(ChatEvent::ToolCall {
                id: "t1".into(),
                name: "f".into(),
                arguments: json!({"a": 1}),
            }),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::ToolUse,
                input_tokens: Some(3),
                output_tokens: Some(4),
            }),
        ]);
        let r = ChatResponse::collect(s).await.unwrap();
        assert_eq!(r.text(), "Hello");
        assert_eq!(r.parts.len(), 3);
        assert!(matches!(r.parts[0], Part::Opaque { .. }));
        assert_eq!(r.tool_calls()[0].arguments, json!({"a": 1}));
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        let m = r.into_message();
        assert_eq!(m.role, Role::Assistant);
    }

    #[tokio::test]
    async fn collect_drops_declined_partial_before_fallback_marker() {
        let s = boxed(vec![
            Ok(ChatEvent::Opaque {
                kind: ProviderKind::Anthropic,
                data: json!({"type": "thinking", "thinking": "", "signature": "a"}),
            }),
            Ok(ChatEvent::TextDelta("partial ".into())),
            Ok(ChatEvent::ToolCall {
                id: "t0".into(),
                name: "f".into(),
                arguments: json!({}),
            }),
            Ok(ChatEvent::Opaque {
                kind: ProviderKind::Anthropic,
                data: json!({"type": "fallback"}),
            }),
            Ok(ChatEvent::TextDelta("continued".into())),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ]);
        let r = ChatResponse::collect(s).await.unwrap();
        assert!(r.tool_calls().is_empty());
        assert_eq!(r.text(), "partial continued");
        assert!(
            !r.parts
                .iter()
                .any(|p| matches!(p, Part::Opaque { data, .. } if data["type"] == "thinking"))
        );
    }

    #[tokio::test]
    async fn collect_errors_without_done() {
        let s = boxed(vec![Ok(ChatEvent::TextDelta("x".into()))]);
        assert!(ChatResponse::collect(s).await.is_err());
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(StopReason::from_wire("end_turn"), StopReason::EndTurn);
        assert_eq!(StopReason::from_wire("stop"), StopReason::EndTurn);
        assert_eq!(StopReason::from_wire("length"), StopReason::MaxTokens);
        assert_eq!(StopReason::from_wire("tool_calls"), StopReason::ToolUse);
        assert_eq!(StopReason::from_wire("refusal"), StopReason::Refusal);
        assert_eq!(
            StopReason::from_wire("pause_turn"),
            StopReason::Other("pause_turn".into())
        );
    }

    #[test]
    fn capability_satisfaction() {
        let have = Capabilities {
            streaming: true,
            vision: true,
            tools: false,
            json_schema: true,
            embeddings: false,
            context_tokens: Some(1000),
        };
        let mut req = Capabilities {
            streaming: true,
            vision: true,
            ..Default::default()
        };
        assert!(satisfies(&have, &req));
        req.tools = true;
        assert!(!satisfies(&have, &req));
        req.tools = false;
        req.context_tokens = Some(2000);
        assert!(!satisfies(&have, &req));
        req.context_tokens = Some(500);
        assert!(satisfies(&have, &req));
    }

    #[test]
    fn required_capabilities_from_request() {
        let req = ChatRequest::new(vec![Message::user(vec![
            Part::text("what is this"),
            Part::image("image/png", "AAAA"),
        ])])
        .with_json_schema(json!({"type": "object"}));
        let caps = req.required_capabilities();
        assert!(caps.vision && caps.json_schema && caps.streaming);
        assert!(!caps.tools && !caps.embeddings);
    }

    #[test]
    fn part_serde_shape() {
        let p = Part::image("image/png", "AAAA");
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({"type": "image", "media_type": "image/png", "data_base64": "AAAA"})
        );
        let req: ChatRequest =
            serde_json::from_value(json!({"system": null, "messages": []})).unwrap();
        assert_eq!(req.feature, FEATURE_CHAT);
    }
}
