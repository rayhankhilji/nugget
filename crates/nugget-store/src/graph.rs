//! The knowledge graph on SQLite: [`SqliteGraph`] implements [`nugget_graph::GraphStore`] over
//! the schema v4 tables (see `schema.rs`).
//!
//! Behaviour follows the `GraphStore` contract and the reference [`nugget_graph::MemGraph`];
//! deliberate differences are listed on [`SqliteGraph`].
//!
//! Every mutation runs in one `IMMEDIATE` transaction on the store's connection, together with
//! its change-feed entries, so readers never see a half-applied merge or retraction.

use std::collections::{HashMap, HashSet, VecDeque};

use nugget_core::ids::{EntityId, ItemRef, SourceId};
use nugget_core::{Error, Result};
use nugget_graph::{
    Claim, ClaimId, ClaimObject, ClaimStatus, Entity, EntityKind, EntityPatch, EntityQuery,
    EntitySummary, Evidence, GraphCounts, GraphEdge, GraphNode, GraphStore, GraphView, Identifier,
    Mention, NewClaim, NewEntity, normalize_name,
};
use rusqlite::{
    Connection, OptionalExtension, Row, ToSql, Transaction, TransactionBehavior, params,
    params_from_iter,
};
use serde_json::Value;
use time::OffsetDateTime;

use crate::Store;
use crate::error::{DbExt, format_time, parse_time};
use crate::items::item_ref;

/// Longest `merged_into` chain followed when resolving an id. Merges flatten chains, so real
/// chains have length one; the bound only guards against a corrupted (cyclic) table.
pub const MAX_MERGE_DEPTH: usize = 64;

/// Upper bound on the nodes [`GraphStore::neighbourhood`] returns, whatever `limit` asks for.
/// A traversal cut by this bound reports `truncated`.
pub const MAX_NEIGHBOURHOOD_NODES: usize = 10_000;

/// Prefix of every entity id.
pub const ENTITY_ID_PREFIX: &str = "ent_";

/// SQL condition for claims that count as active ([`ClaimStatus::is_active`]).
const ACTIVE: &str = "status IN ('active', 'user_asserted')";

/// The knowledge graph stored in a [`Store`]. Cheap to clone; clones share the store's
/// connection. Obtain one with [`Store::graph`].
///
/// Differences from [`nugget_graph::MemGraph`] (none observable through the conformance suite):
/// - Entity ids are random (`ent_` + 16 base32 characters, 80 bits) instead of sequential.
/// - [`GraphStore::add_mention`] keeps one mention per (entity, item, chunk, surface) and keeps
///   the highest confidence, where `MemGraph` keeps one per exactly equal mention.
/// - [`GraphStore::merge`] keeps the **oldest** claim when duplicate triples fold, raises it to
///   `user_asserted` if any folded claim was, and also touches (in the change feed) the other
///   entities of the claims that moved.
/// - [`GraphStore::neighbourhood`] returns at most [`MAX_NEIGHBOURHOOD_NODES`] nodes.
/// - Claim ids are never reused, even for claims folded away by a merge.
#[derive(Clone, Debug)]
pub struct SqliteGraph {
    store: Store,
}

impl Store {
    /// The knowledge graph held in this store.
    pub fn graph(&self) -> SqliteGraph {
        SqliteGraph {
            store: self.clone(),
        }
    }
}

impl SqliteGraph {
    /// A graph over a fresh, unencrypted in-memory store (tests and tooling).
    pub fn open_in_memory() -> Result<Self> {
        Ok(Store::open_in_memory()?.graph())
    }

    /// The store this graph lives in.
    pub fn store(&self) -> &Store {
        &self.store
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.store.with_conn(|c| f(c))
    }

    /// Run `f` in one IMMEDIATE transaction, then record every entity it touched in the change
    /// feed (in the same transaction).
    fn write<T>(
        &self,
        what: &str,
        f: impl FnOnce(&Transaction<'_>, &mut Touched) -> Result<T>,
    ) -> Result<T> {
        let now = now()?;
        self.store.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db(&format!("begin {what}"))?;
            let mut touched = Touched::new(now);
            let out = f(&tx, &mut touched)?;
            touched.flush(&tx)?;
            tx.commit().db(&format!("commit {what}"))?;
            Ok(out)
        })
    }
}

fn now() -> Result<String> {
    format_time(OffsetDateTime::now_utc())
}

// ------------------------------------------------------------------ change feed

/// Entities changed by one mutation, in first-touch order.
pub(crate) struct Touched {
    now: String,
    ids: Vec<String>,
    seen: HashSet<String>,
}

