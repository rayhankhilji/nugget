//! Data export: everything a user may want to take out of the store, in open formats.
//!
//! - [`Store::export_items`] visits every item (one [`ExportItem`] each, written as one JSON
//!   line by [`ExportItem::write_jsonl`]) in rowid order.
//! - [`Store::export_graph_json`] streams the knowledge graph as one JSON document.
//! - [`Store::export_audit_csv`] and [`Store::export_egress_csv`] write the audit log and the
//!   egress ledger as RFC 4180 CSV.
//!
//! # Paging and consistency
//!
//! Every export reads in keyset pages (by rowid / primary key), each page with one SQL
//! statement on a read connection, and writes a page only after that connection is released.
//! So an export never holds a read transaction while the caller's writer runs: WAL checkpoints
//! proceed, a pooled connection is not pinned, and a writer (or item callback) may call back
//! into the store, also on an in-memory store whose reads share the writer connection. Memory
//! is bounded by one page; item pages also stop early once about [`PAGE_BYTE_BUDGET`] bytes of
//! item data are read (always at least one item).
//!
//! Exporting a live store is therefore **per-page consistent**: each page is one snapshot, but
//! different pages may see different states. Rows are exported at most once each, and only
//! those that exist when their page is read. Rowid-keyed scans (items, claims, mentions, audit,
//! egress) are bounded by the largest rowid at the start of the export, so rows inserted
//! meanwhile are left out; in particular an item deleted and re-inserted during the export (it
//! gets a new rowid) is never exported twice. In the graph, a claim or mention may name an
//! entity created after the entity section was read.
//!
//! # Errors
//!
//! On `Err` the output written so far is incomplete (the JSON document is then not valid
//! JSON): callers write to a temporary file and rename it into place only on success. Errors
//! of the caller's writer surface as `Error::Io`. A damaged item does not stop the item export:
//! it is exported with what could be read and an `error` field (see [`ExportItem::error`]).
//!
//! # CSV
//!
//! Cells are written losslessly: a cell is quoted (`"` doubled) when it is empty or holds `,`,
//! `"` or any control character (CR, LF, tab, ...) or starts or ends with whitespace; records
//! end in CRLF. SQL NULL is an empty, unquoted cell; the empty string is `""`. Cells are **not**
//! altered against "CSV injection" (a leading `=`, `+`, `-` or `@` is kept as is): this is a
//! data export that must round-trip exactly, not a report for a spreadsheet. Open it in a
//! spreadsheet with formula evaluation off (import as text) if its contents are untrusted.

use std::io::Write;

use nugget_core::{Error, Result};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::Store;
use crate::error::DbExt;
use crate::items::decode_text_layout;

/// Version of the [`Store::export_graph_json`] document (its `format` field).
pub const GRAPH_EXPORT_FORMAT: u32 = 1;

/// An item page stops once it holds about this many bytes of item data (texts, chunks,
/// metadata), so pages of large documents stay small; it always holds at least one item.
pub const PAGE_BYTE_BUDGET: usize = 16 << 20;

/// Rows per page of the graph export.
const GRAPH_PAGE: usize = 500;
/// Rows per page of the CSV exports.
const CSV_PAGE: usize = 1000;

/// One exported item: every column the store keeps for it, with its full text.
///
/// Dates are the stored RFC 3339 strings, unchanged. Source-specific details (author, sender,
/// participants, labels, ...) are inside `metadata`, which is the stored JSON text verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Source metadata: the stored JSON text, byte for byte (numbers are not re-encoded). A
    /// stored value that is not valid JSON is exported as a JSON string holding it.
    pub metadata: Box<RawValue>,
    /// The item's full text, as written by its connector (rebuilt from its chunks when stored
    /// that way).
    pub text: Option<String>,
    /// When `text` is absent (the item has none, or it could not be rebuilt): the item's chunk
    /// texts in order (omitted when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<String>,
    /// What could not be read as stored (a text layout that no longer matches its chunks,
    /// invalid UTF-8 or JSON, ...), when anything. The item is still exported with everything
    /// readable; such items are also logged. Omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ExportItem {
    /// Write the item as one JSON line (`items.jsonl`). Errors of `w` are `Error::Io`.
    pub fn write_jsonl(&self, w: &mut dyn Write) -> Result<()> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        w.write_all(&line)?;
        Ok(())
    }
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

/// `json` as a raw JSON value, verbatim when valid; otherwise a JSON string holding it, with a
/// note saying so.
fn raw_json(json: String) -> (Box<RawValue>, Option<String>) {
    match RawValue::from_string(json.clone()) {
        Ok(raw) => (raw, None),
        Err(e) => {
            let raw = serde_json::value::to_raw_value(&json)
                .expect("a string always serialises to a valid JSON string");
            (
                raw,
                Some(format!(
                    "stored JSON is invalid ({e}), exported as a string"
                )),
            )
        }
    }
}

/// A text column read leniently: invalid UTF-8 is converted lossily and a non-text value is
/// rendered as text, each with a note in `errors`.
fn lenient_text(
    r: &rusqlite::Row<'_>,
    i: usize,
    what: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    match r.get_ref(i) {
        Ok(ValueRef::Null) => None,
        Ok(ValueRef::Text(t)) => Some(match std::str::from_utf8(t) {
            Ok(s) => s.to_owned(),
            Err(_) => {
                errors.push(format!("{what} is not valid UTF-8 (converted lossily)"));
                String::from_utf8_lossy(t).into_owned()
            }
        }),
        Ok(ValueRef::Integer(n)) => {
            errors.push(format!("{what} is stored as an integer"));
            Some(n.to_string())
        }
        Ok(ValueRef::Real(f)) => {
            errors.push(format!("{what} is stored as a real"));
            Some(f.to_string())
        }
        Ok(ValueRef::Blob(b)) => {
            errors.push(format!("{what} is stored as a blob (converted lossily)"));
            Some(String::from_utf8_lossy(b).into_owned())
        }
        Err(e) => {
            errors.push(format!("{what}: {e}"));
            None
        }
    }
}

