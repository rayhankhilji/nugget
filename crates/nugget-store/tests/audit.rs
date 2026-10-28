//! Keyed audit chain: HMAC links, the head seal, the v1 → v2 upgrade and the external anchor.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nugget_store::{AuditAnchor, AuditEntry, Error, Store, StoreKey};
use serde_json::json;
use sha2::{Digest, Sha256};

const KEY: [u8; 32] = [7u8; 32];
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn key() -> StoreKey {
    StoreKey::from_bytes(KEY)
}

fn db_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("data").join("nugget.db")
}

/// Open the encrypted file with plain rusqlite, as an attacker holding the store key could.
fn raw_conn(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(KEY)))
        .unwrap();
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS audit_no_update; DROP TRIGGER IF EXISTS audit_no_delete;",
    )
    .unwrap();
    conn
}

fn audit(i: u32) -> AuditEntry {
    AuditEntry {
        actor: "agent".into(),
        action: "item.delete".into(),
        target: format!("files:x/doc{i}"),
        detail: json!({"i": i}),
    }
}

/// The unkeyed v1 link: what someone without the audit key can compute.
fn v1_hash(fields: [&str; 7]) -> String {
    let mut h = Sha256::new();
    h.update(b"nugget-audit-v1\0");
    for f in fields {
        h.update((f.len() as u64).to_le_bytes());
        h.update(f.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Re-link every entry from `from` on with v1 hashes and fix the stored head, as a forger
/// without the audit key would.
fn relink_v1(conn: &rusqlite::Connection, from: i64) {
    let rows: Vec<(i64, String, String, String, String, String)> = conn
        .prepare("SELECT seq, at, actor, action, target, detail FROM audit ORDER BY seq")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut prev = GENESIS.to_owned();
    for (seq, at, actor, action, target, detail) in rows {
        if seq >= from {
            let hash = v1_hash([
                &seq.to_string(),
                &at,
                &actor,
                &action,
                &target,
                &detail,
                &prev,
            ]);
            conn.execute(
                "UPDATE audit SET prev_hash = ?1, hash = ?2 WHERE seq = ?3",
                rusqlite::params![prev, hash, seq],
            )
            .unwrap();
        }
        prev = conn
            .query_row("SELECT hash FROM audit WHERE seq = ?1", [seq], |r| r.get(0))
            .unwrap();
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'audit_head'",
            [format!("{seq}:{prev}")],
        )
        .unwrap();
    }
}

fn integrity_error(s: &Store) -> String {
    match s.verify_audit_chain() {
        Err(Error::Integrity(m)) => m,
        other => panic!("expected an integrity error, got {other:?}"),
    }
}

/// In-memory stand-in for the OS keychain entry.
#[derive(Default)]
struct FakeAnchor(Mutex<Option<String>>);

impl FakeAnchor {
    fn value(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
}

impl AuditAnchor for FakeAnchor {
    fn load(&self) -> Result<Option<String>, String> {
        Ok(self.value())
    }
    fn store(&self, value: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(value.to_owned());
        Ok(())
    }
}

struct BrokenAnchor;

impl AuditAnchor for BrokenAnchor {
    fn load(&self) -> Result<Option<String>, String> {
        Err("no secret service on this bus".into())
    }
    fn store(&self, _: &str) -> Result<(), String> {
        Err("no secret service on this bus".into())
    }
}

fn open_anchored(path: &Path, anchor: &Arc<FakeAnchor>) -> Store {
    let s = Store::open(path, &key()).unwrap();
    s.set_audit_anchor(Some(anchor.clone()));
    s
}

#[test]
fn entries_are_keyed_not_plain_sha256() {
    let s = Store::open_in_memory().unwrap();
    let r1 = s.append_audit(&audit(1)).unwrap();
    let at = time::format_description::well_known::Rfc3339;
    let plain = v1_hash([
        "1",
        &r1.at.format(&at).unwrap(),
        &r1.actor,
        &r1.action,
        &r1.target,
        &serde_json::to_string(&r1.detail).unwrap(),
        GENESIS,
    ]);
    assert_ne!(r1.hash, plain);
    assert_eq!(r1.hash.len(), 64);
    s.verify_audit_chain().unwrap();
}

#[test]
fn rewriting_the_chain_without_the_audit_key_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let s = Store::open(&path, &key()).unwrap();
    for i in 1..=4 {
        s.append_audit(&audit(i)).unwrap();
    }
    s.verify_audit_chain().unwrap();
    let raw = raw_conn(&path);

    // Edit entry 2 and re-link it and everything after it, head included, with plain SHA-256.
    raw.execute("UPDATE audit SET detail = '{\"i\":99}' WHERE seq = 2", [])
        .unwrap();
    relink_v1(&raw, 2);
    let m = integrity_error(&s);
    assert!(m.contains("entry 2 was modified"), "{m}");

    // Passing the whole log off as pre-upgrade v1 entries needs moving the version marker,
    // which the seal catches.
    raw.execute(
        "UPDATE meta SET value = '5' WHERE key = 'audit_v2_from'",
        [],
    )
    .unwrap();
    relink_v1(&raw, 1);
    let m = integrity_error(&s);
    assert!(m.contains("seal"), "{m}");

    // Dropping the seal altogether does not help either.
    raw.execute("DELETE FROM meta WHERE key = 'audit_seal'", [])
        .unwrap();
    let m = integrity_error(&s);
    assert!(m.contains("seal"), "{m}");
    drop(s);

    // Nor does reopening: an unsealed log that fails verification is not sealed on open.
    raw.execute(
        "UPDATE meta SET value = '1' WHERE key = 'audit_v2_from'",
        [],
    )
    .unwrap();
    let s = Store::open(&path, &key()).unwrap();
    let m = integrity_error(&s);
    assert!(m.contains("entry 1 was modified"), "{m}");
}

#[test]
fn a_different_store_key_cannot_verify_or_extend_the_chain() {
    // Same database contents, but a MAC key from another store key: the chain does not verify.
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let s = Store::open(&path, &key()).unwrap();
    s.append_audit(&audit(1)).unwrap();
    drop(s);
    raw_conn(&path)
        .execute_batch(&format!(
            "PRAGMA rekey = \"x'{}'\";",
            hex::encode([9u8; 32])
        ))
        .unwrap();
    let s = Store::open(&path, &StoreKey::from_bytes([9u8; 32])).unwrap();
    let m = integrity_error(&s);
    assert!(m.contains("entry 1 was modified"), "{m}");
}

#[test]
fn legacy_v1_entries_verify_and_new_ones_are_keyed() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    drop(Store::open(&path, &key()).unwrap());

    // The state the keyed-chain migration leaves behind on an older database: v1 entries, the
    // v1 head, the v2 marker just past them, and no seal yet.
    {
        let raw = raw_conn(&path);
        let mut prev = GENESIS.to_owned();
        for seq in 1..=2 {
            let at = "2026-01-01T00:00:00Z";
            let detail = format!("{{\"i\":{seq}}}");
            let fields = [
                seq.to_string(),
                at.into(),
                "agent".into(),
                "item.delete".into(),
                format!("files:x/doc{seq}"),
                detail.clone(),
                prev.clone(),
            ];
            let hash = v1_hash(fields.each_ref().map(String::as_str));
            raw.execute(
                "INSERT INTO audit (seq, at, actor, action, target, detail, prev_hash, hash)
                 VALUES (?1, ?2, 'agent', 'item.delete', ?3, ?4, ?5, ?6)",
                rusqlite::params![seq, at, fields[4], detail, prev, hash],
            )
            .unwrap();
            prev = hash;
        }
        raw.execute_batch(&format!(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('audit_head', '2:{prev}');
             INSERT OR REPLACE INTO meta (key, value) VALUES ('audit_v2_from', '3');
             DELETE FROM meta WHERE key = 'audit_seal';"
        ))
        .unwrap();
    }

    // Opening with the store key verifies the legacy chain and seals it.
    let s = Store::open(&path, &key()).unwrap();
    s.verify_audit_chain().unwrap();
    let old = s.recent_audit(10).unwrap();
    let r3 = s.append_audit(&audit(3)).unwrap();
    assert_eq!(r3.seq, 3);
    assert_eq!(r3.prev_hash, old[0].hash);
    let at = time::format_description::well_known::Rfc3339;
    let plain = v1_hash([
        "3",
        &r3.at.format(&at).unwrap(),
        &r3.actor,
        &r3.action,
        &r3.target,
        &serde_json::to_string(&r3.detail).unwrap(),
        &r3.prev_hash,
    ]);
    assert_ne!(r3.hash, plain, "entries after the marker are keyed");
    s.verify_audit_chain().unwrap();
    drop(s);
    let s = Store::open(&path, &key()).unwrap();
    s.verify_audit_chain().unwrap();

    // Old entries are still protected by their v1 links.
    raw_conn(&path)
        .execute(
            "UPDATE audit SET target = 'files:x/other' WHERE seq = 1",
            [],
        )
        .unwrap();
    let m = integrity_error(&s);
    assert!(m.contains("entry 1 was modified"), "{m}");
}

#[test]
fn rollback_to_an_older_copy_is_detected_by_the_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let backup = dir.path().join("backup.db");
    let anchor = Arc::new(FakeAnchor::default());

    let s = open_anchored(&path, &anchor);
    for i in 1..=3 {
        s.append_audit(&audit(i)).unwrap();
    }
    s.verify_audit_chain().unwrap();
    assert!(anchor.value().unwrap().starts_with("v1:3:"));
    drop(s);
    std::fs::copy(&path, &backup).unwrap();

    let s = open_anchored(&path, &anchor);
    s.verify_audit_chain().unwrap();
    for i in 4..=5 {
        s.append_audit(&audit(i)).unwrap();
    }
    s.verify_audit_chain().unwrap();
    drop(s);
    let anchored = anchor.value().unwrap();
    assert!(anchored.starts_with("v1:5:"), "{anchored}");

    // Restore the older copy: a valid, sealed chain, but shorter than the anchored head.
    std::fs::copy(&backup, &path).unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    let s = Store::open(&path, &key()).unwrap();
    s.verify_audit_chain().unwrap(); // Without the anchor the rollback is invisible.
    s.set_audit_anchor(Some(anchor.clone()));
    let m = integrity_error(&s);
    assert!(m.contains("rolled back"), "{m}");

    // Appending to the rolled-back log reuses entry 5's seq with a different hash; the anchor
    // keeps the evidence instead of being overwritten.
    for i in 4..=6 {
        s.append_audit(&audit(i + 10)).unwrap();
    }
    assert_eq!(anchor.value().unwrap(), anchored);
    let m = integrity_error(&s);
    assert!(m.contains("entry 5 differs"), "{m}");
}

