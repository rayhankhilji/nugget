//! The knowledge graph on SQLite: [`SqliteGraph`] implements [`nugget_graph::GraphStore`] over the
//! schema v4 tables (see `schema.rs`).

use std::collections::{HashMap, HashSet, VecDeque};

use nugget_core::ids::{EntityId, ItemRef, SourceId};
use nugget_core::{Error, Result};
use nugget_graph::{
    BatchResult, Claim, ClaimId, ClaimObject, ClaimPatch, ClaimStatus, Entity, EntityDetail,
    EntityKind, EntityPatch, EntityProvenance, EntityQuery, EntitySummary, Evidence, GraphBatch,
    GraphCounts, GraphEdge, GraphNode, GraphOp, GraphStore, GraphView, Identifier, Mention,
    NewClaim, NewEntity, clamp_quote, normalize_name,
};
use rusqlite::{
    Connection, OptionalExtension, Row, ToSql, Transaction, TransactionBehavior, params,
    params_from_iter,
};
use serde_json::Value;
use time::OffsetDateTime;

use crate::Store;
use crate::error::{ConnExt, DbExt, format_time, parse_time};
use crate::items::item_ref;

/// Longest `merged_into` chain followed when resolving an id.
pub const MAX_MERGE_DEPTH: usize = 64;

/// Upper bound on the nodes [`GraphStore::neighbourhood`] returns, whatever `limit` asks for.
pub const MAX_NEIGHBOURHOOD_NODES: usize = 10_000;

/// Prefix of every entity id.
pub const ENTITY_ID_PREFIX: &str = "ent_";

/// SQL condition for claims that count as active ([`ClaimStatus::is_active`]).
const ACTIVE: &str = "status IN ('active', 'user_asserted')";

/// SQL condition for live claims ([`ClaimStatus::is_live`]): at most one per triple.
const LIVE: &str = "status IN ('active', 'user_asserted', 'rejected')";

/// Longest a cached [`GraphStore::counts`] is reused without a commit by this process (writes
/// by another process, such as the CLI, show up within this).
const GRAPH_COUNTS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// The knowledge graph stored in a [`Store`].
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
    s.as_str()
}

fn status_from_str(s: &str) -> Result<ClaimStatus> {
    ClaimStatus::parse(s)
        .ok_or_else(|| Error::Storage(format!("unknown claim status {s:?} in store")))
}

/// A fresh entity id: `ent_` + 16 lowercase base32 characters (80 random bits).
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
    if new.evidence.is_empty() && !new.status.is_user_decision() {
        return Err(Error::Invalid(
            "a claim needs evidence unless it is user-asserted or rejected".into(),
        ));
    }
    if !(0.0..=1.0).contains(&new.confidence) {
        return Err(Error::Invalid("confidence must be within 0..=1".into()));
    }
    Ok(())
}

// ------------------------------------------------------------------ entity rows