/// One page of items (rowid in `(?1, ?2]`, at most `?3`), with what their texts need: the
/// `item_text` layout and, when `items.text` is NULL, the chunks as a JSON array of
/// `[ord, text]`.
const ITEMS_PAGE_SQL: &str = "SELECT i.id, i.source, i.key, i.title, i.mime, i.source_modified_at,
            i.created_at, i.updated_at, i.content_hash, i.version, i.metadata, i.text, t.layout,
            CASE WHEN i.text IS NULL THEN
                (SELECT json_group_array(json_array(c.ord, c.text))
                 FROM chunks c WHERE c.item_id = i.id)
            END
     FROM items i LEFT JOIN item_text t ON t.item_id = i.id
     WHERE i.id > ?1 AND i.id <= ?2 ORDER BY i.id LIMIT ?3";

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
    /// Chunk texts by `ord`, sorted (read only when `text` is NULL).
    chunks: Vec<(u64, String)>,
    errors: Vec<String>,
}

impl RawItem {
    fn read(r: &rusqlite::Row<'_>) -> Result<Self> {
        let id: i64 = r.get(0).db("read item id")?;
        let mut errors = Vec::new();
        let mut text = |i: usize, what: &str| lenient_text(r, i, what, &mut errors);
        let source = text(1, "source").unwrap_or_default();
        let key = text(2, "key").unwrap_or_default();
        let title = text(3, "title");
        let mime = text(4, "mime");
        let source_modified_at = text(5, "source_modified_at");
        let created_at = text(6, "created_at").unwrap_or_default();
        let updated_at = text(7, "updated_at").unwrap_or_default();
        let content_hash = text(8, "content_hash").unwrap_or_default();
        let version = text(9, "version");
        let metadata = text(10, "metadata").unwrap_or_else(|| "null".into());
        let item_text = text(11, "text");
        let chunks_json = text(13, "chunks");
        let layout = match r.get_ref(12) {
            Ok(ValueRef::Blob(b) | ValueRef::Text(b)) => Some(b.to_vec()),
            Ok(ValueRef::Null) => None,
            Ok(_) => {
                errors.push("text layout is not a blob".into());
                None
            }
            Err(e) => {
                errors.push(format!("text layout: {e}"));
                None
            }
        };
        let mut chunks: Vec<(u64, String)> = match chunks_json {
            Some(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                errors.push(format!("chunks could not be read: {e}"));
                Vec::new()
            }),
            None => Vec::new(),
        };
        chunks.sort_unstable_by_key(|(ord, _)| *ord);
        Ok(Self {
            id,
            source,
            key,
            title,
            mime,
            source_modified_at,
            created_at,
            updated_at,
            content_hash,
            version,
            metadata,
            text: item_text,
            layout,
            chunks,
            errors,
        })
    }

    /// Bytes of item data this row holds (for [`PAGE_BYTE_BUDGET`]).
    fn size(&self) -> usize {
        self.metadata.len()
            + self.text.as_ref().map_or(0, String::len)
            + self.layout.as_ref().map_or(0, Vec::len)
            + self.chunks.iter().map(|(_, t)| t.len()).sum::<usize>()
    }

    fn into_export(self) -> ExportItem {
        let mut errors = self.errors;
        let chunks = self.chunks;
        let (text, chunks) = match (self.text, self.layout) {
            (Some(text), _) => (Some(text), Vec::new()),
            (None, Some(layout)) => {
                let decoded = decode_text_layout(self.id, &layout, |ord| {
                    chunks
                        .binary_search_by_key(&ord, |(o, _)| *o)
                        .ok()
                        .map(|i| chunks[i].1.as_str())
                });
                match decoded {
                    Ok(text) => (Some(text), Vec::new()),
                    Err(e) => {
                        errors.push(format!(
                            "text could not be rebuilt from its chunks ({e}); \
                             exporting the chunks that remain"
                        ));
                        (None, chunks.into_iter().map(|(_, t)| t).collect())
                    }
                }
            }
            (None, None) => (None, chunks.into_iter().map(|(_, t)| t).collect()),
        };
        let (metadata, bad) = raw_json(self.metadata);
        errors.extend(bad.map(|e| format!("metadata: {e}")));
        /// Just the `url` field of an object; anything else fails to deserialize.
        #[derive(Deserialize)]
        struct UrlProbe {
            #[serde(default)]
            url: Option<Value>,
        }
        let url = serde_json::from_str::<UrlProbe>(metadata.get())
            .ok()
            .and_then(|p| p.url)
            .and_then(|u| u.as_str().map(str::to_owned));
        ExportItem {
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
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        }
    }
}

/// Read one page of items after `after` (rowid at most `max`, at most `page` rows, stopping
/// early at [`PAGE_BYTE_BUDGET`]). Returns the rows and whether more may follow.
fn read_items_page(
    c: &Connection,
    after: i64,
    max: i64,
    page: usize,
) -> Result<(Vec<RawItem>, bool)> {
    let limit = i64::try_from(page).unwrap_or(i64::MAX);
    let mut stmt = c.prepare_cached(ITEMS_PAGE_SQL).db("prepare item export")?;
    let mut rows = stmt
        .query(params![after, max, limit])
        .db("query item export")?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    while let Some(r) = rows.next().db("read item export")? {
        let raw = RawItem::read(r)?;
        bytes = bytes.saturating_add(raw.size());
        out.push(raw);
        if bytes >= PAGE_BYTE_BUDGET {
            return Ok((out, true));
        }
    }
    let more = out.len() == page;
    Ok((out, more))
}

/// An entity alias in the graph export.
#[derive(Serialize)]
struct ExportAlias {
    alias: String,
    /// Whether this alias row is the entity's current name.
    is_name: bool,
}

