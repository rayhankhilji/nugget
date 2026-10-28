//! Drafts and reminders, kept in the encrypted store (one JSON document per kind under a cursor
//! key, as `nugget-proactive` keeps its card state; SQLCipher encrypts it at rest).

use std::sync::{Mutex, PoisonError};

use nugget_core::Result;
use nugget_core::ids::{ItemRef, SourceId};
use nugget_core::ipc::ReminderView;
use nugget_proactive::{DraftReply, Reminder};
use nugget_store::Store;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Cursor key of the stored drafts.
pub(crate) const DRAFTS_KEY: &str = "proactive:drafts";
/// Cursor key of the stored reminders.
pub(crate) const REMINDERS_KEY: &str = "proactive:reminders";
/// Drafts kept (oldest dropped first).
const MAX_DRAFTS: usize = 200;
/// Fired reminders are forgotten after this long.
const FIRED_RETENTION: time::Duration = time::Duration::days(30);
/// Pending reminders kept at most.
const MAX_REMINDERS: usize = 5_000;

/// One JSON document in the store, read-modify-written under a lock.
struct Doc<T> {
    store: Store,
    key: SourceId,
    lock: Mutex<()>,
    _t: std::marker::PhantomData<fn() -> T>,
}

impl<T: Default + Serialize + DeserializeOwned> Doc<T> {
    fn new(store: Store, key: &str) -> Self {
        Self {
            store,
            key: SourceId::new(key),
            lock: Mutex::new(()),
            _t: std::marker::PhantomData,
        }
    }

    fn read(&self) -> Result<T> {
        Ok(match self.store.get_cursor(&self.key)? {
            Some(v) => serde_json::from_value(v)?,
            None => T::default(),
        })
    }

    fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R> {
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut doc = self.read()?;
        let r = f(&mut doc);
        self.store
            .set_cursor(&self.key, serde_json::to_value(&doc)?)?;
        Ok(r)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StoredDraft {
    pub id: String,
    pub reply_to: ItemRef,
    #[serde(default)]
    pub in_reply_to_message_id: Option<String>,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Drafts {
    #[serde(default)]
    drafts: Vec<StoredDraft>,
}

/// Drafted replies the user copies or opens in their mail app. Never sent from here.
pub(crate) struct DraftStore(Doc<Drafts>);

impl DraftStore {
    pub(crate) fn new(store: Store) -> Self {
        Self(Doc::new(store, DRAFTS_KEY))
    }

    /// Keep `d` (its body must be filled in); returns the draft's id.
    pub(crate) fn save(&self, d: &DraftReply, now: OffsetDateTime) -> Result<String> {
        let id = format!("draft_{}", uuid::Uuid::new_v4().simple());
        let rec = StoredDraft {
            id: id.clone(),
            reply_to: d.reply_to.clone(),
            in_reply_to_message_id: d.in_reply_to_message_id.clone(),
            to: d.to.clone(),
            cc: d.cc.clone(),
            subject: d.subject.clone(),
            body: d.body.clone().unwrap_or_default(),
            created_at: now,
        };
        self.0.update(|doc| {
            doc.drafts.push(rec);
            let n = doc.drafts.len();
            if n > MAX_DRAFTS {
                doc.drafts.drain(..n - MAX_DRAFTS);
            }
        })?;
        Ok(id)
    }

    #[cfg(test)]
    pub(crate) fn get(&self, id: &str) -> Result<Option<StoredDraft>> {
        Ok(self.0.read()?.drafts.into_iter().find(|d| d.id == id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredReminder {
    id: String,
    text: String,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    #[serde(default)]
    about: Option<ItemRef>,
    #[serde(default)]
    card_id: Option<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    fired_at: Option<OffsetDateTime>,
}

impl StoredReminder {
    fn view(&self) -> ReminderView {
        ReminderView {
            id: self.id.clone(),
            text: self.text.clone(),
            at: self.at,
            about: self.about.clone(),
            card_id: self.card_id.clone(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Reminders {
    #[serde(default)]
    reminders: Vec<StoredReminder>,
}

/// Local reminders; the daemon broadcasts `reminder.due` for each once.
pub(crate) struct ReminderStore(Doc<Reminders>);

impl ReminderStore {
    pub(crate) fn new(store: Store) -> Self {
        Self(Doc::new(store, REMINDERS_KEY))
    }

    pub(crate) fn add(&self, r: &Reminder, card_id: Option<String>) -> Result<String> {
        let id = format!("rem_{}", uuid::Uuid::new_v4().simple());
        let rec = StoredReminder {
            id: id.clone(),
            text: r.text.clone(),
            at: r.at,
            about: r.about.clone(),
            card_id,
            fired_at: None,
        };
        self.0.update(|doc| {
            doc.reminders.push(rec);
            let pending = doc
                .reminders
                .iter()
                .filter(|r| r.fired_at.is_none())
                .count();
            if pending > MAX_REMINDERS {
                // Drop the furthest-out pending ones.
                doc.reminders.sort_by_key(|r| r.at);
                let mut extra = pending - MAX_REMINDERS;
                while extra > 0 {
                    if let Some(i) = doc.reminders.iter().rposition(|r| r.fired_at.is_none()) {
                        doc.reminders.remove(i);
                    }
                    extra -= 1;
                }
            }
        })?;
        Ok(id)
    }

    /// Not yet fired, soonest first.
    pub(crate) fn pending(&self) -> Result<Vec<ReminderView>> {
        let mut out: Vec<ReminderView> = self
            .0
            .read()?
            .reminders
            .iter()
            .filter(|r| r.fired_at.is_none())
            .map(StoredReminder::view)
            .collect();
        out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Due at or before `now` and not fired: marked fired (each fires once) and returned,
    /// soonest first. Fired reminders older than the retention are forgotten.
    pub(crate) fn take_due(&self, now: OffsetDateTime) -> Result<Vec<ReminderView>> {
        // Cheap read first: most ticks have nothing due and should not write.
        let any_due = self
            .0
            .read()?
            .reminders
            .iter()
            .any(|r| r.fired_at.is_none() && r.at <= now);
        if !any_due {
            return Ok(Vec::new());
        }
        self.0.update(|doc| {
            let mut due = Vec::new();
            for r in &mut doc.reminders {
                if r.fired_at.is_none() && r.at <= now {
                    r.fired_at = Some(now);
                    due.push(r.view());
                }
            }
            doc.reminders
                .retain(|r| r.fired_at.is_none_or(|f| now - f < FIRED_RETENTION));
            due.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
            due
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::ItemKey;

    fn reminder(text: &str, at: OffsetDateTime) -> Reminder {
        Reminder {
            text: text.into(),
            at,
            about: Some(ItemRef {
                source: SourceId::new("mail"),
                key: ItemKey::new("k"),
            }),
        }
    }

    #[test]
    fn reminders_are_pending_until_due_and_fire_once() {
        let store = Store::open_in_memory().unwrap();
        let rs = ReminderStore::new(store.clone());
        let now = OffsetDateTime::now_utc();
        let late = rs
            .add(&reminder("later", now + time::Duration::hours(2)), None)
            .unwrap();
        let soon = rs
            .add(
                &reminder("soon", now + time::Duration::minutes(5)),
                Some("card-1".into()),
            )
            .unwrap();
        let past = rs
            .add(&reminder("past", now - time::Duration::minutes(1)), None)
            .unwrap();
        let ids: Vec<String> = rs.pending().unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![past.clone(), soon.clone(), late.clone()]);

        let due = rs.take_due(now).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, past);
        assert!(rs.take_due(now).unwrap().is_empty(), "fires once");

        let due = rs.take_due(now + time::Duration::minutes(10)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "soon");
        assert_eq!(due[0].card_id.as_deref(), Some("card-1"));
        assert_eq!(
            rs.pending()
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![late.clone()]
        );

        // Survives a reopen of the same store (it is persisted, not in memory).
        let again = ReminderStore::new(store);
        assert_eq!(again.pending().unwrap().len(), 1);
        // Fired ones are pruned after the retention period.
        let far = now + time::Duration::days(40);
        assert_eq!(again.take_due(far).unwrap().len(), 1);
        assert!(again.0.read().unwrap().reminders.len() <= 1);
    }

    #[test]
    fn drafts_are_stored_with_their_body() {
        let store = Store::open_in_memory().unwrap();
        let ds = DraftStore::new(store);
        let d = DraftReply {
            reply_to: ItemRef {
                source: SourceId::new("mail"),
                key: ItemKey::new("k"),
            },
            in_reply_to_message_id: None,
            to: vec!["ana@acme.io".into()],
            cc: vec![],
            subject: "Re: x".into(),
            intent: "reply".into(),
            body: Some("Hi".into()),
        };
        let id = ds.save(&d, OffsetDateTime::now_utc()).unwrap();
        let got = ds.get(&id).unwrap().unwrap();
        assert_eq!(got.body, "Hi");
        assert_eq!(got.to, vec!["ana@acme.io".to_string()]);
        assert!(ds.get("nope").unwrap().is_none());
    }
}
