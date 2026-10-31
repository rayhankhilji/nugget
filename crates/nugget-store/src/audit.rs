//! Tamper-evident audit log: an append-only table whose rows form a keyed hash chain.
//!
//! Every field is length-prefixed (u64 LE) so field boundaries are unambiguous, and
//! `prev_hash(1)` = 64 zeros:
//!
//! - **v2** (current): `hash(n) = HMAC-SHA256(mac_key, "nugget-audit-v2\0" ‖ len‖seq ‖ len‖at ‖
//!   len‖actor ‖ len‖action ‖ len‖target ‖ len‖detail ‖ len‖prev_hash)`, where
//!   `mac_key = HMAC-SHA256(store_key, "nugget-audit-mac-v1")`. Rewriting an entry needs the
//!   store key itself, not just write access to the database.
//! - **v1** (databases written before the keyed chain): the same with plain SHA-256 and the
//!   `"nugget-audit-v1\0"` domain. Entries before the `audit_v2_from` marker (set by the
//!   migration that introduced v2) are still verified this way; later ones must be v2.
//!
//! The latest `(seq, hash)` is kept in `meta` so truncating the tail of the log is detected,
//! and `meta.audit_seal` holds a MAC over that head and the v2 marker, so neither can be moved
//! (for example to pass v2 entries off as v1) without the key. Triggers reject `UPDATE` and
//! `DELETE` on the table; the chain catches edits that bypass them.
//!
//! **Anchor.** Whoever holds the store key can still rewrite the whole log, or restore an older
//! copy of the database. To catch that, each append also records the head (`seq`, `hash`, and a
//! MAC over both) outside the database through an [`AuditAnchor`]: by default the OS keychain,
//! when the store key itself lives there. Verification then fails if the log ends before the
//! anchored entry, or if that entry's hash differs. Anchoring is best effort: a keychain that
//! fails is logged once and skipped, never failing the append.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hmac::{Hmac, Mac};
use nugget_core::{Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::Store;
use crate::egress::limit_i64;
use crate::error::{DbExt, format_time, parse_time};
use crate::key::{KeySource, SecretBackend, StoreKey};
use crate::types::{AuditEntry, AuditRecord};

type HmacSha256 = Hmac<Sha256>;

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const DOMAIN_V1: &[u8] = b"nugget-audit-v1\0";
const DOMAIN_V2: &[u8] = b"nugget-audit-v2\0";
const SEAL_DOMAIN: &[u8] = b"nugget-audit-seal-v1\0";
const ANCHOR_DOMAIN: &[u8] = b"nugget-audit-anchor-v1\0";
/// KDF labels: `HMAC-SHA256(store_key, label)`.
const MAC_KEY_LABEL: &[u8] = b"nugget-audit-mac-v1";
const ANCHOR_ID_LABEL: &[u8] = b"nugget-audit-anchor-id-v1";
/// Prefix of the anchor value, so the format can change later.
const ANCHOR_PREFIX: &str = "v1";
const META_HEAD: &str = "audit_head";
const META_V2_FROM: &str = "audit_v2_from";
const META_SEAL: &str = "audit_seal";

/// Somewhere outside the database to keep the audit chain's head, so a rolled-back or
/// truncated log is detected even by someone holding the store key.
///
/// Values are opaque, short ASCII strings written by the store; an implementation only has to
/// persist the last one. [`Store::open`] anchors in the OS keychain when the store key came from
/// it; use [`Store::set_audit_anchor`] to supply another location (or a fake in tests).
pub trait AuditAnchor: Send + Sync {
    /// The last stored value: `Ok(None)` when nothing was stored yet, `Err` when the backend is
    /// unusable (the anchor is then skipped).
    fn load(&self) -> std::result::Result<Option<String>, String>;
    /// Replace the stored value.
    fn store(&self, value: &str) -> std::result::Result<(), String>;
}

/// [`AuditAnchor`] over a keychain entry.
struct KeychainAnchor(Box<dyn SecretBackend + Send + Sync>);

impl AuditAnchor for KeychainAnchor {
    fn load(&self) -> std::result::Result<Option<String>, String> {
        Ok(self.0.get()?.map(|v| v.as_str().to_owned()))
    }

    fn store(&self, value: &str) -> std::result::Result<(), String> {
        self.0.set(value)
    }
}

/// Per-store audit state: the MAC key and the head anchor.
pub(crate) struct AuditState {
    mac_key: RwLock<Zeroizing<[u8; 32]>>,
    anchor: Mutex<AnchorSlot>,
    /// Anchor failures are logged once per store, not on every append.
    anchor_warned: AtomicBool,
}

#[derive(Default)]
struct AnchorSlot {
    backend: Option<Arc<dyn AuditAnchor>>,
    /// The anchor already stored was checked against this database.
    checked: bool,
    /// The stored anchor does not match this database: never overwrite it (it is the evidence).
    conflict: bool,
    /// Highest seq written to the anchor by this process.
    written: i64,
}

impl AuditState {
    /// State with a random MAC key and no anchor, for stores without a key (in-memory).
    pub(crate) fn ephemeral() -> Result<Self> {
        let mut key = Zeroizing::new([0u8; 32]);
        getrandom::fill(key.as_mut_slice())
            .map_err(|e| Error::Other(format!("OS random number generator failed: {e}")))?;
        Ok(Self {
            mac_key: RwLock::new(key),
            anchor: Mutex::new(AnchorSlot::default()),
            anchor_warned: AtomicBool::new(false),
        })
    }

    fn mac_key(&self) -> Zeroizing<[u8; 32]> {
        self.mac_key
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn backend(&self) -> Option<Arc<dyn AuditAnchor>> {
        self.anchor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .backend
            .clone()
    }

    fn warn_anchor(&self, error: &str) {
        if !self.anchor_warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                error,
                "cannot use the audit anchor; the audit head is not anchored"
            );
        }
    }
}

