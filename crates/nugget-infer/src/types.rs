//! Provider-neutral request and response types.

use std::fmt;
use std::pin::Pin;

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
    Done {
        stop_reason: StopReason,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    },
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
}

impl ChatResponse {
    /// Drain a stream into a response. Fails if the stream errors or ends without `Done`.
    pub async fn collect(mut stream: ChatStream) -> Result<Self> {
        let mut parts: Vec<Part> = Vec::new();
        while let Some(event) = stream.next().await {
            match event? {
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
