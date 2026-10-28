//! Capability probe: prove, with live requests, what a provider's model can actually do.
//!
//! | Capability   | Check |
//! |--------------|-------|
//! | streaming    | a chat streams at least one text delta and ends with `Done` |
//! | vision       | a random 4-digit code rendered into a PNG in-process is read back |
//! | tools        | the model calls a given tool with a given (random) argument |
//! | json_schema  | the reply parses as JSON and validates against the requested schema |
//! | embeddings   | two texts embed to equal-length, finite, non-zero vectors |
//!
//! Checks run concurrently for cloud providers and sequentially for local servers (see
//! [`ProbeOptions::concurrent`]), each with a timeout. Requests are tagged `probe` in the egress
//! ledger.

pub mod vision;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures::StreamExt;
use nugget_core::capability::{Capabilities, Locality};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::provider::Provider;
use crate::schema;
use crate::types::{ChatEvent, ChatRequest, ChatResponse, FEATURE_PROBE, Message, Part, ToolSpec};

/// Probe tunables.
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// Per-check timeout.
    pub timeout: Duration,
    /// Output cap per probe request (leave room for models that think before answering).
    pub max_tokens: u32,
    /// Run the embeddings check (skip for chat-only providers to save a request).
    pub check_embeddings: bool,
    /// Run checks concurrently. `None` (default) = concurrently for cloud providers, one at a
    /// time for on-device / LAN servers, which often serve a single request at a time.
    pub concurrent: Option<bool>,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(90),
            max_tokens: 2048,
            check_embeddings: true,
            concurrent: None,
        }
    }
}

/// Outcome of one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeCheck {
    /// `streaming`, `vision`, `tools`, `json_schema` or `embeddings`.
    pub name: String,
    pub passed: bool,
    /// Why it failed (empty on success).
    pub detail: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeReport {
    pub capabilities: Capabilities,
    pub checks: Vec<ProbeCheck>,
}

/// Probe with default options and return the proven capabilities.
pub async fn probe(provider: &dyn Provider) -> Capabilities {
    probe_with(provider, &ProbeOptions::default())
        .await
        .capabilities
}

/// Probe and return per-check details.
pub async fn probe_with(provider: &dyn Provider, opts: &ProbeOptions) -> ProbeReport {
    let embeddings_check = async {
        if opts.check_embeddings {
            timed("embeddings", opts, check_embeddings(provider)).await
        } else {
            ProbeCheck {
                name: "embeddings".into(),
                passed: false,
                detail: "skipped".into(),
                elapsed_ms: 0,
            }
        }
    };
    let concurrent = opts
        .concurrent
        .unwrap_or(provider.meta().locality == Locality::Cloud);
    let (streaming, vision, tools, json_schema, embeddings) = if concurrent {
        futures::join!(
            timed("streaming", opts, check_streaming(provider, opts)),
            timed("vision", opts, check_vision(provider, opts)),
            timed("tools", opts, check_tools(provider, opts)),
            timed("json_schema", opts, check_json_schema(provider, opts)),
            embeddings_check,
        )
    } else {
        (
            timed("streaming", opts, check_streaming(provider, opts)).await,
            timed("vision", opts, check_vision(provider, opts)).await,
            timed("tools", opts, check_tools(provider, opts)).await,
            timed("json_schema", opts, check_json_schema(provider, opts)).await,
            embeddings_check.await,
        )
    };
    let capabilities = Capabilities {
        streaming: streaming.passed,
        vision: vision.passed,
        tools: tools.passed,
        json_schema: json_schema.passed,
        embeddings: embeddings.passed,
        context_tokens: provider.meta().context_tokens,
    };
    for c in [&streaming, &vision, &tools, &json_schema, &embeddings] {
        if !c.passed {
            tracing::info!(provider = %provider.id(), check = %c.name, detail = %c.detail, "probe check failed");
        }
    }
    ProbeReport {
        capabilities,
        checks: vec![streaming, vision, tools, json_schema, embeddings],
    }
}

