//! A real model for the `nugget` arm: built on the agent thread, instrumented, and made robust
//! to what small open models served by llama-server actually send.
//!
//! - **Built where it runs.** Each task's agent runs on its own short-lived tokio runtime (see
//!   [`super::nugget`]). A reqwest client keeps pooled connections on the runtime it first ran
//!   on, so the provider is built lazily from a [`ProviderFactory`] on the first model turn, on
//!   the agent's runtime, and dropped with the task.
//! - **Counted, never hidden.** Provider failures (request, mid-stream error, a stream without
//!   an end) are counted in [`ModelCounters::errors`] and passed to the loop, which ends the run
//!   with an error; tool calls whose arguments are not a JSON object and replies that write a
//!   tool call as text are counted as malformed (the loop then rejects or nudges them as it would
//!   in the app).
//! - **Normalised, not repaired.** Two things are fixed so that the measurement is about the
//!   model and not the wire: a tool call without an id, or with an id already used in the run,
//!   gets a fresh one (`nugget-agent` derives the action id from the call id, so a reused id
//!   would replay the first call's recorded result instead of acting); and `<think>` blocks that
//!   reach the content (a server without reasoning extraction) are dropped from the reply text.
//!   Arguments are never rewritten: invalid ones stay invalid and the loop rejects them.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_infer::{ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta};

/// Builds the provider for one task, on the agent thread's runtime.
pub type ProviderFactory = Arc<dyn Fn() -> Result<Arc<dyn Provider>, String> + Send + Sync>;

/// What the real model did, over every task of a run.
#[derive(Debug, Default)]
pub struct ModelCounters {
    /// Model turns requested.
    pub turns: AtomicU64,
    /// Provider failures: the provider could not be built, the request failed, the stream
    /// carried an error or ended without finishing.
    pub errors: AtomicU64,
    /// Tool calls without an id, or reusing one, that got a fresh id.
    pub missing_ids: AtomicU64,
    /// Tool calls whose arguments were not a JSON object (unparseable or double-encoded).
    pub string_args: AtomicU64,
    /// Replies without tool calls whose text looks like a tool call.
    pub text_tool_calls: AtomicU64,
    /// `<think>` blocks dropped from reply text.
    pub think_blocks: AtomicU64,
}

impl ModelCounters {
    pub fn get(c: &AtomicU64) -> u64 {
        c.load(Ordering::Relaxed)
    }

    fn bump(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }
}

/// A provider built on first use, with the normalisation and counts of the module docs. One per
/// task.
pub struct RealModel {
    id: ProviderId,
    label: String,
    factory: ProviderFactory,
    inner: Mutex<Option<Arc<dyn Provider>>>,
    counters: Arc<ModelCounters>,
    /// Tool call ids seen in this task.
    seen_ids: Mutex<HashSet<String>>,
    turn: AtomicU64,
}

impl RealModel {
    pub fn new(label: &str, factory: ProviderFactory, counters: Arc<ModelCounters>) -> Self {
        Self {
            id: ProviderId::new("cua-model"),
            label: label.to_owned(),
            factory,
            inner: Mutex::new(None),
            counters,
            seen_ids: Mutex::new(HashSet::new()),
            turn: AtomicU64::new(0),
        }
    }

