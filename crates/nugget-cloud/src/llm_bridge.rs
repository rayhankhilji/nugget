//! The AI gateway as a `nugget_infer::Provider`, so the reused crates' model steps (knowledge
//! extraction) run on the gateway's fast model: metered in `usage_events`, held to the user's
//! quota, and cached in `llm_cache` by prompt hash, so an unchanged item is never paid for twice.
//! Each job gets a cap on uncached requests; past it, requests fail and the item keeps its
//! deterministic facts until a later run.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use nugget_core::capability::{Capabilities, Locality, Tier as InferTier};
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, Role as InferRole,
    Router, StopReason,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{Db, DbError};
use crate::gateway::{Gateway, GatewayError, LlmRequest, Plan, Role, Tier};

pub const PROVIDER_ID: &str = "nugget-cloud-gateway";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct BridgeStats {
    pub cached: usize,
    pub fresh: usize,
    pub refused: usize,
}

pub struct GatewayProvider {
    id: ProviderId,
    db: Db,
    gateway: Gateway,
    user: Uuid,
    plan: Plan,
    fresh_left: AtomicUsize,
    cached: AtomicUsize,
    fresh: AtomicUsize,
    refused: AtomicUsize,
}

impl GatewayProvider {
    pub fn new(db: Db, gateway: Gateway, user: Uuid, plan: Plan, max_fresh: usize) -> Self {
        Self {
            id: ProviderId::new(PROVIDER_ID),
            db,
            gateway,
            user,
            plan,
            fresh_left: AtomicUsize::new(max_fresh),
            cached: AtomicUsize::new(0),
            fresh: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
        }
    }

    pub fn stats(&self) -> BridgeStats {
        BridgeStats {
            cached: self.cached.load(Ordering::Relaxed),
            fresh: self.fresh.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
        }
    }

    /// A router whose synthesis and light tiers are this provider.
    pub fn router(self: &Arc<Self>) -> Router {
        let p: Arc<dyn Provider> = self.clone();
        let tiers = [InferTier::Synthesis, InferTier::Light]
            .into_iter()
            .map(|t| (t, vec![self.id.clone()]))
            .collect();
        let r = Router::new(vec![p], tiers);
        r.set_capabilities(
            &self.id,
            Capabilities {
                streaming: true,
                json_schema: true,
                ..Default::default()
            },
        );
        r
    }
}

/// System prompt (with the JSON schema spelt out when one is required) and the conversation.
pub fn flatten(req: &ChatRequest) -> (String, Vec<(Role, String)>) {
    let mut system = req.system.clone().unwrap_or_default();
    if let Some(schema) = &req.json_schema {
        system.push_str(
            "\n\nReply with one JSON object only, no prose and no code fences, valid against this JSON Schema:\n",
        );
        system.push_str(&schema.to_string());
    }
    let messages = req
        .messages
        .iter()
        .map(|m| {
            let role = if m.role == InferRole::User {
                Role::User
            } else {
                Role::Assistant
            };
            let text: String = m
                .content
                .iter()
                .filter_map(|p| match p {
                    nugget_infer::Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            (role, text)
        })
        .collect();
    (system, messages)
}

pub fn cache_key(
    model: &str,
    system: &str,
    messages: &[(Role, String)],
    max_tokens: u32,
) -> String {
    let mut h = Sha256::new();
    for part in [model, system] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    for (r, t) in messages {
        h.update([u8::from(*r == Role::User)]);
        h.update(t.as_bytes());
        h.update([0]);
    }
    h.update(max_tokens.to_be_bytes());
    format!("{:x}", h.finalize())
}

async fn cached(db: &Db, user: Uuid, key: &str) -> std::result::Result<Option<String>, DbError> {
    db.scoped(Some(user), async |tx| {
        Ok(tx
            .query_opt(
                "select response from public.llm_cache where user_id = $1 and key = $2",
                &[&user, &key],
            )
            .await?
            .map(|r| r.get(0)))
    })
    .await
}

async fn remember(
    db: &Db,
    user: Uuid,
    key: &str,
    text: &str,
    tokens: (u32, u32),
) -> std::result::Result<(), DbError> {
    let (i, o) = (tokens.0 as i32, tokens.1 as i32);
    db.scoped(Some(user), async |tx| {
        tx.execute(
            "insert into public.llm_cache (user_id, key, response, input_tokens, output_tokens)
             values ($1, $2, $3, $4, $5) on conflict (user_id, key) do nothing",
            &[&user, &key, &text, &i, &o],
        )
        .await?;
        Ok(())
    })
    .await
}

fn stream(text: String, tokens: Option<(u32, u32)>) -> ChatStream {
    let events: Vec<Result<ChatEvent>> = vec![
        Ok(ChatEvent::TextDelta(text)),
        Ok(ChatEvent::Done {
            stop_reason: StopReason::EndTurn,
            input_tokens: tokens.map(|t| t.0),
            output_tokens: tokens.map(|t| t.1),
        }),
    ];
    Box::pin(futures::stream::iter(events))
}

#[async_trait]
impl Provider for GatewayProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::Anthropic,
            model: self.gateway.model_for(Tier::Fast, self.plan).to_owned(),
            locality: Locality::Cloud,
            context_tokens: None,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let (system, messages) = flatten(&req);
        let max_tokens = req.max_tokens.unwrap_or(1024);
        let model = self.gateway.model_for(Tier::Fast, self.plan).to_owned();
        let key = cache_key(&model, &system, &messages, max_tokens);
        let db_err = |e: DbError| Error::Storage(e.to_string());
        if let Some(text) = cached(&self.db, self.user, &key).await.map_err(db_err)? {
            self.cached.fetch_add(1, Ordering::Relaxed);
            return Ok(stream(text, None));
        }
        if self
            .fresh_left
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_err()
        {
            self.refused.fetch_add(1, Ordering::Relaxed);
            return Err(Error::RateLimited {
                retry_after_ms: None,
            });
        }
        let res = self
            .gateway
            .complete(
                &self.db,
                self.user,
                self.plan,
                Tier::Fast,
                LlmRequest {
                    model: String::new(),
                    system,
                    messages,
                    max_tokens,
                },
            )
            .await
            .map_err(|e| match e {
                GatewayError::QuotaExceeded => Error::RateLimited {
                    retry_after_ms: None,
                },
                other => Error::Provider(other.to_string()),
            })?;
        self.fresh.fetch_add(1, Ordering::Relaxed);
        let tokens = (res.input_tokens, res.output_tokens);
        remember(&self.db, self.user, &key, &res.text, tokens)
            .await
            .map_err(db_err)?;
        Ok(stream(res.text, Some(tokens)))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported(
            "embeddings go through the ingest path".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_infer::{Message, Part};

    #[test]
    fn schema_goes_into_the_system_prompt_and_keys_are_stable() {
        let req = ChatRequest::new(vec![Message::user(vec![Part::text("hello")])])
            .with_system("sys")
            .with_json_schema(serde_json::json!({"type": "object"}));
        let (system, messages) = flatten(&req);
        assert!(system.starts_with("sys\n\nReply with one JSON object"));
        assert!(system.ends_with(r#"{"type":"object"}"#));
        assert_eq!(messages, vec![(Role::User, "hello".to_owned())]);
        let k = cache_key("m", &system, &messages, 10);
        assert_eq!(k, cache_key("m", &system, &messages, 10));
        assert_ne!(k, cache_key("m2", &system, &messages, 10));
        assert_eq!(k.len(), 64);
    }
}