/// Follow `merged_into` from `id`.
fn resolve(conn: &Connection, id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT merged_into, deleted_at IS NOT NULL FROM entities WHERE id = ?1")
        .db("prepare resolve")?;
    let mut cur = id.to_owned();
    for _ in 0..=MAX_MERGE_DEPTH {
        let next: Option<(Option<String>, bool)> = stmt
            .query_row([&cur], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .db("resolve entity")?;
        match next {
            None | Some((None, true)) => return Ok(None),
            Some((None, false)) => return Ok(Some(cur)),
            Some((Some(n), _)) => cur = n,
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
    tx.exec(
        "INSERT OR IGNORE INTO entity_aliases (entity_id, alias, alias_norm, alias_lower, is_name)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            entity,
            text,
            normalize_name(text),
            text.to_lowercase(),
            is_name
        ],
        "insert alias",
    )?;
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
    tx.exec(
        "INSERT OR IGNORE INTO entity_identifiers (entity_id, scheme, value) VALUES (?1, ?2, ?3)",
        params![entity, n.scheme, n.value],
        "insert identifier",
    )?;
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
        conn.rows(sql, [id], |r| r.get(0), "read entity detail")
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
        conn.rows(
            "SELECT scheme, value FROM entity_identifiers WHERE entity_id = ?1 ORDER BY id",
            [id],
            |r| {
                Ok(Identifier::new(
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                ))
            },
            "read identifiers",
        )?
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
    conn.rows(
        "SELECT source, key, chunk_id, quote FROM claim_evidence
             WHERE claim_id = ?1 ORDER BY id",
        [claim],
        |r| read_evidence(r, 0),
        "read evidence",
    )
}

fn insert_evidence(tx: &Connection, claim: ClaimId, ev: &Evidence, now: &str) -> Result<()> {
    tx.exec(
        "INSERT OR IGNORE INTO claim_evidence (claim_id, source, key, chunk_id, quote, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            claim,
            ev.item.source.as_str(),
            ev.item.key.as_str(),
            ev.chunk_id,
            ev.quote.as_deref().map(clamp_quote),
            now
        ],
        "insert evidence",
    )?;
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

/// Drop every evidence row, mention and detail-provenance row from the items in `scope`, retract
/// claims left without evidence (unless user-asserted or rejected) and record the touched entities.
pub(crate) fn retract_tx(
    tx: &Connection,
    scope: ItemScope<'_>,
    touched: &mut Touched,
) -> Result<Vec<ClaimId>> {
    let (filter, args) = scope.filter();
    let affected: Vec<ClaimId> = {
        tx.rows(
            &format!(
                "SELECT DISTINCT claim_id FROM claim_evidence WHERE {filter} ORDER BY claim_id"
            ),
            params_from_iter(&args),
            |r| r.get(0),
            "read retract lookup",
        )?
    };
    if !affected.is_empty() {
        tx.exec(
            &format!("DELETE FROM claim_evidence WHERE {filter}"),
            params_from_iter(&args),
            "delete evidence",
        )?;
        let mut update = tx
            .prepare_cached(
                "UPDATE claims SET
                     status = CASE
                         WHEN status NOT IN ('user_asserted', 'rejected')
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
        tx.rows(
            &format!("DELETE FROM mentions WHERE {filter} RETURNING entity_id"),
            params_from_iter(&args),
            |r| r.get(0),
            "read deleted mentions",
        )?
    };
    for e in &mentioned {
        touched.add(e);
    }
    let sourced: Vec<String> = {
        tx.rows(
            &format!("DELETE FROM entity_detail_sources WHERE {filter} RETURNING entity_id"),
            params_from_iter(&args),
            |r| r.get(0),
            "read deleted detail sources",
        )?
    };
    for e in &sourced {
        touched.add(e);
    }
    Ok(affected)
}

// ------------------------------------------------------------------ writes (inside a transaction)

/// Insert a new entity.
fn create_entity_tx(
    tx: &Connection,
    touched: &mut Touched,
    new: &NewEntity,
    allow_owner: Option<&str>,
) -> Result<String> {
    let name = new.name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("entity name is empty".into()));
    }
    let identifiers: Vec<Identifier> = new.identifiers.iter().map(Identifier::normalized).collect();
    let attributes = match &new.attributes {
        Value::Null => "{}".to_owned(),
        v => serde_json::to_string(v)?,
    };
    let mut moving = Vec::new();
    for n in &identifiers {
        match identifier_owner(tx, n)? {
            Some(o) if Some(o.as_str()) == allow_owner => moving.push(n.clone()),
            Some(_) => {
                return Err(Error::Invalid(format!(
                    "identifier {}:{} already belongs to another entity",
                    n.scheme, n.value
                )));
            }
            None => {}
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
    tx.exec(
        "INSERT INTO entities (id, kind, name, name_lower, attributes, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            id,
            new.kind.as_str(),
            name,
            name.to_lowercase(),
            attributes,
            touched.now()
        ],
        "insert entity",
    )?;
    insert_name_row(tx, &id, name, true)?;
    let name_norm = normalize_name(name);
    for a in &new.aliases {
        add_alias_row(tx, &id, &name_norm, a)?;
    }
    for n in &identifiers {
        if moving.contains(n) {
            tx.exec(
                "UPDATE entity_identifiers SET entity_id = ?1 WHERE scheme = ?2 AND value = ?3",
                params![id, n.scheme, n.value],
                "move identifier",
            )?;
        } else {
            insert_identifier(tx, &id, n)?;
        }
    }
    touched.add(&id);
    Ok(id)
}

fn update_entity_tx(
    tx: &Connection,
    touched: &mut Touched,
    id: &EntityId,
    patch: &EntityPatch,
) -> Result<String> {
    let new_name = match &patch.name {
        Some(n) if n.trim().is_empty() => {
            return Err(Error::Invalid("entity name is empty".into()));
        }
        Some(n) => Some(n.trim().to_owned()),
        None => None,
    };
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
}

fn add_alias_tx(tx: &Connection, touched: &mut Touched, id: &EntityId, alias: &str) -> Result<()> {
    let id = live(tx, id)?;
    let name_norm = normalize_name(&entity_name(tx, &id)?);
    add_alias_row(tx, &id, &name_norm, alias)?;
    touched.add(&id);
    Ok(())
}

fn add_identifier_tx(
    tx: &Connection,
    touched: &mut Touched,
    id: &EntityId,
    ident: &Identifier,
) -> Result<()> {
    let n = ident.normalized();
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
}

fn add_detail_source_tx(
    tx: &Connection,
    touched: &mut Touched,
    id: &EntityId,
    detail: &EntityDetail,
    item: &ItemRef,
) -> Result<()> {
    let id = live(tx, id)?;
    let (kind, scheme, value) = detail.key();
    if value.is_empty() {
        return Ok(());
    }
    tx.exec(
        "INSERT OR IGNORE INTO entity_detail_sources
             (entity_id, kind, scheme, value, source, key, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            kind,
            scheme,
            value,
            item.source.as_str(),
            item.key.as_str(),
            touched.now()
        ],
        "insert detail source",
    )?;
    touched.add(&id);
    Ok(())
}

fn touch_ends(touched: &mut Touched, subject: &str, object: Option<&str>) {
    touched.add(subject);
    if let Some(o) = object {
        touched.add(o);
    }
}

fn add_claim_tx(tx: &Connection, touched: &mut Touched, new: &NewClaim) -> Result<ClaimId> {
    validate_claim(new)?;
    let valid_from = new.valid_from.map(format_time).transpose()?;
    let valid_to = new.valid_to.map(format_time).transpose()?;
    let subject = live(tx, &new.subject)?;
    let (object_entity, object_literal) = match &new.object {
        ClaimObject::Entity(o) => (Some(live(tx, o)?), None),
        ClaimObject::Literal(l) => (None, Some(l.as_str())),
    };
    let existing: Option<(ClaimId, f64)> = tx
        .prepare_cached(&format!(
            "SELECT id, confidence FROM claims
             WHERE subject = ?1 AND predicate = ?2 AND object_entity IS ?3
               AND object_literal IS ?4 AND {LIVE}
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
            tx.exec(
                "UPDATE claims SET confidence = ?2, updated_at = ?3,
                     status = CASE WHEN ?4 THEN ?5 ELSE status END,
                     text = CASE WHEN ?6 THEN ?7 ELSE text END
                 WHERE id = ?1",
                params![
                    id,
                    confidence,
                    touched.now(),
                    new.status.is_user_decision(),
                    status_str(new.status),
                    new.status == ClaimStatus::UserAsserted,
                    new.text
                ],
                "update claim",
            )?;
            if new.status == ClaimStatus::UserAsserted {
                record_assertion(tx, id, touched.now())?;
            }
            if new.status.is_user_decision() {
                release_if_not_current(tx, id)?;
            }
            id
        }
        None if let Some(id) =
            replaced_twin(tx, new, &subject, object_entity.as_deref(), object_literal)? =>
        {
            // Extracted again from the evidence of a claim the user replaced: it stays
            // replaced (see `GraphStore::supersede_by_user`).
            tx.exec(
                "UPDATE claims SET confidence = max(confidence, ?2), updated_at = ?3,
                     status = 'superseded'
                 WHERE id = ?1",
                params![id, f64::from(new.confidence), touched.now()],
                "update replaced claim",
            )?;
            id
        }
        None => {
            tx.exec(
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
                "insert claim",
            )?;
            let id = tx.last_insert_rowid();
            if new.status == ClaimStatus::UserAsserted {
                record_assertion(tx, id, touched.now())?;
            }
            id
        }
    };
    let now = touched.now().to_owned();
    for ev in &new.evidence {
        insert_evidence(tx, id, ev, &now)?;
    }
    touch_ends(touched, &subject, object_entity.as_deref());
    Ok(id)
}

/// When `claim` was made user-asserted (see `GraphStore::asserted_at`).
fn record_assertion(tx: &Connection, claim: ClaimId, at: &str) -> Result<()> {
    tx.exec(
        "INSERT INTO claim_assertions (claim_id, asserted_at) VALUES (?1, ?2)
         ON CONFLICT (claim_id) DO UPDATE SET asserted_at = excluded.asserted_at",
        params![claim, at],
        "record assertion",
    )?;
    Ok(())
}

/// SQL condition: the evidence row `(?claim, source, key, quote)` named by the given columns is
/// remembered by the user's replacement of the claim: from an item recorded then, with the same
/// quote, or with the item's content unchanged since (no item row then and now counts as
/// unchanged), or with the quote recorded then still in the item's text (the source still says
/// it, whatever quote an extractor picks this time).
fn remembered_sql(claim: &str, source: &str, key: &str, quote: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM claim_replacement_evidence r
                 WHERE r.claim_id = {claim} AND r.source = {source} AND r.key = {key}
                   AND (r.quote = {quote}
                        OR r.item_hash IS (SELECT i.content_hash FROM items i
                                           WHERE i.source = {source} AND i.key = {key})
                        OR (r.quote <> ''
                            AND EXISTS (SELECT 1 FROM chunks ch JOIN items i ON i.id = ch.item_id
                                        WHERE i.source = {source} AND i.key = {key}
                                          AND instr(ch.text, r.quote) > 0))))"
    )
}

