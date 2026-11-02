//! `pending_actions` and the `action_audit` hash chain, and the approve → execute step.
//!
//! State machine: `pending` → (`executing` → `succeeded` | `failed`) | `cancelled` | `expired`;
//! `denied` when the policy refused at proposal. Only `pending` can move, and the move to
//! `executing` happens under a row lock, so two approvals race to one execution. An action that
//! failed is not retried: sending twice is worse than not at all; the user proposes it again.

use std::sync::Arc;

use deadpool_postgres::Transaction;
use nugget_policy::{Decision, PolicyEngine};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio_postgres::Row;
use uuid::Uuid;

use super::{ActionKind, EventBefore, Invalid, assess, canonical, digest, plan};
use crate::composio::{Composio, ComposioError};
use crate::db::{Db, DbError, UsageKind};

/// `prev_hash` of a user's first audit entry.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// An action as the API shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ActionView {
    pub id: Uuid,
    pub kind: String,
    pub toolkit: String,
    pub status: String,
    pub risk: String,
    pub decision: String,
    pub reason: Option<String>,
    pub summary: String,
    pub preview: Value,
    pub digest: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub executed_at: Option<OffsetDateTime>,
    pub result: Option<Value>,
    pub error: Option<String>,
    #[serde(skip)]
    tool: String,
    #[serde(skip)]
    arguments: Value,
    #[serde(skip)]
    connection_id: Option<Uuid>,
}

const COLUMNS: &str = "id, kind, toolkit, status, risk, decision, reason, summary, preview, digest, \
    idempotency_key, created_at, expires_at, decided_at, executed_at, result, error, tool, arguments, connection_id";

