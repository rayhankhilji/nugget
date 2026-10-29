//! M2 IPC methods: `knowledge.*`, `mcp.clients.*` and the daemon-private `mcp.bridge.*`.

use nugget_core::ipc::{
    ClaimStatusRequest, EntitiesRequest, EntityDetail, EntityRef, GraphRequest, KnowledgeStatus,
    McpClientCreate, McpClientRef, MergeRequest, SplitRequest,
};
use nugget_core::{Error, Result};
use nugget_graph::{Claim, ClaimStatus, EntityKind, EntityQuery, Identifier, NewEntity};
use nugget_ipc::{Notifier, RpcError, to_result};
use serde_json::{Value, json};

use crate::daemon::blocking;
use crate::graphview::{self, Lookups, parse_kind, status_str};
use crate::handler::RpcHandler;
use crate::mcp::{BridgeLine, BridgeOpen, BridgeSession, McpService};

const DEFAULT_ENTITY_LIMIT: u32 = 50;
const MAX_ENTITY_LIMIT: u32 = 500;
const DEFAULT_GRAPH_LIMIT: u32 = 100;
const MAX_GRAPH_DEPTH: u32 = 3;
/// Mentions shown on an entity's detail page.
const DETAIL_MENTIONS: usize = 100;

/// Longest entity name, alias or identifier a split accepts.
const MAX_NAME_LEN: usize = 1024;

fn kinds(list: &[String]) -> Result<Vec<EntityKind>> {
    list.iter().map(|k| parse_kind(k)).collect()
}

/// A user's decision about a fact (`ClaimStatusRequest::status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// `rejected` (or the older `retracted`): sticky rejection.
    Reject,
    /// `active`: restore a fact that is not current.
    Restore,
    /// `user_asserted`: the user vouches for the fact.
    Assert,
    /// `unasserted`: undo the user's assertion.
    Unassert,
}

impl Decision {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "rejected" | "retracted" => Self::Reject,
            "active" => Self::Restore,
            "user_asserted" => Self::Assert,
            "unasserted" => Self::Unassert,
            _ => return None,
        })
    }

    /// The status `c` ends up with.
    fn target(self, c: &Claim) -> Result<ClaimStatus> {
        let sourced = !c.evidence.is_empty();
        Ok(match self {
            Self::Reject => ClaimStatus::Rejected,
            // Already current: nothing to restore (and never downgrade the user's own fact).
            Self::Restore if c.status.is_active() => c.status,
            Self::Restore if sourced => ClaimStatus::Active,
            // Without evidence only the user's word makes it a fact.
            Self::Restore => ClaimStatus::UserAsserted,
            Self::Assert => ClaimStatus::UserAsserted,
            Self::Unassert if c.status != ClaimStatus::UserAsserted => {
                return Err(Error::Invalid(format!(
                    "claim {} is not a fact you asserted (it is {})",
                    c.id,
                    status_str(c.status)
                )));
            }
            Self::Unassert if sourced => ClaimStatus::Active,
            Self::Unassert => ClaimStatus::Retracted,
        })
    }
}

