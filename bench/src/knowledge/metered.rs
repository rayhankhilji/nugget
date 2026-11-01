//! [`MeteredProvider`]: a [`Provider`] decorator that counts, times and bounds every chat
//! request a real model gets from the knowledge pipeline (`knowledge --local-models`).
//!
//! The bench's per-request timeout is shorter than the pipeline's own so it always fires first.
//! Each request is classified once (ok, timeout, failure, unparseable, schema-invalid) and its
//! answer replayed to the pipeline unchanged.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use nugget_infer::{ChatEvent, ChatRequest, ChatStream, Provider, ProviderMeta, StopReason};
use serde_json::{Value, json};

/// Counters over the requests a [`MeteredProvider`] has seen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meter {
    pub requests: usize,
    pub ok: usize,
    pub failures: usize,
    pub timeouts: usize,
    pub unparseable: usize,
    pub schema_invalid: usize,
    /// Answers that stopped at the output-token cap (also counted in their class).
    pub truncated: usize,
    /// Wall time of each request in seconds, in order (timeouts at the timeout).
    pub latencies_s: Vec<f64>,
    /// Token counts the server reported (`Done`), summed over requests that reported them.
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Meter {
    /// Requests that produced no usable extraction: failures, timeouts and unparseable answers.
    pub fn lost(&self) -> usize {
        self.failures + self.timeouts + self.unparseable
    }

    pub fn to_json(&self) -> Value {
        json!({
            "requests": self.requests,
            "ok": self.ok,
            "failures": self.failures,
            "timeouts": self.timeouts,
            "unparseable": self.unparseable,
            "schema_invalid": self.schema_invalid,
            "truncated": self.truncated,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
        })
    }
}

/// One request as the model saw and answered it, for the suite's diagnostic dump.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestLog {
    /// The items the request is about (its egress provenance).
    pub items: Vec<ItemRef>,
    /// The `[chunk N]` numbers in the prompt.
    pub chunk_ords: Vec<u32>,
    /// `ok`, `failure`, `timeout`, `unparseable` or `schema_invalid`.
    pub outcome: &'static str,
    pub latency_s: f64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// The answer text (empty for failures and timeouts).
    pub answer: String,
}

/// The `[chunk N]` numbers in a prompt.
fn chunk_ords(prompt: &str) -> Vec<u32> {
    prompt
        .match_indices("[chunk ")
        .filter_map(|(at, m)| {
            let rest = &prompt[at + m.len()..];
            rest[..rest.find(']')?].trim().parse().ok()
        })
        .collect()
}

/// How one request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ok,
    Failure,
    Timeout,
    Unparseable,
    SchemaInvalid,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failure => "failure",
            Outcome::Timeout => "timeout",
            Outcome::Unparseable => "unparseable",
            Outcome::SchemaInvalid => "schema_invalid",
        }
    }
}

pub struct MeteredProvider {
    inner: Arc<dyn Provider>,
    timeout: Duration,
    meter: Mutex<Meter>,
    log: Mutex<Vec<RequestLog>>,
}

impl MeteredProvider {
    pub fn new(inner: Arc<dyn Provider>, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            meter: Mutex::new(Meter::default()),
            log: Mutex::new(Vec::new()),
        }
    }

    /// Every request so far, in order.
    pub fn requests(&self) -> Vec<RequestLog> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn log(&self, entry: RequestLog) {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry);
    }

    /// A copy of the counters so far.
    pub fn snapshot(&self) -> Meter {
        self.meter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record(&self, outcome: Outcome, elapsed: Duration, done: Option<&ChatEvent>) {
        let mut m = self.meter.lock().unwrap_or_else(PoisonError::into_inner);
        m.requests += 1;
        m.latencies_s.push(elapsed.as_secs_f64());
        match outcome {
            Outcome::Ok => m.ok += 1,
            Outcome::Failure => m.failures += 1,
            Outcome::Timeout => m.timeouts += 1,
            Outcome::Unparseable => m.unparseable += 1,
            Outcome::SchemaInvalid => m.schema_invalid += 1,
        }
        if let Some(ChatEvent::Done {
            stop_reason,
            input_tokens,
            output_tokens,
        }) = done
        {
            if *stop_reason == StopReason::MaxTokens {
                m.truncated += 1;
            }
            m.input_tokens += u64::from(input_tokens.unwrap_or(0));
            m.output_tokens += u64::from(output_tokens.unwrap_or(0));
        }
    }
}

