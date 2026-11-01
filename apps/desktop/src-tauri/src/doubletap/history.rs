//! Draft history: the last [`MAX_ENTRIES`] drafts inserted or dismissed, so a previous one can be
//! inserted again from the overlay or copied from the Double Tap page.
//!
//! Privacy: kept **in memory only** (never written to disk, gone when Nugget quits), clearable,
//! and never holding a sensitive draft: the overlay state machine drops practice drafts and drafts
//! that drew on items marked sensitive, [`History::push`] drops text that looks like a card
//! number or a long digit run, and the flow keeps a draft only once the daemon's PII scan
//! (`draft.check_sensitive`: cards, IBANs, SSNs, API keys and other credentials) finds nothing.

use std::collections::VecDeque;

use serde::Serialize;

/// How many drafts are kept.
pub const MAX_ENTRIES: usize = 20;
/// Longest draft kept, in characters (longer ones are skipped, not cut).
pub const MAX_CHARS: usize = 20_000;

/// One draft (wire type of `doubletap_history` and the `doubletap:history` event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub id: u64,
    pub text: String,
    /// The app it was written for (display name).
    pub app: String,
    /// Unix milliseconds.
    pub at: u64,
}

#[derive(Debug, Default)]
pub struct History {
    entries: VecDeque<Entry>,
    next_id: u64,
}

/// Whether the text looks like it holds a secret that must not be kept: a card number or a run
/// of nine or more digits.
pub fn looks_sensitive(text: &str) -> bool {
    nugget_policy::redact::mask_digits(text) != text
}

impl History {
    /// Newest first.
    pub fn list(&self) -> Vec<Entry> {
        self.entries.iter().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Keep `text` on top (an equal draft moves up). Returns whether the history changed.
    pub fn push(&mut self, text: &str, app: &str, at: u64) -> bool {
        if text.trim().is_empty() || text.chars().count() > MAX_CHARS || looks_sensitive(text) {
            return false;
        }
        self.entries.retain(|e| e.text != text);
        self.next_id += 1;
        self.entries.push_front(Entry {
            id: self.next_id,
            text: text.to_owned(),
            app: app.trim().chars().take(120).collect(),
            at,
        });
        self.entries.truncate(MAX_ENTRIES);
        true
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        before != self.entries.len()
    }

    pub fn clear(&mut self) -> bool {
        let had = !self.entries.is_empty();
        self.entries.clear();
        had
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_twenty_newest_first_without_duplicates() {
        let mut h = History::default();
        for i in 0..25 {
            assert!(h.push(&format!("draft {i}"), "Slack", i));
        }
        let list = h.list();
        assert_eq!(list.len(), MAX_ENTRIES);
        assert_eq!(list[0].text, "draft 24");
        assert_eq!(list[19].text, "draft 5");
        // The same text moves to the top with a new id.
        let old = list.iter().find(|e| e.text == "draft 10").unwrap().id;
        assert!(h.push("draft 10", "Mail", 99));
        let list = h.list();
        assert_eq!(list.len(), MAX_ENTRIES);
        assert_eq!(list[0].text, "draft 10");
        assert_eq!(list[0].app, "Mail");
        assert_ne!(list[0].id, old);
        assert!(h.get(old).is_none());
        assert_eq!(list.iter().filter(|e| e.text == "draft 10").count(), 1);
    }

    #[test]
    fn never_keeps_blank_huge_or_secret_looking_drafts() {
        let mut h = History::default();
        assert!(!h.push("   ", "Slack", 1));
        assert!(!h.push(&"x".repeat(MAX_CHARS + 1), "Slack", 1));
        assert!(!h.push("My card is 4111 1111 1111 1111", "Slack", 1));
        assert!(!h.push("Account 123456789 is yours", "Slack", 1));
        assert!(h.push("Call me at 555-0100 on Friday", "Slack", 1));
        assert_eq!(h.list().len(), 1);
    }

    #[test]
    fn remove_and_clear() {
        let mut h = History::default();
        h.push("a", "x", 1);
        h.push("b", "x", 2);
        let id = h.list()[1].id;
        assert!(h.remove(id));
        assert!(!h.remove(id));
        assert_eq!(h.list().len(), 1);
        assert!(h.clear());
        assert!(!h.clear());
        assert!(h.list().is_empty());
    }

    #[test]
    fn serializes_per_contract() {
        let mut h = History::default();
        h.push("Hi", "Mail", 1_700_000_000_000);
        assert_eq!(
            serde_json::to_value(&h.list()[0]).unwrap(),
            serde_json::json!({ "id": 1, "text": "Hi", "app": "Mail", "at": 1_700_000_000_000u64 })
        );
    }
}
