//! Items whose fetch failed, carried in a cursor to be fetched again on later passes, with the
//! number of passes each has failed in (so a poison item is dropped, never retried forever).

use std::collections::{HashMap, HashSet};

use nugget_core::Error;
use serde::{Deserialize, Serialize};

/// Passes an item may fail in before it is dropped.
pub(crate) const MAX_ATTEMPTS: u32 = 5;

/// One carried item: `{"id":"<id>","n":<passes failed>}`. A bare `"<id>"` (older cursors) is
/// read as `n: 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "CarriedRepr")]
pub(crate) struct Carried {
    pub id: String,
    pub n: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CarriedRepr {
    Plain(String),
    Counted {
        id: String,
        #[serde(default)]
        n: u32,
    },
}

impl From<CarriedRepr> for Carried {
    fn from(r: CarriedRepr) -> Self {
        match r {
            CarriedRepr::Plain(id) => Self { id, n: 0 },
            CarriedRepr::Counted { id, n } => Self { id, n },
        }
    }
}

/// The pending carried items of one pass.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    /// First-failure order; ids no longer in `attempts` are resolved.
    order: Vec<String>,
    in_order: HashSet<String>,
    attempts: HashMap<String, u32>,
    /// Ids whose count was raised in this pass (once per pass).
    failed_now: HashSet<String>,
    /// The carried-in ids, in order.
    carried: Vec<String>,
    max: usize,
    what: &'static str,
}

impl Pending {
    /// From a cursor's list; `max` bounds the list, `what` names an item in logs.
    pub(crate) fn new(list: &[Carried], max: usize, what: &'static str) -> Self {
        let mut p = Self {
            max,
            what,
            ..Default::default()
        };
        for c in list.iter().take(max) {
            if p.in_order.insert(c.id.clone()) {
                p.order.push(c.id.clone());
                p.attempts.insert(c.id.clone(), c.n);
                p.carried.push(c.id.clone());
            }
        }
        p
    }

    /// The list for the cursor.
    pub(crate) fn list(&self) -> Vec<Carried> {
        self.order
            .iter()
            .filter_map(|id| {
                self.attempts.get(id).map(|n| Carried {
                    id: id.clone(),
                    n: *n,
                })
            })
            .collect()
    }

    /// What the cursor carried in (taken once, for the end-of-pass refetch).
    pub(crate) fn take_carried(&mut self) -> Vec<String> {
        std::mem::take(&mut self.carried)
    }

    pub(crate) fn is_pending(&self, id: &str) -> bool {
        self.attempts.contains_key(id)
    }

    /// The item was fetched, removed or dropped: no longer pending.
    pub(crate) fn resolve(&mut self, id: &str) {
        self.attempts.remove(id);
    }

    /// Record a failure of `id`. Returns whether it counts towards a pass's failure streak:
    /// only a first failure does (items that failed in earlier passes never end a pass).
    pub(crate) fn fail(&mut self, id: &str, source: &str, e: &Error) -> bool {
        let what = self.what;
        if !self.failed_now.insert(id.to_string()) {
            return false;
        }
        let n = self.attempts.get(id).copied().unwrap_or(0) + 1;
        if n > MAX_ATTEMPTS {
            self.attempts.remove(id);
            tracing::warn!(
                source,
                id,
                "giving up on a {what} that failed in {MAX_ATTEMPTS} passes: {e}"
            );
            return false;
        }
        if !self.attempts.contains_key(id) && self.attempts.len() >= self.max {
            tracing::error!(
                source,
                id,
                "too many failing items to carry ({}); dropping a {what}: {e}",
                self.max
            );
            return true;
        }
        tracing::warn!(
            source,
            id,
            "{what} failed (pass {n} of {MAX_ATTEMPTS}); retrying next pass: {e}"
        );
        self.attempts.insert(id.to_string(), n);
        if self.in_order.insert(id.to_string()) {
            self.order.push(id.to_string());
        }
        n == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_old_and_new_forms_and_drops_after_the_limit() {
        let list: Vec<Carried> = serde_json::from_value(json!(["a", {"id": "b", "n": 2}])).unwrap();
        assert_eq!(
            list[0],
            Carried {
                id: "a".into(),
                n: 0
            }
        );
        let mut p = Pending::new(&list, 10, "item");
        let e = Error::Network("x".into());
        assert!(!p.fail("b", "s", &e), "an earlier failure never counts");
        assert!(!p.fail("b", "s", &e), "once per pass");
        assert!(p.fail("c", "s", &e));
        p.resolve("a");
        assert_eq!(
            serde_json::to_value(p.list()).unwrap(),
            json!([{"id": "b", "n": 3}, {"id": "c", "n": 1}])
        );
        let mut p = Pending::new(
            &[Carried {
                id: "b".into(),
                n: MAX_ATTEMPTS,
            }],
            10,
            "item",
        );
        assert!(!p.fail("b", "s", &e));
        assert!(p.list().is_empty());
    }

    #[test]
    fn a_full_list_drops_new_items() {
        let mut p = Pending::new(&[], 1, "item");
        let e = Error::Network("x".into());
        assert!(p.fail("a", "s", &e));
        assert!(p.fail("b", "s", &e));
        assert_eq!(p.list().len(), 1);
    }
}
