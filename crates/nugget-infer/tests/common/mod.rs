#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::{Error, Result};
use nugget_infer::{
    ApiKeyRef, ChatEvent, ChatStream, Provider, ProviderConfig, Secret, SecretResolver,
    build_provider_with,
};
use serde_json::Value;
use wiremock::ResponseTemplate;

#[derive(Default)]
pub struct RecordingSink(Mutex<Vec<EgressEvent>>);

impl RecordingSink {
    pub fn events(&self) -> Vec<EgressEvent> {
        self.0.lock().unwrap().clone()
    }
}

impl EgressSink for RecordingSink {
    fn record(&self, event: EgressEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// Resolves every key to `test-key` (and `env:MISSING` to NotFound).
pub struct FakeSecrets;

impl SecretResolver for FakeSecrets {
    fn resolve(&self, key: &ApiKeyRef) -> Result<Secret> {
        match key {
            ApiKeyRef::Env(v) if v == "MISSING" => Err(Error::NotFound("unset".into())),
            _ => Ok(Secret::new("test-key")),
        }
    }
}

/// Build a provider from a TOML body (without `base_url`) pointed at `base_url`.
pub fn provider(toml_body: &str, base_url: &str) -> (Arc<dyn Provider>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let cfg = ProviderConfig::from_toml_str(&format!(
        "base_url = \"{base_url}\"\nretry_base_ms = 1\n{toml_body}"
    ))
    .unwrap();
    let p = build_provider_with(&cfg, sink.clone(), &FakeSecrets).unwrap();
    (p, sink)
}

/// An SSE body from `(event name, json data)` pairs.
pub fn sse(events: &[(Option<&str>, Value)]) -> String {
    let mut s = String::new();
    for (name, data) in events {
        if let Some(n) = name {
            s.push_str(&format!("event: {n}\n"));
        }
        s.push_str(&format!("data: {data}\n\n"));
    }
    s
}

/// SSE from raw `data:` payloads (e.g. `[DONE]`).
pub fn sse_raw(datas: &[&str]) -> String {
    datas.iter().map(|d| format!("data: {d}\n\n")).collect()
}

pub fn sse_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

pub async fn collect_all(mut stream: ChatStream) -> Vec<Result<ChatEvent>> {
    use futures::StreamExt;
    let mut out = Vec::new();
    while let Some(e) = stream.next().await {
        out.push(e);
    }
    out
}
