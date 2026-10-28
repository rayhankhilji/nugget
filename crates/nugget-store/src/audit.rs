//! Tamper-evident audit log: an append-only table whose rows form a SHA-256 hash chain.
//!
//! `hash(n) = SHA-256(domain ‖ len‖seq ‖ len‖at ‖ len‖actor ‖ len‖action ‖ len‖target ‖
//! len‖detail ‖ len‖prev_hash)`, with every field length-prefixed (u64 LE) so field boundaries
//! are unambiguous, and `prev_hash(1)` = 64 zeros. The latest `(seq, hash)` is also kept in
//! `meta` so truncating the tail of the log is detected too. Triggers reject `UPDATE` and
//! `DELETE` on the table; the chain catches edits that bypass them.

use nugget_core::{Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::Store;
use crate::egress::limit_i64;
use crate::error::{DbExt, format_time, parse_time};
use crate::types::{AuditEntry, AuditRecord};

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const DOMAIN: &[u8] = b"nugget-audit-v1\0";
const META_HEAD: &str = "audit_head";

struct RawRecord {
    seq: i64,
    at: String,
    actor: String,
    action: String,
    target: String,
    detail: String,
    prev_hash: String,
    hash: String,
}

const COLUMNS: &str = "seq, at, actor, action, target, detail, prev_hash, hash";

fn read_raw(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecord> {
    Ok(RawRecord {
        seq: r.get(0)?,
        at: r.get(1)?,
        actor: r.get(2)?,
        action: r.get(3)?,
        target: r.get(4)?,
        detail: r.get(5)?,
        prev_hash: r.get(6)?,
        hash: r.get(7)?,
    })
}

fn chain_hash(
    seq: i64,
    at: &str,
    actor: &str,
    action: &str,
    target: &str,
    detail: &str,
    prev: &str,
) -> String {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    let seq = seq.to_string();
    for field in [seq.as_str(), at, actor, action, target, detail, prev] {
        h.update((field.len() as u64).to_le_bytes());
        h.update(field.as_bytes());
    }
    hex::encode(h.finalize())
}

impl RawRecord {
    fn expected_hash(&self) -> String {
        chain_hash(
            self.seq,
            &self.at,
            &self.actor,
            &self.action,
            &self.target,
            &self.detail,
            &self.prev_hash,
        )
    }

    fn into_record(self) -> Result<AuditRecord> {
        Ok(AuditRecord {
            seq: self.seq as u64,
            at: parse_time(&self.at)?,
            actor: self.actor,
            action: self.action,
            target: self.target,
            detail: serde_json::from_str(&self.detail)?,
            prev_hash: self.prev_hash,
            hash: self.hash,
        })
    }
}

fn head(conn: &Connection) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [META_HEAD], |r| {
        r.get(0)
    })
    .optional()
    .db("read audit head")
}

impl Store {
    /// Append an entry to the audit log, chained to the previous entry. Returns the stored record.
    pub fn append_audit(&self, entry: &AuditEntry) -> Result<AuditRecord> {
        if entry.actor.is_empty() || entry.action.is_empty() {
            return Err(Error::Invalid(
                "audit actor and action must be non-empty".into(),
            ));
        }
        let at = format_time(OffsetDateTime::now_utc())?;
        let detail = serde_json::to_string(&entry.detail)?;
        let raw = self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin audit append")?;
            let (last_seq, prev_hash) = tx
                .query_row(
                    "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .db("read audit tail")?
                .unwrap_or((0, GENESIS.to_owned()));
            let seq = last_seq + 1;
            let hash = chain_hash(
                seq,
                &at,
                &entry.actor,
                &entry.action,
                &entry.target,
                &detail,
                &prev_hash,
            );
            tx.execute(
                &format!("INSERT INTO audit ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
                params![
                    seq,
                    at,
                    entry.actor,
                    entry.action,
                    entry.target,
                    detail,
                    prev_hash,
                    hash
                ],
            )
            .db("append audit entry")?;
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![META_HEAD, format!("{seq}:{hash}")],
            )
            .db("update audit head")?;
            tx.commit().db("commit audit append")?;
            Ok(RawRecord {
                seq,
                at: at.clone(),
                actor: entry.actor.clone(),
                action: entry.action.clone(),
                target: entry.target.clone(),
                detail: detail.clone(),
                prev_hash,
                hash,
            })
        })?;
        raw.into_record()
    }

    /// The most recent audit records, newest first.
    pub fn recent_audit(&self, limit: usize) -> Result<Vec<AuditRecord>> {
        let raws: Vec<RawRecord> = self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM audit ORDER BY seq DESC LIMIT ?1"
                ))
                .db("prepare audit query")?;
            let rows = stmt
                .query_map([limit_i64(limit)], read_raw)
                .db("read audit")?;
            rows.collect::<rusqlite::Result<_>>().db("read audit")
        })?;
        raws.into_iter().map(RawRecord::into_record).collect()
    }

    /// Walk the whole audit log and check sequence continuity, every `prev_hash` link, every
    /// record hash and the stored chain head. Returns `Error::Integrity` naming the first bad
    /// record.
    pub fn verify_audit_chain(&self) -> Result<()> {
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(&format!("SELECT {COLUMNS} FROM audit ORDER BY seq"))
                .db("prepare audit scan")?;
            let mut rows = stmt.query([]).db("scan audit")?;
            let mut expected_seq = 1i64;
            let mut prev = GENESIS.to_owned();
            while let Some(row) = rows.next().db("scan audit")? {
                let rec = read_raw(row).db("scan audit")?;
                if rec.seq != expected_seq {
                    return Err(Error::Integrity(format!(
                        "audit log: expected entry {expected_seq}, found {} (entries removed or \
                         inserted)",
                        rec.seq
                    )));
                }
                if rec.prev_hash != prev {
                    return Err(Error::Integrity(format!(
                        "audit log: entry {} does not link to the previous entry",
                        rec.seq
                    )));
                }
                if rec.expected_hash() != rec.hash {
                    return Err(Error::Integrity(format!(
                        "audit log: entry {} was modified (hash mismatch)",
                        rec.seq
                    )));
                }
                prev = rec.hash;
                expected_seq += 1;
            }
            let last_seq = expected_seq - 1;
            let expected_head = (last_seq > 0).then(|| format!("{last_seq}:{prev}"));
            if head(c)? != expected_head {
                return Err(Error::Integrity(
                    "audit log: chain head does not match the last entry (log truncated or \
                     head modified)"
                        .into(),
                ));
            }
            Ok(())
        })
    }
}