/// An alias as SQLite's `json_object` writes it (`is_name` is the stored 0/1).
#[derive(Deserialize)]
struct AliasRow {
    alias: String,
    is_name: i64,
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
    /// The stored JSON text verbatim (a JSON string holding it when invalid).
    attributes: Box<RawValue>,
    created_at: String,
    updated_at: String,
    merged_into: Option<String>,
    merged_at: Option<String>,
    deleted_at: Option<String>,
    aliases: Vec<ExportAlias>,
    identifiers: Vec<ExportIdentifier>,
    detail_sources: Vec<ExportDetailSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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

/// A page of entities after `?1` (by id, at most `?2`) with their aliases, identifiers and
/// detail sources, each a JSON array in row order.
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
     FROM entities e WHERE e.id > ?1 ORDER BY e.id LIMIT ?2";

/// A page of claims (id in `(?1, ?2]`, at most `?3`) with their evidence (a JSON array).
const CLAIMS_SQL: &str = "SELECT c.id, c.subject, c.predicate, c.object_entity, c.object_literal,
            c.text, c.confidence, c.status, c.valid_from, c.valid_to, c.created_at, c.updated_at,
            (SELECT json_group_array(json_object('source', v.source, 'key', v.key,
                                                 'chunk_id', v.chunk_id, 'quote', v.quote,
                                                 'created_at', v.created_at))
             FROM (SELECT source, key, chunk_id, quote, created_at
                   FROM claim_evidence WHERE claim_id = c.id ORDER BY id) v)
     FROM claims c WHERE c.id > ?1 AND c.id <= ?2 ORDER BY c.id LIMIT ?3";

/// A page of mentions (id in `(?1, ?2]`, at most `?3`).
const MENTIONS_SQL: &str = "SELECT id, entity_id, source, key, chunk_id, surface, confidence,
            created_at
     FROM mentions WHERE id > ?1 AND id <= ?2 ORDER BY id LIMIT ?3";

fn parse_list<T: serde::de::DeserializeOwned>(
    r: &rusqlite::Row<'_>,
    i: usize,
    what: &str,
) -> Result<T> {
    let json: String = r.get(i).db("read graph export")?;
    serde_json::from_str(&json).map_err(|e| Error::Storage(format!("export: bad {what}: {e}")))
}

fn read_entity(r: &rusqlite::Row<'_>) -> Result<ExportEntity> {
    let (attributes, error) = raw_json(r.get(3).db("read entity")?);
    Ok(ExportEntity {
        id: r.get(0).db("read entity")?,
        kind: r.get(1).db("read entity")?,
        name: r.get(2).db("read entity")?,
        attributes,
        created_at: r.get(4).db("read entity")?,
        updated_at: r.get(5).db("read entity")?,
        merged_into: r.get(6).db("read entity")?,
        merged_at: r.get(7).db("read entity")?,
        deleted_at: r.get(8).db("read entity")?,
        aliases: parse_list::<Vec<AliasRow>>(r, 9, "aliases")?
            .into_iter()
            .map(|a| ExportAlias {
                alias: a.alias,
                is_name: a.is_name != 0,
            })
            .collect(),
        identifiers: parse_list(r, 10, "identifiers")?,
        detail_sources: parse_list(r, 11, "detail sources")?,
        error: error.map(|e| format!("attributes: {e}")),
    })
}

fn read_claim(r: &rusqlite::Row<'_>) -> Result<ExportClaim> {
    Ok(ExportClaim {
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
        evidence: parse_list(r, 12, "evidence")?,
    })
}

fn read_mention(r: &rusqlite::Row<'_>) -> Result<ExportMention> {
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
}

/// Run one page query and map every row.
fn query_page<T>(
    c: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    mut map: impl FnMut(&rusqlite::Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let mut stmt = c.prepare_cached(sql).db("prepare export page")?;
    let mut rows = stmt.query(params).db("query export page")?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().db("read export page")? {
        out.push(map(r)?);
    }
    Ok(out)
}

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

const AUDIT_PAGE_SQL: &str = "SELECT seq, at, actor, action, target, detail, prev_hash, hash
     FROM audit WHERE seq > ?1 AND seq <= ?2 ORDER BY seq LIMIT ?3";

const EGRESS_PAGE_SQL: &str =
    "SELECT * FROM egress WHERE id > ?1 AND id <= ?2 ORDER BY id LIMIT ?3";

/// Whether a CSV cell must be quoted to read back as itself (and as text, not NULL).
fn needs_quotes(cell: &str) -> bool {
    cell.is_empty()
        || cell.starts_with(char::is_whitespace)
        || cell.ends_with(char::is_whitespace)
        || cell.contains(|c: char| c == ',' || c == '"' || c.is_control())
}

/// Write one CSV cell: `None` (SQL NULL) as nothing, text quoted when [`needs_quotes`].
fn write_csv_cell(w: &mut dyn Write, cell: Option<&str>) -> std::io::Result<()> {
    match cell {
        None => Ok(()),
        Some(cell) if needs_quotes(cell) => {
            w.write_all(b"\"")?;
            w.write_all(cell.replace('"', "\"\"").as_bytes())?;
            w.write_all(b"\"")
        }
        Some(cell) => w.write_all(cell.as_bytes()),
    }
}

/// Write one CSV record, CRLF-terminated.
fn write_csv_record<'a>(
    w: &mut dyn Write,
    cells: impl IntoIterator<Item = Option<&'a str>>,
) -> std::io::Result<()> {
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        write_csv_cell(w, cell)?;
    }
    w.write_all(b"\r\n")
}

/// A SQL value as a CSV cell: NULL is `None`, blobs are lowercase hex, invalid UTF-8 is
/// converted lossily.
fn cell_text(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(hex::encode(b)),
    }
}

/// One CSV page: the column names (when asked for) and `(key, cells)` per row.
type CsvPage = (Option<Vec<String>>, Vec<(i64, Vec<Option<String>>)>);

