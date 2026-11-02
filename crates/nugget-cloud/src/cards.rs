//! "Needs you" cards: `nugget-proactive` (generators for unanswered mail, commitments, meeting
//! prep, renewals and follow-ups; the scoring model; the learned per-sender/domain/kind layer
//! with decay; the multilingual text rules and `LocalText` keys) over a user's snapshot. The
//! proactive state lives in `proactive_state` as JSON and is written with a compare-and-swap on
//! `updated_at`, so a refresh never loses a dismissal made while it ran. Every save also projects
//! the cards into `cards`.

use std::sync::{Arc, OnceLock};

use nugget_graph::GraphStore;
use nugget_proactive::{
    Card, CardStore, DismissReason, MemCardStore, MorningBrief, Proactive, ProactiveConfig,
    ProactiveState, RefreshReport,
};
use nugget_store::Store;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{Db, DbError};

#[derive(Debug, thiserror::Error)]
pub enum CardsError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("{0}")]
    Proactive(#[from] nugget_core::Error),
    #[error("the cards changed while saving; try again")]
    Conflict,
}

/// Attempts at a compare-and-swap save before giving up.
const CAS_TRIES: usize = 4;

/// The stored state and its version (`updated_at`), if any.
pub async fn load(
    db: &Db,
    user: Uuid,
) -> Result<Option<(ProactiveState, OffsetDateTime)>, CardsError> {
    let row = db
        .scoped(Some(user), async |tx| {
            Ok(tx
                .query_opt(
                    "select state, updated_at from public.proactive_state where user_id = $1",
                    &[&user],
                )
                .await?
                .map(|r| (r.get::<_, Value>(0), r.get::<_, OffsetDateTime>(1))))
        })
        .await?;
    Ok(match row {
        // A state this build cannot read starts over rather than failing every request.
        Some((v, at)) => serde_json::from_value(v).ok().map(|s| (s, at)),
        None => None,
    })
}

fn status(c: &Card) -> &'static str {
    c.state.as_str()
}

/// Saves `state` if the stored version is still `version` (None: no row yet). False on conflict.
async fn save(
    db: &Db,
    user: Uuid,
    state: &ProactiveState,
    version: Option<OffsetDateTime>,
) -> Result<bool, CardsError> {
    let v = serde_json::to_value(state).map_err(nugget_core::Error::from)?;
    let ok = db
        .scoped(Some(user), async |tx| {
            let n = match version {
                Some(at) => {
                    tx.execute(
                        "update public.proactive_state set state = $2, updated_at = clock_timestamp()
                         where user_id = $1 and updated_at = $3",
                        &[&user, &v, &at],
                    )
                    .await?
                }
                None => {
                    tx.execute(
                        "insert into public.proactive_state (user_id, state, updated_at)
                         values ($1, $2, clock_timestamp()) on conflict (user_id) do nothing",
                        &[&user, &v],
                    )
                    .await?
                }
            };
            if n == 0 {
                return Ok(false);
            }
            let upsert = tx
                .prepare(
                    "insert into public.cards (user_id, card_key, kind, title, body, item_ids, score, status,
                       due_at, snoozed_until, data, created_at, updated_at)
                     values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                     on conflict (user_id, card_key) do update set
                       kind = excluded.kind, title = excluded.title, body = excluded.body,
                       item_ids = excluded.item_ids, score = excluded.score, status = excluded.status,
                       due_at = excluded.due_at, snoozed_until = excluded.snoozed_until,
                       data = excluded.data, updated_at = excluded.updated_at",
                )
                .await?;
            let mut keys = Vec::new();
            for c in state.cards.values() {
                let items: Vec<Uuid> = c
                    .evidence
                    .iter()
                    .filter_map(|e| e.item.key.as_str().parse().ok())
                    .collect();
                let data = serde_json::to_value(c).unwrap_or(Value::Null);
                let score = c.rank as f32;
                tx.execute(
                    &upsert,
                    &[
                        &user,
                        &c.id,
                        &c.kind.as_str(),
                        &c.title,
                        &c.detail,
                        &items,
                        &score,
                        &status(c),
                        &c.due,
                        &c.snoozed_until,
                        &data,
                        &c.created_at,
                        &c.updated_at,
                    ],
                )
                .await?;
                keys.push(c.id.clone());
            }
            tx.execute(
                "delete from public.cards where user_id = $1 and (card_key is null or not (card_key = any($2)))",
                &[&user, &keys],
            )
            .await?;
            Ok(true)
        })
        .await?;
    Ok(ok)
}

/// An empty store for card operations that read no items (accept, dismiss, snooze, inbox).
fn empty_store() -> nugget_core::Result<Store> {
    static EMPTY: OnceLock<Store> = OnceLock::new();
    if let Some(s) = EMPTY.get() {
        return Ok(s.clone());
    }
    let s = Store::open_in_memory()?;
    Ok(EMPTY.get_or_init(|| s).clone())
}

pub fn config(timezone: Option<String>) -> ProactiveConfig {
    ProactiveConfig {
        timezone,
        ..ProactiveConfig::default()
    }
}

