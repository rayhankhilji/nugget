//! AI gateway: every model call goes through here, with our provider keys, so it is metered
//! (`usage_events`) and held to the user's monthly quota. No key ever reaches a client.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::config::Config;
use crate::db::{Db, DbError, UsageKind};

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("monthly AI quota used up")]
    QuotaExceeded,
    #[error("no LLM provider configured (set ANTHROPIC_API_KEY or OPENAI_API_KEY)")]
    NotConfigured,
    #[error("provider error: {0}")]
    Provider(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<(Role, String)>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[async_trait]
pub trait Llm: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, GatewayError>;
}

#[async_trait]
pub trait Embedder: Send + Sync {
    fn model(&self) -> &str;
    fn dims(&self) -> usize;
    /// One vector per text, plus the input tokens billed.
    async fn embed(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, u32), GatewayError>;
}

/// The user's plan, which picks the answer model and the quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Free,
    Pro,
}

impl Plan {
    pub fn parse(s: &str) -> Self {
        if s == "pro" { Self::Pro } else { Self::Free }
    }
    /// Monthly AI budget in micro-USD (see the doc's cost table).
    pub fn quota_micros(self) -> i64 {
        match self {
            Self::Free => 750_000,
            Self::Pro => 8_000_000,
        }
    }
}

/// Which kind of work a call is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Extraction, classification: always the cheap model.
    Fast,
    /// Answers: the plan's model.
    Answer,
}

/// Price in USD per million tokens (input, output), from the providers' official pages
/// (2026-11). Unknown models are priced like the most expensive one we use, so metering errs
/// high, never low.
pub fn price_per_mtok(model: &str) -> (f64, f64) {
    const TABLE: &[(&str, f64, f64)] = &[
        ("claude-haiku-5-5", 0.10, 0.50),
        ("claude-sonnet-5-5", 2.0, 10.0),
        ("gpt-5-nano", 0.05, 0.40),
        ("text-embedding-3-small", 0.02, 0.0),
    ];
    TABLE
        .iter()
        .find(|(m, ..)| model.starts_with(m))
        .map(|&(_, i, o)| (i, o))
        .unwrap_or((2.0, 10.0))
}

pub fn cost_micros(model: &str, input_tokens: u32, output_tokens: u32) -> i64 {
    let (i, o) = price_per_mtok(model);
    // USD per million tokens == micro-USD per token.
    (f64::from(input_tokens) * i + f64::from(output_tokens) * o).ceil() as i64
}

#[derive(Clone)]
pub struct Gateway {
    llm: Option<Arc<dyn Llm>>,
    embedder: Option<Arc<dyn Embedder>>,
    model_fast: String,
    model_smart: String,
}

impl Gateway {
    pub fn new(
        llm: Option<Arc<dyn Llm>>,
        embedder: Option<Arc<dyn Embedder>>,
        model_fast: String,
        model_smart: String,
    ) -> Self {
        Self {
            llm,
            embedder,
            model_fast,
            model_smart,
        }
    }

    /// Providers from the environment: Anthropic first, then OpenAI; embeddings need OpenAI.
    pub fn from_config(cfg: &Config, http: reqwest::Client) -> Self {
        let (llm, fast, smart): (Option<Arc<dyn Llm>>, &str, &str) =
            if let Some(key) = &cfg.anthropic_api_key {
                let a = Anthropic {
                    http: http.clone(),
                    base: cfg.anthropic_base.clone(),
                    key: key.clone(),
                };
                (Some(Arc::new(a)), "claude-haiku-5-5", "claude-sonnet-5-5")
            } else if let Some(key) = &cfg.openai_api_key {
                let o = OpenAi {
                    http: http.clone(),
                    base: cfg.openai_base.clone(),
                    key: key.clone(),
                };
                (Some(Arc::new(o)), "gpt-5-nano", "gpt-5-nano")
            } else {
                (None, "", "")
            };
        let embedder: Option<Arc<dyn Embedder>> = cfg.openai_api_key.as_ref().map(|key| {
            Arc::new(OpenAi {
                http,
                base: cfg.openai_base.clone(),
                key: key.clone(),
            }) as Arc<dyn Embedder>
        });
        Self::new(
            llm,
            embedder,
            cfg.model_fast.clone().unwrap_or_else(|| fast.into()),
            cfg.model_smart.clone().unwrap_or_else(|| smart.into()),
        )
    }

    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    pub fn has_embeddings(&self) -> bool {
        self.embedder.is_some()
    }