fn check_name(what: &str, s: &str) -> Result<()> {
    if s.trim().is_empty() {
        return Err(Error::Invalid(format!("the {what} is empty")));
    }
    if s.len() > MAX_NAME_LEN || s.chars().any(char::is_control) {
        return Err(Error::Invalid(format!("the {what} is not a valid name")));
    }
    Ok(())
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
        let view = blocking(move || {
            let source = req.source.as_ref();
            match &req.center {
                Some(c) => {
                    graphview::neighbourhood(graph.as_ref(), c, depth, limit, &kinds, source)
                }
                None => graphview::overview(graph.as_ref(), &kinds, limit, source),
            }
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
        let Some(decision) = Decision::parse(req.status.as_str()) else {
            return Err(RpcError::invalid_params(
                "status must be `rejected` (reject the fact), `active` (restore it), \
                 `user_asserted` (assert it) or `unasserted` (withdraw your assertion)",
            ));
        };
        let k = self.state.knowledge.clone();
        let claim = req.claim;
        let (from, to_name, touched) = blocking(move || {
            let c = k
                .graph
                .get_claim(claim)?
                .ok_or_else(|| Error::NotFound(format!("claim {claim}")))?;
            let to = decision.target(&c)?;
            let mut touched = vec![c.subject.clone()];
            if let nugget_graph::ClaimObject::Entity(o) = &c.object {
                touched.push(o.clone());
            }
            if c.status != to {
                k.graph.set_claim_status(claim, to)?;
            }
            Ok((status_str(c.status), status_str(to), touched))
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

    pub(crate) async fn knowledge_split(&self, req: SplitRequest) -> Result<Value, RpcError> {
        check_name("new entity's name", &req.name)?;
        for a in &req.aliases {
            check_name("alias", a)?;
        }
        for (scheme, value) in &req.identifiers {
            check_name("identifier", scheme)?;
            check_name("identifier", value)?;
        }
        if req.claims.is_empty() {
            return Err(RpcError::invalid_params(
                "choose at least one fact to split off",
            ));
        }
        let kind = req.kind.as_deref().map(parse_kind).transpose()?;
        let k = self.state.knowledge.clone();
        let r = req.clone();
        let (from, from_name, new_id, mut touched) = blocking(move || {
            let e = k
                .graph
                .get_entity(&r.entity)?
                .ok_or_else(|| Error::NotFound(format!("entity {}", r.entity)))?;
            let mut touched = vec![e.id.clone()];
            for &id in &r.claims {
                let c = k
                    .graph
                    .get_claim(id)?
                    .ok_or_else(|| Error::NotFound(format!("claim {id}")))?;
                touched.push(c.subject.clone());
                if let nugget_graph::ClaimObject::Entity(o) = &c.object {
                    touched.push(o.clone());
                }
            }
            let mut new = NewEntity::new(kind.unwrap_or(e.kind), r.name.trim());
            new.aliases = r.aliases.iter().map(|a| a.trim().to_owned()).collect();
            new.identifiers = r
                .identifiers
                .iter()
                .map(|(s, v)| Identifier::new(s.trim(), v.trim()))
                .collect();
            let id = k.graph.split(&e.id, &r.claims, &new)?;
            Ok((e.id, e.name, id, touched))
        })
        .await?;
        tracing::info!(entity = %from, new = %new_id, claims = req.claims.len(), "entity split");
        self.audit(
            "knowledge.split",
            from.as_str(),
            json!({"entity": from, "name": from_name, "new": new_id, "new_name": req.name.trim(), "claims": req.claims}),
        )
        .await;
        touched.push(new_id.clone());
        touched.sort();
        touched.dedup();
        self.after_graph_edit(touched).await;
        to_result(EntityRef { id: new_id })
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

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
    use nugget_graph::{ClaimObject, Evidence};

    fn claim(status: ClaimStatus, sourced: bool) -> Claim {
        let evidence = if sourced {
            vec![Evidence {
                item: ItemRef {
                    source: SourceId::new("notes"),
                    key: ItemKey::new("a.md"),
                },
                chunk_id: None,
                quote: None,
            }]
        } else {
            Vec::new()
        };
        Claim {
            id: 7,
            subject: EntityId::new("ent_a"),
            predicate: "note".into(),
            object: ClaimObject::Literal("x".into()),
            text: "x".into(),
            confidence: 1.0,
            status,
            evidence,
            valid_from: None,
            valid_to: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn decisions_map_to_statuses() {
        use ClaimStatus::*;
        for name in ["rejected", "retracted"] {
            assert_eq!(Decision::parse(name), Some(Decision::Reject));
        }
        for bad in ["superseded", "", "Active"] {
            assert_eq!(Decision::parse(bad), None, "{bad}");
        }
        let t = |d: &str, s, sourced| Decision::parse(d).unwrap().target(&claim(s, sourced));
        assert_eq!(t("rejected", Active, true).unwrap(), Rejected);
        assert_eq!(t("rejected", UserAsserted, false).unwrap(), Rejected);
        // Restore: back to active when sourced, else the user's own fact again.
        assert_eq!(t("active", Rejected, true).unwrap(), Active);
        assert_eq!(t("active", Rejected, false).unwrap(), UserAsserted);
        assert_eq!(t("active", Superseded, true).unwrap(), Active);
        assert_eq!(t("active", UserAsserted, true).unwrap(), UserAsserted);
        assert_eq!(t("user_asserted", Retracted, false).unwrap(), UserAsserted);
        // Undoing an assertion: the sources decide.
        assert_eq!(t("unasserted", UserAsserted, false).unwrap(), Retracted);
        assert_eq!(t("unasserted", UserAsserted, true).unwrap(), Active);
        assert!(t("unasserted", Active, true).is_err());
    }
}