/// Whether `claim` was replaced by the user and remembers every piece of `evidence` (non-empty).
fn is_replaced_by_user(conn: &Connection, claim: ClaimId, evidence: &[Evidence]) -> Result<bool> {
    if evidence.is_empty() {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {}",
            remembered_sql("?1", "?2", "?3", "?4")
        ))
        .db("prepare replaced lookup")?;
    for ev in evidence {
        let quote = clamp_quote(ev.quote.as_deref().unwrap_or(""));
        let hit: bool = stmt
            .query_row(
                params![claim, ev.item.source.as_str(), ev.item.key.as_str(), quote],
                |r| r.get(0),
            )
            .db("read replaced lookup")?;
        if !hit {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether the user replaced `claim` and all of its evidence is remembered.
fn all_evidence_remembered(tx: &Connection, claim: ClaimId) -> Result<bool> {
    tx.prepare_cached(&format!(
        "SELECT EXISTS (SELECT 1 FROM claim_replacements WHERE claim_id = ?1)
            AND NOT EXISTS (SELECT 1 FROM claim_evidence e WHERE e.claim_id = ?1
                            AND NOT {})",
        remembered_sql("?1", "e.source", "e.key", "coalesce(e.quote, '')")
    ))
    .db("prepare replaced evidence check")?
    .query_row([claim], |r| r.get(0))
    .db("check replaced evidence")
}

/// When `claim` (a user's replacement) is not current, what it replaced is free again.
fn release_if_not_current(tx: &Connection, claim: ClaimId) -> Result<()> {
    tx.exec(
        &format!(
            "DELETE FROM claim_replacements WHERE replaced_by = ?1
           AND NOT EXISTS (SELECT 1 FROM claims WHERE id = ?1 AND {ACTIVE})"
        ),
        [claim],
        "release replacements",
    )?;
    Ok(())
}

fn has_replacement(tx: &Connection, claim: ClaimId) -> Result<bool> {
    tx.prepare_cached("SELECT EXISTS (SELECT 1 FROM claim_replacements WHERE claim_id = ?1)")
        .db("prepare replacement lookup")?
        .query_row([claim], |r| r.get(0))
        .db("read replacement lookup")
}

/// The oldest superseded or retracted claim with `new`'s triple that the user replaced and that
/// remembers all of `new`'s evidence (only for extracted claims with evidence).
fn replaced_twin(
    tx: &Connection,
    new: &NewClaim,
    subject: &str,
    object_entity: Option<&str>,
    object_literal: Option<&str>,
) -> Result<Option<ClaimId>> {
    if new.status.is_user_decision() || new.evidence.is_empty() {
        return Ok(None);
    }
    let candidates: Vec<ClaimId> = {
        tx.rows(
            "SELECT id FROM claims
                 WHERE subject = ?1 AND predicate = ?2 AND object_entity IS ?3
                   AND object_literal IS ?4 AND status IN ('superseded', 'retracted')
                   AND EXISTS (SELECT 1 FROM claim_replacements r WHERE r.claim_id = claims.id)
                 ORDER BY id",
            params![subject, new.predicate, object_entity, object_literal],
            |r| r.get(0),
            "read replaced twin",
        )?
    };
    for id in candidates {
        if is_replaced_by_user(tx, id, &new.evidence)? {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn forget_user_replaced(tx: &Connection, claim: ClaimId) -> Result<()> {
    tx.exec(
        "DELETE FROM claim_replacements WHERE claim_id = ?1",
        [claim],
        "forget replaced claim",
    )?;
    Ok(())
}

/// Whether a live claim other than `claim` has its triple.
fn has_live_twin(tx: &Connection, claim: ClaimId) -> Result<bool> {
    tx.prepare_cached(&format!(
        "SELECT EXISTS (SELECT 1 FROM claims c JOIN claims o
           ON o.subject = c.subject AND o.predicate = c.predicate
          AND o.object_entity IS c.object_entity AND o.object_literal IS c.object_literal
         WHERE c.id = ?1 AND o.id <> ?1 AND o.{LIVE})"
    ))
    .db("prepare live twin check")?
    .query_row([claim], |r| r.get(0))
    .db("check live twin")
}

fn supersede_by_user_tx(
    tx: &Connection,
    touched: &mut Touched,
    claim: ClaimId,
    by: &NewClaim,
) -> Result<ClaimId> {
    let (subject, object, _) = claim_ends(tx, claim)?;
    forget_user_replaced(tx, claim)?;
    tx.exec(
        "UPDATE claims SET status = 'superseded', updated_at = ?2 WHERE id = ?1",
        params![claim, touched.now()],
        "supersede claim",
    )?;
    touch_ends(touched, &subject, object.as_deref());
    let new = add_claim_tx(tx, touched, by)?;
    // Claims `claim` itself replaced are now replaced by the user's newer version.
    tx.exec(
        "UPDATE claim_replacements SET replaced_by = ?2 WHERE replaced_by = ?1",
        params![claim, new],
        "chain replacements",
    )?;
    tx.exec(
        "INSERT INTO claim_replacements (claim_id, replaced_by) VALUES (?1, ?2)",
        params![claim, new],
        "record replacement",
    )?;
    tx.exec(
        "INSERT OR IGNORE INTO claim_replacement_evidence (claim_id, source, key, quote, item_hash)
         SELECT e.claim_id, e.source, e.key, coalesce(e.quote, ''),
                (SELECT i.content_hash FROM items i WHERE i.source = e.source AND i.key = e.key)
         FROM claim_evidence e WHERE e.claim_id = ?1",
        [claim],
        "record replaced evidence",
    )?;
    Ok(new)
}

fn add_evidence_tx(
    tx: &Connection,
    touched: &mut Touched,
    claim: ClaimId,
    ev: &Evidence,
) -> Result<()> {
    let (subject, object, status) = claim_ends(tx, claim)?;
    let now = touched.now().to_owned();
    insert_evidence(tx, claim, ev, &now)?;
    // A claim the user replaced stays superseded while all its evidence is remembered (decided
    // on the whole set, so the order evidence arrives in does not matter), unless its
    // retraction left a live twin (the user's own claim) to fold into.
    let replaced =
        matches!(status.as_str(), "retracted" | "superseded") && has_replacement(tx, claim)?;
    let stays = replaced
        && all_evidence_remembered(tx, claim)?
        && !(status == "retracted" && has_live_twin(tx, claim)?);
    let revived = !stays && (status == "retracted" || replaced);
    let next = if stays {
        "superseded"
    } else if revived {
        "active"
    } else {
        status.as_str()
    };
    tx.exec(
        "UPDATE claims SET updated_at = ?2, status = ?3 WHERE id = ?1",
        params![claim, now, next],
        "update claim",
    )?;
    touch_ends(touched, &subject, object.as_deref());
    if revived {
        forget_user_replaced(tx, claim)?;
        fold_tx(tx, claim, None, &now)?;
    }
    Ok(())
}

fn set_claim_status_tx(
    tx: &Connection,
    touched: &mut Touched,
    claim: ClaimId,
    status: ClaimStatus,
) -> Result<()> {
    let (subject, object, _) = claim_ends(tx, claim)?;
    if status == ClaimStatus::Active {
        let has_evidence: bool = tx
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM claim_evidence WHERE claim_id = ?1)")
            .db("prepare evidence check")?
            .query_row([claim], |r| r.get(0))
            .db("check evidence")?;
        if !has_evidence {
            return Err(Error::Invalid(
                "a claim without evidence cannot be active".into(),
            ));
        }
    }
    let now = touched.now().to_owned();
    tx.exec(
        "UPDATE claims SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![claim, status_str(status), now],
        "set claim status",
    )?;
    touch_ends(touched, &subject, object.as_deref());
    if status == ClaimStatus::UserAsserted {
        record_assertion(tx, claim, &now)?;
    }
    let survivor = if status.is_live() {
        forget_user_replaced(tx, claim)?;
        fold_tx(tx, claim, status.is_user_decision().then_some(status), &now)?
    } else {
        claim
    };
    // Keyed on the survivor's final status: a user's replacement that is no longer current
    // frees what it replaced.
    release_if_not_current(tx, survivor)?;
    Ok(())
}

fn update_claim_tx(
    tx: &Connection,
    touched: &mut Touched,
    claim: ClaimId,
    patch: &ClaimPatch,
) -> Result<()> {
    nugget_graph::mem::validate_patch(patch)?;
    let (subject, object, _) = claim_ends(tx, claim)?;
    let time = |v: Option<Option<OffsetDateTime>>| -> Result<(bool, Option<String>)> {
        Ok(match v {
            None => (false, None),
            Some(t) => (true, t.map(format_time).transpose()?),
        })
    };
    let (set_from, from) = time(patch.valid_from)?;
    let (set_to, to) = time(patch.valid_to)?;
    tx.exec(
        "UPDATE claims SET confidence = coalesce(?2, confidence), text = coalesce(?3, text),
             valid_from = CASE WHEN ?4 THEN ?5 ELSE valid_from END,
             valid_to = CASE WHEN ?6 THEN ?7 ELSE valid_to END,
             updated_at = ?8
         WHERE id = ?1",
        params![
            claim,
            patch.confidence.map(f64::from),
            patch.text.as_deref().map(str::trim),
            set_from,
            from,
            set_to,
            to,
            touched.now()
        ],
        "update claim",
    )?;
    touch_ends(touched, &subject, object.as_deref());
    Ok(())
}

/// Fold `claim` (just made live) and every live claim with the same triple into the user's own
/// claim (the oldest of them), else the oldest one: evidence moves, confidence is the maximum,
/// the status is `explicit` or else the strongest. Returns the survivor.
fn fold_tx(
    tx: &Connection,
    claim: ClaimId,
    explicit: Option<ClaimStatus>,
    now: &str,
) -> Result<ClaimId> {
    let group: Vec<(ClaimId, f64, String)> = {
        tx.rows(
            &format!(
                "SELECT o.id, o.confidence, o.status FROM claims c JOIN claims o
                   ON o.subject = c.subject AND o.predicate = c.predicate
                  AND o.object_entity IS c.object_entity AND o.object_literal IS c.object_literal
                 WHERE c.id = ?1 AND c.{LIVE} AND o.{LIVE}
                 ORDER BY o.status <> 'user_asserted', o.id"
            ),
            [claim],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            "read fold group",
        )?
    };
    if group.len() < 2 {
        return Ok(claim);
    }
    let keep = group[0].0;
    let mut status = status_from_str(&group[0].2)?;
    let mut confidence = group[0].1;
    for (gone, conf, st) in &group[1..] {
        status = status.strongest(status_from_str(st)?);
        confidence = confidence.max(*conf);
        move_evidence(tx, keep, *gone)?;
        tx.exec(
            "INSERT INTO claim_assertions (claim_id, asserted_at)
             SELECT ?1, asserted_at FROM claim_assertions WHERE claim_id = ?2
             ON CONFLICT (claim_id) DO UPDATE
                 SET asserted_at = max(asserted_at, excluded.asserted_at)",
            params![keep, gone],
            "move assertion time",
        )?;
        tx.exec(
            "UPDATE claim_replacements SET replaced_by = ?1 WHERE replaced_by = ?2",
            params![keep, gone],
            "move replacement links",
        )?;
        tx.exec(
            "DELETE FROM claims WHERE id = ?1",
            [gone],
            "drop folded claim",
        )?;
    }
    let status = explicit.unwrap_or(status);
    tx.exec(
        "UPDATE claims SET status = ?2, confidence = ?3, updated_at = ?4 WHERE id = ?1",
        params![keep, status_str(status), confidence, now],
        "update fold survivor",
    )?;
    forget_user_replaced(tx, keep)?;
    Ok(keep)
}

/// Copy `gone`'s evidence onto `keep` (duplicates ignored).
fn move_evidence(tx: &Connection, keep: ClaimId, gone: ClaimId) -> Result<()> {
    tx.exec(
        "INSERT OR IGNORE INTO claim_evidence (claim_id, source, key, chunk_id, quote,
                                               created_at)
         SELECT ?1, source, key, chunk_id, quote, created_at
         FROM claim_evidence WHERE claim_id = ?2 ORDER BY id",
        params![keep, gone],
        "move folded evidence",
    )?;
    Ok(())
}

fn add_mention_tx(tx: &Connection, touched: &mut Touched, m: &Mention) -> Result<()> {
    let entity = live(tx, &m.entity)?;
    tx.exec(
        "INSERT INTO mentions (entity_id, source, key, chunk_id, surface, confidence,
                               created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT DO UPDATE SET confidence = max(confidence, excluded.confidence)",
        params![
            entity,
            m.item.source.as_str(),
            m.item.key.as_str(),
            m.chunk_id,
            m.surface,
            f64::from(m.confidence),
            touched.now()
        ],
        "insert mention",
    )?;
    touched.add(&entity);
    Ok(())
}

fn get_claim_conn(c: &Connection, id: ClaimId) -> Result<Option<Claim>> {
    let row = c
        .prepare_cached(&format!(
            "SELECT {CLAIM_COLUMNS} FROM claims c WHERE c.id = ?1"
        ))
        .db("prepare get_claim")?
        .query_row([id], read_claim_row)
        .optional()
        .db("read claim")?;
    row.map(|r| r.into_claim(evidence_of(c, id)?)).transpose()
}

fn split_tx(
    tx: &Connection,
    touched: &mut Touched,
    entity: &EntityId,
    claims: &[ClaimId],
    new: &NewEntity,
) -> Result<String> {
    let from = live(tx, entity)?;
    let mut ids: Vec<ClaimId> = claims.to_vec();
    ids.sort_unstable();
    ids.dedup();
    for &id in &ids {
        let (subject, object, _) = claim_ends(tx, id)?;
        if subject != from && object.as_deref() != Some(from.as_str()) {
            return Err(Error::Invalid(format!(
                "claim {id} does not reference entity {from}"
            )));
        }
    }
    let to = create_entity_tx(tx, touched, new, Some(&from))?;
    let now = touched.now().to_owned();

    // Aliases named by `new` leave `from`; their provenance (and that of moved identifiers)
    // follows them.
    let mut norms: Vec<String> = new
        .aliases
        .iter()
        .chain(std::iter::once(&new.name))
        .map(|a| normalize_name(a))
        .filter(|n| !n.is_empty())
        .collect();
    norms.sort();
    norms.dedup();
    for norm in &norms {
        let n = tx.exec(
            "DELETE FROM entity_aliases WHERE entity_id = ?1 AND is_name = 0
                 AND alias_norm = ?2",
            params![from, norm],
            "move aliases",
        )?;
        if n > 0 {
            tx.exec(
                "UPDATE OR IGNORE entity_detail_sources SET entity_id = ?2
                 WHERE entity_id = ?1 AND kind = 'alias' AND value = ?3",
                params![from, to, norm],
                "move alias provenance",
            )?;
        }
    }
    for n in new.identifiers.iter().map(Identifier::normalized) {
        tx.exec(
            "UPDATE OR IGNORE entity_detail_sources SET entity_id = ?2
             WHERE entity_id = ?1 AND kind = 'identifier' AND scheme = ?3 AND value = ?4",
            params![from, to, n.scheme, n.value],
            "move identifier provenance",
        )?;
    }

    // Claims.
    for &id in &ids {
        let (subject, object): (String, Option<String>) = tx
            .prepare_cached(
                "UPDATE claims SET
                     subject = CASE WHEN subject = ?2 THEN ?3 ELSE subject END,
                     object_entity = CASE WHEN object_entity = ?2 THEN ?3 ELSE object_entity END,
                     updated_at = ?4
                 WHERE id = ?1 RETURNING subject, object_entity",
            )
            .db("prepare claim move")?
            .query_row(params![id, from, to, now], |r| Ok((r.get(0)?, r.get(1)?)))
            .db("move claim")?;
        touch_ends(touched, &subject, object.as_deref());
    }

    // Mentions follow the items that now support only the new entity.
    let movable: Vec<(String, String)> = {
        tx.rows(
            "SELECT ev.source, ev.key FROM claim_evidence ev JOIN claims c ON c.id = ev.claim_id
                 WHERE c.subject = ?1 OR c.object_entity = ?1
                 EXCEPT
                 SELECT ev.source, ev.key FROM claim_evidence ev JOIN claims c ON c.id = ev.claim_id
                 WHERE c.subject = ?2 OR c.object_entity = ?2",
            params![to, from],
            |r| Ok((r.get(0)?, r.get(1)?)),
            "read movable mentions",
        )?
    };
    for (source, key) in &movable {
        tx.exec(
            "UPDATE mentions SET entity_id = ?2 WHERE entity_id = ?1 AND source = ?3 AND key = ?4",
            params![from, to, source, key],
            "move mentions",
        )?;
    }
    for &id in &ids {
        fold_tx(tx, id, None, &now)?;
    }

    // Ids merged into `from` under the new entity's name (or, failing that, the one id merged
    // under one of its aliases) now resolve to it.
    let own = normalize_name(&entity_name(tx, &from)?);
    let merged: Vec<(String, String)> = {
        tx.rows(
            "SELECT id, name FROM entities WHERE merged_into = ?1 ORDER BY id",
            [&from],
            |r| Ok((r.get(0)?, r.get(1)?)),
            "read merged ids",
        )?
    };
    let merged: Vec<(String, String)> = merged
        .into_iter()
        .map(|(id, name)| (id, normalize_name(&name)))
        .filter(|(_, n)| *n != own)
        .collect();
    let aliases: HashSet<String> = norms.iter().cloned().collect();
    for id in nugget_graph::mem::split_repoint(&merged, &normalize_name(&new.name), &aliases) {
        tx.exec(
            "UPDATE entities SET merged_into = ?2 WHERE id = ?1",
            params![id, to],
            "re-point merged id",
        )?;
        touched.add(&id);
    }
    touched.add(&from);
    touched.add(&to);
    Ok(to)
}

/// SQL condition (over `entities e`) for a live orphan entity (see `prune_orphans`).
const ORPHAN: &str = "e.merged_into IS NULL AND e.deleted_at IS NULL
    AND coalesce(json_type(e.attributes, '$.is_me'), '') <> 'true'
    AND coalesce(json_type(e.attributes, '$.user_asserted'), '') <> 'true'
    AND NOT EXISTS (SELECT 1 FROM mentions m WHERE m.entity_id = e.id)
    AND NOT EXISTS (SELECT 1 FROM claims c WHERE c.subject = e.id AND c.status <> 'retracted')
    AND NOT EXISTS (SELECT 1 FROM claims c WHERE c.object_entity = e.id
                                             AND c.status <> 'retracted')";

fn prune_tx(
    tx: &Connection,
    touched: &mut Touched,
    among: Option<&[EntityId]>,
) -> Result<Vec<EntityId>> {
    let orphans: Vec<String> = match among {
        None => tx.rows(
            &format!("SELECT e.id FROM entities e WHERE {ORPHAN} ORDER BY e.id"),
            [],
            |r| r.get(0),
            "read orphans",
        )?,
        Some(ids) => {
            let mut live_ids: Vec<String> = Vec::new();
            for id in ids {
                if let Some(r) = resolve(tx, id.as_str())? {
                    live_ids.push(r);
                }
            }
            live_ids.sort();
            live_ids.dedup();
            let mut stmt = tx
                .prepare_cached(&format!(
                    "SELECT EXISTS (SELECT 1 FROM entities e WHERE e.id = ?1 AND {ORPHAN})"
                ))
                .db("prepare orphan check")?;
            let mut out = Vec::new();
            for id in live_ids {
                let orphan: bool = stmt.query_row([&id], |r| r.get(0)).db("check orphan")?;
                if orphan {
                    out.push(id);
                }
            }
            out
        }
    };
    for id in &orphans {
        {
            let mut stmt = tx
                .prepare_cached(
                    "DELETE FROM claims WHERE subject = ?1 OR object_entity = ?1
                     RETURNING subject, object_entity",
                )
                .db("prepare orphan claims delete")?;
            let rows = stmt
                .query_map([id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })
                .db("delete orphan claims")?;
            for row in rows {
                let (s, o) = row.db("read deleted orphan claim")?;
                touch_ends(touched, &s, o.as_deref());
            }
        }
        for sql in [
            "DELETE FROM entity_aliases WHERE entity_id = ?1",
            "DELETE FROM entity_identifiers WHERE entity_id = ?1",
            "DELETE FROM entity_detail_sources WHERE entity_id = ?1",
        ] {
            tx.exec(sql, [id], "clean up orphan")?;
        }
        tx.exec(
            "UPDATE entities SET deleted_at = ?2 WHERE id = ?1",
            params![id, touched.now()],
            "tombstone orphan",
        )?;
        touched.add(id);
    }
    Ok(orphans.into_iter().map(EntityId::new).collect())
}

fn apply_op_tx(tx: &Connection, touched: &mut Touched, op: &GraphOp) -> Result<BatchResult> {
    Ok(match op {
        GraphOp::CreateEntity { entity } => {
            BatchResult::Entity(EntityId::new(create_entity_tx(tx, touched, entity, None)?))
        }
        GraphOp::UpdateEntity { entity, patch } => {
            update_entity_tx(tx, touched, entity, patch)?;
            BatchResult::Done
        }
        GraphOp::AddAlias { entity, alias } => {
            add_alias_tx(tx, touched, entity, alias)?;
            BatchResult::Done
        }
        GraphOp::AddIdentifier { entity, identifier } => {
            add_identifier_tx(tx, touched, entity, identifier)?;
            BatchResult::Done
        }
        GraphOp::AddDetailSource {
            entity,
            detail,
            item,
        } => {
            add_detail_source_tx(tx, touched, entity, detail, item)?;
            BatchResult::Done
        }
        GraphOp::AddClaim { claim } => BatchResult::Claim(add_claim_tx(tx, touched, claim)?),
        GraphOp::AddEvidence { claim, evidence } => {
            add_evidence_tx(tx, touched, *claim, evidence)?;
            BatchResult::Done
        }
        GraphOp::SetClaimStatus { claim, status } => {
            set_claim_status_tx(tx, touched, *claim, *status)?;
            BatchResult::Done
        }
        GraphOp::UpdateClaim { claim, patch } => {
            update_claim_tx(tx, touched, *claim, patch)?;
            BatchResult::Done
        }
        GraphOp::AddMention { mention } => {
            add_mention_tx(tx, touched, mention)?;
            BatchResult::Done
        }
        GraphOp::RetractItem { item } => {
            BatchResult::Claims(retract_tx(tx, ItemScope::Item(item), touched)?)
        }
    })
}

// ------------------------------------------------------------------ the contract

impl GraphStore for SqliteGraph {
    fn create_entity(&self, new: &NewEntity) -> Result<EntityId> {
        self.write("create_entity", |tx, touched| {
            create_entity_tx(tx, touched, new, None).map(EntityId::new)
        })
    }

    fn get_entity(&self, id: &EntityId) -> Result<Option<Entity>> {
        self.read(|c| match resolve(c, id.as_str())? {
            Some(r) => load_entity(c, &r),
            None => Ok(None),
        })
    }

    fn update_entity(&self, id: &EntityId, patch: &EntityPatch) -> Result<Entity> {
        let id = self.write("update_entity", |tx, touched| {
            update_entity_tx(tx, touched, id, patch)
        })?;
        self.read(|c| load_entity(c, &id))?
            .ok_or_else(|| Error::NotFound(format!("entity {id}")))
    }

    fn add_alias(&self, id: &EntityId, alias: &str) -> Result<()> {
        self.write("add_alias", |tx, touched| {
            add_alias_tx(tx, touched, id, alias)
        })
    }

    fn add_identifier(&self, id: &EntityId, ident: &Identifier) -> Result<()> {
        self.write("add_identifier", |tx, touched| {
            add_identifier_tx(tx, touched, id, ident)
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
            c.rows(
                "SELECT DISTINCT a.entity_id
                     FROM entity_aliases a JOIN entities e ON e.id = a.entity_id
                     WHERE a.alias_norm = ?1 AND e.merged_into IS NULL AND e.deleted_at IS NULL
                       AND (?2 IS NULL OR e.kind = ?2)
                     ORDER BY a.entity_id",
                params![want, kind.map(EntityKind::as_str)],
                |r| r.get::<_, String>(0).map(EntityId::new),
                "read find_by_name",
            )
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
        let mut conds = vec![
            "e.merged_into IS NULL".to_owned(),
            "e.deleted_at IS NULL".to_owned(),
        ];
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

    fn top_entities(
        &self,
        kinds: &[EntityKind],
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EntitySummary>> {
        // `entity_stats` (schema v15) holds the counts, kept exact by triggers.
        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        let mut kind_cond = String::new();
        if !kinds.is_empty() {
            let mut seen = Vec::new();
            for k in kinds {
                if !seen.contains(k) {
                    seen.push(*k);
                    args.push(Box::new(k.as_str()));
                }
            }
            let marks: Vec<String> = (1..=args.len()).map(|i| format!("?{i}")).collect();
            kind_cond = format!(" AND e.kind IN ({})", marks.join(", "));
        }
        args.push(Box::new(sql_limit(limit)));
        let lim = args.len();
        args.push(Box::new(i64::try_from(offset).unwrap_or(i64::MAX)));
        let off = args.len();
        let sql = format!(
            "SELECT e.id, e.kind, e.name, e.updated_at, s.claims, s.mentions
             FROM entity_stats s CROSS JOIN entities e ON e.id = s.entity_id
             WHERE e.merged_into IS NULL AND e.deleted_at IS NULL{kind_cond}
             ORDER BY s.claims DESC, s.mentions DESC, e.name, e.id
             LIMIT ?{lim} OFFSET ?{off}"
        );
        self.read(|c| {
            let mut stmt = c.prepare_cached(&sql).db("prepare top_entities")?;
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
                .db("query top_entities")?;
            let mut out = Vec::new();
            for row in rows {
                let (id, kind, name, updated_at, claims, mentions) = row.db("read top_entities")?;
                out.push(EntitySummary {
                    id: EntityId::new(id),
                    kind: kind_from_str(&kind)?,
                    name,
                    claims: claims.max(0) as u64,
                    mentions: mentions.max(0) as u64,
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
        self.write("add_claim", |tx, touched| add_claim_tx(tx, touched, new))
    }

    fn get_claim(&self, id: ClaimId) -> Result<Option<Claim>> {
        self.read(|c| get_claim_conn(c, id))
    }

    fn add_evidence(&self, claim: ClaimId, ev: &Evidence) -> Result<()> {
        self.write("add_evidence", |tx, touched| {
            add_evidence_tx(tx, touched, claim, ev)
        })
    }

    fn set_claim_status(&self, claim: ClaimId, status: ClaimStatus) -> Result<()> {
        self.write("set_claim_status", |tx, touched| {
            set_claim_status_tx(tx, touched, claim, status)
        })
    }

    fn supersede_by_user(&self, claim: ClaimId, by: &NewClaim) -> Result<ClaimId> {
        validate_claim(by)?;
        self.write("supersede_by_user", |tx, touched| {
            supersede_by_user_tx(tx, touched, claim, by)
        })
    }

    fn replaced_by_user(&self, claim: ClaimId, evidence: &[Evidence]) -> Result<bool> {
        self.read(|c| is_replaced_by_user(c, claim, evidence))
    }

    fn has_user_replacement(&self, claim: ClaimId) -> Result<bool> {
        self.read(|c| has_replacement(c, claim))
    }

    fn asserted_at(&self, claim: ClaimId) -> Result<Option<OffsetDateTime>> {
        self.read(|c| {
            c.prepare_cached("SELECT asserted_at FROM claim_assertions WHERE claim_id = ?1")
                .db("prepare assertion lookup")?
                .query_row([claim], |r| r.get::<_, String>(0))
                .optional()
                .db("read assertion time")?
                .map(|t| parse_time(&t))
                .transpose()
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
                c.rows(
                    &format!("SELECT {CLAIM_COLUMNS} FROM claims c WHERE {filter} ORDER BY c.id"),
                    [&id],
                    read_claim_row,
                    "read claims_for",
                )?
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
        self.write("add_mention", |tx, touched| add_mention_tx(tx, touched, m))
    }

    fn mentions_for(&self, entity: &EntityId, limit: usize) -> Result<Vec<Mention>> {
        self.read(|c| {
            let Some(id) = resolve(c, entity.as_str())? else {
                return Ok(Vec::new());
            };
            c.rows(
                "SELECT source, key, chunk_id, surface, confidence FROM mentions
                     WHERE entity_id = ?1 ORDER BY id LIMIT ?2",
                params![id, sql_limit(limit)],
                |r| {
                    Ok(Mention {
                        entity: EntityId::new(id.clone()),
                        item: item_ref(r.get(0)?, r.get(1)?),
                        chunk_id: r.get(2)?,
                        surface: r.get(3)?,
                        confidence: r.get::<_, f64>(4)? as f32,
                    })
                },
                "read mentions_for",
            )
        })
    }

    fn retract_item(&self, item: &ItemRef) -> Result<Vec<ClaimId>> {
        self.write("retract_item", |tx, touched| {
            retract_tx(tx, ItemScope::Item(item), touched)
        })
    }

    fn neighbourhood(&self, center: &EntityId, depth: u32, limit: usize) -> Result<GraphView> {
        let limit = if limit == 0 {
            MAX_NEIGHBOURHOOD_NODES
        } else {
            limit.min(MAX_NEIGHBOURHOOD_NODES)
        };
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
        // `count(*)` over every mention (a million rows at 250k items) decrypts its whole index on
        // each call: ~100 ms, polled by every open knowledge page.
        let db = self.store.db().clone();
        self.read(|c| {
            let changes = c.total_changes();
            let mut cache = db
                .graph_counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((at, when, [entities, claims, mentions])) = *cache
                && at == changes
                && when.elapsed() < GRAPH_COUNTS_MAX_AGE
            {
                return Ok(GraphCounts {
                    entities,
                    claims,
                    mentions,
                });
            }
            let (entities, claims, mentions): (i64, i64, i64) = c
                .query_row(
                    &format!(
                        "SELECT (SELECT count(*) FROM entities
                                 WHERE merged_into IS NULL AND deleted_at IS NULL),
                                (SELECT count(*) FROM claims WHERE {ACTIVE}),
                                (SELECT count(*) FROM mentions)"
                    ),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .db("count graph")?;
            let counts = [entities as u64, claims as u64, mentions as u64];
            *cache = Some((changes, std::time::Instant::now(), counts));
            Ok(GraphCounts {
                entities: counts[0],
                claims: counts[1],
                mentions: counts[2],
            })
        })
    }

    fn update_claim(&self, claim: ClaimId, patch: &ClaimPatch) -> Result<Claim> {
        nugget_graph::mem::validate_patch(patch)?;
        self.write("update_claim", |tx, touched| {
            update_claim_tx(tx, touched, claim, patch)?;
            get_claim_conn(tx, claim)?.ok_or_else(|| Error::NotFound(format!("claim {claim}")))
        })
    }

    fn add_detail_source(
        &self,
        entity: &EntityId,
        detail: &EntityDetail,
        item: &ItemRef,
    ) -> Result<()> {
        self.write("add_detail_source", |tx, touched| {
            add_detail_source_tx(tx, touched, entity, detail, item)
        })
    }

    fn entity_provenance(&self, entity: &EntityId) -> Result<Option<EntityProvenance>> {
        self.read(|c| {
            let Some(id) = resolve(c, entity.as_str())? else {
                return Ok(None);
            };
            let Some(e) = load_entity(c, &id)? else {
                return Ok(None);
            };
            let mut sources: HashMap<(String, String, String), Vec<ItemRef>> = HashMap::new();
            {
                let mut stmt = c
                    .prepare_cached(
                        "SELECT kind, scheme, value, source, key FROM entity_detail_sources
                         WHERE entity_id = ?1 ORDER BY id",
                    )
                    .db("prepare provenance")?;
                let rows = stmt
                    .query_map([&id], |r| {
                        Ok((
                            (r.get(0)?, r.get(1)?, r.get(2)?),
                            item_ref(r.get(3)?, r.get(4)?),
                        ))
                    })
                    .db("query provenance")?;
                for row in rows {
                    let (k, item) = row.db("read provenance")?;
                    sources.entry(k).or_default().push(item);
                }
            }
            let items = |d: EntityDetail| {
                let (kind, scheme, value) = d.key();
                sources
                    .get(&(kind.to_owned(), scheme, value))
                    .cloned()
                    .unwrap_or_default()
            };
            Ok(Some(EntityProvenance {
                aliases: e
                    .aliases
                    .iter()
                    .map(|a| (a.clone(), items(EntityDetail::Alias(a.clone()))))
                    .collect(),
                identifiers: e
                    .identifiers
                    .iter()
                    .map(|i| (i.clone(), items(EntityDetail::Identifier(i.clone()))))
                    .collect(),
            }))
        })
    }

    fn split(&self, entity: &EntityId, claims: &[ClaimId], new: &NewEntity) -> Result<EntityId> {
        self.write("split", |tx, touched| {
            split_tx(tx, touched, entity, claims, new).map(EntityId::new)
        })
    }

    fn prune_orphans(&self, among: Option<&[EntityId]>) -> Result<Vec<EntityId>> {
        self.write("prune_orphans", |tx, touched| prune_tx(tx, touched, among))
    }

    fn entities_supported_by(&self, source: &SourceId) -> Result<Vec<EntityId>> {
        // One query, driven by the (source, key) indexes on mentions and claim_evidence.
        self.read(|c| {
            c.rows(
                &format!(
                    "SELECT e.id FROM entities e
                     JOIN (SELECT entity_id AS id FROM mentions WHERE source = ?1
                           UNION
                           SELECT c.subject FROM claim_evidence ev
                             JOIN claims c ON c.id = ev.claim_id
                             WHERE ev.source = ?1 AND c.{ACTIVE}
                           UNION
                           SELECT c.object_entity FROM claim_evidence ev
                             JOIN claims c ON c.id = ev.claim_id
                             WHERE ev.source = ?1 AND c.{ACTIVE}
                               AND c.object_entity IS NOT NULL) s ON s.id = e.id
                     WHERE e.merged_into IS NULL AND e.deleted_at IS NULL
                     ORDER BY e.id"
                ),
                [source.as_str()],
                |r| Ok(EntityId::new(r.get::<_, String>(0)?)),
                "read entities_supported_by",
            )
        })
    }

    fn apply_batch(&self, batch: &GraphBatch) -> Result<Vec<BatchResult>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        self.write("apply_batch", |tx, touched| {
            batch
                .ops
                .iter()
                .map(|op| apply_op_tx(tx, touched, op))
                .collect()
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
        tx.rows(
            "SELECT alias FROM entity_aliases WHERE entity_id = ?1 ORDER BY is_name DESC, id",
            [&from],
            |r| r.get(0),
            "read merge names",
        )?
    };
    tx.execute("DELETE FROM entity_aliases WHERE entity_id = ?1", [&from])
        .db("drop merged names")?;
    for n in &names {
        add_alias_row(tx, &into, &into_norm, n)?;
    }

    // Identifiers move as they are (each belongs to one entity, so no conflicts), and so does
    // the provenance of every alias and identifier.
    tx.execute(
        "UPDATE entity_identifiers SET entity_id = ?1 WHERE entity_id = ?2",
        params![into, from],
    )
    .db("move identifiers")?;
    tx.execute(
        "INSERT OR IGNORE INTO entity_detail_sources
             (entity_id, kind, scheme, value, source, key, created_at)
         SELECT ?1, kind, scheme, value, source, key, created_at
         FROM entity_detail_sources WHERE entity_id = ?2 ORDER BY id",
        params![into, from],
    )
    .db("move detail provenance")?;
    tx.execute(
        "DELETE FROM entity_detail_sources WHERE entity_id = ?1",
        [&from],
    )
    .db("drop moved detail provenance")?;

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

/// Fold live claims of `entity` that share a triple into the oldest one: evidence moves, confidence
/// is the maximum, and the survivor takes the strongest status ([`ClaimStatus::strongest`]).
fn fold_duplicate_claims(tx: &Connection, entity: &str, now: &str) -> Result<()> {
    let ids: Vec<ClaimId> = {
        tx.rows(
            &format!(
                "SELECT id FROM claims WHERE (subject = ?1 OR object_entity = ?1) AND {LIVE}
                 ORDER BY id"
            ),
            [entity],
            |r| r.get(0),
            "read claim fold",
        )?
    };
    let mut gone: HashSet<ClaimId> = HashSet::new();
    for id in ids {
        if gone.contains(&id) {
            continue;
        }
        // Ascending ids: `id` and its twins fold together here; whichever survives, none of
        // them needs another pass.
        let before: Vec<ClaimId> = {
            tx.rows(
                &format!(
                    "SELECT o.id FROM claims c JOIN claims o
                       ON o.subject = c.subject AND o.predicate = c.predicate
                      AND o.object_entity IS c.object_entity
                      AND o.object_literal IS c.object_literal
                     WHERE c.id = ?1 AND o.id <> ?1 AND o.{LIVE}"
                ),
                [id],
                |r| r.get(0),
                "read fold twins",
            )?
        };
        if before.is_empty() {
            continue;
        }
        fold_tx(tx, id, None, now)?;
        gone.extend(before);
    }
    Ok(())
}

// ------------------------------------------------------------------ neighbourhood

/// Breadth-first over active entity→entity claims, in the same order as `MemGraph`: nodes in BFS
/// order, each node's claims by id.
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

    /// `entity_stats` against counts recomputed as `list_entities` defines them, for every
    /// entity row (tombstones included), and `top_entities` against the scan-and-sort default.
    fn assert_counts_exact(g: &SqliteGraph, step: &str) {
        let read = |sql: &str| -> Vec<(String, i64, i64)> {
            g.read(|c| c.rows(sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)), "read"))
                .unwrap()
        };
        let kept = read(
            "SELECT e.id, s.claims, s.mentions FROM entities e
             LEFT JOIN entity_stats s ON s.entity_id = e.id ORDER BY e.id",
        );
        let actual = read(&format!(
            "SELECT e.id,
                    (SELECT count(*) FROM claims c WHERE c.subject = e.id AND c.{ACTIVE})
                  + (SELECT count(*) FROM claims c WHERE c.object_entity = e.id
                        AND c.subject <> e.id AND c.{ACTIVE}),
                    (SELECT count(*) FROM mentions m WHERE m.entity_id = e.id)
             FROM entities e ORDER BY e.id"
        ));
        assert_eq!(kept, actual, "{step}");
        let strays = read(
            "SELECT entity_id, claims, mentions FROM entity_stats
             WHERE entity_id NOT IN (SELECT id FROM entities)",
        );
        assert!(strays.is_empty(), "{step}: {strays:?}");
        let kind_sets: [&[EntityKind]; 4] = [
            &[],
            &[EntityKind::Person],
            &[EntityKind::Person, EntityKind::Organization],
            &[EntityKind::Topic],
        ];
        for kinds in kind_sets {
            for (limit, offset) in [(0, 0), (3, 0), (2, 1), (50, 4), (1, 100)] {
                assert_eq!(
                    g.top_entities(kinds, limit, offset).unwrap(),
                    nugget_graph::top_entities_scan(g, kinds, limit, offset).unwrap(),
                    "{step}: {kinds:?} {limit} {offset}"
                );
            }
        }
    }

    #[test]
    fn entity_counts_stay_exact_through_every_write_path() {
        let store = Store::open_in_memory().unwrap();
        let g = store.graph();
        let item = |k: &str| ItemRef {
            source: SourceId::new(if k.starts_with('m') { "mail" } else { "files" }),
            key: nugget_core::ids::ItemKey::new(k),
        };
        let ev = |k: &str| Evidence {
            item: item(k),
            chunk_id: Some(1),
            quote: Some(format!("quote {k}")),
        };
        let claim = |s: &EntityId, p: &str, o: ClaimObject, evs: &[&str]| NewClaim {
            subject: s.clone(),
            predicate: p.into(),
            object: o,
            text: format!("{s} {p}"),
            confidence: 0.5,
            status: ClaimStatus::Active,
            evidence: evs.iter().map(|k| ev(k)).collect(),
            valid_from: None,
            valid_to: None,
        };
        let mention = |e: &EntityId, k: &str, surface: &str| Mention {
            entity: e.clone(),
            item: item(k),
            chunk_id: Some(1),
            surface: surface.into(),
            confidence: 0.7,
        };
        let ent = |kind, name: &str| g.create_entity(&NewEntity::new(kind, name)).unwrap();
        let obj = |e: &EntityId| ClaimObject::Entity(e.clone());
        let lit = |s: &str| ClaimObject::Literal(s.into());

        let sam = ent(EntityKind::Person, "Sam");
        let ana = ent(EntityKind::Person, "Ana");
        let acme = ent(EntityKind::Organization, "Acme");
        let acme2 = ent(EntityKind::Organization, "ACME Inc");
        let topic = ent(EntityKind::Topic, "Budget");
        let _idle = ent(EntityKind::Person, "Idle");
        assert_counts_exact(&g, "created");

        for (e, k, s) in [
            (&sam, "f1", "Sam"),
            (&sam, "m1", "Sam"),
            (&sam, "m1", "Sammy"),
            (&ana, "f1", "Ana"),
            (&acme, "f2", "Acme"),
            (&acme2, "m2", "ACME"),
            (&topic, "m2", "budget"),
        ] {
            g.add_mention(&mention(e, k, s)).unwrap();
        }
        g.add_mention(&mention(&sam, "f1", "Sam")).unwrap();
        assert_counts_exact(&g, "mentioned");

        let works = g
            .add_claim(&claim(&sam, "works_at", obj(&acme), &["f1"]))
            .unwrap();
        let likes = g
            .add_claim(&claim(&sam, "likes", lit("tea"), &["m1"]))
            .unwrap();
        let knows = g
            .add_claim(&claim(&ana, "knows", obj(&sam), &["f1", "m1"]))
            .unwrap();
        let _self_loop = g
            .add_claim(&claim(&sam, "reports_to", obj(&sam), &["f2"]))
            .unwrap();
        let ana_acme = g
            .add_claim(&claim(&ana, "works_at", obj(&acme2), &["m2"]))
            .unwrap();
        let _ = g
            .add_claim(&claim(&acme2, "about", obj(&topic), &["m2"]))
            .unwrap();
        g.add_claim(&claim(&sam, "works_at", obj(&acme), &["f2"]))
            .unwrap();
        assert_counts_exact(&g, "claimed");

        let mut by = claim(&sam, "likes", lit("coffee"), &[]);
        by.status = ClaimStatus::UserAsserted;
        g.supersede_by_user(likes, &by).unwrap();
        assert_counts_exact(&g, "superseded by user");

        g.set_claim_status(knows, ClaimStatus::Rejected).unwrap();
        assert_counts_exact(&g, "rejected");
        g.set_claim_status(knows, ClaimStatus::Active).unwrap();
        g.set_claim_status(works, ClaimStatus::Retracted).unwrap();
        assert_counts_exact(&g, "retracted");
        g.add_claim(&claim(&sam, "works_at", obj(&acme), &["m1"]))
            .unwrap();
        g.add_evidence(works, &ev("m2")).unwrap();
        assert_counts_exact(&g, "re-activated and folded");
        g.update_claim(
            ana_acme,
            &ClaimPatch {
                confidence: Some(0.9),
                ..Default::default()
            },
        )
        .unwrap();
        g.update_entity(
            &acme2,
            &EntityPatch {
                name: Some("Acme Incorporated".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_counts_exact(&g, "updated");

        g.add_claim(&claim(&ana, "works_at", obj(&acme), &["f2"]))
            .unwrap();
        g.merge(&acme, &acme2).unwrap();
        assert_counts_exact(&g, "merged");

        let acme_claims: Vec<ClaimId> = g
            .claims_for(&acme, false)
            .unwrap()
            .into_iter()
            .filter(|c| c.predicate == "about")
            .map(|c| c.id)
            .collect();
        let split = g.split(
            &acme,
            &acme_claims,
            &NewEntity::new(EntityKind::Organization, "Acme Labs"),
        );
        split.unwrap();
        assert_counts_exact(&g, "split");

        let mut batch = GraphBatch::new();
        batch.ops.push(GraphOp::AddMention {
            mention: mention(&topic, "f3", "Budget"),
        });
        batch.ops.push(GraphOp::AddClaim {
            claim: claim(&topic, "owned_by", obj(&ana), &["f3"]),
        });
        batch.ops.push(GraphOp::RetractItem { item: item("f2") });
        g.apply_batch(&batch).unwrap();
        assert_counts_exact(&g, "batch");

        store.delete_item(&item("f1")).unwrap();
        assert_counts_exact(&g, "item forgotten");
        g.retract_item(&item("m1")).unwrap();
        assert_counts_exact(&g, "item retracted");
        store.delete_source(&SourceId::new("mail")).unwrap();
        assert_counts_exact(&g, "source deleted");
        let pruned = g.prune_orphans(None).unwrap();
        assert!(!pruned.is_empty());
        assert_counts_exact(&g, "pruned");

        g.write("raw", |tx, _| {
            tx.execute_batch(&format!(
                "UPDATE mentions SET entity_id = '{ana}' WHERE entity_id = '{topic}';
                 UPDATE claims SET object_entity = subject WHERE object_entity IS NOT NULL;
                 DELETE FROM claims WHERE id = (SELECT min(id) FROM claims);"
            ))
            .db("raw writes")
        })
        .unwrap();
        assert_counts_exact(&g, "raw writes");
        store
            .check_consistency()
            .map(|r| assert!(r.ok, "{:?}", r.problems))
            .unwrap();
    }
}