impl Store {
    /// Visit every item in rowid order, reading up to `page` items per query (keyset
    /// pagination; a page also ends early at [`PAGE_BYTE_BUDGET`] bytes of item data). The read
    /// connection is released before `f` runs, so `f` may use the store. Memory is bounded by
    /// one page. Returns the number of items visited.
    ///
    /// Per-page consistent on a live store: each page is one statement (one snapshot); pages
    /// may see different states. Only items whose rowid is at most the largest one when the
    /// export starts are visited, each at most once: an item deleted before its page is read
    /// is skipped, and one inserted (or deleted and re-inserted, which gives it a new rowid)
    /// meanwhile is left out.
    ///
    /// A damaged item does not stop the export: it is passed to `f` with what could be read
    /// and [`ExportItem::error`] set, and logged; callers count those. An error from `f` stops
    /// the export and is returned; whatever `f` wrote so far is then incomplete (write to a
    /// temporary file and rename it on success). `page == 0` is `Error::Invalid`.
    pub fn export_items(
        &self,
        page: usize,
        f: &mut dyn FnMut(ExportItem) -> Result<()>,
    ) -> Result<u64> {
        if page == 0 {
            return Err(Error::Invalid("export page size must be at least 1".into()));
        }
        let max: Option<i64> = self.with_read_conn(|c| {
            c.query_row("SELECT max(id) FROM items", [], |r| r.get(0))
                .db("read item export bound")
        })?;
        let Some(max) = max else {
            return Ok(0);
        };
        let mut after = i64::MIN;
        let mut n = 0u64;
        loop {
            let (rows, more) = self.with_read_conn(|c| read_items_page(c, after, max, page))?;
            let Some(last) = rows.last() else {
                return Ok(n);
            };
            after = last.id;
            for raw in rows {
                let item = raw.into_export();
                if let Some(error) = &item.error {
                    tracing::warn!(item = item.id, %error, "export: item exported with errors");
                }
                f(item)?;
                n += 1;
            }
            if !more {
                return Ok(n);
            }
        }
    }

    /// Write the knowledge graph to `w` as one JSON document:
    /// `{"format":1,"entities":[...],"claims":[...],"mentions":[...]}`, read in keyset pages
    /// (never the whole graph in memory; `w` is written only while no read is open). Entities
    /// carry their `aliases`, `identifiers` and `detail_sources`, claims their `evidence`;
    /// `attributes` is the stored JSON verbatim. Merged-away and deleted entities and claims of
    /// every status are included. Per-page consistent on a live store (see the module docs: a
    /// claim may name an entity created after the entity section was read). Returns counts.
    ///
    /// On `Err` the output is truncated, invalid JSON: write to a temporary file and rename it
    /// on success. Errors of `w` are `Error::Io`.
    pub fn export_graph_json(&self, w: &mut dyn Write) -> Result<GraphExportCounts> {
        self.export_graph_paged(w, GRAPH_PAGE)
    }

