//! Idempotent action ids and the ledger that makes a replayed call a no-op.
//!
//! An action id is a stable hash of (task id, the model's call id, tool name, canonical
//! arguments). Replaying the same model output (a retried stream, a resumed run, a duplicated
//! call) yields the same id, and the ledger returns the recorded result instead of acting again.
//! A deliberate retry by the model is a new call with a new call id, so it does execute.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool::{ToolResult, Verification};

/// Deterministic id for one proposed action.
pub fn action_id(task_id: &str, call_id: &str, tool: &str, args: &Value) -> String {
    let mut h = Fnv128::new();
    for part in [task_id, call_id, tool] {
        h.write(part.as_bytes());
        h.write(&[0xff]);
    }
    h.write(canonical_json(args).as_bytes());
    format!("act_{:032x}", h.finish())
}

/// JSON with object keys sorted at every level, independent of serde_json's map feature flags.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

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

/// FNV-1a, 128-bit. Stable across processes and platforms (unlike `DefaultHasher`).
pub(crate) struct Fnv128(u128);

impl Fnv128 {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013B;

    pub(crate) fn new() -> Self {
        Self(Self::OFFSET)
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u128::from(*b);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    pub(crate) fn finish(&self) -> u128 {
        self.0
    }
}

/// What the ledger knows about an action id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LedgerEntry {
    /// Execution started and never completed (crash, kill switch mid-action). Its effect is
    /// unknown, so it is never re-run automatically.
    InFlight,
    Done {
        result: ToolResult,
        verification: Option<Verification>,
    },
}

/// Durable record of executed actions. The daemon backs this with the store so ids survive
/// restarts; [`MemoryLedger`] is for tests and single-process use.
pub trait ActionLedger: Send + Sync {
    fn get(&self, action_id: &str) -> Option<LedgerEntry>;
    /// Claim an id before executing. Returns `false` if it is already known (do not execute).
    fn begin(&self, action_id: &str) -> bool;
    fn complete(&self, action_id: &str, result: &ToolResult, verification: Option<&Verification>);
}

#[derive(Debug, Default)]
pub struct MemoryLedger {
    entries: Mutex<HashMap<String, LedgerEntry>>,
}

impl MemoryLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, LedgerEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ActionLedger for MemoryLedger {
    fn get(&self, action_id: &str) -> Option<LedgerEntry> {
        self.lock().get(action_id).cloned()
    }

    fn begin(&self, action_id: &str) -> bool {
        let mut m = self.lock();
        if m.contains_key(action_id) {
            return false;
        }
        m.insert(action_id.to_string(), LedgerEntry::InFlight);
        true
    }

    fn complete(&self, action_id: &str, result: &ToolResult, verification: Option<&Verification>) {
        self.lock().insert(
            action_id.to_string(),
            LedgerEntry::Done {
                result: result.clone(),
                verification: verification.cloned(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_are_stable_and_key_order_independent() {
        let a = action_id(
            "t",
            "c1",
            "set",
            &json!({"a": 1, "b": {"y": 2, "x": [1, 2]}}),
        );
        let b = action_id(
            "t",
            "c1",
            "set",
            &json!({"b": {"x": [1, 2], "y": 2}, "a": 1}),
        );
        assert_eq!(a, b);
        assert_ne!(a, action_id("t", "c2", "set", &json!({"a": 1})));
        assert_ne!(
            action_id("t", "c1", "set", &json!({"a": 1})),
            action_id("t", "c1", "set", &json!({"a": 2}))
        );
        // Separators keep fields from running together.
        assert_ne!(
            action_id("ab", "c", "x", &json!({})),
            action_id("a", "bc", "x", &json!({}))
        );
        assert_eq!(a.len(), 4 + 32);
    }

    #[test]
    fn ledger_claims_once() {
        let l = MemoryLedger::new();
        assert!(l.begin("a"));
        assert!(!l.begin("a"));
        assert_eq!(l.get("a"), Some(LedgerEntry::InFlight));
        l.complete("a", &ToolResult::ok("x"), None);
        assert!(matches!(l.get("a"), Some(LedgerEntry::Done { .. })));
        assert!(!l.begin("a"));
    }
}