impl Touched {
    pub(crate) fn new(now: String) -> Self {
        Self {
            now,
            ids: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn now(&self) -> &str {
        &self.now
    }

    fn add(&mut self, id: &str) {
        if self.seen.insert(id.to_owned()) {
            self.ids.push(id.to_owned());
        }
    }

    /// Give each touched entity a new seq and bump its `updated_at`.
    pub(crate) fn flush(self, tx: &Connection) -> Result<()> {
        if self.ids.is_empty() {
            return Ok(());
        }
        let mut next = tx
            .prepare_cached("UPDATE graph_seq SET seq = seq + 1 WHERE id = 1 RETURNING seq")
            .db("prepare graph seq")?;
        let mut record = tx
            .prepare_cached(
                "INSERT INTO graph_changes (entity_id, seq) VALUES (?1, ?2)
                 ON CONFLICT (entity_id) DO UPDATE SET seq = excluded.seq",
            )
            .db("prepare change feed")?;
        let mut stamp = tx
            .prepare_cached("UPDATE entities SET updated_at = ?2 WHERE id = ?1")
            .db("prepare entity stamp")?;
        for id in &self.ids {
            let seq: i64 = next.query_row([], |r| r.get(0)).db("advance graph seq")?;
            record.execute(params![id, seq]).db("record change")?;
            stamp.execute(params![id, self.now]).db("stamp entity")?;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ encoding helpers

fn kind_from_str(s: &str) -> Result<EntityKind> {
    EntityKind::ALL
        .into_iter()
        .find(|k| k.as_str() == s)
        .ok_or_else(|| Error::Storage(format!("unknown entity kind {s:?} in store")))
}

fn status_str(s: ClaimStatus) -> &'static str {
    match s {
        ClaimStatus::Active => "active",
        ClaimStatus::Superseded => "superseded",
        ClaimStatus::Retracted => "retracted",
        ClaimStatus::UserAsserted => "user_asserted",
    }
}

fn status_from_str(s: &str) -> Result<ClaimStatus> {
    Ok(match s {
        "active" => ClaimStatus::Active,
        "superseded" => ClaimStatus::Superseded,
        "retracted" => ClaimStatus::Retracted,
        "user_asserted" => ClaimStatus::UserAsserted,
        other => {
            return Err(Error::Storage(format!(
                "unknown claim status {other:?} in store"
            )));
        }
    })
}

/// A fresh entity id: `ent_` + 16 lowercase base32 characters (80 random bits). URL-safe and
/// valid as a file-name stem.
fn new_entity_id() -> Result<String> {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 10];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::Other(format!("cannot generate an entity id: {e}")))?;
    let mut bits: u128 = 0;
    for b in bytes {
        bits = (bits << 8) | u128::from(b);
    }
    let mut id = String::with_capacity(ENTITY_ID_PREFIX.len() + 16);
    id.push_str(ENTITY_ID_PREFIX);
    for i in (0..16).rev() {
        id.push(ALPHABET[((bits >> (i * 5)) & 31) as usize] as char);
    }
    Ok(id)
}

fn sql_limit(limit: usize) -> i64 {
    if limit == 0 {
        -1
    } else {
        i64::try_from(limit).unwrap_or(i64::MAX)
    }
}

fn opt_time(s: Option<String>) -> Result<Option<OffsetDateTime>> {
    s.as_deref().map(parse_time).transpose()
}

fn merge_attrs(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                b.insert(k.clone(), v.clone());
            }
        }
        (b, p) => *b = p.clone(),
    }
}

fn parse_attrs(raw: &str) -> Result<Value> {
    serde_json::from_str(raw)
        .map_err(|e| Error::Storage(format!("bad entity attributes in store: {e}")))
}

fn validate_claim(new: &NewClaim) -> Result<()> {
    if new.predicate.trim().is_empty() {
        return Err(Error::Invalid("claim predicate is empty".into()));
    }
    if new.evidence.is_empty() && new.status != ClaimStatus::UserAsserted {
        return Err(Error::Invalid(
            "a claim needs evidence unless it is user-asserted".into(),
        ));
    }
    if !(0.0..=1.0).contains(&new.confidence) {
        return Err(Error::Invalid("confidence must be within 0..=1".into()));
    }
    Ok(())
}

// ------------------------------------------------------------------ entity rows

/// Follow `merged_into` from `id`. `None` if the id never existed (or the chain is broken).
fn resolve(conn: &Connection, id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT merged_into FROM entities WHERE id = ?1")
        .db("prepare resolve")?;
    let mut cur = id.to_owned();
    for _ in 0..=MAX_MERGE_DEPTH {
        let next: Option<Option<String>> = stmt
            .query_row([&cur], |r| r.get(0))
            .optional()
            .db("resolve entity")?;
        match next {
            None => return Ok(None),
            Some(None) => return Ok(Some(cur)),
            Some(Some(n)) => cur = n,
        }
    }
    tracing::warn!(
        entity = id,
        "merge chain too long or cyclic; treating as missing"
    );
    Ok(None)
}

/// The surviving id for `id`, or `NotFound`.
fn live(conn: &Connection, id: &EntityId) -> Result<String> {
    resolve(conn, id.as_str())?.ok_or_else(|| Error::NotFound(format!("entity {id}")))
}

fn entity_name(conn: &Connection, id: &str) -> Result<String> {
    conn.prepare_cached("SELECT name FROM entities WHERE id = ?1")
        .db("prepare entity name")?
        .query_row([id], |r| r.get(0))
        .db("read entity name")
}

/// Insert an alias row unless an identical one exists.
fn insert_name_row(tx: &Connection, entity: &str, text: &str, is_name: bool) -> Result<()> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO entity_aliases (entity_id, alias, alias_norm, alias_lower, is_name)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .db("prepare alias insert")?
    .execute(params![
        entity,
        text,
        normalize_name(text),
        text.to_lowercase(),
        is_name
    ])
    .db("insert alias")?;
    Ok(())
}

/// Add `alias` (trimmed) unless it is blank or normalises to the entity's name (`name_norm`).
fn add_alias_row(tx: &Connection, entity: &str, name_norm: &str, alias: &str) -> Result<()> {
    let a = alias.trim();
    if !a.is_empty() && normalize_name(a) != name_norm {
        insert_name_row(tx, entity, a, false)?;
    }
    Ok(())
}

/// Owner of a normalised identifier, if any.
fn identifier_owner(conn: &Connection, n: &Identifier) -> Result<Option<String>> {
    conn.prepare_cached("SELECT entity_id FROM entity_identifiers WHERE scheme = ?1 AND value = ?2")
        .db("prepare identifier lookup")?
        .query_row(params![n.scheme, n.value], |r| r.get(0))
        .optional()
        .db("look up identifier")
}

fn insert_identifier(tx: &Connection, entity: &str, n: &Identifier) -> Result<()> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO entity_identifiers (entity_id, scheme, value) VALUES (?1, ?2, ?3)",
    )
    .db("prepare identifier insert")?
    .execute(params![entity, n.scheme, n.value])
    .db("insert identifier")?;
    Ok(())
}