    fn export_graph_paged(&self, w: &mut dyn Write, page: usize) -> Result<GraphExportCounts> {
        let limit = i64::try_from(page).unwrap_or(i64::MAX);
        let (max_claim, max_mention): (i64, i64) = self.with_read_conn(|c| {
            c.query_row(
                "SELECT coalesce((SELECT max(id) FROM claims), 0),
                        coalesce((SELECT max(id) FROM mentions), 0)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .db("read graph export bounds")
        })?;
        let mut counts = GraphExportCounts::default();
        write!(w, "{{\"format\":{GRAPH_EXPORT_FORMAT},\"entities\":")?;
        counts.entities = self.write_json_section(
            w,
            String::new(),
            page,
            |c, after| query_page(c, ENTITIES_SQL, params![after, limit], read_entity),
            |e| e.id.clone(),
            |_| {},
        )?;
        w.write_all(b",\"claims\":")?;
        let mut evidence = 0u64;
        counts.claims = self.write_json_section(
            w,
            i64::MIN,
            page,
            |c, after| query_page(c, CLAIMS_SQL, params![after, max_claim, limit], read_claim),
            |c| c.id,
            |c| evidence += c.evidence.len() as u64,
        )?;
        counts.evidence = evidence;
        w.write_all(b",\"mentions\":")?;
        counts.mentions = self.write_json_section(
            w,
            i64::MIN,
            page,
            |c, after| {
                query_page(
                    c,
                    MENTIONS_SQL,
                    params![after, max_mention, limit],
                    read_mention,
                )
            },
            |m| m.id,
            |_| {},
        )?;
        w.write_all(b"}\n")?;
        w.flush()?;
        Ok(counts)
    }

    /// Write a JSON array (`[`, one element per line, `]`) of the rows `read` returns page by
    /// page (`read(conn, after)` gives the page after key `after`; `key` is a row's key). Each
    /// page is serialised and written after its read connection is released.
    fn write_json_section<T: Serialize, K>(
        &self,
        w: &mut dyn Write,
        mut after: K,
        page: usize,
        read: impl Fn(&Connection, &K) -> Result<Vec<T>>,
        key: impl Fn(&T) -> K,
        mut seen: impl FnMut(&T),
    ) -> Result<u64> {
        let mut n = 0u64;
        w.write_all(b"[")?;
        loop {
            let rows = self.with_read_conn(|c| read(c, &after))?;
            let Some(last) = rows.last() else {
                break;
            };
            after = key(last);
            let full = rows.len() >= page;
            let mut buf = Vec::new();
            for row in &rows {
                buf.extend_from_slice(if n == 0 { b"\n" } else { b",\n" });
                serde_json::to_writer(&mut buf, row)?;
                seen(row);
                n += 1;
            }
            drop(rows);
            w.write_all(&buf)?;
            if !full {
                break;
            }
        }
        w.write_all(if n == 0 { b"]" } else { b"\n]" })?;
        Ok(n)
    }

    /// Write the audit log to `w` as CSV, oldest entry first, with the header
    /// `seq,at,actor,action,target,detail,prev_hash,hash` (`detail` is the stored JSON). Read in
    /// keyset pages, per-page consistent; entries appended after the export starts are left
    /// out. Returns the number of entries written.
    ///
    /// On `Err` the output is truncated: write to a temporary file and rename it on success.
    /// Errors of `w` are `Error::Io`.
    pub fn export_audit_csv(&self, w: &mut dyn Write) -> Result<u64> {
        self.export_csv_paged(
            w,
            CSV_PAGE,
            AUDIT_PAGE_SQL,
            "SELECT coalesce(max(seq), 0) FROM audit",
            "seq",
            Some(&AUDIT_COLUMNS),
        )
    }

    /// Write the egress ledger to `w` as CSV, oldest row first: a header row with every column
    /// of the ledger table (today `id,at,host,feature,bytes_sent,bytes_received,items,state,
    /// owner`; `items` is the stored JSON array of item refs), then one record per row. Events
    /// queued but not yet written are flushed first. Read in keyset pages, per-page consistent;
    /// rows added after the export starts are left out. Returns the number of rows written.
    ///
    /// On `Err` the output is truncated: write to a temporary file and rename it on success.
    /// Errors of `w` are `Error::Io`.
    pub fn export_egress_csv(&self, w: &mut dyn Write) -> Result<u64> {
        self.flush_egress();
        self.export_csv_paged(
            w,
            CSV_PAGE,
            EGRESS_PAGE_SQL,
            "SELECT coalesce(max(id), 0) FROM egress",
            "id",
            None,
        )
    }

    /// Write a CSV export page by page: `page_sql` takes `(after, max, limit)` and orders by
    /// the integer column `key`; `max_sql` gives the bound read once at the start. The header
    /// is `header`, or the statement's column names.
    fn export_csv_paged(
        &self,
        w: &mut dyn Write,
        page: usize,
        page_sql: &str,
        max_sql: &str,
        key: &str,
        header: Option<&[&str]>,
    ) -> Result<u64> {
        let limit = i64::try_from(page).unwrap_or(i64::MAX);
        let max: i64 = self.with_read_conn(|c| {
            c.query_row(max_sql, [], |r| r.get(0))
                .db("read csv export bound")
        })?;
        let mut need_header = true;
        if let Some(h) = header {
            write_csv_record(w, h.iter().map(|s| Some(*s)))?;
            need_header = false;
        }
        let mut after = i64::MIN;
        let mut n = 0u64;
        loop {
            let (names, rows): CsvPage = self.with_read_conn(|c| {
                let mut stmt = c.prepare_cached(page_sql).db("prepare csv export")?;
                let names = need_header
                    .then(|| stmt.column_names().into_iter().map(str::to_owned).collect());
                let cols = stmt.column_count();
                let mut q = stmt
                    .query(params![after, max, limit])
                    .db("query csv export")?;
                let mut rows = Vec::new();
                while let Some(r) = q.next().db("read csv export")? {
                    let k: i64 = r.get(key).db("read csv export key")?;
                    let mut cells = Vec::with_capacity(cols);
                    for i in 0..cols {
                        cells.push(cell_text(r.get_ref(i).db("read csv cell")?));
                    }
                    rows.push((k, cells));
                }
                Ok((names, rows))
            })?;
            if let Some(names) = names {
                write_csv_record(w, names.iter().map(|s| Some(s.as_str())))?;
                need_header = false;
            }
            let Some((last, _)) = rows.last() else {
                break;
            };
            after = *last;
            let full = rows.len() >= page;
            let mut buf = Vec::new();
            for (_, cells) in &rows {
                write_csv_record(&mut buf, cells.iter().map(Option::as_deref))?;
                n += 1;
            }
            drop(rows);
            w.write_all(&buf)?;
            if !full {
                break;
            }
        }
        w.flush()?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::io;

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
            .export_items(page, &mut |item| item.write_jsonl(&mut buf))
            .unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn count(store: &Store, sql: &str) -> i64 {
        store
            .with_read_conn(|c| c.query_row(sql, [], |r| r.get(0)).db("count"))
            .unwrap()
    }

    fn exec(store: &Store, sql: &str) {
        store
            .with_conn(|c| c.execute_batch(sql).db("exec"))
            .unwrap();
    }

    fn stored_metadata(store: &Store, id: i64) -> String {
        store
            .with_read_conn(|c| {
                c.query_row("SELECT metadata FROM items WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .db("read metadata")
            })
            .unwrap()
    }

    /// Chunks that cover `text` (so it is stored as a layout), split at a char boundary.
    fn halves(text: &str) -> [NewChunk; 2] {
        let mut mid = text.len() / 2;
        while !text.is_char_boundary(mid) {
            mid += 1;
        }
        let (a, b) = text.split_at(mid);
        [NewChunk::text(a), NewChunk::text(b)]
    }

    fn long_text(tag: &str) -> String {
        (0..40)
            .map(|i| format!("Paragraph {i} of {tag} about things.\n\n"))
            .collect()
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
        let long = long_text("long");
        store
            .upsert_item_with_chunks(
                &NewItem::new("files", "long.md", content_hash(&long)).with_text(long.clone()),
                &halves(&long),
            )
            .unwrap();
        assert_eq!(
            count(&store, "SELECT count(*) FROM item_text"),
            1,
            "the long item's text should be stored as a layout"
        );
        // No text of its own, only chunks (with control characters).
        store
            .upsert_item_with_chunks(
                &NewItem::new("mail", "m1", content_hash("m1")),
                &[
                    NewChunk::text("first, \"chunk\" \u{1}\u{7}\u{1b}[0m"),
                    NewChunk::text("second\nchunk\u{7f}\u{0}\u{85}"),
                ],
            )
            .unwrap();
        // Metadata that is not a JSON object.
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
            assert!(v.get("error").is_none(), "{v}");
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
        assert_eq!(
            serde_json::from_str::<Value>(first.metadata.get()).unwrap(),
            stored.metadata
        );
        assert_eq!(first.metadata.get(), stored_metadata(&store, first.id));
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

        assert_eq!(items[1].text.as_deref(), Some(long.as_str()));
        assert!(items[1].chunks.is_empty());
        assert_eq!(items[2].text, None);
        assert_eq!(
            items[2].chunks,
            [
                "first, \"chunk\" \u{1}\u{7}\u{1b}[0m",
                "second\nchunk\u{7f}\u{0}\u{85}"
            ]
        );
        assert!(values[3].get("chunks").is_none());
        assert_eq!(items[3].metadata.get(), "42");
        assert_eq!(items[3].url, None);
    }

    #[test]
    fn metadata_is_exported_byte_for_byte() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_item(&NewItem::new("s", "a", "h").with_metadata(json!({
                "x": 1.0715660391465826e-75,
                "big": 12345678901234567890u64,
                "neg": -9223372036854775808i64,
                "f": 0.1,
                "e": 1e300,
            })))
            .unwrap();
        add(&store, "b");
        // Written by hand: odd spacing, exponents and key order survive too.
        exec(
            &store,
            r#"UPDATE items SET metadata = '{"b": 1.0E+2,  "a" : [1.50, -0.0], "url":"u"}'
               WHERE key = 'b'"#,
        );
        let items = collect(&store, 10);
        for item in &items {
            assert_eq!(item.metadata.get(), stored_metadata(&store, item.id));
            let line = {
                let mut buf = Vec::new();
                item.write_jsonl(&mut buf).unwrap();
                String::from_utf8(buf).unwrap()
            };
            assert!(line.contains(item.metadata.get()), "{line}");
        }
        assert!(items[0].metadata.get().contains("1.0715660391465826e-75"));
        assert_eq!(
            items[1].metadata.get(),
            r#"{"b": 1.0E+2,  "a" : [1.50, -0.0], "url":"u"}"#
        );
        assert_eq!(items[1].url.as_deref(), Some("u"));
    }

    #[test]
    fn invalid_metadata_is_exported_as_a_string_with_an_error() {
        let store = Store::open_in_memory().unwrap();
        add(&store, "a");
        exec(&store, "UPDATE items SET metadata = 'not json'");
        let items = collect(&store, 10);
        assert_eq!(items[0].metadata.get(), "\"not json\"");
        assert!(items[0].error.as_deref().unwrap().contains("metadata"));
        assert_eq!(items[0].text.as_deref(), Some("a"));
    }

    #[test]
    fn damaged_items_are_exported_with_an_error_and_the_export_goes_on() {
        let store = Store::open_in_memory().unwrap();
        let (one, two) = (long_text("one"), long_text("two"));
        for (key, text) in [("one", &one), ("two", &two)] {
            store
                .upsert_item_with_chunks(
                    &NewItem::new("s", key, content_hash(text)).with_text(text.clone()),
                    &halves(text),
                )
                .unwrap();
        }
        add(&store, "fine");
        assert_eq!(count(&store, "SELECT count(*) FROM item_text"), 2);
        // `one` loses a chunk its layout points at; `two` gets a garbage layout.
        exec(
            &store,
            "DELETE FROM chunks WHERE ord = 1 AND item_id = (SELECT id FROM items WHERE key = 'one');
             UPDATE item_text SET layout = X'FF00'
                 WHERE item_id = (SELECT id FROM items WHERE key = 'two');
             UPDATE items SET title = CAST(X'C328' AS TEXT) WHERE key = 'fine';",
        );
        let items = collect(&store, 1);
        assert_eq!(items.len(), 3);
        let [one_x, two_x, fine] = &items[..] else {
            unreachable!()
        };
        assert_eq!(one_x.text, None);
        assert!(
            one_x
                .error
                .as_deref()
                .unwrap()
                .contains("chunk 1 is missing")
        );
        assert_eq!(one_x.chunks, [halves(&one)[0].text.clone()]);
        assert_eq!(two_x.text, None);
        assert!(two_x.error.is_some());
        assert_eq!(two_x.chunks.concat(), two);
        // Invalid UTF-8 is converted lossily, with a note.
        assert_eq!(fine.text.as_deref(), Some("fine"));
        assert_eq!(fine.title.as_deref(), Some("\u{fffd}("));
        assert!(fine.error.as_deref().unwrap().contains("title"));
        // Still valid JSONL.
        for line in jsonl(&store, 2).lines() {
            serde_json::from_str::<ExportItem>(line).unwrap();
        }
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
    fn pages_stop_at_the_byte_budget() {
        let store = Store::open_in_memory().unwrap();
        let big = "x".repeat(3 << 20);
        for i in 0..12 {
            store
                .upsert_item(
                    &NewItem::new("s", format!("k{i:02}"), content_hash(i.to_string()))
                        .with_text(big.clone()),
                )
                .unwrap();
        }
        let (rows, more) = store
            .with_read_conn(|c| read_items_page(c, i64::MIN, i64::MAX, 500))
            .unwrap();
        assert!(more);
        assert_eq!(rows.len(), PAGE_BYTE_BUDGET.div_ceil(big.len()));
        let keys: Vec<String> = collect(&store, 500).into_iter().map(|i| i.key).collect();
        let want: Vec<String> = (0..12).map(|i| format!("k{i:02}")).collect();
        assert_eq!(keys, want);
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
                        // Inserted after the export started: left out.
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
            let mut want: Vec<String> = (0..12).map(|i| format!("k{i:02}")).collect();
            if page < 12 {
                want.retain(|k| k != "k10");
            }
            assert_eq!(seen, want, "page {page}");
        }
    }

    #[test]
    fn deleted_and_reinserted_items_are_not_exported_twice() {
        for page in [1, 2, 100] {
            let store = Store::open_in_memory().unwrap();
            for i in 0..6 {
                add(&store, &format!("k{i}"));
            }
            let writer = store.clone();
            let mut seen = Vec::new();
            store
                .export_items(page, &mut |item| {
                    // Re-insert the item just exported and one not read yet: both get new
                    // rowids past the bound.
                    if item.key == "k0" {
                        for k in ["k0", "k4"] {
                            writer.delete_item(&iref("src", k)).unwrap();
                            add(&writer, k);
                        }
                    }
                    seen.push(item.key);
                    Ok(())
                })
                .unwrap();
            let unique: HashSet<&String> = seen.iter().collect();
            assert_eq!(unique.len(), seen.len(), "page {page}: {seen:?}");
            assert_eq!(seen.iter().filter(|k| *k == "k0").count(), 1);
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
        new.attributes = json!({"born": 1815, "ratio": 1.0715660391465826e-75});
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
            let expected = GraphExportCounts {
                entities: entities as u64,
                claims: claims as u64,
                evidence: evidence as u64,
                mentions: mentions as u64,
            };
            // Every page size gives the same document.
            let mut docs = Vec::new();
            for page in [1, 2, GRAPH_PAGE] {
                let mut buf = Vec::new();
                assert_eq!(store.export_graph_paged(&mut buf, page).unwrap(), expected);
                docs.push(buf);
            }
            assert!(docs.windows(2).all(|d| d[0] == d[1]));
            let mut buf = Vec::new();
            assert_eq!(store.export_graph_json(&mut buf).unwrap(), expected);
            assert_eq!(buf, docs[0]);
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
            let stored_attributes: String = store
                .with_read_conn(|c| {
                    c.query_row(
                        "SELECT attributes FROM entities WHERE id = ?1",
                        [ada["id"].as_str().unwrap()],
                        |r| r.get(0),
                    )
                    .db("attributes")
                })
                .unwrap();
            let text = String::from_utf8(buf.clone()).unwrap();
            assert!(text.contains(&format!("\"attributes\":{stored_attributes}")));
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

    /// A CSV cell as read back: `None` for an empty unquoted cell (SQL NULL).
    type Cell = Option<String>;

    /// Minimal RFC 4180 reader for the tests: records of cells, CRLF-terminated.
    fn parse_csv(s: &str) -> Vec<Vec<Cell>> {
        let mut records = Vec::new();
        let mut record = Vec::new();
        let mut cell = String::new();
        let mut was_quoted = false;
        let mut chars = s.chars().peekable();
        let mut quoted = false;
        let end_cell = |cell: &mut String, was_quoted: &mut bool, record: &mut Vec<Cell>| {
            let c = std::mem::take(cell);
            record.push((!c.is_empty() || *was_quoted).then_some(c));
            *was_quoted = false;
        };
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
                    assert!(cell.is_empty() && !was_quoted, "stray quote");
                    quoted = true;
                    was_quoted = true;
                }
                ',' => end_cell(&mut cell, &mut was_quoted, &mut record),
                '\r' => {
                    assert_eq!(chars.next(), Some('\n'), "bare CR outside quotes");
                    end_cell(&mut cell, &mut was_quoted, &mut record);
                    records.push(std::mem::take(&mut record));
                }
                c if c.is_control() => panic!("unquoted control character {c:?}"),
                _ => {
                    assert!(!was_quoted, "text after a closing quote");
                    cell.push(ch);
                }
            }
        }
        assert!(
            !quoted && cell.is_empty() && record.is_empty(),
            "truncated CSV"
        );
        records
    }

    fn some(s: &str) -> Cell {
        Some(s.to_owned())
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
            "lone\rcr",
            "=1+2",
            "-starts with dash",
            "@at",
            "\ttab",
            " leading space",
            "trailing space ",
            "bell\u{7}esc\u{1b}nul\u{0}del\u{7f}nel\u{85}",
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
        assert_eq!(
            records[0],
            AUDIT_COLUMNS.iter().map(|c| some(c)).collect::<Vec<_>>()
        );
        assert_eq!(records.len(), nasty.len() + 1);
        let stored = store.recent_audit(100).unwrap();
        for (row, target) in records[1..].iter().zip(nasty) {
            assert_eq!(row.len(), 8);
            let row: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap()).collect();
            let seq: u64 = row[0].parse().unwrap();
            let rec = stored.iter().find(|r| r.seq == seq).unwrap();
            assert_eq!(row[4], target);
            assert_eq!(row[2], rec.actor);
            assert_eq!(row[3], rec.action);
            assert_eq!(row[4], rec.target);
            assert_eq!(serde_json::from_str::<Value>(row[5]).unwrap(), rec.detail);
            assert_eq!(row[6], rec.prev_hash);
            assert_eq!(row[7], rec.hash);
            assert_eq!(parse_time(row[1]).unwrap(), rec.at);
        }
        // Paging does not change the output.
        let mut paged = Vec::new();
        store
            .export_csv_paged(
                &mut paged,
                2,
                AUDIT_PAGE_SQL,
                "SELECT coalesce(max(seq), 0) FROM audit",
                "seq",
                Some(&AUDIT_COLUMNS),
            )
            .unwrap();
        assert_eq!(paged, buf);
        store.verify_audit_chain().unwrap();
    }