/// The JSON in a model's answer, with `nugget-knowledge`'s tolerance: the whole text, or the
/// outermost `{…}` (code fences and prose around it).
pub fn parse_answer(raw: &str) -> Option<Value> {
    let t = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    match (t.find('{'), t.rfind('}')) {
        (Some(a), Some(b)) if b > a => serde_json::from_str(&t[a..=b]).ok(),
        _ => None,
    }
}

/// Classify a complete event list (the stream ran to its end or to its first error).
fn classify(events: &[Result<ChatEvent>], schema: Option<&Value>) -> Outcome {
    if events.iter().any(Result::is_err) {
        return Outcome::Failure;
    }
    let Some(Ok(ChatEvent::Done { stop_reason, .. })) = events
        .iter()
        .find(|e| matches!(e, Ok(ChatEvent::Done { .. })))
    else {
        return Outcome::Failure;
    };
    if matches!(stop_reason, StopReason::Refusal | StopReason::ContentFilter) {
        return Outcome::Failure;
    }
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Ok(ChatEvent::TextDelta(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let Some(v) = parse_answer(&text) else {
        return Outcome::Unparseable;
    };
    match schema {
        Some(s) if nugget_infer::schema::validate(s, &v).is_err() => Outcome::SchemaInvalid,
        _ => Outcome::Ok,
    }
}

#[async_trait]
impl Provider for MeteredProvider {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }

    fn meta(&self) -> ProviderMeta {
        self.inner.meta()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let schema = req.json_schema.clone();
        let items = req.items.clone();
        let ords = req
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|p| match p {
                nugget_infer::Part::Text { text } => Some(chunk_ords(text)),
                _ => None,
            })
            .flatten()
            .collect::<Vec<u32>>();
        let entry = |outcome: Outcome, elapsed: Duration, done: Option<&ChatEvent>, answer| {
            let (input_tokens, output_tokens) = match done {
                Some(ChatEvent::Done {
                    input_tokens,
                    output_tokens,
                    ..
                }) => (*input_tokens, *output_tokens),
                _ => (None, None),
            };
            RequestLog {
                items: items.clone(),
                chunk_ords: ords.clone(),
                outcome: outcome.as_str(),
                latency_s: elapsed.as_secs_f64(),
                input_tokens,
                output_tokens,
                answer,
            }
        };
        let started = Instant::now();
        let inner = self.inner.clone();
        let res = tokio::time::timeout(self.timeout, async move {
            let mut s = inner.chat(req).await?;
            let mut events = Vec::new();
            while let Some(ev) = s.next().await {
                let stop = ev.is_err() || matches!(ev, Ok(ChatEvent::Done { .. }));
                events.push(ev);
                if stop {
                    break;
                }
            }
            Ok::<_, Error>(events)
        })
        .await;
        let elapsed = started.elapsed();
        match res {
            Err(_) => {
                self.record(Outcome::Timeout, elapsed, None);
                self.log(entry(Outcome::Timeout, elapsed, None, String::new()));
                Err(Error::Provider(format!(
                    "{}: no answer within the bench timeout of {} s",
                    self.inner.id(),
                    self.timeout.as_secs_f64()
                )))
            }
            Ok(Err(e)) => {
                self.record(Outcome::Failure, elapsed, None);
                self.log(entry(Outcome::Failure, elapsed, None, String::new()));
                Err(e)
            }
            Ok(Ok(events)) => {
                let outcome = classify(&events, schema.as_ref());
                let done = events
                    .iter()
                    .filter_map(|e| e.as_ref().ok())
                    .find(|e| matches!(e, ChatEvent::Done { .. }));
                self.record(outcome, elapsed, done);
                let text: String = events
                    .iter()
                    .filter_map(|e| match e {
                        Ok(ChatEvent::TextDelta(t)) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                self.log(entry(outcome, elapsed, done, text));
                Ok(Box::pin(stream::iter(events)))
            }
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed(texts).await
    }

    async fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_query(texts).await
    }

    async fn embed_items(
        &self,
        texts: &[String],
        items: &[nugget_core::ids::ItemRef],
    ) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_items(texts, items).await
    }

    fn embed_prefixes(&self) -> (String, String) {
        self.inner.embed_prefixes()
    }
}