fn kdf(key: &[u8], label: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(label);
    mac.finalize().into_bytes().into()
}

fn mac_hex(key: &[u8; 32], domain: &[u8], fields: &[&str]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(domain);
    for field in fields {
        mac.update(&(field.len() as u64).to_le_bytes());
        mac.update(field.as_bytes());
    }
    hex::encode(mac.finalize().into_bytes())
}

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

/// How an entry is linked: v1 = plain SHA-256, v2 = HMAC with the audit key.
enum Link<'a> {
    V1,
    V2(&'a [u8; 32]),
}

#[allow(clippy::too_many_arguments)]
fn chain_hash(
    link: &Link<'_>,
    seq: i64,
    at: &str,
    actor: &str,
    action: &str,
    target: &str,
    detail: &str,
    prev: &str,
) -> String {
    let seq = seq.to_string();
    let fields = [seq.as_str(), at, actor, action, target, detail, prev];
    match link {
        Link::V1 => {
            let mut h = Sha256::new();
            h.update(DOMAIN_V1);
            for field in fields {
                h.update((field.len() as u64).to_le_bytes());
                h.update(field.as_bytes());
            }
            hex::encode(h.finalize())
        }
        Link::V2(key) => mac_hex(key, DOMAIN_V2, &fields),
    }
}

