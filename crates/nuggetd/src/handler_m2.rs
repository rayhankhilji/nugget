//! M2 IPC methods: `knowledge.*`, `mcp.clients.*` and the daemon-private `mcp.bridge.*`.

use nugget_core::ipc::{
    ClaimStatusRequest, EntitiesRequest, EntityDetail, EntityRef, GraphRequest, KnowledgeStatus,
    McpClientCreate, McpClientRef, MergeRequest,
};
use nugget_core::{Error, Result};
use nugget_graph::{EntityKind, EntityQuery};
use nugget_ipc::{Notifier, RpcError, to_result};
use serde_json::{Value, json};

use crate::daemon::blocking;
use crate::graphview::{self, Lookups, parse_kind, parse_status, status_str};
use crate::handler::RpcHandler;
use crate::mcp::{BridgeLine, BridgeOpen, BridgeSession, McpService};

const DEFAULT_ENTITY_LIMIT: u32 = 50;
const MAX_ENTITY_LIMIT: u32 = 500;
const DEFAULT_GRAPH_LIMIT: u32 = 100;
const MAX_GRAPH_DEPTH: u32 = 3;
/// Mentions shown on an entity's detail page.
const DETAIL_MENTIONS: usize = 100;

fn kinds(list: &[String]) -> Result<Vec<EntityKind>> {
    list.iter().map(|k| parse_kind(k)).collect()
}

impl RpcHandler {
    fn mcp(&self) -> Result<&std::sync::Arc<McpService>, RpcError> {
        self.state.mcp.as_ref().ok_or_else(|| {
            RpcError::from(Error::Invalid(
                "MCP is disabled in config.toml ([mcp] enabled = false)".into(),
            ))
        })
    }

    // ---- knowledge ----

    pub(crate) async fn knowledge_entities(&self, req: EntitiesRequest) -> Result<Value, RpcError> {
        let limit = match req.limit {
            0 => DEFAULT_ENTITY_LIMIT,
            n => n.min(MAX_ENTITY_LIMIT),
        } as usize;
        let kind = req.kind.as_deref().map(parse_kind).transpose()?;
        let graph = self.state.knowledge.graph.clone();
        #[allow(clippy::needless_update)] // future fields keep their defaults
        let q = EntityQuery {
            text: req.text.filter(|t| !t.trim().is_empty()),
            kind,
            limit,
            offset: req.offset as usize,
            ..Default::default()
        };
        let list = blocking(move || graph.list_entities(&q)).await?;
        to_result(
            list.into_iter()
                .map(graphview::summary_view)
                .collect::<Vec<_>>(),
        )
    }

    pub(crate) async fn knowledge_entity(&self, req: EntityRef) -> Result<Value, RpcError> {
        let k = self.state.knowledge.clone();
        let detail = blocking(move || {
            let mut lk = Lookups::new(&k.store, k.graph.as_ref());
            let e = lk
                .entity(&req.id)?
                .ok_or_else(|| Error::NotFound(format!("entity {}", req.id)))?;
            let claims = k.graph.claims_for(&e.id, true)?;
            let mut claim_views = Vec::with_capacity(claims.len());
            for c in &claims {
                claim_views.push(graphview::claim_view(&mut lk, c)?);
            }
            let mut mentions = Vec::new();
            for m in k.graph.mentions_for(&e.id, DETAIL_MENTIONS)? {
                mentions.push(graphview::mention_view(&mut lk, &m)?);
            }
            let vault_path = match &k.vault {
                Some(v) => v.page_path(&e.id)?.and_then(|p| {
                    p.strip_prefix(v.root())
                        .ok()
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                }),
                None => None,
            };
            Ok(EntityDetail {
                id: e.id.clone(),
                kind: e.kind.as_str().to_owned(),
                name: e.name.clone(),
                aliases: e.aliases.clone(),
                identifiers: e
                    .identifiers
                    .iter()
                    .map(|i| (i.scheme.clone(), i.value.clone()))
                    .collect(),
                attributes: e.attributes.clone(),
                claims: claim_views,
                mentions,
                vault_path,
            })
        })
        .await?;
        to_result(detail)
    }

    pub(crate) async fn knowledge_graph(&self, req: GraphRequest) -> Result<Value, RpcError> {
        let kinds = kinds(&req.kinds)?;
        let limit = match req.limit {
            0 => DEFAULT_GRAPH_LIMIT as usize,
            n => (n as usize).min(nugget_store::MAX_NEIGHBOURHOOD_NODES),
        };
        let depth = req.depth.min(MAX_GRAPH_DEPTH);
        let graph = self.state.knowledge.graph.clone();
        let view = blocking(move || match &req.center {
            Some(c) => graphview::neighbourhood(graph.as_ref(), c, depth, limit, &kinds),
            None => graphview::overview(graph.as_ref(), &kinds, limit),
        })
        .await?;
        to_result(view)
    }