#[test]
fn forged_anchor_is_rejected() {
    let s = Store::open_in_memory().unwrap();
    let anchor = Arc::new(FakeAnchor::default());
    s.set_audit_anchor(Some(anchor.clone()));
    let r = s.append_audit(&audit(1)).unwrap();
    s.verify_audit_chain().unwrap();
    // A head someone wrote without the audit key.
    *anchor.0.lock().unwrap() = Some(format!("v1:1:{}:{}", r.hash, "0".repeat(64)));
    let m = integrity_error(&s);
    assert!(m.contains("anchored chain head"), "{m}");
}

#[test]
fn unusable_anchor_is_skipped() {
    let s = Store::open_in_memory().unwrap();
    s.set_audit_anchor(Some(Arc::new(BrokenAnchor)));
    for i in 1..=3 {
        s.append_audit(&audit(i)).unwrap();
    }
    s.verify_audit_chain().unwrap();

    // Switching anchoring off works too.
    s.set_audit_anchor(None);
    s.append_audit(&audit(4)).unwrap();
    s.verify_audit_chain().unwrap();
}

#[test]
fn anchor_survives_reopen_and_tracks_the_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let anchor = Arc::new(FakeAnchor::default());
    let s = open_anchored(&path, &anchor);
    s.verify_audit_chain().unwrap(); // Empty log, no anchor yet.
    let r1 = s.append_audit(&audit(1)).unwrap();
    assert!(
        anchor
            .value()
            .unwrap()
            .starts_with(&format!("v1:1:{}:", r1.hash))
    );
    drop(s);

    let s = open_anchored(&path, &anchor);
    s.verify_audit_chain().unwrap();
    let r2 = s.append_audit(&audit(2)).unwrap();
    assert!(
        anchor
            .value()
            .unwrap()
            .starts_with(&format!("v1:2:{}:", r2.hash))
    );
    s.verify_audit_chain().unwrap();
}
