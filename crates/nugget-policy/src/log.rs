//! Tamper-evident action log: append-only, SHA-256 hash-chained, exportable as JSONL.
//!
//! # Format
//! One JSON object per line ([`LogEntry`]):
//!
//! ```json
//! {"seq":0,"ts_ms":1730000000000,"prev":"000…000","hash":"9f2c…","event":{"type":"decision",…}}
//! ```
//!
//! `hash = SHA-256(prev ‖ "\n" ‖ canonical_json({"event":…,"seq":…,"ts_ms":…}))`, hex encoded.
//! Canonical JSON sorts object keys and has no whitespace, so the hash does not depend on how a
//! serializer orders fields. The first entry's `prev` is 64 zeros. [`verify_entries`] recomputes
//! every hash and checks the links and sequence numbers; any edit, insertion, deletion or
//! reordering breaks it. Truncating the tail is only detectable against a remembered head
//! ([`ActionLog::head`], [`ActionLog::verify_against`]), so store the head elsewhere (the
//! encrypted store's audit table) if that matters.
//!
//! # Redaction
//! Actions enter the log as [`RedactedAction`]s: secure-field text is replaced by `[redacted]`,
//! card numbers and long digit runs are masked, previews are capped (see [`crate::redact`]).
//!
//! # Adapter for `nugget-agent`
//! `nugget-agent` defines its own `ActionLog` trait and depends on this crate, so the adapter
//! lives there. It wraps an `Arc<nugget_policy::ActionLog>` and forwards each record:
//!
//! ```ignore
//! struct PolicyLog(Arc<nugget_policy::ActionLog>);
//!
//! impl nugget_agent::ActionLog for PolicyLog {
//!     fn record(&self, step: &nugget_agent::StepRecord) -> nugget_core::Result<()> {
//!         // Executed actions: keeps the redaction and the chain.
//!         self.0
//!             .record_execution(&step.action, &step.context, step.ok, step.detail.clone())
//!             .map(|_| ())
//!         // Anything else (plans, verifications, evidence hashes):
//!         // self.0.append(LogEvent::External { source: "agent".into(), kind, detail })
//!     }
//! }
//! ```
//!
//! Give the same `Arc` to [`crate::PolicyEngineBuilder::log`] so decisions, confirmations,
//! halts and executions share one chain.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::broker::ConfirmOutcome;
use crate::redact::RedactedAction;
use crate::text::cap;
use crate::{ActionContext, ActionDescriptor, Decision, RiskClass};

/// `prev` of the first entry.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// What happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogEvent {
    /// The policy engine decided on an action.
    Decision {
        action: RedactedAction,
        risk: RiskClass,
        decision: Decision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reasons: Vec<String>,
    },
    /// An executor asked the user.
    ConfirmationRequested {
        id: u64,
        action: RedactedAction,
        risk: RiskClass,
        explanation: String,
    },
    /// The user (or the timeout, or the kill switch) answered.
    ConfirmationResolved {
        id: u64,
        task_id: String,
        outcome: ConfirmOutcome,
    },
    /// A per-task approval was granted.
    TaskApproved {
        task_id: String,
        risk: RiskClass,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// Per-task state was cleared.
    TaskEnded { task_id: String },
    /// The kill switch was engaged.
    Halted,
    /// The kill switch was released.
    Resumed,
    /// The policy configuration changed.
    ConfigChanged { summary: String },
    /// An executor performed an action (or failed to).
    Executed {
        action: RedactedAction,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Anything another crate wants in the chain (agent plans, verification results, evidence
    /// hashes). Keep secrets out of `detail`.
    External {
        source: String,
        kind: String,
        detail: Value,
    },
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub ts_ms: u64,
    /// Hash of the previous entry ([`GENESIS`] for the first).
    pub prev: String,
    /// SHA-256 over `prev` and this entry's canonical body.
    pub hash: String,
    pub event: LogEvent,
}

/// Why a chain failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    /// A line is not a valid entry.
    Malformed { line: usize, message: String },
    /// `seq` is not the expected next number (insertion, deletion or reordering).
    Sequence { expected: u64, found: u64 },
    /// `prev` does not match the previous entry's hash.
    BrokenLink { seq: u64 },
    /// The entry's content does not hash to its `hash` (edited).
    BadHash { seq: u64 },
    /// The chain does not end at the remembered head (truncated or replaced).
    HeadMismatch,
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::Malformed { line, message } => write!(f, "line {line}: {message}"),
            ChainError::Sequence { expected, found } => {
                write!(f, "expected entry {expected}, found {found}")
            }
            ChainError::BrokenLink { seq } => {
                write!(f, "entry {seq} does not link to its predecessor")
            }
            ChainError::BadHash { seq } => write!(f, "entry {seq} was modified"),
            ChainError::HeadMismatch => f.write_str("log does not end at the remembered head"),
        }
    }
}

impl std::error::Error for ChainError {}

impl From<ChainError> for Error {
    fn from(e: ChainError) -> Self {
        Error::Integrity(e.to_string())
    }
}