    #[test]
    fn egress_csv_has_every_column_and_keeps_null_apart_from_empty() {
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
        assert_eq!(
            header,
            vec![columns.iter().map(|c| some(c)).collect::<Vec<_>>()]
        );
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
        exec(
            &store,
            "INSERT INTO egress (at, host, feature, bytes_sent, bytes_received, items, state, owner)
             VALUES ('2026-01-01T00:00:00Z', 'h', '', 0, 0, '[]', 'done', NULL)",
        );
        let mut buf = Vec::new();
        assert_eq!(store.export_egress_csv(&mut buf).unwrap(), 2);
        let records = parse_csv(std::str::from_utf8(&buf).unwrap());
        assert_eq!(records.len(), 3);
        let row = |i: usize| -> HashMap<String, Cell> {
            records[0]
                .iter()
                .map(|c| c.clone().unwrap())
                .zip(records[i].iter().cloned())
                .collect()
        };
        let first = row(1);
        assert_eq!(first["host"], some("api.example.com"));
        assert_eq!(first["feature"], some(&event.feature));
        assert_eq!(first["bytes_sent"], some("1234"));
        assert_eq!(first["bytes_received"], some("5678"));
        assert_eq!(first["state"], some("done"));
        assert_eq!(
            serde_json::from_str::<Vec<ItemRef>>(first["items"].as_deref().unwrap()).unwrap(),
            event.items
        );
        assert_eq!(
            parse_time(first["at"].as_deref().unwrap()).unwrap(),
            event.at
        );
        let second = row(2);
        assert_eq!(second["feature"], some(""));
        assert_eq!(second["owner"], None);
    }