/// Load a live entity (by its resolved id) with aliases, identifiers and merged ids.
fn load_entity(conn: &Connection, id: &str) -> Result<Option<Entity>> {
    let row = conn
        .prepare_cached(
            "SELECT kind, name, attributes, created_at, updated_at FROM entities WHERE id = ?1",
        )
        .db("prepare entity")?
        .query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .optional()
        .db("read entity")?;
    let Some((kind, name, attributes, created_at, updated_at)) = row else {
        return Ok(None);
    };
    let strings = |sql: &str| -> Result<Vec<String>> {
        let mut stmt = conn.prepare_cached(sql).db("prepare entity detail")?;
        let rows = stmt
            .query_map([id], |r| r.get(0))
            .db("query entity detail")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("read entity detail")
    };
    let aliases = strings(
        "SELECT alias FROM entity_aliases WHERE entity_id = ?1 AND is_name = 0 ORDER BY id",
    )?;
    let merged_ids =
        strings("SELECT id FROM entities WHERE merged_into = ?1 ORDER BY merged_at, id")?
            .into_iter()
            .map(EntityId::new)
            .collect();
    let identifiers = {
        let mut stmt = conn
            .prepare_cached(
                "SELECT scheme, value FROM entity_identifiers WHERE entity_id = ?1 ORDER BY id",
            )
            .db("prepare identifiers")?;
        let rows = stmt
            .query_map([id], |r| {
                Ok(Identifier::new(
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                ))
            })
            .db("query identifiers")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("read identifiers")?
    };
    Ok(Some(Entity {
        id: EntityId::new(id),
        kind: kind_from_str(&kind)?,
        name,
        aliases,
        identifiers,
        attributes: parse_attrs(&attributes)?,
        created_at: parse_time(&created_at)?,
        updated_at: parse_time(&updated_at)?,
        merged_ids,
    }))
}

// ------------------------------------------------------------------ claim rows

const CLAIM_COLUMNS: &str = "c.id, c.subject, c.predicate, c.object_entity, c.object_literal, \
                             c.text, c.confidence, c.status, c.valid_from, c.valid_to, \
                             c.created_at, c.updated_at";

/// A claim row as stored (strings not yet parsed).
struct ClaimRow {
    id: i64,
    subject: String,
    predicate: String,
    object_entity: Option<String>,
    object_literal: Option<String>,
    text: String,
    confidence: f64,
    status: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
    created_at: String,
    updated_at: String,
}

