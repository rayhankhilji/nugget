//! Data export: everything a user may want to take out of the store, in open formats.
//!
//! - [`Store::export_items`] visits every item (one [`ExportItem`] each, meant to be written as
//!   one JSON line) in rowid order, reading `page` items per query with keyset pagination, so
//!   memory stays bounded by the page size whatever the store holds.
//! - [`Store::export_graph_json`] streams the knowledge graph as one JSON document.
//! - [`Store::export_audit_csv`] and [`Store::export_egress_csv`] write the audit log and the
//!   egress ledger as RFC 4180 CSV.
//!
//! # Consistency
//!
//! Exporting a live store is allowed. Item export is **per-page consistent**: each page is read
//! by one SQL statement, so an item's row, its text and the chunks that text is rebuilt from all
//! come from one snapshot, but different pages may see different states. An item deleted before
//! its page is read is not exported, one inserted meanwhile is exported only if its rowid is past
//! the pages already read, and no item is ever exported twice. The graph export reads all three
//! sections in one read transaction (claims never name an entity missing from the document);
//! each CSV export is one statement.
//!
//! # CSV
//!
//! Cells are written losslessly: a cell holding `,`, `"`, CR or LF is quoted, with `"` doubled;
//! records end in CRLF; SQL NULL is an empty cell. Cells are **not** altered against "CSV
//! injection" (a leading `=`, `+`, `-`, `@`, tab or CR is kept as is): this is a data export
//! that must round-trip exactly, not a report for a spreadsheet. Open it in a spreadsheet with
//! formula evaluation off (import as text) if its contents are untrusted.

use std::io::{BufWriter, Write};

use nugget_core::{Error, Result};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Store;
use crate::error::DbExt;
use crate::items::decode_text_layout;

/// Version of the [`Store::export_graph_json`] document (its `format` field).
pub const GRAPH_EXPORT_FORMAT: u32 = 1;

/// One exported item: every column the store keeps for it, with its full text.
///
/// Dates are the stored RFC 3339 strings, unchanged. Source-specific details (author, sender,
/// participants, labels, ...) are inside `metadata`, which is the stored JSON verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportItem {
    /// Row id in this store (stable for the item's life in this store, not across stores).
    pub id: i64,
    /// Source (connector instance) id.
    pub source: String,
    /// The item's id within its source (the source's external id).
    pub key: String,
    pub title: Option<String>,
    /// MIME type of the original content, if known.
    pub mime: Option<String>,
    /// `metadata.url` when it is a string (copied for convenience; it stays in `metadata`).
    pub url: Option<String>,
    /// Modification time reported by the source.
    pub source_modified_at: Option<String>,
    /// When the store first indexed the item.
    pub created_at: String,
    /// When the store last changed the item.
    pub updated_at: String,
    pub content_hash: String,
    /// Connector-defined version token.
    pub version: Option<String>,
    /// Source metadata as stored. A stored value that is not valid JSON is exported as a JSON
    /// string holding it, so nothing is dropped.
    pub metadata: Value,
    /// The item's full text, as written by its connector (rebuilt from its chunks when stored
    /// that way).
    pub text: Option<String>,
    /// When the item has no text of its own: its chunk texts in order (omitted when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<String>,
}

/// What [`Store::export_graph_json`] wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphExportCounts {
    /// Entity rows, including merged-away and deleted tombstones.
    pub entities: u64,
    /// Claims of every status.
    pub claims: u64,
    /// Evidence rows across all claims.
    pub evidence: u64,
    pub mentions: u64,
}

/// One page of items, rowid order, with what their texts need: the `item_text` layout and,
/// when `items.text` is NULL, the chunks as a JSON array of `[ord, text]`.
const ITEMS_PAGE_SQL: &str = "SELECT i.id, i.source, i.key, i.title, i.mime, i.source_modified_at,
            i.created_at, i.updated_at, i.content_hash, i.version, i.metadata, i.text, t.layout,
            CASE WHEN i.text IS NULL THEN
                (SELECT json_group_array(json_array(c.ord, c.text))
                 FROM chunks c WHERE c.item_id = i.id)
            END
     FROM items i LEFT JOIN item_text t ON t.item_id = i.id
     WHERE i.id > ?1 ORDER BY i.id LIMIT ?2";

/// An item row as read, before its text is rebuilt (done off the read connection).
struct RawItem {
    id: i64,
    source: String,
    key: String,
    title: Option<String>,
    mime: Option<String>,
    source_modified_at: Option<String>,
    created_at: String,
    updated_at: String,
    content_hash: String,
    version: Option<String>,
    metadata: String,
    text: Option<String>,
    layout: Option<Vec<u8>>,
    chunks: Option<String>,
}