struct Inner {
    entries: Vec<LogEntry>,
    sink: Option<BufWriter<File>>,
}

/// The append-only, hash-chained log. Thread-safe; share it behind an `Arc`.
pub struct ActionLog {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for ActionLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionLog")
            .field("len", &self.len())
            .finish()
    }
}

impl Default for ActionLog {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl ActionLog {
    pub fn in_memory() -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                sink: None,
            }),
        }
    }

    /// Open a JSONL file, verify what is there, and append to it. Fails with
    /// `Error::Integrity` if the existing chain does not verify.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let entries = if path.exists() {
            let entries = read_jsonl(BufReader::new(File::open(path)?))?;
            verify_entries(&entries)?;
            entries
        } else {
            Vec::new()
        };
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                entries,
                sink: Some(BufWriter::new(file)),
            }),
        })
    }

    /// Load and verify an exported JSONL log (in memory, no file sink).
    pub fn from_jsonl(reader: impl BufRead) -> Result<Self> {
        let entries = read_jsonl(reader)?;
        verify_entries(&entries)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                entries,
                sink: None,
            }),
        })
    }

    /// Append an event and return the new entry.
    pub fn append(&self, event: LogEvent) -> Result<LogEntry> {
        let mut inner = self.lock();
        let (seq, prev) = match inner.entries.last() {
            Some(last) => (last.seq + 1, last.hash.clone()),
            None => (0, GENESIS.to_string()),
        };
        let ts_ms = now_ms();
        let hash = entry_hash(&prev, seq, ts_ms, &event);
        let entry = LogEntry {
            seq,
            ts_ms,
            prev,
            hash,
            event,
        };
        if let Some(sink) = inner.sink.as_mut() {
            serde_json::to_writer(&mut *sink, &entry)?;
            sink.write_all(b"\n")?;
            sink.flush()?;
        }
        inner.entries.push(entry.clone());
        Ok(entry)
    }

    /// Record that an executor performed `action` (redacted on the way in).
    pub fn record_execution(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
        ok: bool,
        detail: Option<String>,
    ) -> Result<LogEntry> {
        self.append(LogEvent::Executed {
            action: RedactedAction::new(action, ctx),
            ok,
            detail: detail.map(|d| cap(&d, 500)),
        })
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.lock().entries.clone()
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `(seq, hash)` of the last entry. Remember it elsewhere to detect truncation.
    pub fn head(&self) -> Option<(u64, String)> {
        self.lock().entries.last().map(|e| (e.seq, e.hash.clone()))
    }

    /// Recompute the whole chain.
    pub fn verify_chain(&self) -> std::result::Result<(), ChainError> {
        verify_entries(&self.lock().entries)
    }

    /// Verify the chain and that it ends at (or passes through) a remembered head.
    pub fn verify_against(&self, head: &(u64, String)) -> std::result::Result<(), ChainError> {
        let inner = self.lock();
        verify_entries(&inner.entries)?;
        match inner.entries.get(head.0 as usize) {
            Some(e) if e.hash == head.1 => Ok(()),
            _ => Err(ChainError::HeadMismatch),
        }
    }

    /// Write every entry as one JSON object per line.
    pub fn export_jsonl(&self, mut out: impl Write) -> Result<()> {
        for entry in self.lock().entries.iter() {
            serde_json::to_writer(&mut out, entry)?;
            out.write_all(b"\n")?;
        }
        out.flush()?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock cannot leave a half-written entry in `entries` (the push
        // is last), so the data is still consistent.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Verify a chain given as entries.
pub fn verify_entries(entries: &[LogEntry]) -> std::result::Result<(), ChainError> {
    let mut prev = GENESIS.to_string();
    for (i, e) in entries.iter().enumerate() {
        let expected = i as u64;
        if e.seq != expected {
            return Err(ChainError::Sequence {
                expected,
                found: e.seq,
            });
        }
        if e.prev != prev {
            return Err(ChainError::BrokenLink { seq: e.seq });
        }
        if entry_hash(&e.prev, e.seq, e.ts_ms, &e.event) != e.hash {
            return Err(ChainError::BadHash { seq: e.seq });
        }
        prev = e.hash.clone();
    }
    Ok(())
}

/// Verify exported JSONL without keeping it. Returns the number of entries.
pub fn verify_jsonl(reader: impl BufRead) -> std::result::Result<usize, ChainError> {
    let entries = read_jsonl(reader).map_err(|e| ChainError::Malformed {
        line: 0,
        message: e.to_string(),
    })?;
    verify_entries(&entries)?;
    Ok(entries.len())
}

fn read_jsonl(reader: impl BufRead) -> Result<Vec<LogEntry>> {
    let mut entries = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: LogEntry = serde_json::from_str(&line).map_err(|e| {
            Error::Integrity(
                ChainError::Malformed {
                    line: i + 1,
                    message: e.to_string(),
                }
                .to_string(),
            )
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

fn entry_hash(prev: &str, seq: u64, ts_ms: u64, event: &LogEvent) -> String {
    let body = serde_json::json!({ "seq": seq, "ts_ms": ts_ms, "event": event });
    let mut canonical = String::new();
    write_canonical(&body, &mut canonical);
    let mut hasher = Sha256::new();
    hasher.update(prev.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical.as_bytes());
    hex::encode(hasher.finalize())
}

/// JSON with object keys sorted and no whitespace.
fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(preview: &str) -> ActionDescriptor {
        ActionDescriptor {
            target_app: "com.apple.mail".into(),
            verb: "type".into(),
            target_label: Some("Body".into()),
            text_preview: Some(preview.into()),
            declared_risk: RiskClass::Edit,
            task_id: "t1".into(),
        }
    }

    fn filled() -> ActionLog {
        let log = ActionLog::in_memory();
        log.append(LogEvent::Halted).unwrap();
        log.record_execution(&action("hello"), &ActionContext::default(), true, None)
            .unwrap();
        log.append(LogEvent::External {
            source: "agent".into(),
            kind: "plan".into(),
            detail: serde_json::json!({"b": 1, "a": [1, 2]}),
        })
        .unwrap();
        log.append(LogEvent::Resumed).unwrap();
        log
    }

    #[test]
    fn chain_verifies() {
        let log = filled();
        assert_eq!(log.len(), 4);
        log.verify_chain().unwrap();
        let entries = log.entries();
        assert_eq!(entries[0].prev, GENESIS);
        assert_eq!(entries[1].prev, entries[0].hash);
        log.verify_against(&log.head().unwrap()).unwrap();
    }

    #[test]
    fn edits_are_detected() {
        let log = filled();
        let mut buf = Vec::new();
        log.export_jsonl(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(verify_jsonl(text.as_bytes()), Ok(4));

        // Edit content.
        let edited = text.replace("hello", "HELLO");
        assert_eq!(
            verify_jsonl(edited.as_bytes()),
            Err(ChainError::BadHash { seq: 1 })
        );

        // Drop a middle line.
        let lines: Vec<&str> = text.lines().collect();
        let dropped = [lines[0], lines[2], lines[3]].join("\n");
        assert!(matches!(
            verify_jsonl(dropped.as_bytes()),
            Err(ChainError::Sequence {
                expected: 1,
                found: 2
            })
        ));

        // Swap two lines.
        let swapped = [lines[0], lines[2], lines[1], lines[3]].join("\n");
        assert!(verify_jsonl(swapped.as_bytes()).is_err());

        // Rewrite a hash and the next link consistently: the recomputed hash still fails.
        let mut entries = log.entries();
        entries[2].event = LogEvent::Resumed;
        assert_eq!(
            verify_entries(&entries),
            Err(ChainError::BadHash { seq: 2 })
        );

        // Garbage line.
        assert!(matches!(
            verify_jsonl("{not json}\n".as_bytes()),
            Err(ChainError::Malformed { .. })
        ));
    }

    #[test]
    fn truncation_detected_against_head() {
        let log = filled();
        let head = log.head().unwrap();
        let mut buf = Vec::new();
        log.export_jsonl(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let truncated: String = text.lines().take(2).map(|l| format!("{l}\n")).collect();
        let short = ActionLog::from_jsonl(truncated.as_bytes()).unwrap();
        short.verify_chain().unwrap();
        assert_eq!(short.verify_against(&head), Err(ChainError::HeadMismatch));
    }

    #[test]
    fn file_log_persists_and_rejects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/actions.jsonl");
        {
            let log = ActionLog::open(&path).unwrap();
            log.append(LogEvent::Halted).unwrap();
            log.append(LogEvent::Resumed).unwrap();
        }
        {
            let log = ActionLog::open(&path).unwrap();
            assert_eq!(log.len(), 2);
            log.append(LogEvent::Halted).unwrap();
            log.verify_chain().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 3);
        std::fs::write(&path, text.replacen("resumed", "halted", 1)).unwrap();
        assert!(matches!(ActionLog::open(&path), Err(Error::Integrity(_))));
    }

    #[test]
    fn executions_are_redacted() {
        let log = ActionLog::in_memory();
        let mut a = action("4111 1111 1111 1111");
        log.record_execution(&a, &ActionContext::default(), true, None)
            .unwrap();
        a.text_preview = Some("hunter2".into());
        log.record_execution(
            &a,
            &ActionContext::default().with_secure_field(true),
            true,
            None,
        )
        .unwrap();
        let mut buf = Vec::new();
        log.export_jsonl(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(!text.contains("4111 1111"));
        assert!(text.contains("••••1111"));
        assert!(!text.contains("hunter2"));
        assert!(text.contains("[redacted]"));
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let mut s = String::new();
        write_canonical(
            &serde_json::json!({"b": {"d": 1, "c": [true, null]}, "a": "x"}),
            &mut s,
        );
        assert_eq!(s, r#"{"a":"x","b":{"c":[true,null],"d":1}}"#);
    }
}