fn read_claim_row(r: &Row<'_>) -> rusqlite::Result<ClaimRow> {
    Ok(ClaimRow {
        id: r.get(0)?,
        subject: r.get(1)?,
        predicate: r.get(2)?,
        object_entity: r.get(3)?,
        object_literal: r.get(4)?,
        text: r.get(5)?,
        confidence: r.get(6)?,
        status: r.get(7)?,
        valid_from: r.get(8)?,
        valid_to: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

impl ClaimRow {
    fn into_claim(self, evidence: Vec<Evidence>) -> Result<Claim> {
        let object = match (self.object_entity, self.object_literal) {
            (Some(e), _) => ClaimObject::Entity(EntityId::new(e)),
            (None, Some(l)) => ClaimObject::Literal(l),
            (None, None) => {
                return Err(Error::Storage(format!("claim {} has no object", self.id)));
            }
        };
        Ok(Claim {
            id: self.id,
            subject: EntityId::new(self.subject),
            predicate: self.predicate,
            object,
            text: self.text,
            confidence: self.confidence as f32,
            status: status_from_str(&self.status)?,
            evidence,
            valid_from: opt_time(self.valid_from)?,
            valid_to: opt_time(self.valid_to)?,
            created_at: parse_time(&self.created_at)?,
            updated_at: parse_time(&self.updated_at)?,
        })
    }
}

fn read_evidence(r: &Row<'_>, offset: usize) -> rusqlite::Result<Evidence> {
    Ok(Evidence {
        item: item_ref(r.get(offset)?, r.get(offset + 1)?),
        chunk_id: r.get(offset + 2)?,
        quote: r.get(offset + 3)?,
    })
}

fn evidence_of(conn: &Connection, claim: ClaimId) -> Result<Vec<Evidence>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT source, key, chunk_id, quote FROM claim_evidence
             WHERE claim_id = ?1 ORDER BY id",
        )
        .db("prepare evidence")?;
    let rows = stmt
        .query_map([claim], |r| read_evidence(r, 0))
        .db("query evidence")?;
    rows.collect::<rusqlite::Result<_>>().db("read evidence")
}

fn insert_evidence(tx: &Connection, claim: ClaimId, ev: &Evidence, now: &str) -> Result<()> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO claim_evidence (claim_id, source, key, chunk_id, quote, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .db("prepare evidence insert")?
    .execute(params![
        claim,
        ev.item.source.as_str(),
        ev.item.key.as_str(),
        ev.chunk_id,
        ev.quote,
        now
    ])
    .db("insert evidence")?;
    Ok(())
}

/// Subject and entity object of a claim, or `NotFound`.
fn claim_ends(conn: &Connection, claim: ClaimId) -> Result<(String, Option<String>, String)> {
    conn.prepare_cached("SELECT subject, object_entity, status FROM claims WHERE id = ?1")
        .db("prepare claim lookup")?
        .query_row([claim], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()
        .db("read claim")?
        .ok_or_else(|| Error::NotFound(format!("claim {claim}")))
}

// ------------------------------------------------------------------ provenance

/// Which items' provenance to drop.
pub(crate) enum ItemScope<'a> {
    Item(&'a ItemRef),
    Source(&'a SourceId),
}

impl ItemScope<'_> {
    fn filter(&self) -> (&'static str, Vec<&str>) {
        match self {
            Self::Item(i) => (
                "source = ?1 AND key = ?2",
                vec![i.source.as_str(), i.key.as_str()],
            ),
            Self::Source(s) => ("source = ?1", vec![s.as_str()]),
        }
    }
}

/// Drop every evidence row and mention from the items in `scope`, retract claims left without
/// evidence (unless user-asserted) and record the touched entities. Returns the affected claim
/// ids in ascending order. Runs inside the caller's transaction (also used by
/// `Store::delete_item` and `Store::delete_source`).
pub(crate) fn retract_tx(
    tx: &Connection,
    scope: ItemScope<'_>,
    touched: &mut Touched,
) -> Result<Vec<ClaimId>> {
    let (filter, args) = scope.filter();
    let affected: Vec<ClaimId> = {
        let mut stmt = tx
            .prepare_cached(&format!(
                "SELECT DISTINCT claim_id FROM claim_evidence WHERE {filter} ORDER BY claim_id"
            ))
            .db("prepare retract lookup")?;
        let rows = stmt
            .query_map(params_from_iter(&args), |r| r.get(0))
            .db("query retract lookup")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("read retract lookup")?
    };
    if !affected.is_empty() {
        tx.prepare_cached(&format!("DELETE FROM claim_evidence WHERE {filter}"))
            .db("prepare evidence delete")?
            .execute(params_from_iter(&args))
            .db("delete evidence")?;
        let mut update = tx
            .prepare_cached(
                "UPDATE claims SET
                     status = CASE
                         WHEN status <> 'user_asserted'
                              AND NOT EXISTS (SELECT 1 FROM claim_evidence e
                                              WHERE e.claim_id = claims.id)
                         THEN 'retracted' ELSE status END,
                     updated_at = ?2
                 WHERE id = ?1
                 RETURNING subject, object_entity",
            )
            .db("prepare claim retract")?;
        for &id in &affected {
            let (subject, object): (String, Option<String>) = update
                .query_row(params![id, touched.now()], |r| Ok((r.get(0)?, r.get(1)?)))
                .db("retract claim")?;
            touched.add(&subject);
            if let Some(o) = object {
                touched.add(&o);
            }
        }
    }
    let mentioned: Vec<String> = {
        let mut stmt = tx
            .prepare_cached(&format!(
                "DELETE FROM mentions WHERE {filter} RETURNING entity_id"
            ))
            .db("prepare mention delete")?;
        let rows = stmt
            .query_map(params_from_iter(&args), |r| r.get(0))
            .db("delete mentions")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("read deleted mentions")?
    };
    for e in &mentioned {
        touched.add(e);
    }
    Ok(affected)
}

// ------------------------------------------------------------------ the contract

impl GraphStore for SqliteGraph {
    fn create_entity(&self, new: &NewEntity) -> Result<EntityId> {
        let name = new.name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("entity name is empty".into()));
        }
        let identifiers: Vec<Identifier> =
            new.identifiers.iter().map(Identifier::normalized).collect();
        let attributes = match &new.attributes {
            Value::Null => "{}".to_owned(),
            v => serde_json::to_string(v)?,
        };
        self.write("create_entity", |tx, touched| {
            for n in &identifiers {
                if identifier_owner(tx, n)?.is_some() {
                    return Err(Error::Invalid(format!(
                        "identifier {}:{} already belongs to another entity",
                        n.scheme, n.value
                    )));
                }
            }
            let id = loop {
                let candidate = new_entity_id()?;
                let taken: bool = tx
                    .query_row(
                        "SELECT EXISTS (SELECT 1 FROM entities WHERE id = ?1)",
                        [&candidate],
                        |r| r.get(0),
                    )
                    .db("check entity id")?;
                if !taken {
                    break candidate;
                }
            };
            tx.execute(
                "INSERT INTO entities (id, kind, name, name_lower, attributes, created_at,
                                       updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    id,
                    new.kind.as_str(),
                    name,
                    name.to_lowercase(),
                    attributes,
                    touched.now()
                ],
            )
            .db("insert entity")?;
            insert_name_row(tx, &id, name, true)?;
            let name_norm = normalize_name(name);
            for a in &new.aliases {
                add_alias_row(tx, &id, &name_norm, a)?;
            }
            for n in &identifiers {
                insert_identifier(tx, &id, n)?;
            }
            touched.add(&id);
            Ok(EntityId::new(id))
        })
    }

    fn get_entity(&self, id: &EntityId) -> Result<Option<Entity>> {
        self.read(|c| match resolve(c, id.as_str())? {
            Some(r) => load_entity(c, &r),
            None => Ok(None),
        })
    }

    fn update_entity(&self, id: &EntityId, patch: &EntityPatch) -> Result<Entity> {
        let new_name = match &patch.name {
            Some(n) if n.trim().is_empty() => {
                return Err(Error::Invalid("entity name is empty".into()));
            }
            Some(n) => Some(n.trim().to_owned()),
            None => None,
        };
        let id = self.write("update_entity", |tx, touched| {
            let id = live(tx, id)?;
            if let Some(name) = &new_name {
                let old = entity_name(tx, &id)?;
                tx.execute(
                    "UPDATE entities SET name = ?2, name_lower = ?3 WHERE id = ?1",
                    params![id, name, name.to_lowercase()],
                )
                .db("rename entity")?;
                tx.execute(
                    "UPDATE entity_aliases SET alias = ?2, alias_norm = ?3, alias_lower = ?4
                     WHERE entity_id = ?1 AND is_name = 1",
                    params![id, name, normalize_name(name), name.to_lowercase()],
                )
                .db("rename entity name row")?;
                if normalize_name(&old) != normalize_name(name) {
                    insert_name_row(tx, &id, &old, false)?;
                }
            }
            if let Some(kind) = patch.kind {
                tx.execute(
                    "UPDATE entities SET kind = ?2 WHERE id = ?1",
                    params![id, kind.as_str()],
                )
                .db("change entity kind")?;
            }
            if let Some(a) = &patch.attributes {
                let raw: String = tx
                    .query_row(
                        "SELECT attributes FROM entities WHERE id = ?1",
                        [&id],
                        |r| r.get(0),
                    )
                    .db("read attributes")?;
                let mut attrs = parse_attrs(&raw)?;
                merge_attrs(&mut attrs, a);
                tx.execute(
                    "UPDATE entities SET attributes = ?2 WHERE id = ?1",
                    params![id, serde_json::to_string(&attrs)?],
                )
                .db("write attributes")?;
            }
            touched.add(&id);
            Ok(id)
        })?;
        self.read(|c| load_entity(c, &id))?
            .ok_or_else(|| Error::NotFound(format!("entity {id}")))
    }

    fn add_alias(&self, id: &EntityId, alias: &str) -> Result<()> {
        self.write("add_alias", |tx, touched| {
            let id = live(tx, id)?;
            let name_norm = normalize_name(&entity_name(tx, &id)?);
            add_alias_row(tx, &id, &name_norm, alias)?;
            touched.add(&id);
            Ok(())
        })
    }

    fn add_identifier(&self, id: &EntityId, ident: &Identifier) -> Result<()> {
        let n = ident.normalized();
        self.write("add_identifier", |tx, touched| {
            let id = live(tx, id)?;
            match identifier_owner(tx, &n)? {
                Some(other) if other != id => {
                    return Err(Error::Invalid(format!(
                        "identifier {}:{} already belongs to {other}",
                        n.scheme, n.value
                    )));
                }
                Some(_) => {}
                None => insert_identifier(tx, &id, &n)?,
            }
            touched.add(&id);
            Ok(())
        })
    }

    fn find_by_identifier(&self, ident: &Identifier) -> Result<Option<EntityId>> {
        let n = ident.normalized();
        self.read(|c| match identifier_owner(c, &n)? {
            Some(owner) => Ok(resolve(c, &owner)?.map(EntityId::new)),
            None => Ok(None),
        })
    }

    fn find_by_name(&self, name: &str, kind: Option<EntityKind>) -> Result<Vec<EntityId>> {
        let want = normalize_name(name);
        if want.is_empty() {
            return Ok(Vec::new());
        }
        self.read(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT DISTINCT a.entity_id
                     FROM entity_aliases a JOIN entities e ON e.id = a.entity_id
                     WHERE a.alias_norm = ?1 AND e.merged_into IS NULL
                       AND (?2 IS NULL OR e.kind = ?2)
                     ORDER BY a.entity_id",
                )
                .db("prepare find_by_name")?;
            let rows = stmt
                .query_map(params![want, kind.map(EntityKind::as_str)], |r| {
                    r.get::<_, String>(0).map(EntityId::new)
                })
                .db("query find_by_name")?;
            rows.collect::<rusqlite::Result<_>>()
                .db("read find_by_name")
        })
    }

    fn list_entities(&self, q: &EntityQuery) -> Result<Vec<EntitySummary>> {
        let needle = q
            .text
            .as_deref()
            .map(str::to_lowercase)
            .filter(|n| !n.is_empty());
        // Filters are only added when set, so the planner can use the (kind, name) index; the
        // per-entity counts are computed in an outer query, for the returned page only.
        let mut conds = vec!["e.merged_into IS NULL".to_owned()];
        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        let mut bind = |v: Box<dyn ToSql>| {
            args.push(v);
            format!("?{}", args.len())
        };
        if let Some(k) = q.kind {
            conds.push(format!("e.kind = {}", bind(Box::new(k.as_str()))));
        }
        // Substring search over names and aliases: the trigram index narrows candidates for
        // needles of three or more characters; `instr` keeps the match exact either way.
        if let Some(n) = &needle {
            if n.chars().count() >= 3 {
                // FTS5 phrase syntax: the needle in double quotes, inner quotes doubled.
                let phrase = bind(Box::new(format!("\"{}\"", n.replace('"', "\"\""))));
                let needle = bind(Box::new(n.clone()));
                conds.push(format!(
                    "e.id IN (SELECT a.entity_id FROM entity_names_fts f
                              JOIN entity_aliases a ON a.id = f.rowid
                              WHERE entity_names_fts MATCH {phrase}
                                AND instr(a.alias_lower, {needle}) > 0)"
                ));
            } else {
                let needle = bind(Box::new(n.clone()));
                conds.push(format!(
                    "EXISTS (SELECT 1 FROM entity_aliases a
                             WHERE a.entity_id = e.id AND instr(a.alias_lower, {needle}) > 0)"
                ));
            }
        }
        let limit = bind(Box::new(sql_limit(q.limit)));
        let offset = bind(Box::new(i64::try_from(q.offset).unwrap_or(i64::MAX)));
        let sql = format!(
            "SELECT p.id, p.kind, p.name, p.updated_at,
                    (SELECT count(*) FROM claims c WHERE c.subject = p.id AND c.{ACTIVE})
                  + (SELECT count(*) FROM claims c WHERE c.object_entity = p.id
                        AND c.subject <> p.id AND c.{ACTIVE}),
                    (SELECT count(*) FROM mentions m WHERE m.entity_id = p.id)
             FROM (SELECT e.id, e.kind, e.name, e.name_lower, e.updated_at FROM entities e
                   WHERE {}
                   ORDER BY e.name_lower, e.id
                   LIMIT {limit} OFFSET {offset}) p
             ORDER BY p.name_lower, p.id",
            conds.join(" AND ")
        );
        self.read(|c| {
            let mut stmt = c.prepare_cached(&sql).db("prepare list_entities")?;
            let rows = stmt
                .query_map(params_from_iter(args.iter()), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })
                .db("query list_entities")?;
            let mut out = Vec::new();
            for row in rows {
                let (id, kind, name, updated_at, claims, mentions) =
                    row.db("read list_entities")?;
                out.push(EntitySummary {
                    id: EntityId::new(id),
                    kind: kind_from_str(&kind)?,
                    name,
                    claims: claims as u64,
                    mentions: mentions as u64,
                    updated_at: parse_time(&updated_at)?,
                });
            }
            Ok(out)
        })
    }

    fn merge(&self, into: &EntityId, from: &EntityId) -> Result<()> {
        self.write("merge", |tx, touched| merge_tx(tx, into, from, touched))
    }

    fn add_claim(&self, new: &NewClaim) -> Result<ClaimId> {
        validate_claim(new)?;
        let valid_from = new.valid_from.map(format_time).transpose()?;
        let valid_to = new.valid_to.map(format_time).transpose()?;
        self.write("add_claim", |tx, touched| {
            let subject = live(tx, &new.subject)?;
            let (object_entity, object_literal) = match &new.object {
                ClaimObject::Entity(o) => (Some(live(tx, o)?), None),
                ClaimObject::Literal(l) => (None, Some(l.as_str())),
            };
            let existing: Option<(ClaimId, f64)> = tx
                .prepare_cached(&format!(
                    "SELECT id, confidence FROM claims
                     WHERE subject = ?1 AND predicate = ?2 AND object_entity IS ?3
                       AND object_literal IS ?4 AND {ACTIVE}
                     ORDER BY id LIMIT 1"
                ))
                .db("prepare claim dedupe")?
                .query_row(
                    params![subject, new.predicate, object_entity, object_literal],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .db("find claim by triple")?;
            let id = match existing {
                Some((id, confidence)) => {
                    let confidence = confidence.max(f64::from(new.confidence));
                    tx.execute(
                        "UPDATE claims SET confidence = ?2, updated_at = ?3,
                             status = CASE WHEN ?4 THEN 'user_asserted' ELSE status END
                         WHERE id = ?1",
                        params![
                            id,
                            confidence,
                            touched.now(),
                            new.status == ClaimStatus::UserAsserted
                        ],
                    )
                    .db("update claim")?;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO claims (subject, predicate, object_entity, object_literal,
                                             text, confidence, status, valid_from, valid_to,
                                             created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                        params![
                            subject,
                            new.predicate,
                            object_entity,
                            object_literal,
                            new.text,
                            f64::from(new.confidence),
                            status_str(new.status),
                            valid_from,
                            valid_to,
                            touched.now()
                        ],
                    )
                    .db("insert claim")?;
                    tx.last_insert_rowid()
                }
            };
            let now = touched.now().to_owned();
            for ev in &new.evidence {
                insert_evidence(tx, id, ev, &now)?;
            }
            touched.add(&subject);
            if let Some(o) = &object_entity {
                touched.add(o);
            }
            Ok(id)
        })
    }

    fn get_claim(&self, id: ClaimId) -> Result<Option<Claim>> {
        self.read(|c| {
            let row = c
                .prepare_cached(&format!(
                    "SELECT {CLAIM_COLUMNS} FROM claims c WHERE c.id = ?1"
                ))
                .db("prepare get_claim")?
                .query_row([id], read_claim_row)
                .optional()
                .db("read claim")?;
            row.map(|r| r.into_claim(evidence_of(c, id)?)).transpose()
        })
    }

    fn add_evidence(&self, claim: ClaimId, ev: &Evidence) -> Result<()> {
        self.write("add_evidence", |tx, touched| {
            let (subject, object, _) = claim_ends(tx, claim)?;
            let now = touched.now().to_owned();
            insert_evidence(tx, claim, ev, &now)?;
            tx.execute(
                "UPDATE claims SET updated_at = ?2,
                     status = CASE WHEN status = 'retracted' THEN 'active' ELSE status END
                 WHERE id = ?1",
                params![claim, now],
            )
            .db("update claim")?;
            touched.add(&subject);
            if let Some(o) = object {
                touched.add(&o);
            }
            Ok(())
        })
    }

    fn set_claim_status(&self, claim: ClaimId, status: ClaimStatus) -> Result<()> {
        self.write("set_claim_status", |tx, touched| {
            let (subject, object, _) = claim_ends(tx, claim)?;
            if status == ClaimStatus::Active {
                let has_evidence: bool = tx
                    .query_row(
                        "SELECT EXISTS (SELECT 1 FROM claim_evidence WHERE claim_id = ?1)",
                        [claim],
                        |r| r.get(0),
                    )
                    .db("check evidence")?;
                if !has_evidence {
                    return Err(Error::Invalid(
                        "a claim without evidence cannot be active".into(),
                    ));
                }
            }
            tx.execute(
                "UPDATE claims SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![claim, status_str(status), touched.now()],
            )
            .db("set claim status")?;
            touched.add(&subject);
            if let Some(o) = object {
                touched.add(&o);
            }
            Ok(())
        })
    }

    fn claims_for(&self, entity: &EntityId, include_inactive: bool) -> Result<Vec<Claim>> {
        let status = if include_inactive {
            String::new()
        } else {
            format!("AND c.{ACTIVE}")
        };
        self.read(|c| {
            let Some(id) = resolve(c, entity.as_str())? else {
                return Ok(Vec::new());
            };
            let filter = format!("(c.subject = ?1 OR c.object_entity = ?1) {status}");
            let rows: Vec<ClaimRow> = {
                let mut stmt = c
                    .prepare_cached(&format!(
                        "SELECT {CLAIM_COLUMNS} FROM claims c WHERE {filter} ORDER BY c.id"
                    ))
                    .db("prepare claims_for")?;
                let rows = stmt
                    .query_map([&id], read_claim_row)
                    .db("query claims_for")?;
                rows.collect::<rusqlite::Result<_>>()
                    .db("read claims_for")?
            };
            let mut evidence: HashMap<ClaimId, Vec<Evidence>> = HashMap::new();
            {
                let mut stmt = c
                    .prepare_cached(&format!(
                        "SELECT ev.claim_id, ev.source, ev.key, ev.chunk_id, ev.quote
                         FROM claims c JOIN claim_evidence ev ON ev.claim_id = c.id
                         WHERE {filter} ORDER BY ev.id"
                    ))
                    .db("prepare claims_for evidence")?;
                let rows = stmt
                    .query_map([&id], |r| Ok((r.get::<_, i64>(0)?, read_evidence(r, 1)?)))
                    .db("query claims_for evidence")?;
                for row in rows {
                    let (claim, ev) = row.db("read claims_for evidence")?;
                    evidence.entry(claim).or_default().push(ev);
                }
            }
            rows.into_iter()
                .map(|r| {
                    let ev = evidence.remove(&r.id).unwrap_or_default();
                    r.into_claim(ev)
                })
                .collect()
        })
    }

    fn add_mention(&self, m: &Mention) -> Result<()> {
        self.write("add_mention", |tx, touched| {
            let entity = live(tx, &m.entity)?;
            tx.prepare_cached(
                "INSERT INTO mentions (entity_id, source, key, chunk_id, surface, confidence,
                                       created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT DO UPDATE SET confidence = max(confidence, excluded.confidence)",
            )
            .db("prepare mention insert")?
            .execute(params![
                entity,
                m.item.source.as_str(),
                m.item.key.as_str(),
                m.chunk_id,
                m.surface,
                f64::from(m.confidence),
                touched.now()
            ])
            .db("insert mention")?;
            touched.add(&entity);
            Ok(())
        })
    }

    fn mentions_for(&self, entity: &EntityId, limit: usize) -> Result<Vec<Mention>> {
        self.read(|c| {
            let Some(id) = resolve(c, entity.as_str())? else {
                return Ok(Vec::new());
            };
            let mut stmt = c
                .prepare_cached(
                    "SELECT source, key, chunk_id, surface, confidence FROM mentions
                     WHERE entity_id = ?1 ORDER BY id LIMIT ?2",
                )
                .db("prepare mentions_for")?;
            let rows = stmt
                .query_map(params![id, sql_limit(limit)], |r| {
                    Ok(Mention {
                        entity: EntityId::new(id.clone()),
                        item: item_ref(r.get(0)?, r.get(1)?),
                        chunk_id: r.get(2)?,
                        surface: r.get(3)?,
                        confidence: r.get::<_, f64>(4)? as f32,
                    })
                })
                .db("query mentions_for")?;
            rows.collect::<rusqlite::Result<_>>()
                .db("read mentions_for")
        })
    }

    fn retract_item(&self, item: &ItemRef) -> Result<Vec<ClaimId>> {
        self.write("retract_item", |tx, touched| {
            retract_tx(tx, ItemScope::Item(item), touched)
        })
    }

    fn neighbourhood(&self, center: &EntityId, depth: u32, limit: usize) -> Result<GraphView> {
        let limit = limit.clamp(1, MAX_NEIGHBOURHOOD_NODES);
        self.read(|c| neighbourhood(c, center, depth, limit))
    }

    fn changes_since(&self, seq: u64, limit: usize) -> Result<(Vec<EntityId>, u64)> {
        let from = i64::try_from(seq).unwrap_or(i64::MAX);
        self.read(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT entity_id, seq FROM graph_changes WHERE seq > ?1 ORDER BY seq LIMIT ?2",
                )
                .db("prepare changes_since")?;
            let rows = stmt
                .query_map(params![from, sql_limit(limit)], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })
                .db("query changes_since")?;
            let mut ids = Vec::new();
            let mut high = seq;
            for row in rows {
                let (id, s) = row.db("read changes_since")?;
                ids.push(EntityId::new(id));
                high = s as u64;
            }
            Ok((ids, high))
        })
    }

    fn counts(&self) -> Result<GraphCounts> {
        self.read(|c| {
            let (entities, claims, mentions): (i64, i64, i64) = c
                .query_row(
                    &format!(
                        "SELECT (SELECT count(*) FROM entities WHERE merged_into IS NULL),
                                (SELECT count(*) FROM claims WHERE {ACTIVE}),
                                (SELECT count(*) FROM mentions)"
                    ),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .db("count graph")?;
            Ok(GraphCounts {
                entities: entities as u64,
                claims: claims as u64,
                mentions: mentions as u64,
            })
        })
    }
}