    #[test]
    fn csv_cells_are_quoted_only_when_needed() {
        let mut buf = Vec::new();
        write_csv_record(
            &mut buf,
            [
                Some("a"),
                Some("b,c"),
                Some("d\"e"),
                Some("f\ng"),
                Some("h\ri"),
                Some("=j"),
                Some(""),
                None,
                Some(" k"),
                Some("l "),
                Some("m\u{1}"),
                Some("n\to"),
                Some("p q"),
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "a,\"b,c\",\"d\"\"e\",\"f\ng\",\"h\ri\",=j,\"\",,\" k\",\"l \",\"m\u{1}\",\
             \"n\to\",p q\r\n"
        );
    }

    /// Accepts `limit` bytes, then fails like a full disk. Keeps what it accepted.
    struct FullDisk {
        limit: usize,
        written: Vec<u8>,
    }

    impl Write for FullDisk {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let room = self.limit - self.written.len();
            if room == 0 {
                return Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"));
            }
            let n = room.min(buf.len());
            self.written.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_errors_are_io_errors_and_leave_truncated_output() {
        let store = Store::open_in_memory().unwrap();
        populate_graph(&store);
        for i in 0..5 {
            add(&store, &format!("k{i}"));
            store
                .append_audit(&AuditEntry {
                    actor: "user".into(),
                    action: "a".into(),
                    target: format!("t{i}"),
                    detail: json!({}),
                })
                .unwrap();
        }
        store
            .record_egress(&EgressEvent {
                at: OffsetDateTime::now_utc(),
                host: "h".into(),
                feature: "f".into(),
                bytes_sent: 1,
                bytes_received: 1,
                items: Vec::new(),
            })
            .unwrap();
        let is_io =
            |e: &Error| matches!(e, Error::Io(io) if io.kind() == io::ErrorKind::StorageFull);

        let mut w = FullDisk {
            limit: 100,
            written: Vec::new(),
        };
        let err = store.export_graph_paged(&mut w, 1).unwrap_err();
        assert!(is_io(&err), "{err:?}");
        assert_eq!(w.written.len(), 100);
        // Documented: the output of a failed export is not a valid document.
        assert!(serde_json::from_slice::<Value>(&w.written).is_err());

        for limit in [0, 10, 60] {
            let mut w = FullDisk {
                limit,
                written: Vec::new(),
            };
            let err = store.export_audit_csv(&mut w).unwrap_err();
            assert!(is_io(&err), "{err:?}");
            let mut w = FullDisk {
                limit,
                written: Vec::new(),
            };
            let err = store.export_egress_csv(&mut w).unwrap_err();
            assert!(is_io(&err), "{err:?}");
            let mut w = FullDisk {
                limit,
                written: Vec::new(),
            };
            let err = store
                .export_items(2, &mut |item| item.write_jsonl(&mut w))
                .unwrap_err();
            assert!(is_io(&err), "{err:?}");
        }
    }

    /// A writer that checkpoints the WAL and writes to the store on every write, recording each
    /// checkpoint's `(busy, log frames, checkpointed frames)`.
    struct Meddler {
        store: Store,
        checkpoints: Vec<(i64, i64, i64)>,
        writes: usize,
    }

    impl Write for Meddler {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            // A write that lands in the WAL, so every checkpoint has frames to copy.
            add(&self.store, &format!("meddle-{}", self.writes));
            let ckpt = self
                .store
                .with_conn(|c| {
                    c.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .db("checkpoint")
                })
                .unwrap();
            self.checkpoints.push(ckpt);
            // Reads through the store work too (on an in-memory store they share the writer).
            self.store.counts().unwrap();
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn exports_hold_no_read_while_the_writer_runs() {
        let dir = tempfile::tempdir().unwrap();
        for store in [file_store(&dir), Store::open_in_memory().unwrap()] {
            populate_graph(&store);
            for i in 0..5 {
                store
                    .append_audit(&AuditEntry {
                        actor: "user".into(),
                        action: "a".into(),
                        target: format!("t{i}"),
                        detail: json!({}),
                    })
                    .unwrap();
                store
                    .record_egress(&EgressEvent {
                        at: OffsetDateTime::now_utc(),
                        host: format!("h{i}"),
                        feature: "f".into(),
                        bytes_sent: 1,
                        bytes_received: 1,
                        items: Vec::new(),
                    })
                    .unwrap();
            }
            let mut w = Meddler {
                store: store.clone(),
                checkpoints: Vec::new(),
                writes: 0,
            };
            store.export_graph_paged(&mut w, 1).unwrap();
            let graph_writes = w.writes;
            assert!(graph_writes > 8, "{graph_writes} writes");
            for (sql, max, key, header) in [
                (
                    AUDIT_PAGE_SQL,
                    "SELECT coalesce(max(seq), 0) FROM audit",
                    "seq",
                    Some(&AUDIT_COLUMNS[..]),
                ),
                (
                    EGRESS_PAGE_SQL,
                    "SELECT coalesce(max(id), 0) FROM egress",
                    "id",
                    None,
                ),
            ] {
                let before = w.writes;
                assert_eq!(
                    store
                        .export_csv_paged(&mut w, 1, sql, max, key, header)
                        .unwrap(),
                    5
                );
                assert!(w.writes - before >= 5);
            }
            if store.is_encrypted() {
                for &(busy, log, done) in &w.checkpoints {
                    assert_eq!(busy, 0);
                    assert!(log > 0);
                    assert_eq!(done, log, "checkpoint blocked by the export");
                }
            }
        }
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
        let mut sink = io::sink();
        store
            .export_items(page, &mut |item| item.write_jsonl(&mut sink))
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