impl RawItem {
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            source: r.get(1)?,
            key: r.get(2)?,
            title: r.get(3)?,
            mime: r.get(4)?,
            source_modified_at: r.get(5)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
            content_hash: r.get(8)?,
            version: r.get(9)?,
            metadata: r.get(10)?,
            text: r.get(11)?,
            layout: r.get(12)?,
            chunks: r.get(13)?,
        })
    }

    fn into_export(self) -> Result<ExportItem> {
        let mut chunks: Vec<(u64, String)> = match &self.chunks {
            Some(json) => serde_json::from_str(json)?,
            None => Vec::new(),
        };
        chunks.sort_unstable_by_key(|(ord, _)| *ord);
        let (text, chunks) = match (self.text, &self.layout) {
            (Some(text), _) => (Some(text), Vec::new()),
            (None, Some(layout)) => {
                let text = decode_text_layout(self.id, layout, |ord| {
                    chunks
                        .binary_search_by_key(&ord, |(o, _)| *o)
                        .ok()
                        .map(|i| chunks[i].1.as_str())
                })?;
                (Some(text), Vec::new())
            }
            (None, None) => (None, chunks.into_iter().map(|(_, t)| t).collect()),
        };
        let metadata = serde_json::from_str(&self.metadata).unwrap_or(Value::String(self.metadata));
        let url = metadata
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(ExportItem {
            id: self.id,
            source: self.source,
            key: self.key,
            title: self.title,
            mime: self.mime,
            url,
            source_modified_at: self.source_modified_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            content_hash: self.content_hash,
            version: self.version,
            metadata,
            text,
            chunks,
        })
    }
}

/// An entity alias in the graph export.
#[derive(Serialize)]
struct ExportAlias {
    alias: String,
    /// Whether this alias row is the entity's current name.
    is_name: bool,
}

#[derive(Serialize, Deserialize)]
struct ExportIdentifier {
    scheme: String,
    value: String,
}

/// Which item supplied an alias or identifier of an entity.
#[derive(Serialize, Deserialize)]
struct ExportDetailSource {
    kind: String,
    scheme: String,
    value: String,
    source: String,
    key: String,
    created_at: String,
}

#[derive(Serialize)]
struct ExportEntity {
    id: String,
    kind: String,
    name: String,
    attributes: Value,
    created_at: String,
    updated_at: String,
    merged_into: Option<String>,
    merged_at: Option<String>,
    deleted_at: Option<String>,
    aliases: Vec<ExportAlias>,
    identifiers: Vec<ExportIdentifier>,
    detail_sources: Vec<ExportDetailSource>,
}

#[derive(Serialize, Deserialize)]
struct ExportEvidence {
    source: String,
    key: String,
    chunk_id: Option<i64>,
    quote: Option<String>,
    created_at: String,
}

#[derive(Serialize)]
struct ExportClaim {
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
    evidence: Vec<ExportEvidence>,
}

#[derive(Serialize)]
struct ExportMention {
    id: i64,
    entity_id: String,
    source: String,
    key: String,
    chunk_id: Option<i64>,
    surface: String,
    confidence: f64,
    created_at: String,
}

/// Entities with their aliases, identifiers and detail sources (each a JSON array, in row
/// order), in id order.
const ENTITIES_SQL: &str = "SELECT e.id, e.kind, e.name, e.attributes, e.created_at, e.updated_at,
            e.merged_into, e.merged_at, e.deleted_at,
            (SELECT json_group_array(json_object('alias', a.alias, 'is_name', a.is_name))
             FROM (SELECT alias, is_name FROM entity_aliases
                   WHERE entity_id = e.id ORDER BY id) a),
            (SELECT json_group_array(json_object('scheme', x.scheme, 'value', x.value))
             FROM (SELECT scheme, value FROM entity_identifiers
                   WHERE entity_id = e.id ORDER BY id) x),
            (SELECT json_group_array(json_object('kind', s.kind, 'scheme', s.scheme,
                                                 'value', s.value, 'source', s.source,
                                                 'key', s.key, 'created_at', s.created_at))
             FROM (SELECT kind, scheme, value, source, key, created_at
                   FROM entity_detail_sources WHERE entity_id = e.id ORDER BY id) s)
     FROM entities e ORDER BY e.id";

/// Claims with their evidence (a JSON array, in row order), in id order.
const CLAIMS_SQL: &str = "SELECT c.id, c.subject, c.predicate, c.object_entity, c.object_literal,
            c.text, c.confidence, c.status, c.valid_from, c.valid_to, c.created_at, c.updated_at,
            (SELECT json_group_array(json_object('source', v.source, 'key', v.key,
                                                 'chunk_id', v.chunk_id, 'quote', v.quote,
                                                 'created_at', v.created_at))
             FROM (SELECT source, key, chunk_id, quote, created_at
                   FROM claim_evidence WHERE claim_id = c.id ORDER BY id) v)
     FROM claims c ORDER BY c.id";

const MENTIONS_SQL: &str = "SELECT id, entity_id, source, key, chunk_id, surface, confidence,
            created_at
     FROM mentions ORDER BY id";

/// Header of [`Store::export_audit_csv`]; the columns of the `audit` table.
const AUDIT_COLUMNS: [&str; 8] = [
    "seq",
    "at",
    "actor",
    "action",
    "target",
    "detail",
    "prev_hash",
    "hash",
];

fn parse_json<T: DeserializeOwned>(json: &str, what: &str) -> Result<T> {
    serde_json::from_str(json).map_err(|e| Error::Storage(format!("export: bad {what}: {e}")))
}