// ------------------------------------------------------------------ merge

fn merge_tx(
    tx: &Connection,
    into: &EntityId,
    from: &EntityId,
    touched: &mut Touched,
) -> Result<()> {
    let into = live(tx, into)?;
    let from = live(tx, from)?;
    if into == from {
        return Err(Error::Invalid("cannot merge an entity into itself".into()));
    }
    let now = touched.now().to_owned();

    // Names: `from`'s name and aliases become aliases of `into` (unless they normalise to its
    // name); `from` keeps no name rows, so it drops out of name lookup and search.
    let into_norm = normalize_name(&entity_name(tx, &into)?);
    let names: Vec<String> = {
        let mut stmt = tx
            .prepare_cached(
                "SELECT alias FROM entity_aliases WHERE entity_id = ?1 ORDER BY is_name DESC, id",
            )
            .db("prepare merge names")?;
        let rows = stmt
            .query_map([&from], |r| r.get(0))
            .db("query merge names")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("read merge names")?
    };
    tx.execute("DELETE FROM entity_aliases WHERE entity_id = ?1", [&from])
        .db("drop merged names")?;
    for n in &names {
        add_alias_row(tx, &into, &into_norm, n)?;
    }

    // Identifiers move as they are (each belongs to one entity, so no conflicts).
    tx.execute(
        "UPDATE entity_identifiers SET entity_id = ?1 WHERE entity_id = ?2",
        params![into, from],
    )
    .db("move identifiers")?;

    // Attributes: `into` wins; keys only `from` has are added.
    let attrs = |id: &str| -> Result<Value> {
        let raw: String = tx
            .query_row("SELECT attributes FROM entities WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .db("read attributes")?;
        parse_attrs(&raw)
    };
    if let (Value::Object(extra), Value::Object(mut own)) = (attrs(&from)?, attrs(&into)?) {
        let before = own.len();
        for (k, v) in extra {
            own.entry(k).or_insert(v);
        }
        if own.len() != before {
            tx.execute(
                "UPDATE entities SET attributes = ?2 WHERE id = ?1",
                params![into, serde_json::to_string(&Value::Object(own))?],
            )
            .db("merge attributes")?;
        }
    }

    // Tombstone `from`, and re-point earlier tombstones so chains stay one hop long.
    tx.execute(
        "UPDATE entities SET merged_into = ?1 WHERE merged_into = ?2",
        params![into, from],
    )
    .db("flatten merge chain")?;
    tx.execute(
        "UPDATE entities SET merged_into = ?1, merged_at = ?3 WHERE id = ?2",
        params![into, from, now],
    )
    .db("tombstone merged entity")?;

    // Claims: re-point both ends, remembering the other entities whose pages change.
    {
        let mut stmt = tx
            .prepare_cached(
                "SELECT subject, object_entity FROM claims
                 WHERE subject = ?1 OR object_entity = ?1",
            )
            .db("prepare merge claims")?;
        let rows = stmt
            .query_map([&from], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .db("query merge claims")?;
        for row in rows {
            let (s, o) = row.db("read merge claims")?;
            for e in std::iter::once(s).chain(o) {
                if e != from && e != into {
                    touched.add(&e);
                }
            }
        }
    }
    tx.execute(
        "UPDATE claims SET subject = ?1, updated_at = ?3 WHERE subject = ?2",
        params![into, from, now],
    )
    .db("move claim subjects")?;
    tx.execute(
        "UPDATE claims SET object_entity = ?1, updated_at = ?3 WHERE object_entity = ?2",
        params![into, from, now],
    )
    .db("move claim objects")?;
    fold_duplicate_claims(tx, &into, &now)?;

    // Mentions: move, folding ones `into` already has.
    tx.execute(
        "INSERT INTO mentions (entity_id, source, key, chunk_id, surface, confidence, created_at)
         SELECT ?1, source, key, chunk_id, surface, confidence, created_at
         FROM mentions WHERE entity_id = ?2 ORDER BY id
         ON CONFLICT DO UPDATE SET confidence = max(confidence, excluded.confidence)",
        params![into, from],
    )
    .db("move mentions")?;
    tx.execute("DELETE FROM mentions WHERE entity_id = ?1", [&from])
        .db("drop moved mentions")?;

    touched.add(&into);
    touched.add(&from);
    Ok(())
}

/// Fold active claims of `entity` that share a triple into the oldest one: evidence moves,
/// confidence is the maximum, and the survivor becomes `user_asserted` if any folded claim was.
fn fold_duplicate_claims(tx: &Connection, entity: &str, now: &str) -> Result<()> {
    type Triple = (String, String, Option<String>, Option<String>);
    let rows: Vec<(ClaimId, Triple, f64, String)> = {
        let mut stmt = tx
            .prepare_cached(&format!(
                "SELECT id, subject, predicate, object_entity, object_literal, confidence, status
                 FROM claims WHERE (subject = ?1 OR object_entity = ?1) AND {ACTIVE}
                 ORDER BY id"
            ))
            .db("prepare claim fold")?;
        let rows = stmt
            .query_map([entity], |r| {
                Ok((
                    r.get(0)?,
                    (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .db("query claim fold")?;
        rows.collect::<rusqlite::Result<_>>()
            .db("read claim fold")?
    };
    let mut survivors: HashMap<Triple, (ClaimId, f64, bool)> = HashMap::new();
    let mut folds: Vec<(ClaimId, ClaimId)> = Vec::new();
    for (id, triple, confidence, status) in rows {
        let asserted = status == "user_asserted";
        match survivors.get_mut(&triple) {
            Some((keep, conf, ua)) => {
                *conf = conf.max(confidence);
                *ua |= asserted;
                folds.push((*keep, id));
            }
            None => {
                survivors.insert(triple, (id, confidence, asserted));
            }
        }
    }
    if folds.is_empty() {
        return Ok(());
    }
    for (keep, gone) in &folds {
        tx.execute(
            "INSERT OR IGNORE INTO claim_evidence (claim_id, source, key, chunk_id, quote,
                                                   created_at)
             SELECT ?1, source, key, chunk_id, quote, created_at
             FROM claim_evidence WHERE claim_id = ?2 ORDER BY id",
            params![keep, gone],
        )
        .db("move folded evidence")?;
        // Evidence left behind (duplicates) goes with the claim (ON DELETE CASCADE).
        tx.execute("DELETE FROM claims WHERE id = ?1", [gone])
            .db("drop folded claim")?;
    }
    let kept: HashSet<ClaimId> = folds.iter().map(|(k, _)| *k).collect();
    for (id, confidence, asserted) in survivors.into_values() {
        if kept.contains(&id) {
            tx.execute(
                "UPDATE claims SET confidence = ?2, updated_at = ?3,
                     status = CASE WHEN ?4 THEN 'user_asserted' ELSE status END
                 WHERE id = ?1",
                params![id, confidence, now, asserted],
            )
            .db("update folded claim")?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ neighbourhood

/// Breadth-first over active entity→entity claims, in the same order as `MemGraph`: nodes in
/// BFS order, each node's claims by id. Once `limit` nodes are seen, further new nodes are
/// skipped (and `truncated` set), but edges among seen nodes are still collected.
fn neighbourhood(c: &Connection, center: &EntityId, depth: u32, limit: usize) -> Result<GraphView> {
    let start = live(c, center)?;
    let mut node_stmt = c
        .prepare_cached("SELECT kind, name FROM entities WHERE id = ?1")
        .db("prepare neighbourhood node")?;
    let mut edge_stmt = c
        .prepare_cached(&format!(
            "SELECT id, subject, object_entity, predicate FROM claims
             WHERE object_entity IS NOT NULL AND (subject = ?1 OR object_entity = ?1) AND {ACTIVE}
             ORDER BY id"
        ))
        .db("prepare neighbourhood edges")?;

    let mut view = GraphView::default();
    let mut seen: HashSet<String> = HashSet::from([start.clone()]);
    let mut queue = VecDeque::from([(start, 0u32)]);
    let mut edges: HashSet<ClaimId> = HashSet::new();
    while let Some((id, d)) = queue.pop_front() {
        let (kind, name): (String, String) = node_stmt
            .query_row([&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .db("read neighbourhood node")?;
        view.nodes.push(GraphNode {
            id: EntityId::new(id.clone()),
            kind: kind_from_str(&kind)?,
            name,
            depth: d,
        });
        if d >= depth {
            continue;
        }
        let rows = edge_stmt
            .query_map([&id], |r| {
                Ok((
                    r.get::<_, ClaimId>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .db("query neighbourhood edges")?;
        for row in rows {
            let (claim, subject, object, predicate) = row.db("read neighbourhood edge")?;
            let other = if subject == id { &object } else { &subject };
            if !seen.contains(other) {
                if seen.len() >= limit {
                    view.truncated = true;
                    continue;
                }
                seen.insert(other.clone());
                queue.push_back((other.clone(), d + 1));
            }
            if edges.insert(claim) {
                view.edges.push(GraphEdge {
                    claim,
                    from: EntityId::new(subject),
                    to: EntityId::new(object),
                    predicate,
                });
            }
        }
    }
    view.edges
        .retain(|e| seen.contains(e.from.as_str()) && seen.contains(e.to.as_str()));
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_ids_are_prefixed_url_safe_and_distinct() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let id = new_entity_id().unwrap();
            assert_eq!(id.len(), ENTITY_ID_PREFIX.len() + 16);
            assert!(id.starts_with(ENTITY_ID_PREFIX));
            assert!(
                id[ENTITY_ID_PREFIX.len()..]
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
            );
            assert!(seen.insert(id));
        }
    }

    #[test]
    fn status_round_trips() {
        for s in [
            ClaimStatus::Active,
            ClaimStatus::Superseded,
            ClaimStatus::Retracted,
            ClaimStatus::UserAsserted,
        ] {
            assert_eq!(status_from_str(status_str(s)).unwrap(), s);
        }
        for k in EntityKind::ALL {
            assert_eq!(kind_from_str(k.as_str()).unwrap(), k);
        }
    }
}