async fn timed(
    name: &str,
    opts: &ProbeOptions,
    fut: impl Future<Output = Result<(), String>>,
) -> ProbeCheck {
    let start = Instant::now();
    let result = match tokio::time::timeout(opts.timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(format!("timed out after {:?}", opts.timeout)),
    };
    ProbeCheck {
        name: name.to_string(),
        passed: result.is_ok(),
        detail: result.err().unwrap_or_default(),
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

fn request(opts: &ProbeOptions, content: Vec<Part>) -> ChatRequest {
    ChatRequest::new(vec![Message::user(content)])
        .with_max_tokens(opts.max_tokens)
        .with_feature(FEATURE_PROBE)
}

async fn run(provider: &dyn Provider, req: ChatRequest) -> Result<ChatResponse, String> {
    let stream = provider.chat(req).await.map_err(|e| e.to_string())?;
    ChatResponse::collect(stream)
        .await
        .map_err(|e| e.to_string())
}

fn nonce() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    h.finish()
}

async fn check_streaming(provider: &dyn Provider, opts: &ProbeOptions) -> Result<(), String> {
    let req = request(opts, vec![Part::text("Reply with exactly one word: pong")]);
    let mut stream = provider.chat(req).await.map_err(|e| e.to_string())?;
    let mut deltas = 0usize;
    while let Some(ev) = stream.next().await {
        match ev.map_err(|e| e.to_string())? {
            ChatEvent::TextDelta(t) if !t.is_empty() => deltas += 1,
            ChatEvent::Done { .. } => {
                return if deltas > 0 {
                    Ok(())
                } else {
                    Err("stream finished without any text".into())
                };
            }
            _ => {}
        }
    }
    Err("stream ended without Done".into())
}

async fn check_vision(provider: &dyn Provider, opts: &ProbeOptions) -> Result<(), String> {
    let code = vision::random_code();
    let png = vision::render_code_png(&code).map_err(|e| e.to_string())?;
    let req = request(
        opts,
        vec![
            Part::image("image/png", B64.encode(&png)),
            Part::text(
                "The image shows a number with exactly 4 digits. Reply with only those 4 digits.",
            ),
        ],
    );
    let text = run(provider, req).await?.text();
    let runs: Vec<String> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if runs.contains(&code) || runs.concat() == code {
        Ok(())
    } else {
        Err(format!(
            "expected {code}, got {:?}",
            text.chars().take(80).collect::<String>()
        ))
    }
}

async fn check_tools(provider: &dyn Provider, opts: &ProbeOptions) -> Result<(), String> {
    let value = format!("nugget-{}", nonce() % 1_000_000);
    let tool = ToolSpec {
        name: "record_value".into(),
        description: "Records a value exactly as given.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"value": {"type": "string", "description": "The value to record."}},
            "required": ["value"],
            "additionalProperties": false,
        }),
    };
    let req = request(
        opts,
        vec![Part::text(format!(
            "Call the record_value tool with value set to exactly \"{value}\". Do not answer in text."
        ))],
    )
    .with_tools(vec![tool]);
    let resp = run(provider, req).await?;
    let calls = resp.tool_calls();
    if calls.is_empty() {
        return Err(format!(
            "no tool call (stop: {:?}, text: {:?})",
            resp.stop_reason,
            resp.text().chars().take(80).collect::<String>()
        ));
    }
    for c in &calls {
        if c.name == "record_value"
            && c.arguments
                .get("value")
                .and_then(Value::as_str)
                .map(str::trim)
                == Some(&value)
        {
            return Ok(());
        }
    }
    Err(format!("wrong tool call: {:?}", calls))
}

async fn check_json_schema(provider: &dyn Provider, opts: &ProbeOptions) -> Result<(), String> {
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": {"type": "integer"},
            "unit": {"type": "string", "enum": ["apples"]}
        },
        "required": ["answer", "unit"],
        "additionalProperties": false
    });
    let req = request(
        opts,
        vec![Part::text(
            "Tom has 3 apples and buys 4 more. Give the total as `answer` and the unit as `unit`.",
        )],
    )
    .with_json_schema(schema.clone());
    let text = run(provider, req).await?.text();
    let value: Value = serde_json::from_str(text.trim()).map_err(|e| {
        format!(
            "reply is not JSON ({e}): {:?}",
            text.chars().take(80).collect::<String>()
        )
    })?;
    schema::validate(&schema, &value).map_err(|e| e.to_string())
}

async fn check_embeddings(provider: &dyn Provider) -> Result<(), String> {
    let texts = vec![
        "The cat sat on the mat.".to_string(),
        "Quarterly revenue grew by four percent.".to_string(),
    ];
    let vecs = provider.embed(&texts).await.map_err(|e| e.to_string())?;
    if vecs.len() != texts.len() {
        return Err(format!(
            "expected {} vectors, got {}",
            texts.len(),
            vecs.len()
        ));
    }
    let dim = vecs[0].len();
    if dim == 0 || vecs.iter().any(|v| v.len() != dim) {
        return Err("empty or inconsistent dimensions".into());
    }
    if vecs
        .iter()
        .any(|v| v.iter().any(|x| !x.is_finite()) || v.iter().all(|x| *x == 0.0))
    {
        return Err("non-finite or all-zero vector".into());
    }
    Ok(())
}