    fn provider(&self) -> nugget_core::Result<Arc<dyn Provider>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = inner.as_ref() {
            return Ok(p.clone());
        }
        let p = (self.factory)().map_err(nugget_core::Error::Provider)?;
        *inner = Some(p.clone());
        Ok(p)
    }

    fn failed<T>(&self, e: nugget_core::Error) -> nugget_core::Result<T> {
        ModelCounters::bump(&self.counters.errors);
        Err(e)
    }

    /// Fresh ids, think blocks out of the text, counts.
    fn normalise(&self, turn: u64, events: Vec<ChatEvent>) -> Vec<ChatEvent> {
        let c = &self.counters;
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let (clean, blocks) = strip_think(&text);
        for _ in 0..blocks {
            ModelCounters::bump(&c.think_blocks);
        }
        let has_calls = events
            .iter()
            .any(|e| matches!(e, ChatEvent::ToolCall { .. }));
        if !has_calls && looks_like_tool_call(&clean) {
            ModelCounters::bump(&c.text_tool_calls);
        }
        let mut seen = self.seen_ids.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::with_capacity(events.len());
        let mut text_done = false;
        let mut n = 0;
        for e in events {
            match e {
                ChatEvent::TextDelta(_) => {
                    if !text_done && !clean.is_empty() {
                        out.push(ChatEvent::TextDelta(clean.clone()));
                    }
                    text_done = true;
                }
                ChatEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    n += 1;
                    let id = id.trim().to_owned();
                    let id = if id.is_empty() || seen.contains(&id) {
                        ModelCounters::bump(&c.missing_ids);
                        let mut fresh = format!("call_t{turn}_{n}");
                        while seen.contains(&fresh) {
                            fresh.push('_');
                        }
                        fresh
                    } else {
                        id
                    };
                    seen.insert(id.clone());
                    if !arguments.is_object() {
                        ModelCounters::bump(&c.string_args);
                    }
                    out.push(ChatEvent::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                other => out.push(other),
            }
        }
        out
    }
}

/// Drop `<think>…</think>` blocks (and an unclosed `<think>` to the end, and reasoning before a
/// lone `</think>` when the template opened the block in the prompt). Returns the text and the
/// number of blocks dropped.
pub fn strip_think(text: &str) -> (String, u32) {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut blocks = 0;
    let mut rest = text;
    // Reasoning whose opening tag was in the prompt.
    if let Some(close) = rest.find(CLOSE)
        && rest.find(OPEN).is_none_or(|open| open > close)
    {
        blocks += 1;
        rest = &rest[close + CLOSE.len()..];
    }
    let mut out = String::with_capacity(rest.len());
    while let Some(open) = rest.find(OPEN) {
        out.push_str(&rest[..open]);
        blocks += 1;
        let after = &rest[open + OPEN.len()..];
        match after.find(CLOSE) {
            Some(close) => rest = &after[close + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    if blocks == 0 {
        return (text.to_owned(), 0);
    }
    (out.trim().to_owned(), blocks)
}

/// A tool call written into the reply text instead of `tool_calls` (Hermes/Qwen `<tool_call>`
/// tags, a function tag, or a bare `{"name": .., "arguments": ..}` object).
pub fn looks_like_tool_call(text: &str) -> bool {
    text.contains("<tool_call>")
        || text.contains("<function=")
        || (text.contains("\"name\"") && text.contains("\"arguments\""))
}

#[async_trait]
impl Provider for RealModel {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.as_ref() {
            Some(p) => p.meta(),
            None => ProviderMeta {
                kind: ProviderKind::OpenaiCompatible,
                model: self.label.clone(),
                locality: Locality::OnDevice,
                context_tokens: None,
            },
        }
    }

    async fn chat(&self, req: ChatRequest) -> nugget_core::Result<ChatStream> {
        let turn = self.turn.fetch_add(1, Ordering::Relaxed) + 1;
        ModelCounters::bump(&self.counters.turns);
        let provider = match self.provider() {
            Ok(p) => p,
            Err(e) => return self.failed(e),
        };
        let mut stream = match provider.chat(req).await {
            Ok(s) => s,
            Err(e) => return self.failed(e),
        };
        let mut events = Vec::new();
        let mut done = false;
        while let Some(ev) = stream.next().await {
            match ev {
                Err(e) => return self.failed(e),
                Ok(ev) => {
                    let end = matches!(ev, ChatEvent::Done { .. });
                    events.push(ev);
                    if end {
                        done = true;
                        break;
                    }
                }
            }
        }
        if !done {
            return self.failed(nugget_core::Error::Provider(
                "the model's stream ended without finishing".into(),
            ));
        }
        let events = self.normalise(turn, events);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    async fn embed(&self, _texts: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
        Err(nugget_core::Error::Unsupported("embed".into()))
    }
}