    pub fn model_for(&self, tier: Tier, plan: Plan) -> &str {
        match (tier, plan) {
            (Tier::Answer, Plan::Pro) => &self.model_smart,
            _ => &self.model_fast,
        }
    }

    /// Fails with [`GatewayError::QuotaExceeded`] when the month's usage has reached the plan's quota.
    pub async fn check_quota(&self, db: &Db, user: Uuid, plan: Plan) -> Result<(), GatewayError> {
        if db.month_cost_micros(user).await? >= plan.quota_micros() {
            return Err(GatewayError::QuotaExceeded);
        }
        Ok(())
    }

    /// A metered completion. The model is chosen here from `tier` and `plan`; `req.model` is
    /// overwritten.
    pub async fn complete(
        &self,
        db: &Db,
        user: Uuid,
        plan: Plan,
        tier: Tier,
        mut req: LlmRequest,
    ) -> Result<LlmResponse, GatewayError> {
        let llm = self.llm.as_ref().ok_or(GatewayError::NotConfigured)?;
        self.check_quota(db, user, plan).await?;
        let model = self.model_for(tier, plan).to_owned();
        req.model = model.clone();
        let res = llm.complete(&req).await?;
        let cost = cost_micros(&model, res.input_tokens, res.output_tokens);
        crate::ops::METRICS.gateway_cost(cost);
        db.record_usage(
            user,
            UsageKind::Llm,
            &model,
            res.input_tokens,
            res.output_tokens,
            cost,
        )
        .await?;
        Ok(res)
    }

    /// Metered embeddings; `None` when no embedder is configured (full-text search only).
    pub async fn embed(
        &self,
        db: &Db,
        user: Uuid,
        texts: &[String],
    ) -> Result<Option<Vec<Vec<f32>>>, GatewayError> {
        let Some(e) = &self.embedder else {
            return Ok(None);
        };
        if texts.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let (vecs, tokens) = e.embed(texts).await?;
        if vecs.len() != texts.len() || vecs.iter().any(|v| v.len() != e.dims()) {
            return Err(GatewayError::Provider(
                "embedding count or size mismatch".into(),
            ));
        }
        crate::ops::METRICS.gateway_cost(cost_micros(e.model(), tokens, 0));
        db.record_usage(
            user,
            UsageKind::Embed,
            e.model(),
            tokens,
            0,
            cost_micros(e.model(), tokens, 0),
        )
        .await?;
        Ok(Some(vecs))
    }
}

fn provider_err(e: impl std::fmt::Display) -> GatewayError {
    GatewayError::Provider(e.to_string())
}

async fn post_json(req: reqwest::RequestBuilder, body: Value) -> Result<Value, GatewayError> {
    let res = req.json(&body).send().await.map_err(provider_err)?;
    let status = res.status();
    let v: Value = res.json().await.map_err(provider_err)?;
    if !status.is_success() {
        let msg = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        return Err(GatewayError::Provider(format!("{status}: {msg}")));
    }
    Ok(v)
}

fn tokens(v: &Value, ptr: &str) -> u32 {
    v.pointer(ptr)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

/// Anthropic Messages API.
pub struct Anthropic {
    pub http: reqwest::Client,
    pub base: String,
    pub key: String,
}

#[async_trait]
impl Llm for Anthropic {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, GatewayError> {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|(r, c)| json!({"role": if *r == Role::User {"user"} else {"assistant"}, "content": c}))
            .collect();
        let body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "system": req.system,
            "messages": messages,
        });
        let r = self
            .http
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01");
        let v = post_json(r, body).await?;
        let text = v["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect::<String>()
            })
            .unwrap_or_default();
        Ok(LlmResponse {
            text,
            input_tokens: tokens(&v, "/usage/input_tokens"),
            output_tokens: tokens(&v, "/usage/output_tokens"),
        })
    }
}

/// OpenAI Chat Completions and Embeddings.
pub struct OpenAi {
    pub http: reqwest::Client,
    pub base: String,
    pub key: String,
}