impl RawRecord {
    fn expected_hash(&self, link: &Link<'_>) -> String {
        chain_hash(
            link,
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

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .db("read audit metadata")
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .db("write audit metadata")?;
    Ok(())
}

/// First seq chained with v2. Set by the migration that introduced v2, so it is always present.
fn v2_from(conn: &Connection) -> Result<i64> {
    let raw = meta(conn, META_V2_FROM)?.ok_or_else(|| {
        Error::Integrity("audit log: chain version marker is missing (metadata modified)".into())
    })?;
    raw.parse().map_err(|_| {
        Error::Integrity("audit log: chain version marker is malformed (metadata modified)".into())
    })
}

fn seal(key: &[u8; 32], v2_from: i64, head: Option<&str>) -> String {
    mac_hex(
        key,
        SEAL_DOMAIN,
        &[&v2_from.to_string(), head.unwrap_or_default()],
    )
}

fn anchor_value(key: &[u8; 32], seq: i64, hash: &str) -> String {
    let seq = seq.to_string();
    let tag = mac_hex(key, ANCHOR_DOMAIN, &[&seq, hash]);
    format!("{ANCHOR_PREFIX}:{seq}:{hash}:{tag}")
}

/// Parse and authenticate an anchor value: `(seq, hash)`.
fn parse_anchor(key: &[u8; 32], value: &str) -> Result<(i64, String)> {
    let bad = || {
        Error::Integrity(
            "audit log: the anchored chain head is malformed or was not written for this store"
                .into(),
        )
    };
    let mut parts = value.trim().split(':');
    let (Some(ANCHOR_PREFIX), Some(seq), Some(hash), Some(_), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(bad());
    };
    let seq: i64 = seq.parse().map_err(|_| bad())?;
    // Seq 0 (with the genesis hash) anchors an empty log, after a forced reset.
    let empty_ok = seq != 0 || hash == GENESIS;
    if seq < 0 || !empty_ok || anchor_value(key, seq, hash) != value.trim() {
        return Err(bad());
    }
    Ok((seq, hash.to_owned()))
}

/// Check an authenticated anchor against the log: the anchored entry must still be there,
/// unchanged.
fn check_anchor(conn: &Connection, anchor: (i64, &str)) -> Result<()> {
    let (seq, hash) = anchor;
    if seq == 0 {
        // "No entries yet": every log extends it.
        return Ok(());
    }
    let last: i64 = conn
        .query_row("SELECT coalesce(max(seq), 0) FROM audit", [], |r| r.get(0))
        .db("read audit tail")?;
    if seq > last {
        return Err(Error::Integrity(format!(
            "audit log: ends at entry {last} but the anchored head is entry {seq} (log truncated \
             or rolled back)"
        )));
    }
    let stored: Option<String> = conn
        .query_row("SELECT hash FROM audit WHERE seq = ?1", [seq], |r| r.get(0))
        .optional()
        .db("read anchored audit entry")?;
    if stored.as_deref() != Some(hash) {
        return Err(Error::Integrity(format!(
            "audit log: entry {seq} differs from the anchored head (log rewritten or rolled back)"
        )));
    }
    Ok(())
}

/// Walk the chain and check sequence continuity, links, hashes and the stored head.
fn verify_chain(conn: &Connection, key: &[u8; 32], v2_from: i64) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("SELECT {COLUMNS} FROM audit ORDER BY seq"))
        .db("prepare audit scan")?;
    let mut rows = stmt.query([]).db("scan audit")?;
    let mut expected_seq = 1i64;
    let mut prev = GENESIS.to_owned();
    while let Some(row) = rows.next().db("scan audit")? {
        let rec = read_raw(row).db("scan audit")?;
        if rec.seq != expected_seq {
            return Err(Error::Integrity(format!(
                "audit log: expected entry {expected_seq}, found {} (entries removed or inserted)",
                rec.seq
            )));
        }
        if rec.prev_hash != prev {
            return Err(Error::Integrity(format!(
                "audit log: entry {} does not link to the previous entry",
                rec.seq
            )));
        }
        let link = if rec.seq < v2_from {
            Link::V1
        } else {
            Link::V2(key)
        };
        if rec.expected_hash(&link) != rec.hash {
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
    if meta(conn, META_HEAD)? != expected_head {
        return Err(Error::Integrity(
            "audit log: chain head does not match the last entry (log truncated or head modified)"
                .into(),
        ));
    }
    Ok(())
}

/// Account name of a store's anchor in the OS keychain. Derived from the store key, so it
/// follows the database when it moves and separate stores never share an anchor.
fn anchor_account(key: &StoreKey) -> String {
    let id = Zeroizing::new(kdf(key.as_bytes(), ANCHOR_ID_LABEL));
    format!("audit-head-{}", hex::encode(&id[..8]))
}

/// Switch a freshly opened store to the MAC key derived from `key`, pick the default anchor,
/// and seal a log written before the keyed chain existed.
pub(crate) fn attach_store_key(store: &Store, key: &StoreKey) -> Result<()> {
    let state = store.audit_state();
    let mac_key = Zeroizing::new(kdf(key.as_bytes(), MAC_KEY_LABEL));
    *state
        .mac_key
        .write()
        .unwrap_or_else(PoisonError::into_inner) = mac_key.clone();

    if key.source() == KeySource::Keychain {
        match crate::key::keychain_entry(anchor_account(key)) {
            Some(entry) => {
                store.set_audit_anchor(Some(Arc::new(KeychainAnchor(entry))));
            }
            None => tracing::info!("OS keychain disabled; the audit head is not anchored"),
        }
    } else {
        tracing::debug!(
            source = ?key.source(),
            "store key is not in the OS keychain; the audit head is not anchored"
        );
    }

    store.with_conn(|c| {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .db("begin audit seal")?;
        if meta(&tx, META_SEAL)?.is_some() {
            return Ok(());
        }
        let from = v2_from(&tx)?;
        // Only seal a log that verifies: sealing a tampered one would bless it.
        match verify_chain(&tx, &mac_key, from) {
            Ok(()) => {
                let head = meta(&tx, META_HEAD)?;
                set_meta(&tx, META_SEAL, &seal(&mac_key, from, head.as_deref()))?;
                tx.commit().db("commit audit seal")
            }
            Err(Error::Integrity(e)) => {
                tracing::warn!(error = %e, "audit log does not verify; leaving it unsealed");
                Ok(())
            }
            Err(e) => Err(e),
        }
    })
}

impl Store {
    /// Keep the audit chain's head in `anchor` from now on (`None`: stop anchoring), replacing
    /// the default chosen by [`Store::open`]. Existing anchor contents are checked before they
    /// are first overwritten and kept if they do not match this log.
    pub fn set_audit_anchor(&self, anchor: Option<Arc<dyn AuditAnchor>>) {
        let state = self.audit_state();
        *state.anchor.lock().unwrap_or_else(PoisonError::into_inner) = AnchorSlot {
            backend: anchor,
            ..AnchorSlot::default()
        };
        state.anchor_warned.store(false, Ordering::Relaxed);
    }

    /// Write-through of the new head to the anchor, after the append committed. Failures are
    /// logged, never returned: the entry is already durable.
    fn anchor_head(&self, seq: i64, hash: &str) {
        let state = self.audit_state();
        let mut slot = state.anchor.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(backend) = slot.backend.clone() else {
            return;
        };
        if slot.conflict || seq <= slot.written {
            return;
        }
        let key = state.mac_key();
        if !slot.checked {
            match backend.load() {
                Err(e) => return state.warn_anchor(&e),
                Ok(Some(value)) => {
                    let checked = parse_anchor(&key, &value).and_then(|(aseq, ahash)| {
                        self.with_conn(|c| check_anchor(c, (aseq, &ahash)))
                    });
                    if let Err(e) = checked {
                        tracing::error!(
                            error = %e,
                            "audit anchor does not match this log; keeping it as evidence"
                        );
                        slot.conflict = true;
                        return;
                    }
                }
                Ok(None) => {}
            }
            slot.checked = true;
        }
        match backend.store(&anchor_value(&key, seq, hash)) {
            Ok(()) => slot.written = seq,
            Err(e) => state.warn_anchor(&e),
        }
    }

    /// Append an entry to the audit log, chained to the previous entry. Returns the stored record.
    pub fn append_audit(&self, entry: &AuditEntry) -> Result<AuditRecord> {
        if entry.actor.is_empty() || entry.action.is_empty() {
            return Err(Error::Invalid(
                "audit actor and action must be non-empty".into(),
            ));
        }
        let at = format_time(OffsetDateTime::now_utc())?;
        let detail = serde_json::to_string(&entry.detail)?;
        let key = self.audit_state().mac_key();
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
            let from = v2_from(&tx)?;
            if seq < from {
                return Err(Error::Integrity(format!(
                    "audit log: ends at entry {last_seq} before the keyed chain starts at {from} \
                     (log truncated)"
                )));
            }
            let hash = chain_hash(
                &Link::V2(&key),
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
            let head = format!("{seq}:{hash}");
            set_meta(&tx, META_HEAD, &head)?;
            set_meta(&tx, META_SEAL, &seal(&key, from, Some(&head)))?;
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
        self.anchor_head(raw.seq, &raw.hash);
        raw.into_record()
    }

    /// The most recent audit records, newest first.
    pub fn recent_audit(&self, limit: usize) -> Result<Vec<AuditRecord>> {
        let raws: Vec<RawRecord> = self.with_read_conn(|c| {
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
    /// record hash (SHA-256 before the keyed chain started, HMAC after), the stored chain head
    /// and its seal, and, when an anchor holds a head, that the log still contains that entry
    /// unchanged. Returns `Error::Integrity` naming the first problem.
    pub fn verify_audit_chain(&self) -> Result<()> {
        let state = self.audit_state();
        let key = state.mac_key();
        // Read the anchor outside the database lock: a keychain can be slow.
        let anchor = match state.backend().map(|b| b.load()) {
            Some(Ok(Some(value))) => Some(parse_anchor(&key, &value)?),
            Some(Ok(None)) | None => None,
            Some(Err(e)) => {
                state.warn_anchor(&e);
                None
            }
        };
        self.with_conn(|c| {
            verify_log(c, &key)?;
            match &anchor {
                Some((seq, hash)) => check_anchor(c, (*seq, hash)),
                None => Ok(()),
            }
        })
    }
}

/// Everything [`Store::verify_audit_chain`] checks except the anchor: the chain, the stored head
/// and its seal.
fn verify_log(c: &Connection, key: &[u8; 32]) -> Result<()> {
    let from = v2_from(c)?;
    verify_chain(c, key, from)?;
    let head = meta(c, META_HEAD)?;
    match meta(c, META_SEAL)? {
        Some(s) if s == seal(key, from, head.as_deref()) => Ok(()),
        None if head.is_none() => Ok(()),
        _ => Err(Error::Integrity(
            "audit log: chain head or version marker was modified (seal mismatch)".into(),
        )),
    }
}

/// Point the anchor at this log's current head after a restore (see
/// [`Store::reset_audit_anchor`] for when it refuses and what `force` overrides). Verifies the
/// log first, ignoring the anchor, and leaves the anchor untouched on any refusal. The new value
/// is stored before this returns; for an empty log it is the seq-0 "no entries yet" value.
pub(crate) fn reset_anchor(store: &Store, force: bool) -> Result<()> {
    let state = store.audit_state();
    let key = state.mac_key();
    // Held throughout, so an append cannot anchor in between (it waits, then writes its newer
    // head: `written` stays below its seq).
    let mut slot = state.anchor.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(backend) = slot.backend.clone() else {
        return store.with_conn(|c| verify_log(c, &key));
    };
    let stored = backend
        .load()
        .map_err(|e| Error::Storage(format!("cannot read the audit anchor: {e}")))?;
    let (head, anchored_entry) = store.with_conn(|c| {
        verify_log(c, &key)?;
        let head = c
            .query_row(
                "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .db("read audit tail")?;
        // The anchor as parsed, and the hash the log has at the anchored seq.
        let anchored = match stored.as_deref().map(|v| parse_anchor(&key, v)) {
            Some(Ok((seq, hash))) => {
                let in_log: Option<String> = c
                    .query_row("SELECT hash FROM audit WHERE seq = ?1", [seq], |r| r.get(0))
                    .optional()
                    .db("read anchored audit entry")?;
                Some(Ok((seq, hash, in_log)))
            }
            Some(Err(e)) => Some(Err(e)),
            None => None,
        };
        Ok((head, anchored))
    })?;
    let last = head.as_ref().map_or(0, |h| h.0);
    if !force {
        match &anchored_entry {
            None => {}
            Some(Err(e)) => {
                return Err(Error::Integrity(format!(
                    "{e}; not resetting it without force"
                )));
            }
            Some(Ok((0, _, _))) => {}
            Some(Ok((_, _, _))) if last == 0 => {
                return Err(Error::Integrity(
                    "audit log: the log is empty but an anchor names an entry; not resetting it \
                     without force"
                        .into(),
                ));
            }
            // Ahead of the tail: a genuine rollback to an older copy.
            Some(Ok((seq, _, _))) if *seq > last => {}
            Some(Ok((_, hash, Some(in_log)))) if in_log == hash => {}
            Some(Ok((seq, _, _))) => {
                return Err(Error::Integrity(format!(
                    "audit log: entry {seq} differs from the anchored head, so the log was \
                     rewritten rather than rolled back; not resetting the anchor without force"
                )));
            }
        }
    }
    let value = match &head {
        Some((seq, hash)) => anchor_value(&key, *seq, hash),
        None => anchor_value(&key, 0, GENESIS),
    };
    backend
        .store(&value)
        .map_err(|e| Error::Storage(format!("cannot reset the audit anchor: {e}")))?;
    slot.written = last;
    slot.checked = true;
    slot.conflict = false;
    state.anchor_warned.store(false, Ordering::Relaxed);
    tracing::warn!(
        seq = last,
        force,
        "audit anchor reset to this log's head after a restore"
    );
    Ok(())
}