    pub(crate) async fn knowledge_merge(&self, req: MergeRequest) -> Result<Value, RpcError> {
        if req.into == req.from {
            return Err(RpcError::invalid_params(
                "cannot merge an entity into itself",
            ));
        }
        let k = self.state.knowledge.clone();
        let (into, from) = (req.into.clone(), req.from.clone());
        let (into_name, from_name) = blocking(move || {
            let a = k
                .graph
                .get_entity(&into)?
                .ok_or_else(|| Error::NotFound(format!("entity {into}")))?;
            let b = k
                .graph
                .get_entity(&from)?
                .ok_or_else(|| Error::NotFound(format!("entity {from}")))?;
            if a.id == b.id {
                return Err(Error::Invalid(format!(
                    "{from} is already merged into {into}"
                )));
            }
            k.graph.merge(&a.id, &b.id)?;
            Ok((a.name, b.name))
        })
        .await?;
        tracing::info!(into = %req.into, from = %req.from, "entities merged");
        self.audit(
            "knowledge.merge",
            req.into.as_str(),
            json!({"into": req.into, "from": req.from, "into_name": into_name, "from_name": from_name}),
        )
        .await;
        self.after_graph_edit(vec![req.into, req.from]).await;
        Ok(Value::Null)
    }

    pub(crate) async fn knowledge_claim_status(
        &self,
        req: ClaimStatusRequest,
    ) -> Result<Value, RpcError> {
        let to = match req.status.as_str() {
            "retracted" | "rejected" => graphview::user_rejected(),
            "active" => parse_status("active")?,
            _ => {
                return Err(RpcError::invalid_params(
                    "status must be `retracted` (reject the fact) or `active` (restore it)",
                ));
            }
        };
        let to_name = status_str(to);
        let k = self.state.knowledge.clone();
        let claim = req.claim;
        let (from, touched) = blocking(move || {
            let c = k
                .graph
                .get_claim(claim)?
                .ok_or_else(|| Error::NotFound(format!("claim {claim}")))?;
            let mut touched = vec![c.subject.clone()];
            if let nugget_graph::ClaimObject::Entity(o) = &c.object {
                touched.push(o.clone());
            }
            if c.status != to {
                k.graph.set_claim_status(claim, to)?;
            }
            Ok((status_str(c.status), touched))
        })
        .await?;
        if from != to_name {
            tracing::info!(claim, %from, to = %to_name, "claim status changed");
            self.audit(
                "knowledge.claim.status",
                &claim.to_string(),
                json!({"claim": claim, "from": from, "to": to_name, "subject": touched[0]}),
            )
            .await;
            self.after_graph_edit(touched).await;
        }
        Ok(Value::Null)
    }

    /// Re-render the vault pages a user edit affected and tell clients.
    async fn after_graph_edit(&self, entities: Vec<nugget_core::ids::EntityId>) {
        if let Err(e) = self.state.knowledge.sync_vault().await {
            tracing::warn!(error = %e, "vault render after a graph edit failed");
        }
        self.state.knowledge.notify_changed(entities);
    }

    pub(crate) async fn knowledge_status(&self) -> Result<Value, RpcError> {
        let k = self.state.knowledge.clone();
        let graph = k.graph.clone();
        let counts = blocking(move || graph.counts()).await?;
        let st = k.knowledge.status();
        let pending = st.last_run.as_ref().map_or(0, |r| r.remaining);
        to_result(KnowledgeStatus {
            entities: counts.entities,
            claims: counts.claims,
            mentions: counts.mentions,
            pending_items: pending,
            last_run: k.last_run(),
            extractor_provider: k.extractor_provider(),
            vault: k.vault_status(),
        })
    }

    // ---- MCP clients ----

    pub(crate) async fn mcp_clients_list(&self) -> Result<Value, RpcError> {
        let reg = self.mcp()?.registry.clone();
        to_result(blocking(move || reg.list()).await?)
    }

    pub(crate) async fn mcp_clients_create(&self, req: McpClientCreate) -> Result<Value, RpcError> {
        let mcp = self.mcp()?.clone();
        let reg = mcp.registry.clone();
        let (name, scopes) = (req.name.clone(), req.scopes.clone());
        let (info, token) = blocking(move || reg.create(&name, scopes)).await?;
        tracing::info!(client = %info.id, name = %info.name, "mcp client created");
        self.audit(
            "mcp.client.create",
            &info.id,
            json!({"name": info.name, "scopes": info.scopes}),
        )
        .await;
        to_result(mcp.created(info, &token))
    }

    pub(crate) async fn mcp_clients_revoke(&self, req: McpClientRef) -> Result<Value, RpcError> {
        let reg = self.mcp()?.registry.clone();
        let id = req.id.clone();
        let removed = blocking(move || reg.revoke(&id)).await?;
        if !removed {
            return Err(Error::NotFound(format!("mcp client {}", req.id)).into());
        }
        tracing::info!(client = %req.id, "mcp client revoked");
        self.audit("mcp.client.revoke", &req.id, json!({})).await;
        Ok(Value::Null)
    }

    // ---- bridge (`nuggetd mcp --client`) ----

    pub(crate) async fn mcp_bridge_open(
        &self,
        req: BridgeOpen,
        notify: Notifier,
    ) -> Result<Value, RpcError> {
        let mcp = self.mcp()?.clone();
        let client = req.client.clone();
        let session: BridgeSession = mcp
            .bridge_open(req, notify, &self.state.tasks, self.state.shutdown.clone())
            .await
            .inspect_err(|e| tracing::warn!(%client, error = %e, "mcp bridge refused"))?;
        to_result(session)
    }

    pub(crate) async fn mcp_bridge_send(&self, req: BridgeLine) -> Result<Value, RpcError> {
        self.mcp()?.bridge_send(req).await?;
        Ok(Value::Null)
    }

    pub(crate) fn mcp_bridge_close(&self, req: BridgeSession) -> Result<Value, RpcError> {
        self.mcp()?.bridge_close(&req.session);
        Ok(Value::Null)
    }
}