#[async_trait]
impl Llm for OpenAi {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, GatewayError> {
        let mut messages = vec![json!({"role": "system", "content": req.system})];
        messages.extend(req.messages.iter().map(
            |(r, c)| json!({"role": if *r == Role::User {"user"} else {"assistant"}, "content": c}),
        ));
        let body = json!({"model": req.model, "messages": messages, "max_completion_tokens": req.max_tokens});
        let r = self
            .http
            .post(format!("{}/v1/chat/completions", self.base))
            .bearer_auth(&self.key);
        let v = post_json(r, body).await?;
        Ok(LlmResponse {
            text: v
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            input_tokens: tokens(&v, "/usage/prompt_tokens"),
            output_tokens: tokens(&v, "/usage/completion_tokens"),
        })
    }
}

#[async_trait]
impl Embedder for OpenAi {
    fn model(&self) -> &str {
        "text-embedding-3-small"
    }
    fn dims(&self) -> usize {
        1536
    }
    async fn embed(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, u32), GatewayError> {
        let body = json!({"model": self.model(), "input": texts});
        let r = self
            .http
            .post(format!("{}/v1/embeddings", self.base))
            .bearer_auth(&self.key);
        let v = post_json(r, body).await?;
        let mut rows: Vec<(u64, Vec<f32>)> = v["data"]
            .as_array()
            .ok_or_else(|| provider_err("no data"))?
            .iter()
            .map(|d| {
                let idx = d["index"].as_u64().unwrap_or(0);
                let e = d["embedding"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_f64)
                            .map(|x| x as f32)
                            .collect()
                    })
                    .unwrap_or_default();
                (idx, e)
            })
            .collect();
        rows.sort_by_key(|(i, _)| *i);
        Ok((
            rows.into_iter().map(|(_, e)| e).collect(),
            tokens(&v, "/usage/prompt_tokens"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn costs_and_quotas() {
        // 7,800 in + 520 out on Haiku 5.5: 780 + 260 = 1,040 micro-USD.
        assert_eq!(cost_micros("claude-haiku-5-5", 7_800, 520), 1_040);
        assert_eq!(
            cost_micros("claude-sonnet-5-5-20261001", 1_000_000, 0),
            2_000_000
        );
        // Unknown models are priced high.
        assert_eq!(cost_micros("mystery", 1_000, 100), 3_000);
        assert_eq!(Plan::parse("pro"), Plan::Pro);
        assert_eq!(Plan::parse("anything"), Plan::Free);
        assert!(Plan::Pro.quota_micros() > Plan::Free.quota_micros());
    }

    #[test]
    fn model_per_tier() {
        let g = Gateway::new(None, None, "fast".into(), "smart".into());
        assert_eq!(g.model_for(Tier::Answer, Plan::Free), "fast");
        assert_eq!(g.model_for(Tier::Answer, Plan::Pro), "smart");
        assert_eq!(g.model_for(Tier::Fast, Plan::Pro), "fast");
    }

    #[tokio::test]
    async fn anthropic_request_and_parse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "k"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "Hello [1]"}],
                "usage": {"input_tokens": 12, "output_tokens": 3}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let a = Anthropic {
            http: reqwest::Client::new(),
            base: server.uri(),
            key: "k".into(),
        };
        let r = a
            .complete(&LlmRequest {
                model: "m".into(),
                system: "s".into(),
                messages: vec![(Role::User, "q".into())],
                max_tokens: 10,
            })
            .await
            .unwrap();
        assert_eq!(
            r,
            LlmResponse {
                text: "Hello [1]".into(),
                input_tokens: 12,
                output_tokens: 3
            }
        );
        let body: Value = server.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        assert_eq!(body["system"], "s");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[tokio::test]
    async fn provider_errors_surface() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"error": {"message": "bad model"}})),
            )
            .mount(&server)
            .await;
        let o = OpenAi {
            http: reqwest::Client::new(),
            base: server.uri(),
            key: "k".into(),
        };
        let err = o
            .complete(&LlmRequest {
                model: "m".into(),
                system: String::new(),
                messages: vec![],
                max_tokens: 1,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bad model"), "{err}");
    }

    #[tokio::test]
    async fn openai_embeddings_keep_input_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 1, "embedding": [0.0, 1.0]}, {"index": 0, "embedding": [1.0, 0.0]}],
                "usage": {"prompt_tokens": 7}
            })))
            .mount(&server)
            .await;
        let o = OpenAi {
            http: reqwest::Client::new(),
            base: server.uri(),
            key: "k".into(),
        };
        let (v, t) = o.embed(&["a".into(), "b".into()]).await.unwrap();
        assert_eq!(v, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert_eq!(t, 7);
    }
}