/// Run `f` in a read transaction, so all its statements see one snapshot.
fn read_snapshot<T>(c: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    c.execute_batch("BEGIN DEFERRED").db("begin export read")?;
    let res = f(c);
    let end = c.execute_batch("COMMIT").db("end export read");
    match res {
        Ok(v) => end.map(|()| v),
        Err(e) => {
            let _ = c.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Run `sql` and write each row, mapped by `map`, as an element of a JSON array (`[` and `]`
/// included, one element per line). Returns the number of elements.
fn write_json_array<T: Serialize>(
    c: &Connection,
    sql: &str,
    w: &mut dyn Write,
    mut map: impl FnMut(&rusqlite::Row<'_>) -> Result<T>,
) -> Result<u64> {
    let mut stmt = c.prepare(sql).db("prepare graph export")?;
    let mut rows = stmt.query([]).db("query graph export")?;
    let mut n = 0u64;
    w.write_all(b"[")?;
    while let Some(row) = rows.next().db("read graph export")? {
        let value = map(row)?;
        w.write_all(if n == 0 { b"\n" } else { b",\n" })?;
        serde_json::to_writer(&mut *w, &value)?;
        n += 1;
    }
    w.write_all(if n == 0 { b"]" } else { b"\n]" })?;
    Ok(n)
}

/// Write one CSV cell (RFC 4180: quoted only when it holds `,`, `"`, CR or LF).
fn write_csv_cell(w: &mut dyn Write, cell: &str) -> std::io::Result<()> {
    if cell.contains([',', '"', '\r', '\n']) {
        w.write_all(b"\"")?;
        w.write_all(cell.replace('"', "\"\"").as_bytes())?;
        w.write_all(b"\"")
    } else {
        w.write_all(cell.as_bytes())
    }
}

/// Write one CSV record, CRLF-terminated.
fn write_csv_record<'a>(
    w: &mut dyn Write,
    cells: impl IntoIterator<Item = &'a str>,
) -> std::io::Result<()> {
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        write_csv_cell(w, cell)?;
    }
    w.write_all(b"\r\n")
}

/// A SQL value as CSV cell text: NULL is empty, blobs are lowercase hex.
fn cell_text(v: ValueRef<'_>) -> Result<String> {
    Ok(match v {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => std::str::from_utf8(t)
            .map_err(|e| Error::Storage(format!("export: text cell is not UTF-8: {e}")))?
            .to_owned(),
        ValueRef::Blob(b) => hex::encode(b),
    })
}

/// Run `sql` and write a CSV header (`header`, or the statement's column names) and one record
/// per row. Returns the number of data records.
fn write_csv(c: &Connection, sql: &str, header: Option<&[&str]>, w: &mut dyn Write) -> Result<u64> {
    let mut stmt = c.prepare(sql).db("prepare csv export")?;
    let names: Vec<String> = match header {
        Some(h) => h.iter().map(|s| (*s).to_owned()).collect(),
        None => stmt.column_names().into_iter().map(str::to_owned).collect(),
    };
    write_csv_record(w, names.iter().map(String::as_str))?;
    let cols = stmt.column_count();
    let mut rows = stmt.query([]).db("query csv export")?;
    let mut cells = Vec::with_capacity(cols);
    let mut n = 0u64;
    while let Some(row) = rows.next().db("read csv export")? {
        cells.clear();
        for i in 0..cols {
            cells.push(cell_text(row.get_ref(i).db("read csv cell")?)?);
        }
        write_csv_record(w, cells.iter().map(String::as_str))?;
        n += 1;
    }
    Ok(n)
}

impl Store {
    /// Visit every item in rowid order, reading `page` items per query (keyset pagination on a
    /// read connection, released before `f` runs, so `f` may write to the store). Memory is
    /// bounded by one page. Returns the number of items visited; an error from `f` stops the
    /// export and is returned.
    ///
    /// Per-page consistent on a live store: each page is one statement (one snapshot), pages
    /// may see different states. Every item that exists for the whole export is visited exactly
    /// once; one deleted before its page is read is skipped, one inserted meanwhile is visited
    /// only if its rowid is past the pages already read, and none is visited twice.
    pub fn export_items(
        &self,
        page: usize,
        f: &mut dyn FnMut(ExportItem) -> Result<()>,
    ) -> Result<u64> {
        if page == 0 {
            return Err(Error::Invalid("export page size must be at least 1".into()));
        }
        let limit = i64::try_from(page).unwrap_or(i64::MAX);
        let mut after = i64::MIN;
        let mut n = 0u64;
        loop {
            let rows: Vec<RawItem> = self.with_read_conn(|c| {
                let mut stmt = c.prepare_cached(ITEMS_PAGE_SQL).db("prepare item export")?;
                let rows = stmt
                    .query_map([after, limit], RawItem::read)
                    .db("query item export")?;
                rows.collect::<rusqlite::Result<_>>().db("read item export")
            })?;
            let Some(last) = rows.last() else {
                return Ok(n);
            };
            after = last.id;
            let full = rows.len() == page;
            for raw in rows {
                f(raw.into_export()?)?;
                n += 1;
            }
            if !full {
                return Ok(n);
            }
        }
    }

    /// Write the knowledge graph to `w` as one JSON document:
    /// `{"format":1,"entities":[...],"claims":[...],"mentions":[...]}`, streamed row by row
    /// (never the whole graph in memory). Entities carry their `aliases`, `identifiers` and
    /// `detail_sources`, claims their `evidence`. Merged-away and deleted entities and claims
    /// of every status are included, so the export is complete. All sections come from one
    /// snapshot (a read transaction held for the whole export).
    pub fn export_graph_json(&self, w: &mut dyn Write) -> Result<GraphExportCounts> {
        let mut w = BufWriter::new(w);
        let counts = self.with_read_conn(|c| {
            read_snapshot(c, |c| {
                let mut counts = GraphExportCounts::default();
                write!(w, "{{\"format\":{GRAPH_EXPORT_FORMAT},\"entities\":")?;
                counts.entities = write_json_array(c, ENTITIES_SQL, &mut w, |r| {
                    let attributes: String = r.get(3).db("read entity")?;
                    Ok(ExportEntity {
                        id: r.get(0).db("read entity")?,
                        kind: r.get(1).db("read entity")?,
                        name: r.get(2).db("read entity")?,
                        attributes: serde_json::from_str(&attributes)
                            .unwrap_or(Value::String(attributes)),
                        created_at: r.get(4).db("read entity")?,
                        updated_at: r.get(5).db("read entity")?,
                        merged_into: r.get(6).db("read entity")?,
                        merged_at: r.get(7).db("read entity")?,
                        deleted_at: r.get(8).db("read entity")?,
                        aliases: parse_json::<Vec<AliasRow>>(
                            &r.get::<_, String>(9).db("read entity")?,
                            "aliases",
                        )?
                        .into_iter()
                        .map(|a| ExportAlias {
                            alias: a.alias,
                            is_name: a.is_name != 0,
                        })
                        .collect(),
                        identifiers: parse_json(
                            &r.get::<_, String>(10).db("read entity")?,
                            "identifiers",
                        )?,
                        detail_sources: parse_json(
                            &r.get::<_, String>(11).db("read entity")?,
                            "detail sources",
                        )?,
                    })
                })?;
                w.write_all(b",\"claims\":")?;
                let mut evidence = 0u64;
                counts.claims = write_json_array(c, CLAIMS_SQL, &mut w, |r| {
                    let claim = ExportClaim {
                        id: r.get(0).db("read claim")?,
                        subject: r.get(1).db("read claim")?,
                        predicate: r.get(2).db("read claim")?,
                        object_entity: r.get(3).db("read claim")?,
                        object_literal: r.get(4).db("read claim")?,
                        text: r.get(5).db("read claim")?,
                        confidence: r.get(6).db("read claim")?,
                        status: r.get(7).db("read claim")?,
                        valid_from: r.get(8).db("read claim")?,
                        valid_to: r.get(9).db("read claim")?,
                        created_at: r.get(10).db("read claim")?,
                        updated_at: r.get(11).db("read claim")?,
                        evidence: parse_json(
                            &r.get::<_, String>(12).db("read claim")?,
                            "evidence",
                        )?,
                    };
                    evidence += claim.evidence.len() as u64;
                    Ok(claim)
                })?;
                counts.evidence = evidence;
                w.write_all(b",\"mentions\":")?;
                counts.mentions = write_json_array(c, MENTIONS_SQL, &mut w, |r| {
                    Ok(ExportMention {
                        id: r.get(0).db("read mention")?,
                        entity_id: r.get(1).db("read mention")?,
                        source: r.get(2).db("read mention")?,
                        key: r.get(3).db("read mention")?,
                        chunk_id: r.get(4).db("read mention")?,
                        surface: r.get(5).db("read mention")?,
                        confidence: r.get(6).db("read mention")?,
                        created_at: r.get(7).db("read mention")?,
                    })
                })?;
                w.write_all(b"}\n")?;
                Ok(counts)
            })
        })?;
        w.flush()?;
        Ok(counts)
    }

    /// Write the audit log to `w` as CSV, oldest entry first, with the header
    /// `seq,at,actor,action,target,detail,prev_hash,hash` (`detail` is the stored JSON).
    /// Streamed from one statement. Returns the number of entries written.
    pub fn export_audit_csv(&self, w: &mut dyn Write) -> Result<u64> {
        let mut w = BufWriter::new(w);
        let sql = format!(
            "SELECT {} FROM audit ORDER BY seq",
            AUDIT_COLUMNS.join(", ")
        );
        let n = self.with_read_conn(|c| write_csv(c, &sql, Some(&AUDIT_COLUMNS), &mut w))?;
        w.flush()?;
        Ok(n)
    }

    /// Write the egress ledger to `w` as CSV, oldest row first: a header row with every column
    /// of the ledger table (today `id,at,host,feature,bytes_sent,bytes_received,items,state,
    /// owner`; `items` is the stored JSON array of item refs), then one record per row. Events
    /// queued but not yet written are flushed first. Returns the number of rows written.
    pub fn export_egress_csv(&self, w: &mut dyn Write) -> Result<u64> {
        self.flush_egress();
        let mut w = BufWriter::new(w);
        let n = self
            .with_read_conn(|c| write_csv(c, "SELECT * FROM egress ORDER BY id", None, &mut w))?;
        w.flush()?;
        Ok(n)
    }
}

/// An alias as SQLite's `json_object` writes it (`is_name` is the stored 0/1).
#[derive(Deserialize)]
struct AliasRow {
    alias: String,
    is_name: i64,
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use nugget_core::egress::EgressEvent;
    use nugget_core::ids::{ItemKey, ItemRef, SourceId};
    use nugget_graph::{
        ClaimObject, ClaimStatus, EntityDetail, EntityKind, Evidence, GraphStore, Identifier,
        Mention, NewClaim, NewEntity,
    };
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;
    use crate::error::parse_time;
    use crate::{AuditEntry, NewChunk, NewItem, StoreKey, content_hash};

    fn file_store(dir: &tempfile::TempDir) -> Store {
        Store::open(
            dir.path().join("nugget.db"),
            &StoreKey::from_bytes([7u8; 32]),
        )
        .unwrap()
    }

    fn iref(source: &str, key: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new(source),
            key: ItemKey::new(key),
        }
    }

    fn add(store: &Store, key: &str) {
        store
            .upsert_item(&NewItem::new("src", key, content_hash(key)).with_text(key))
            .unwrap();
    }

    fn collect(store: &Store, page: usize) -> Vec<ExportItem> {
        let mut out = Vec::new();
        let n = store
            .export_items(page, &mut |item| {
                out.push(item);
                Ok(())
            })
            .unwrap();
        assert_eq!(n as usize, out.len());
        out
    }

    /// JSONL of every item, as `export.run` writes `items.jsonl`.
    fn jsonl(store: &Store, page: usize) -> String {
        let mut buf = Vec::new();
        store
            .export_items(page, &mut |item| {
                serde_json::to_writer(&mut buf, &item)?;
                buf.push(b'\n');
                Ok(())
            })
            .unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn count(store: &Store, sql: &str) -> i64 {
        store
            .with_read_conn(|c| c.query_row(sql, [], |r| r.get(0)).db("count"))
            .unwrap()
    }

    #[test]
    fn items_round_trip_through_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let store = file_store(&dir);
        let tricky =
            "line one\nline \"two\", with commas\r\n\ttabbed — ünïcödé 🦀 \\ back\u{0}slash";
        let modified = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        store
            .upsert_item(
                &NewItem::new("files", "notes/a.md", content_hash("a"))
                    .with_title("Ünïcödé \"title\"\nnext")
                    .with_text(tricky)
                    .with_mime("text/markdown")
                    .with_version("mtime:1:size:2")
                    .with_modified_at(modified)
                    .with_metadata(json!({
                        "url": "https://example.com/a?b=1&c=\"2\"",
                        "author": "Ada",
                        "participants": ["x@example.com", "y@example.com"],
                        "nested": {"z": 1.5, "a": [true, null]},
                    })),
            )
            .unwrap();
        // Text stored as a layout over its chunks (`items.text` NULL).
        let long: String = (0..40)
            .map(|i| format!("Paragraph {i} about things.\n\n"))
            .collect();
        let (a, b) = long.split_at(long.len() / 2);
        store
            .upsert_item_with_chunks(
                &NewItem::new("files", "long.md", content_hash(&long)).with_text(long.clone()),
                &[NewChunk::text(a), NewChunk::text(b)],
            )
            .unwrap();
        assert_eq!(
            count(&store, "SELECT count(*) FROM item_text"),
            1,
            "the long item's text should be stored as a layout"
        );
        // No text of its own, only chunks.
        store
            .upsert_item_with_chunks(
                &NewItem::new("mail", "m1", content_hash("m1")),
                &[
                    NewChunk::text("first, \"chunk\""),
                    NewChunk::text("second\nchunk"),
                ],
            )
            .unwrap();
        // Metadata that is not a JSON object (stored verbatim too).
        store
            .upsert_item(&NewItem::new("mail", "m2", content_hash("m2")).with_metadata(json!(42)))
            .unwrap();

        let out = jsonl(&store, 2);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4);
        let values: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        for v in &values {
            for field in [
                "id",
                "source",
                "key",
                "title",
                "mime",
                "url",
                "source_modified_at",
                "created_at",
                "updated_at",
                "content_hash",
                "version",
                "metadata",
                "text",
            ] {
                assert!(v.get(field).is_some(), "missing {field} in {v}");
            }
        }
        let items: Vec<ExportItem> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let first = &items[0];
        let stored = store
            .get_item(&iref("files", "notes/a.md"))
            .unwrap()
            .unwrap();
        assert_eq!(first.source, "files");
        assert_eq!(first.key, "notes/a.md");
        assert_eq!(first.text.as_deref(), Some(tricky));
        assert_eq!(first.title, stored.title);
        assert_eq!(first.mime.as_deref(), Some("text/markdown"));
        assert_eq!(first.version.as_deref(), Some("mtime:1:size:2"));
        assert_eq!(first.content_hash, content_hash("a"));
        assert_eq!(first.metadata, stored.metadata);
        assert_eq!(
            first.url.as_deref(),
            Some("https://example.com/a?b=1&c=\"2\"")
        );
        assert_eq!(
            first.source_modified_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(parse_time(&first.created_at).unwrap(), stored.created_at);
        assert_eq!(parse_time(&first.updated_at).unwrap(), stored.updated_at);
        assert!(first.chunks.is_empty());
        // The stored metadata text is reproduced byte for byte.
        let raw: String = store
            .with_read_conn(|c| {
                c.query_row(
                    "SELECT metadata FROM items WHERE id = ?1",
                    [first.id],
                    |r| r.get(0),
                )
                .db("read metadata")
            })
            .unwrap();
        assert_eq!(serde_json::to_string(&first.metadata).unwrap(), raw);

        assert_eq!(items[1].text.as_deref(), Some(long.as_str()));
        assert!(items[1].chunks.is_empty());
        assert_eq!(items[2].text, None);
        assert_eq!(items[2].chunks, ["first, \"chunk\"", "second\nchunk"]);
        assert!(values[3].get("chunks").is_none());
        assert_eq!(items[3].metadata, json!(42));
        assert_eq!(items[3].url, None);
    }

    #[test]
    fn invalid_metadata_is_exported_as_a_string() {
        let store = Store::open_in_memory().unwrap();
        add(&store, "a");
        store
            .with_conn(|c| {
                c.execute("UPDATE items SET metadata = 'not json'", [])
                    .db("corrupt")
            })
            .unwrap();
        let items = collect(&store, 10);
        assert_eq!(items[0].metadata, json!("not json"));
    }

    #[test]
    fn pagination_visits_every_item_once_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = file_store(&dir);
        assert_eq!(collect(&store, 3).len(), 0);
        for i in 0..10 {
            add(&store, &format!("k{i:02}"));
        }
        let want: Vec<String> = (0..10).map(|i| format!("k{i:02}")).collect();
        for page in [1, 3, 5, 10, 11, 1000] {
            let got: Vec<String> = collect(&store, page).into_iter().map(|i| i.key).collect();
            assert_eq!(got, want, "page size {page}");
        }
        let ids: Vec<i64> = collect(&store, 4).iter().map(|i| i.id).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
        assert!(matches!(
            store.export_items(0, &mut |_| Ok(())),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn items_changed_during_export_are_seen_at_most_once() {
        for page in [1, 3, 64] {
            let dir = tempfile::tempdir().unwrap();
            let store = file_store(&dir);
            for i in 0..12 {
                add(&store, &format!("k{i:02}"));
            }
            let mut seen = Vec::new();
            let writer = store.clone();
            store
                .export_items(page, &mut |item| {
                    if item.key == "k00" {
                        // Past the first page for page sizes 1 and 3; already read for 64.
                        writer.delete_item(&iref("src", "k10")).unwrap();
                        add(&writer, "late");
                        // Updating an unread item neither moves it nor exports it twice.
                        writer
                            .upsert_item(
                                &NewItem::new("src", "k11", content_hash("new")).with_text("new"),
                            )
                            .unwrap();
                    }
                    seen.push(item.key);
                    Ok(())
                })
                .unwrap();
            let unique: HashSet<&String> = seen.iter().collect();
            assert_eq!(unique.len(), seen.len(), "page {page}: {seen:?}");
            let mut want: Vec<String> = (0..12).map(|i| format!("k{i:02}")).collect();
            if page < 12 {
                want.retain(|k| k != "k10");
                want.push("late".into());
            }
            assert_eq!(seen, want, "page {page}");
        }
    }

    #[test]
    fn callback_errors_stop_the_export() {
        let store = Store::open_in_memory().unwrap();
        for i in 0..5 {
            add(&store, &format!("k{i}"));
        }
        let mut calls = 0;
        let err = store
            .export_items(2, &mut |_| {
                calls += 1;
                if calls == 3 {
                    return Err(Error::Cancelled);
                }
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(calls, 3);
    }

    /// Fills the graph; returns the expected (entities, claims, evidence, mentions).
    fn populate_graph(store: &Store) -> (usize, usize, usize, usize) {
        let g = store.graph();
        let mut new = NewEntity::new(EntityKind::Person, "Ada \"Countess\" Lovelace");
        new.aliases = vec!["Ada".into(), "A. L.".into()];
        let ada = g.create_entity(&new).unwrap();
        g.add_identifier(&ada, &Identifier::new("email", "ada@example.com"))
            .unwrap();
        g.add_detail_source(
            &ada,
            &EntityDetail::Identifier(Identifier::new("email", "ada@example.com")),
            &iref("mail", "m1"),
        )
        .unwrap();
        let acme = g
            .create_entity(&NewEntity::new(EntityKind::Organization, "Acme, Inc.\nLtd"))
            .unwrap();
        let dup = g
            .create_entity(&NewEntity::new(EntityKind::Organization, "ACME"))
            .unwrap();
        g.merge(&acme, &dup).unwrap();
        let ev = |k: &str| Evidence {
            item: iref("mail", k),
            chunk_id: Some(1),
            quote: Some(format!("quote \"{k}\"")),
        };
        let claim = |object, status, evidence| NewClaim {
            subject: ada.clone(),
            predicate: "works_at".into(),
            object,
            text: "Ada works at Acme".into(),
            confidence: 0.75,
            status,
            evidence,
            valid_from: None,
            valid_to: None,
        };
        g.add_claim(&claim(
            ClaimObject::Entity(acme.clone()),
            ClaimStatus::Active,
            vec![ev("m1"), ev("m2")],
        ))
        .unwrap();
        g.add_claim(&claim(
            ClaimObject::Literal("likes tea".into()),
            ClaimStatus::UserAsserted,
            vec![],
        ))
        .unwrap();
        for (e, k) in [(&ada, "m1"), (&ada, "m2"), (&acme, "m1")] {
            g.add_mention(&Mention {
                entity: e.clone(),
                item: iref("mail", k),
                chunk_id: None,
                surface: "Ada, \"the\" one".into(),
                confidence: 0.5,
            })
            .unwrap();
        }
        // ada, acme and the merged-away dup; two claims; two evidence rows; three mentions.
        (3, 2, 2, 3)
    }

    #[test]
    fn graph_json_is_one_document_with_matching_counts() {
        let dir = tempfile::tempdir().unwrap();
        for store in [Store::open_in_memory().unwrap(), file_store(&dir)] {
            let mut empty = Vec::new();
            let counts = store.export_graph_json(&mut empty).unwrap();
            assert_eq!(counts, GraphExportCounts::default());
            let doc: Value = serde_json::from_slice(&empty).unwrap();
            assert_eq!(
                doc,
                json!({"format": 1, "entities": [], "claims": [], "mentions": []})
            );

            let (entities, claims, evidence, mentions) = populate_graph(&store);
            let mut buf = Vec::new();
            let counts = store.export_graph_json(&mut buf).unwrap();
            assert_eq!(
                counts,
                GraphExportCounts {
                    entities: entities as u64,
                    claims: claims as u64,
                    evidence: evidence as u64,
                    mentions: mentions as u64,
                }
            );
            assert_eq!(
                entities as i64,
                count(&store, "SELECT count(*) FROM entities")
            );
            let doc: Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(doc["format"], GRAPH_EXPORT_FORMAT);
            let ents = doc["entities"].as_array().unwrap();
            assert_eq!(ents.len(), entities);
            let ada = ents
                .iter()
                .find(|e| e["name"] == "Ada \"Countess\" Lovelace")
                .unwrap();
            assert_eq!(ada["kind"], "person");
            let aliases = ada["aliases"].as_array().unwrap();
            assert!(aliases.contains(&json!({"alias": "Ada", "is_name": false})));
            assert!(
                aliases.contains(&json!({"alias": "Ada \"Countess\" Lovelace", "is_name": true}))
            );
            assert_eq!(
                ada["identifiers"],
                json!([{"scheme": "email", "value": "ada@example.com"}])
            );
            assert_eq!(ada["detail_sources"][0]["source"], "mail");
            assert_eq!(ada["detail_sources"][0]["kind"], "identifier");
            let merged: Vec<&Value> = ents
                .iter()
                .filter(|e| !e["merged_into"].is_null())
                .collect();
            assert_eq!(merged.len(), 1);
            assert_eq!(merged[0]["name"], "ACME");

            let cls = doc["claims"].as_array().unwrap();
            assert_eq!(cls.len(), claims);
            let total_evidence: usize = cls
                .iter()
                .map(|c| c["evidence"].as_array().unwrap().len())
                .sum();
            assert_eq!(total_evidence, evidence);
            assert_eq!(cls[0]["evidence"][0]["quote"], "quote \"m1\"");
            assert_eq!(cls[0]["confidence"], 0.75);
            assert_eq!(cls[1]["object_literal"], "likes tea");
            assert_eq!(cls[1]["status"], "user_asserted");
            // Every claim's subject is an exported entity.
            let ids: HashSet<&str> = ents.iter().map(|e| e["id"].as_str().unwrap()).collect();
            assert!(
                cls.iter()
                    .all(|c| ids.contains(c["subject"].as_str().unwrap()))
            );
            let ms = doc["mentions"].as_array().unwrap();
            assert_eq!(ms.len(), mentions);
            assert_eq!(ms[0]["surface"], "Ada, \"the\" one");
            assert!(ms[0]["chunk_id"].is_null());
        }
    }

    /// Minimal RFC 4180 reader for the tests: records of cells, CRLF-terminated.
    fn parse_csv(s: &str) -> Vec<Vec<String>> {
        let mut records = Vec::new();
        let mut record = Vec::new();
        let mut cell = String::new();
        let mut chars = s.chars().peekable();
        let mut quoted = false;
        while let Some(ch) = chars.next() {
            if quoted {
                match ch {
                    '"' if chars.peek() == Some(&'"') => {
                        chars.next();
                        cell.push('"');
                    }
                    '"' => quoted = false,
                    _ => cell.push(ch),
                }
                continue;
            }
            match ch {
                '"' => {
                    assert!(cell.is_empty(), "quote inside an unquoted cell");
                    quoted = true;
                }
                ',' => record.push(std::mem::take(&mut cell)),
                '\r' => {
                    assert_eq!(chars.next(), Some('\n'), "bare CR outside quotes");
                    record.push(std::mem::take(&mut cell));
                    records.push(std::mem::take(&mut record));
                }
                '\n' => panic!("bare LF outside quotes"),
                _ => cell.push(ch),
            }
        }
        assert!(
            !quoted && cell.is_empty() && record.is_empty(),
            "truncated CSV"
        );
        records
    }

    #[test]
    fn audit_csv_round_trips_losslessly() {
        let store = Store::open_in_memory().unwrap();
        let nasty = [
            "plain",
            "comma, here",
            "quote \"here\"",
            "new\nline",
            "cr\r\nlf",
            "=1+2",
            "-starts with dash",
            "@at",
            "\ttab",
            "ünïcödé 🦀",
            "",
        ];
        for (i, target) in nasty.iter().enumerate() {
            store
                .append_audit(&AuditEntry {
                    actor: "user".into(),
                    action: format!("act,{i}"),
                    target: (*target).into(),
                    detail: json!({"note": target, "i": i}),
                })
                .unwrap();
        }
        let mut buf = Vec::new();
        assert_eq!(
            store.export_audit_csv(&mut buf).unwrap(),
            nasty.len() as u64
        );
        let records = parse_csv(std::str::from_utf8(&buf).unwrap());
        assert_eq!(records[0], AUDIT_COLUMNS);
        assert_eq!(records.len(), nasty.len() + 1);
        let stored = store.recent_audit(100).unwrap();
        for (row, target) in records[1..].iter().zip(nasty) {
            assert_eq!(row.len(), 8);
            let seq: u64 = row[0].parse().unwrap();
            let rec = stored.iter().find(|r| r.seq == seq).unwrap();
            assert_eq!(row[4], target);
            assert_eq!(row[2], rec.actor);
            assert_eq!(row[3], rec.action);
            assert_eq!(row[4], rec.target);
            assert_eq!(serde_json::from_str::<Value>(&row[5]).unwrap(), rec.detail);
            assert_eq!(row[6], rec.prev_hash);
            assert_eq!(row[7], rec.hash);
            assert_eq!(parse_time(&row[1]).unwrap(), rec.at);
        }
        store.verify_audit_chain().unwrap();
    }

    #[test]
    fn egress_csv_has_every_column_and_round_trips() {
        let store = Store::open_in_memory().unwrap();
        let mut empty = Vec::new();
        assert_eq!(store.export_egress_csv(&mut empty).unwrap(), 0);
        let header = parse_csv(std::str::from_utf8(&empty).unwrap());
        let columns: Vec<String> = store
            .with_read_conn(|c| {
                let mut stmt = c
                    .prepare("SELECT name FROM pragma_table_info('egress') ORDER BY cid")
                    .db("columns")?;
                let rows = stmt.query_map([], |r| r.get(0)).db("columns")?;
                rows.collect::<rusqlite::Result<_>>().db("columns")
            })
            .unwrap();
        assert_eq!(header, vec![columns.clone()]);
        for c in [
            "id",
            "at",
            "host",
            "feature",
            "bytes_sent",
            "bytes_received",
            "items",
            "state",
            "owner",
        ] {
            assert!(columns.iter().any(|x| x == c), "{c}");
        }

        let event = EgressEvent {
            at: OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap(),
            host: "api.example.com".into(),
            feature: "chat, \"quoted\"\nfeature".into(),
            bytes_sent: 1234,
            bytes_received: 5678,
            items: vec![iref("mail", "a,b"), iref("files", "x\"y")],
        };
        store.record_egress(&event).unwrap();
        let mut buf = Vec::new();
        assert_eq!(store.export_egress_csv(&mut buf).unwrap(), 1);
        let records = parse_csv(std::str::from_utf8(&buf).unwrap());
        assert_eq!(records.len(), 2);
        let row: HashMap<&str, &str> = records[0]
            .iter()
            .map(String::as_str)
            .zip(records[1].iter().map(String::as_str))
            .collect();
        assert_eq!(row["host"], "api.example.com");
        assert_eq!(row["feature"], event.feature);
        assert_eq!(row["bytes_sent"], "1234");
        assert_eq!(row["bytes_received"], "5678");
        assert_eq!(row["state"], "done");
        assert_eq!(
            serde_json::from_str::<Vec<ItemRef>>(row["items"]).unwrap(),
            event.items
        );
        assert_eq!(parse_time(row["at"]).unwrap(), event.at);
    }

    #[test]
    fn csv_cells_are_quoted_only_when_needed() {
        let mut buf = Vec::new();
        write_csv_record(&mut buf, ["a", "b,c", "d\"e", "f\ng", "h\ri", "=j", ""]).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "a,\"b,c\",\"d\"\"e\",\"f\ng\",\"h\ri\",=j,\r\n"
        );
    }

    fn bulk_insert(store: &Store, n: usize) {
        store
            .with_conn(|c| {
                let tx = c.transaction().db("begin bulk")?;
                for i in 0..n {
                    let key = format!("item-{i:06}");
                    tx.execute(
                        "INSERT INTO items (source, key, title, content_hash, text, created_at,
                                            updated_at)
                         VALUES ('bulk', ?1, ?1, ?2, ?3, '2026-01-01T00:00:00Z',
                                 '2026-01-01T00:00:00Z')",
                        rusqlite::params![key, content_hash(&key), format!("text of {key}")],
                    )
                    .db("bulk insert")?;
                }
                tx.commit().db("commit bulk")
            })
            .unwrap();
    }

    fn export_to_sink(store: &Store, page: usize) -> u64 {
        let mut sink = std::io::sink();
        store
            .export_items(page, &mut |item| {
                serde_json::to_writer(&mut sink, &item)?;
                sink.write_all(b"\n")?;
                Ok(())
            })
            .unwrap()
    }

    #[test]
    fn exports_many_items_in_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = file_store(&dir);
        bulk_insert(&store, 20_000);
        let started = std::time::Instant::now();
        assert_eq!(export_to_sink(&store, 500), 20_000);
        eprintln!("exported 20000 items in {:?}", started.elapsed());
    }

    /// Peak resident set size of this process in KiB, where the OS reports it.
    fn peak_rss_kib() -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            let status = std::fs::read_to_string("/proc/self/status").ok()?;
            status
                .lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    #[test]
    #[ignore = "slow: 100k items; run with --ignored --nocapture for timings"]
    fn exports_100k_items() {
        let dir = tempfile::tempdir().unwrap();
        let store = file_store(&dir);
        bulk_insert(&store, 100_000);
        let before = peak_rss_kib();
        let started = std::time::Instant::now();
        assert_eq!(export_to_sink(&store, 500), 100_000);
        eprintln!(
            "exported 100000 items in {:?}; peak RSS (KiB) before export {before:?}, after {:?}",
            started.elapsed(),
            peak_rss_kib()
        );
    }
}