impl ActionView {
    fn from_row(r: &Row) -> Self {
        Self {
            id: r.get("id"),
            kind: r.get("kind"),
            toolkit: r.get("toolkit"),
            status: r.get("status"),
            risk: r.get("risk"),
            decision: r.get("decision"),
            reason: r.get("reason"),
            summary: r.get("summary"),
            preview: r.get("preview"),
            digest: r.get("digest"),
            idempotency_key: r.get("idempotency_key"),
            created_at: r.get("created_at"),
            expires_at: r.get("expires_at"),
            decided_at: r.get("decided_at"),
            executed_at: r.get("executed_at"),
            result: r.get("result"),
            error: r.get("error"),
            tool: r.get("tool"),
            arguments: r.get("arguments"),
            connection_id: r.get("connection_id"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error(transparent)]
    Invalid(#[from] Invalid),
    #[error("connect {0} first")]
    NotConnected(String),
    #[error("no such action")]
    NotFound,
    #[error("this idempotency key was already used for a different action")]
    IdempotencyConflict,
    #[error("the action is {0}; it can no longer be approved or cancelled")]
    NotPending(String),
    #[error("the action expired before it was approved; propose it again")]
    Expired,
    #[error("the approval does not match the preview; review the action again")]
    DigestMismatch,
    #[error("refused by policy: {0}")]
    Denied(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

pub struct Proposal<'a> {
    pub kind: ActionKind,
    pub toolkit: &'a str,
    pub input: &'a Value,
    pub idempotency_key: &'a str,
}

#[derive(Clone)]
pub struct Actions {
    pub db: Db,
    pub composio: Option<Arc<dyn Composio>>,
    pub policy: Arc<PolicyEngine>,
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Appends one entry to the user's audit chain (inside the caller's transaction). Concurrent
/// appends for one user serialise on an advisory lock.
async fn audit(
    tx: &Transaction<'_>,
    user: Uuid,
    action: Option<Uuid>,
    event: &str,
    detail: Value,
) -> Result<(), DbError> {
    tx.execute(
        "select pg_advisory_xact_lock(hashtextextended('nugget_audit:' || $1, 0))",
        &[&user.to_string()],
    )
    .await?;
    let last = tx
        .query_opt(
            "select seq, hash from public.action_audit where user_id = $1 order by seq desc limit 1",
            &[&user],
        )
        .await?;
    let (seq, prev): (i64, String) = match last {
        Some(r) => (r.get::<_, i64>(0) + 1, r.get(1)),
        None => (1, GENESIS.into()),
    };
    let hash = chain_hash(&prev, seq, action, event, &detail);
    tx.execute(
        "insert into public.action_audit (user_id, action_id, seq, event, detail, prev_hash, hash)
         values ($1, $2, $3, $4, $5, $6, $7)",
        &[&user, &action, &seq, &event, &detail, &prev, &hash],
    )
    .await?;
    Ok(())
}

pub fn chain_hash(
    prev: &str,
    seq: i64,
    action: Option<Uuid>,
    event: &str,
    detail: &Value,
) -> String {
    let body = canonical(&json!({"seq": seq, "action": action, "event": event, "detail": detail}));
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(body.as_bytes());
    format!("{:x}", h.finalize())
}

/// What the audit keeps about an action: no message bodies, just what and where.
fn audit_detail(a: &ActionView) -> Value {
    json!({"kind": a.kind, "toolkit": a.toolkit, "tool": a.tool, "digest": a.digest,
           "risk": a.risk, "decision": a.decision, "summary": a.summary})
}

async fn load(
    tx: &Transaction<'_>,
    user: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<Option<ActionView>, DbError> {
    let sql = format!(
        "select {COLUMNS} from public.pending_actions where user_id = $1 and id = $2{}",
        if lock { " for update" } else { "" }
    );
    Ok(tx
        .query_opt(&sql, &[&user, &id])
        .await?
        .map(|r| ActionView::from_row(&r)))
}

/// Marks a pending action expired if its time is up. Returns the current view.
async fn expire_if_due(
    tx: &Transaction<'_>,
    user: Uuid,
    a: ActionView,
) -> Result<ActionView, DbError> {
    if a.status != "pending" || a.expires_at > now() {
        return Ok(a);
    }
    let r = tx
        .query_one(
            &format!("update public.pending_actions set status = 'expired' where user_id = $1 and id = $2 returning {COLUMNS}"),
            &[&user, &a.id],
        )
        .await?;
    let a = ActionView::from_row(&r);
    audit(tx, user, Some(a.id), "expired", audit_detail(&a)).await?;
    Ok(a)
}

enum Gate {
    Run(ActionView, String),
    Done(ActionView),
    Err(ActionError),
}

impl Actions {
    /// Validates, plans, assesses and stores an action. The same idempotency key with the same
    /// call returns the stored action; with a different call it is a conflict.
    pub async fn propose(&self, user: Uuid, p: Proposal<'_>) -> Result<ActionView, ActionError> {
        // The connection the action goes through, and (for an update) the event as last synced.
        let conn = self
            .db
            .connections(user)
            .await?
            .into_iter()
            .find(|c| c.toolkit == p.toolkit && c.status == "active")
            .ok_or_else(|| ActionError::NotConnected(p.toolkit.into()))?;
        let before = match (p.kind, p.input.get("event_id").and_then(Value::as_str)) {
            (ActionKind::UpdateEvent, Some(id)) => {
                self.db.event_before(user, p.toolkit, id).await?
            }
            _ => None,
        };
        let plan = plan(p.kind, p.toolkit, p.input, before.as_ref())?;
        let dig = digest(&conn.composio_account_id, plan.tool, &plan.arguments);
        let id = Uuid::new_v4();
        let (risk, decision, rule) = assess(&self.policy, &plan, &id.to_string());
        let (decision_s, reason, status) = match &decision {
            Decision::Deny(r) => ("deny", Some(r.clone()), "denied"),
            Decision::Confirm(r) => ("confirm", Some(r.clone()), "pending"),
            Decision::Allow => unreachable!("assess never allows"),
        };
        let reason = reason.map(|r| match &rule {
            Some(rule) => format!("{r} ({rule})"),
            None => r,
        });
        let preview = serde_json::to_value(&plan.preview).unwrap_or_default();
        let risk_s = risk.as_str();
        let key = p.idempotency_key;
        let kind = p.kind.as_str();
        let view = self
            .db
            .scoped(Some(user), async |tx| {
                let inserted = tx
                    .query_opt(
                        &format!(
                            "insert into public.pending_actions
                               (id, user_id, connection_id, kind, toolkit, tool, arguments, summary, preview, digest,
                                risk, decision, reason, idempotency_key, status, decided_at)
                             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                                     case when $15 = 'denied' then now() end)
                             on conflict (user_id, idempotency_key) do nothing
                             returning {COLUMNS}"
                        ),
                        &[
                            &id, &user, &conn.id, &kind, &p.toolkit, &plan.tool, &plan.arguments, &plan.summary,
                            &preview, &dig, &risk_s, &decision_s, &reason, &key, &status,
                        ],
                    )
                    .await?;
                match inserted {
                    Some(r) => {
                        let a = ActionView::from_row(&r);
                        audit(tx, user, Some(a.id), "proposed", audit_detail(&a)).await?;
                        if a.status == "denied" {
                            audit(tx, user, Some(a.id), "denied", json!({"reason": a.reason})).await?;
                        }
                        Ok(Ok(a))
                    }
                    None => {
                        let r = tx
                            .query_one(
                                &format!("select {COLUMNS} from public.pending_actions where user_id = $1 and idempotency_key = $2"),
                                &[&user, &key],
                            )
                            .await?;
                        let a = ActionView::from_row(&r);
                        Ok(if a.digest == dig { Ok(a) } else { Err(ActionError::IdempotencyConflict) })
                    }
                }
            })
            .await??;
        if view.status == "denied" {
            return Err(ActionError::Denied(view.reason.clone().unwrap_or_default()));
        }
        Ok(view)
    }

    /// The action (expired if its time is up). Used for both status and preview: neither has
    /// side effects upstream.
    pub async fn get(&self, user: Uuid, id: Uuid) -> Result<ActionView, ActionError> {
        self.db
            .scoped(Some(user), async |tx| {
                match load(tx, user, id, true).await? {
                    Some(a) => Ok(Some(expire_if_due(tx, user, a).await?)),
                    None => Ok(None),
                }
            })
            .await?
            .ok_or(ActionError::NotFound)
    }

    pub async fn list(
        &self,
        user: Uuid,
        status: Option<&str>,
    ) -> Result<Vec<ActionView>, ActionError> {
        Ok(self
            .db
            .scoped(Some(user), async |tx| {
                let rows = tx
                    .query(
                        &format!(
                            "select {COLUMNS} from public.pending_actions
                             where user_id = $1 and ($2::text is null or status = $2
                                   or ($2 = 'expired' and status = 'pending' and expires_at <= now()))
                             order by created_at desc limit 100"
                        ),
                        &[&user, &status],
                    )
                    .await?;
                let mut out = Vec::new();
                for r in &rows {
                    out.push(expire_if_due(tx, user, ActionView::from_row(r)).await?);
                }
                Ok(out)
            })
            .await?
            .into_iter()
            .filter(|a| status.is_none_or(|s| a.status == s))
            .collect())
    }

    pub async fn cancel(&self, user: Uuid, id: Uuid) -> Result<ActionView, ActionError> {
        self.db
            .scoped(Some(user), async |tx| {
                let Some(a) = load(tx, user, id, true).await? else {
                    return Ok(Err(ActionError::NotFound));
                };
                let a = expire_if_due(tx, user, a).await?;
                match a.status.as_str() {
                    "cancelled" => Ok(Ok(a)),
                    "pending" => {
                        let r = tx
                            .query_one(
                                &format!("update public.pending_actions set status = 'cancelled', decided_at = now()
                                          where user_id = $1 and id = $2 returning {COLUMNS}"),
                                &[&user, &id],
                            )
                            .await?;
                        let a = ActionView::from_row(&r);
                        audit(tx, user, Some(id), "cancelled", json!({})).await?;
                        Ok(Ok(a))
                    }
                    s => Ok(Err(ActionError::NotPending(s.into()))),
                }
            })
            .await?
    }

    /// Approves and runs the action, once. `approved_digest` must be the digest of the preview
    /// the user saw. Approving an action that already ran returns it as it is.
    pub async fn approve(
        &self,
        user: Uuid,
        id: Uuid,
        approved_digest: &str,
    ) -> Result<ActionView, ActionError> {
        let gate = self
            .db
            .scoped(Some(user), async |tx| {
                let Some(a) = load(tx, user, id, true).await? else {
                    return Ok(Gate::Err(ActionError::NotFound));
                };
                let a = expire_if_due(tx, user, a).await?;
                if a.digest != approved_digest {
                    audit(tx, user, Some(id), "approval_rejected", json!({"why": "digest_mismatch"})).await?;
                    return Ok(Gate::Err(ActionError::DigestMismatch));
                }
                match a.status.as_str() {
                    "pending" => {}
                    "executing" | "succeeded" | "failed" => return Ok(Gate::Done(a)),
                    "expired" => return Ok(Gate::Err(ActionError::Expired)),
                    s => return Ok(Gate::Err(ActionError::NotPending(s.into()))),
                }
                let account = match a.connection_id {
                    Some(c) => tx
                        .query_opt(
                            "select composio_account_id from public.connections
                             where user_id = $1 and id = $2 and status = 'active'",
                            &[&user, &c],
                        )
                        .await?
                        .map(|r| r.get::<_, String>(0)),
                    None => None,
                };
                // The digest covers the account: a reconnected account is a different action.
                let Some(account) = account.filter(|acc| digest(acc, &a.tool, &a.arguments) == a.digest) else {
                    return Ok(Gate::Err(ActionError::NotConnected(a.toolkit.clone())));
                };
                let r = tx
                    .query_one(
                        &format!("update public.pending_actions set status = 'executing', decided_at = now()
                                  where user_id = $1 and id = $2 returning {COLUMNS}"),
                        &[&user, &id],
                    )
                    .await?;
                let a = ActionView::from_row(&r);
                audit(tx, user, Some(id), "approved", json!({"digest": a.digest})).await?;
                Ok(Gate::Run(a, account))
            })
            .await?;
        let (a, account) = match gate {
            Gate::Run(a, acc) => (a, acc),
            Gate::Done(a) => return Ok(a),
            Gate::Err(e) => return Err(e),
        };
        let outcome = match &self.composio {
            Some(c) => {
                c.execute(&user.to_string(), &account, &a.tool, a.arguments.clone())
                    .await
            }
            None => Err(ComposioError::NotConfigured),
        };
        let _ = self
            .db
            .record_usage(user, UsageKind::Tool, &a.tool, 0, 0, 300)
            .await;
        let (status, result, error) = match outcome {
            Ok(v) => ("succeeded", Some(v), None),
            Err(e) => ("failed", None, Some(e.to_string())),
        };
        let tool = a.tool.clone();
        Ok(self
            .db
            .scoped(Some(user), async |tx| {
                let r = tx
                    .query_one(
                        &format!("update public.pending_actions set status = $3, result = $4, error = $5, executed_at = now()
                                  where user_id = $1 and id = $2 returning {COLUMNS}"),
                        &[&user, &id, &status, &result, &error],
                    )
                    .await?;
                audit(tx, user, Some(id), status, json!({"tool": tool, "error": error})).await?;
                Ok(ActionView::from_row(&r))
            })
            .await?)
    }
}

/// One audit entry, for verification and export.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub seq: i64,
    pub action_id: Option<Uuid>,
    pub event: String,
    pub detail: Value,
    pub prev_hash: String,
    pub hash: String,
}

impl Db {
    /// The user's audit chain, oldest first.
    pub async fn audit_chain(&self, user: Uuid) -> Result<Vec<AuditEntry>, DbError> {
        self.scoped(Some(user), async |tx| {
            let rows = tx
                .query(
                    "select seq, action_id, event, detail, prev_hash, hash from public.action_audit
                     where user_id = $1 order by seq",
                    &[&user],
                )
                .await?;
            Ok(rows
                .iter()
                .map(|r| AuditEntry {
                    seq: r.get(0),
                    action_id: r.get(1),
                    event: r.get(2),
                    detail: r.get(3),
                    prev_hash: r.get(4),
                    hash: r.get(5),
                })
                .collect())
        })
        .await
    }

    /// The event's last-synced values, for an update's diff.
    pub async fn event_before(
        &self,
        user: Uuid,
        source: &str,
        external_id: &str,
    ) -> Result<Option<EventBefore>, DbError> {
        self.scoped(Some(user), async |tx| {
            let r = tx
                .query_opt(
                    "select title, metadata from public.items
                     where user_id = $1 and source = $2 and external_id = $3 and kind = 'event'",
                    &[&user, &source, &external_id],
                )
                .await?;
            Ok(r.map(|r| {
                let m: Value = r.get(1);
                let s = |k: &str| m[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
                EventBefore {
                    title: r.get(0),
                    start: s("start"),
                    end: s("end"),
                    location: s("location"),
                    attendees: m["attendees"].as_array().map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }),
                }
            }))
        })
        .await
    }
}

/// Whether a chain is intact: each entry links to the one before and hashes to itself.
pub fn verify_chain(entries: &[AuditEntry]) -> Result<(), i64> {
    let mut prev = GENESIS.to_owned();
    for (i, e) in entries.iter().enumerate() {
        let expected_seq = i as i64 + 1;
        if e.seq != expected_seq
            || e.prev_hash != prev
            || chain_hash(&prev, e.seq, e.action_id, &e.event, &e.detail) != e.hash
        {
            return Err(e.seq);
        }
        prev = e.hash.clone();
    }
    Ok(())
}