/// Proactive over `store` with `state` loaded into an in-memory card store.
fn proactive(
    store: Store,
    graph: Option<Arc<dyn GraphStore>>,
    cfg: ProactiveConfig,
    state: Option<&ProactiveState>,
) -> nugget_core::Result<(Proactive, Arc<MemCardStore>)> {
    let mem = Arc::new(MemCardStore::default());
    if let Some(s) = state {
        mem.save(s)?;
    }
    let p = Proactive::new(store, cfg)
        .with_graph(graph)
        .with_card_store(mem.clone());
    Ok((p, mem))
}

/// Applies `op` to the current state and saves it, retrying on a concurrent change.
async fn modify<T>(
    db: &Db,
    user: Uuid,
    cfg: &ProactiveConfig,
    op: impl Fn(&Proactive) -> nugget_core::Result<T>,
) -> Result<T, CardsError> {
    for _ in 0..CAS_TRIES {
        let current = load(db, user).await?;
        let (out, state) = {
            let (p, mem) = proactive(
                empty_store()?,
                None,
                cfg.clone(),
                current.as_ref().map(|c| &c.0),
            )?;
            (op(&p)?, mem.load()?)
        };
        if save(db, user, &state, current.map(|c| c.1)).await? {
            return Ok(out);
        }
    }
    Err(CardsError::Conflict)
}

/// Generates, scores and merges cards from the snapshot (again on a concurrent change).
pub async fn refresh(
    db: &Db,
    user: Uuid,
    store: &Store,
    graph: Arc<dyn GraphStore>,
    cfg: &ProactiveConfig,
    now: OffsetDateTime,
) -> Result<RefreshReport, CardsError> {
    for _ in 0..CAS_TRIES {
        let current = load(db, user).await?;
        let (p, mem) = proactive(
            store.clone(),
            Some(graph.clone()),
            cfg.clone(),
            current.as_ref().map(|c| &c.0),
        )?;
        let report = p.refresh(now).await?;
        let state = mem.load()?;
        if save(db, user, &state, current.map(|c| c.1)).await? {
            return Ok(report);
        }
    }
    Err(CardsError::Conflict)
}

/// The morning brief from the snapshot and the stored cards, with the state it was built from.
pub async fn brief(
    db: &Db,
    user: Uuid,
    store: &Store,
    cfg: &ProactiveConfig,
    now: OffsetDateTime,
) -> Result<(MorningBrief, ProactiveState), CardsError> {
    let state = load(db, user).await?.map(|c| c.0);
    let (p, mem) = proactive(store.clone(), None, cfg.clone(), state.as_ref())?;
    Ok((p.brief(now)?, mem.load()?))
}

/// The inbox (open cards, best first) and when cards were last refreshed.
pub async fn inbox(
    db: &Db,
    user: Uuid,
    cfg: &ProactiveConfig,
) -> Result<(Vec<Card>, Option<OffsetDateTime>), CardsError> {
    let current = load(db, user).await?;
    let (p, _) = proactive(
        empty_store()?,
        None,
        cfg.clone(),
        current.as_ref().map(|c| &c.0),
    )?;
    Ok((p.inbox()?, p.last_refresh()?))
}

/// What the user did with a card.
#[derive(Debug, Clone)]
pub enum CardOp {
    Seen(Vec<String>),
    Accept(String),
    Dismiss(String, DismissReason),
    Snooze(String, OffsetDateTime),
    Done(String),
}

/// Applies a user action (and the learning it teaches); returns the card afterwards.
pub async fn act(
    db: &Db,
    user: Uuid,
    cfg: &ProactiveConfig,
    op: CardOp,
    now: OffsetDateTime,
) -> Result<Option<Card>, CardsError> {
    modify(db, user, cfg, |p: &Proactive| {
        Ok(match &op {
            CardOp::Seen(ids) => {
                p.mark_seen(ids, now)?;
                None
            }
            CardOp::Accept(id) => {
                p.accept(id, now)?;
                Some(p.card(id)?)
            }
            CardOp::Dismiss(id, r) => {
                p.dismiss(id, r.clone(), now)?;
                Some(p.card(id)?)
            }
            CardOp::Snooze(id, until) => {
                p.snooze(id, *until, now)?;
                Some(p.card(id)?)
            }
            CardOp::Done(id) => {
                p.complete(id, now)?;
                Some(p.card(id)?)
            }
        })
    })
    .await
}

/// `not_relevant`, `already_handled`, `wrong_info`, `not_important`, or `other` with a note.
pub fn dismiss_reason(reason: &str, note: Option<&str>) -> Option<DismissReason> {
    Some(match reason {
        "not_relevant" => DismissReason::NotRelevant,
        "already_handled" => DismissReason::AlreadyHandled,
        "wrong_info" => DismissReason::WrongInfo,
        "not_important" => DismissReason::NotImportant,
        "other" => DismissReason::Other(note.unwrap_or("").chars().take(200).collect()),
        _ => return None,
    })
}

/// A card for the wire: the daemon's `CardView`, which the desktop already renders. Actions are
/// listed but not executable from the cloud yet (`supported: false`).
pub fn wire(c: &Card) -> Value {
    serde_json::to_value(nugget_proactive::view::card_view(c, &|_| false))
        .unwrap_or_else(|_| json!({"id": c.id}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismiss_reasons_parse() {
        assert_eq!(
            dismiss_reason("wrong_info", None),
            Some(DismissReason::WrongInfo)
        );
        assert_eq!(
            dismiss_reason("other", Some("dup")),
            Some(DismissReason::Other("dup".into()))
        );
        assert_eq!(dismiss_reason("nope", None), None);
    }
}
