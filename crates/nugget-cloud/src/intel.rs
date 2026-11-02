//! The intelligence jobs per user: snapshot → knowledge (into `entities`/`claims`) → cards; and
//! the morning brief. [`Intel::tick`] is the background scheduler: users whose items changed
//! since their last run (or whose cards are older than [`STALE`]), then users past 07:00 local
//! with no brief for today.

use std::sync::Arc;

use nugget_graph::GraphStore;
use nugget_knowledge::RunStats;
use nugget_proactive::RefreshReport;
use nugget_store::Store;
use serde::Serialize;
use serde_json::json;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::brief::{self, Brief};
use crate::cards::{self, CardsError};
use crate::db::{Db, DbError};
use crate::gateway::{Allowance, Gateway, GatewayError};
use crate::knowledge;
use crate::llm_bridge::{BridgeStats, GatewayProvider};
use crate::plans::{Catalog, ModelTier, Plan};
use crate::snapshot;

/// Cards depend on the clock (meetings come up, deadlines pass): rerun at least this often.
pub const STALE: &str = "6 hours";
/// Users per scheduler tick at most, per job.
const PER_TICK: i32 = 10;

#[derive(Debug, thiserror::Error)]
pub enum IntelError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Cards(#[from] CardsError),
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error("{0}")]
    Pipeline(#[from] nugget_core::Error),
    /// No live subscription: no jobs run for the user.
    #[error("no live subscription")]
    NotEntitled,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IntelReport {
    pub items: usize,
    pub entities: usize,
    pub claims: usize,
    pub knowledge: Option<RunStats>,
    pub cards: Option<RefreshReport>,
    pub llm: BridgeStats,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub timezone: Option<String>,
}

pub async fn profile(db: &Db, user: Uuid) -> Result<Profile, DbError> {
    db.scoped(Some(user), async |tx| {
        let r = tx
            .query_opt(
                "select timezone from public.profiles where user_id = $1",
                &[&user],
            )
            .await?;
        Ok(Profile {
            timezone: r.and_then(|r| r.get(0)),
        })
    })
    .await
}

/// Stores the user's IANA zone if Postgres knows it (the brief scheduler computes local time in
/// SQL). Returns whether it was accepted.
pub async fn set_timezone(db: &Db, user: Uuid, tz: &str) -> Result<bool, DbError> {
    let tz = tz.trim();
    if tz.is_empty() || tz.len() > 64 {
        return Ok(false);
    }
    db.scoped(Some(user), async |tx| {
        Ok(tx
            .execute(
                "update public.profiles set timezone = $2 where user_id = $1
                   and timezone is distinct from $2
                   and exists (select 1 from pg_timezone_names where name = $2)",
                &[&user, &tz],
            )
            .await?
            > 0
            || tx
                .query_opt(
                    "select 1 from public.profiles where user_id = $1 and timezone = $2",
                    &[&user, &tz],
                )
                .await?
                .is_some())
    })
    .await
}

/// The UTC offset of an IANA zone now (UTC when unknown).
pub fn utc_offset(tz: Option<&str>, now: OffsetDateTime) -> UtcOffset {
    let secs = tz
        .and_then(|n| jiff::tz::TimeZone::get(n).ok())
        .and_then(|z| {
            let ts = jiff::Timestamp::from_second(now.unix_timestamp()).ok()?;
            Some(z.to_offset(ts).seconds())
        })
        .unwrap_or(0);
    UtcOffset::from_whole_seconds(secs).unwrap_or(UtcOffset::UTC)
}

/// A built snapshot and its graph after knowledge ran.
pub struct Snap {
    pub store: Store,
    pub graph: Arc<dyn GraphStore>,
    pub cfg: nugget_proactive::ProactiveConfig,
}

#[derive(Clone)]
pub struct Intel {
    pub db: Db,
    pub gateway: Gateway,
    pub plans: Arc<Catalog>,
}

impl Intel {
    /// The user's live plan (`plans::entitlement`), or [`IntelError::NotEntitled`].
    pub async fn plan(&self, user: Uuid) -> Result<Plan, IntelError> {
        crate::plans::entitlement(&self.db, &self.plans, user)
            .await?
            .map(|e| e.plan)
            .ok_or(IntelError::NotEntitled)
    }

    /// Snapshot, knowledge, projection and a card refresh for one user.
    pub async fn run(
        &self,
        user: Uuid,
        now: OffsetDateTime,
    ) -> Result<(IntelReport, Snap), IntelError> {
        let plan = self.plan(user).await?;
        let p = profile(&self.db, user).await?;
        let (items, through) = snapshot::load(&self.db, user, plan.history_days).await?;
        let mut sources: Vec<String> = items.iter().map(|i| i.source.clone()).collect();
        sources.sort();
        sources.dedup();
        let count = items.len();
        let store = tokio::task::spawn_blocking(move || snapshot::build(&items))
            .await
            .map_err(|e| nugget_core::Error::Other(e.to_string()))??;
        let graph: Arc<dyn GraphStore> = Arc::new(store.graph());
        // The extractor runs only while the plan's allowance has room; every call checks again.
        let allowance = Allowance::from(&plan);
        let llm_ok = self.gateway.has_llm()
            && self
                .gateway
                .choose(&self.db, user, allowance, Some(ModelTier::Fast))
                .await
                .is_ok();
        let provider = llm_ok.then(|| {
            Arc::new(GatewayProvider::new(
                self.db.clone(),
                self.gateway.clone(),
                user,
                allowance,
                knowledge::MAX_FRESH_LLM,
            ))
        });
        let (stats, me) = knowledge::run(&store, graph.clone(), provider.clone(), &sources).await?;
        let (entities, claims) = knowledge::project(graph.as_ref())?;
        let (n_entities, n_claims) = knowledge::store(&self.db, user, &entities, &claims).await?;
        let mut cfg = cards::config(p.timezone.clone());
        cfg.user_entity = me;
        let refresh = cards::refresh(&self.db, user, &store, graph.clone(), &cfg, now).await?;
        let report = IntelReport {
            items: count,
            entities: n_entities,
            claims: n_claims,
            knowledge: Some(stats),
            cards: Some(refresh),
            llm: provider.map(|p| p.stats()).unwrap_or_default(),
        };
        record(&self.db, user, through, Some(&report), None).await?;
        Ok((report, Snap { store, graph, cfg }))
    }

    /// [`Intel::run`], recording a failure instead of returning it.
    pub async fn run_logged(&self, user: Uuid) -> Option<(IntelReport, Snap)> {
        match self.run(user, OffsetDateTime::now_utc()).await {
            Ok(r) => {
                tracing::info!(%user, items = r.0.items, entities = r.0.entities, claims = r.0.claims, "intel run");
                Some(r)
            }
            Err(e) => {
                tracing::warn!(%user, %e, "intel run failed");
                let _ = record(&self.db, user, None, None, Some(&e.to_string())).await;
                None
            }
        }
    }

    /// Today's brief for the user: the stored one, or a fresh run plus brief when it is 07:00 or
    /// later locally. Before 07:00 the latest earlier brief (if any) is returned.
    pub async fn brief(
        &self,
        user: Uuid,
        now: OffsetDateTime,
    ) -> Result<Option<Brief>, IntelError> {
        let plan = self.plan(user).await?;
        let p = profile(&self.db, user).await?;
        let local = now.to_offset(utc_offset(p.timezone.as_deref(), now));
        if let Some(b) = brief::stored(&self.db, user, Some(local.date())).await? {
            return Ok(Some(b));
        }
        if local.hour() < 7 {
            return Ok(brief::stored(&self.db, user, None).await?);
        }
        let (_, snap) = self.run(user, now).await?;
        let (mb, state) = cards::brief(&self.db, user, &snap.store, &snap.cfg, now).await?;
        let tz = p.timezone.unwrap_or_else(|| "UTC".into());
        Ok(Some(
            brief::generate(
                &self.db,
                &self.gateway,
                user,
                Allowance::from(&plan),
                &tz,
                &mb,
                &state,
                &snap.cfg,
            )
            .await?,
        ))
    }

    /// One scheduler pass.
    pub async fn tick(&self) {
        match due(
            &self.db,
            "select user_id from nugget_private.intel_due($1, $2::text::interval)",
            true,
        )
        .await
        {
            Ok(users) => {
                for u in users {
                    self.run_logged(u).await;
                }
            }
            Err(e) => tracing::warn!(%e, "intel_due failed"),
        }
        match due(
            &self.db,
            "select user_id from nugget_private.brief_due($1)",
            false,
        )
        .await
        {
            Ok(users) => {
                for u in users {
                    if let Err(e) = self.brief(u, OffsetDateTime::now_utc()).await {
                        tracing::warn!(user = %u, %e, "brief failed");
                    }
                }
            }
            Err(e) => tracing::warn!(%e, "brief_due failed"),
        }
    }
}

async fn due(db: &Db, sql: &str, stale: bool) -> Result<Vec<Uuid>, DbError> {
    db.scoped(None, async |tx| {
        let rows = if stale {
            tx.query(sql, &[&PER_TICK, &STALE]).await?
        } else {
            tx.query(sql, &[&PER_TICK]).await?
        };
        Ok(rows.iter().map(|r| r.get(0)).collect())
    })
    .await
}

async fn record(
    db: &Db,
    user: Uuid,
    through: Option<OffsetDateTime>,
    report: Option<&IntelReport>,
    error: Option<&str>,
) -> Result<(), DbError> {
    let stats = report.map_or(json!({}), |r| json!(r));
    db.scoped(Some(user), async |tx| {
        tx.execute(
            "insert into public.intel_runs (user_id, items_through, ran_at, stats, last_error)
             values ($1, $2, now(), $3, $4)
             on conflict (user_id) do update set
               items_through = coalesce(excluded.items_through, intel_runs.items_through),
               ran_at = now(),
               stats = case when excluded.last_error is null then excluded.stats else intel_runs.stats end,
               last_error = excluded.last_error",
            &[&user, &through, &stats, &error],
        )
        .await?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_from_zones() {
        let summer = OffsetDateTime::from_unix_timestamp(1_782_000_000).unwrap(); // 2026-06-21
        assert_eq!(utc_offset(Some("Europe/London"), summer).whole_hours(), 1);
        assert_eq!(utc_offset(Some("Nowhere/Else"), summer), UtcOffset::UTC);
        assert_eq!(utc_offset(None, summer), UtcOffset::UTC);
    }
}
