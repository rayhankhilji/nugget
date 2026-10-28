//! Executors for proactive actions, behind `nugget_proactive::ActionRunner` (approval bound to
//! the previewed digest, then the policy gate, then the executor):
//!
//! - **DraftReply**: the reply is written by the Double Tap engine ([`draft`]); executing keeps
//!   it in the encrypted store ([`records`]) and hands the desktop the text plus a `mailto:` link
//!   ([`mailto`]). Nothing is ever sent or written to the mailbox.
//! - **CreateEvent**: `PUT` of a VEVENT to the first enabled CalDAV source's default calendar;
//!   without one (or if the server refuses) an RFC 5545 `.ics` file ([`ics`]) under
//!   `<data>/proactive/events/` that the desktop opens with the OS.
//! - **Reminder**: stored ([`records`]); the daemon fires `reminder.due` from
//!   [`Exec::take_due_reminders`] and the desktop shows the OS notification.
//! - **ArchiveThread**: IMAP `UID MOVE` to the archive mailbox, or Gmail `messages.modify`
//!   removing `INBOX` (only for Gmail sources with `allow_modify`). Other sources cannot archive
//!   ([`Exec::supports`] says so). The action always needs explicit confirmation.
//! - **ResearchNote**: a note written through the vault's own write path (`Notes/<title>.md`).
//!
//! Network calls go through the connectors' metered clients (features `action:caldav`,
//! `action:imap`, `action:gmail`) into the store's egress ledger; credentials come from the
//! keychain through the daemon's secret store.

mod draft;
mod ics;
mod mailto;
mod records;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use nugget_connector::{CancellationToken, ConnectorContext, SecretStore};
use nugget_connector_caldav::{CalDavConnector, CalDavSettings};
use nugget_connector_google::GmailFactory;
use nugget_connector_imap::{ImapConnector, ImapSettings};
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::ipc::{ActionArtifact, ReminderView, SourceInfo};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_ingest::{IngestEvent, StoreKnownItems};
use nugget_proactive::{
    Action, ArchiveThread, CalendarExecutor, CreateEvent, DraftReply, Drafter, Executors,
    MailExecutor, NoteExecutor, Reminder, ReminderExecutor, ResearchNote,
};
use nugget_store::Store;
use nugget_vault::Vault;
use serde_json::Value;
use time::OffsetDateTime;

use crate::daemon::{State, blocking};
use crate::sources::{CLIENT_SECRET_FIELD, client_secret_account};

/// Vault folder for research notes.
const NOTES_FOLDER: &str = "Notes";
/// Where `.ics` files go, under the data dir.
const EVENTS_DIR: &str = "proactive/events";

/// What the executors need to know about a configured source.
#[derive(Debug, Clone, PartialEq)]
struct SourceLite {
    kind: String,
    enabled: bool,
    settings: Value,
}

impl SourceLite {
    fn of(info: &SourceInfo) -> Self {
        Self {
            kind: info.kind.clone(),
            enabled: info.enabled,
            settings: info.settings.clone(),
        }
    }

    /// Can archive mail: IMAP, or Gmail with `allow_modify`.
    fn can_archive(&self) -> bool {
        self.enabled
            && match self.kind.as_str() {
                nugget_connector_imap::KIND => true,
                "gmail" => self
                    .settings
                    .get("allow_modify")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                _ => false,
            }
    }
}

/// The executors' shared state.
struct Inner {
    store: Store,
    paths: Paths,
    events_dir: PathBuf,
    vault: Option<Arc<Vault>>,
    secrets: Arc<dyn SecretStore>,
    egress: Arc<dyn EgressSink>,
    /// Snapshot of the configured sources, kept current from ingest events.
    sources: RwLock<BTreeMap<SourceId, SourceLite>>,
    drafts: records::DraftStore,
    reminders: records::ReminderStore,
    /// Cancelled at daemon shutdown.
    cancel: CancellationToken,
}

/// The proactive executors, built from the daemon's state.
pub(crate) struct Exec {
    inner: Arc<Inner>,
    drafter: Option<Arc<dyn Drafter>>,
}

/// Everything [`Exec`] is built from (tests build it without a daemon).
struct Parts {
    store: Store,
    paths: Paths,
    vault: Option<Arc<Vault>>,
    secrets: Arc<dyn SecretStore>,
    egress: Arc<dyn EgressSink>,
    drafter: Option<Arc<dyn Drafter>>,
    cancel: CancellationToken,
}

impl Exec {
    /// Build the executors from the daemon's state (store, sources, vault, router, secrets).
    /// Keeps a snapshot of the configured sources current in a background task.
    pub(crate) fn new(state: &Arc<State>) -> Result<Self> {
        let engine = nugget_draft::Drafter::new(
            state.store.clone(),
            state.retriever.clone(),
            state.router.clone(),
            Arc::new(nugget_draft::NoVoice),
            nugget_draft::DraftConfig::default(),
        );
        let exec = Self::from_parts(Parts {
            store: state.store.clone(),
            paths: state.paths.clone(),
            vault: state.knowledge.vault.clone(),
            secrets: state.secrets.clone(),
            egress: Arc::new(state.store.clone()),
            drafter: Some(Arc::new(draft::EngineDrafter::new(engine))),
            cancel: state.shutdown.child_token(),
        });
        exec.spawn_source_watch(state);
        Ok(exec)
    }

    fn from_parts(p: Parts) -> Self {
        let events_dir = p.paths.data.join(EVENTS_DIR);
        Self {
            inner: Arc::new(Inner {
                drafts: records::DraftStore::new(p.store.clone()),
                reminders: records::ReminderStore::new(p.store.clone()),
                store: p.store,
                paths: p.paths,
                events_dir,
                vault: p.vault,
                secrets: p.secrets,
                egress: p.egress,
                sources: RwLock::new(BTreeMap::new()),
                cancel: p.cancel,
            }),
            drafter: p.drafter,
        }
    }

    /// Follow `sources.*` changes so [`supports`](Self::supports) stays synchronous.
    fn spawn_source_watch(&self, state: &Arc<State>) {
        let inner = self.inner.clone();
        let st = state.clone();
        let stop = state.shutdown.clone();
        state.tasks.spawn(async move {
            // Subscribe before listing so nothing between the two is missed.
            let mut events = st.ingest.subscribe();
            inner.reload_sources(&st).await;
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    ev = events.recv() => match ev {
                        Ok(IngestEvent::SourceChanged(info)) => inner.set_source(&info),
                        Ok(IngestEvent::SourceRemoved(id)) => {
                            inner.sources_mut().remove(&id);
                        }
                        Ok(IngestEvent::Progress(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            inner.reload_sources(&st).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
        });
    }

    /// The executors to hand to `nugget_proactive::ActionRunner::new(policy, executors)`.
    pub(crate) fn executors(&self) -> Executors {
        Executors {
            drafter: self.drafter.clone(),
            mail: Some(self.inner.clone()),
            calendar: Some(self.inner.clone()),
            reminders: Some(self.inner.clone()),
            notes: self
                .inner
                .vault
                .is_some()
                .then(|| self.inner.clone() as Arc<dyn NoteExecutor>),
        }
    }

    /// Whether this action can run here: drafting needs the drafter, archiving a source that
    /// can archive one of the messages, notes the vault; events and reminders always can.
    pub(crate) fn supports(&self, action: &Action) -> bool {
        match action {
            Action::DraftReply(_) => self.drafter.is_some(),
            Action::CreateEvent(_) | Action::Reminder(_) => true,
            Action::ArchiveThread(a) => {
                let sources = self.inner.sources_ref();
                a.messages
                    .iter()
                    .any(|m| sources.get(&m.source).is_some_and(SourceLite::can_archive))
            }
            Action::ResearchNote(_) => self.inner.vault.is_some(),
        }
    }

    /// What the desktop must finish after `action` executed with the executor's result id.
    pub(crate) fn artifact(&self, action: &Action, result_id: &str) -> Option<ActionArtifact> {
        let mut a = ActionArtifact {
            kind: String::new(),
            path: None,
            text: None,
            url: None,
            at: None,
        };
        match action {
            Action::DraftReply(d) => {
                a.kind = "draft".into();
                a.text = d.body.clone();
                a.url = Some(mailto::link(d));
            }
            Action::CreateEvent(_) => {
                if result_id.starts_with("https://") || result_id.starts_with("http://") {
                    a.kind = "event".into();
                    a.url = Some(result_id.to_owned());
                } else {
                    a.kind = "file".into();
                    a.path = Some(result_id.to_owned());
                }
            }
            Action::Reminder(r) => {
                a.kind = "reminder".into();
                a.text = Some(r.text.clone());
                a.at = Some(r.at);
            }
            Action::ArchiveThread(_) => a.kind = "archived".into(),
            Action::ResearchNote(_) => {
                a.kind = "note".into();
                a.path = Some(result_id.to_owned());
            }
        }
        Some(a)
    }

    /// Stored reminders not yet fired, soonest first.
    pub(crate) fn pending_reminders(&self) -> Result<Vec<ReminderView>> {
        self.inner.reminders.pending()
    }

    /// Reminders due at or before `now` that have not fired; marks them fired (each fires once).
    pub(crate) fn take_due_reminders(&self, now: OffsetDateTime) -> Result<Vec<ReminderView>> {
        self.inner.reminders.take_due(now)
    }
}

impl Inner {
    fn sources_ref(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<SourceId, SourceLite>> {
        self.sources.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn sources_mut(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<SourceId, SourceLite>> {
        self.sources.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_source(&self, info: &SourceInfo) {
        self.sources_mut()
            .insert(info.id.clone(), SourceLite::of(info));
    }

    async fn reload_sources(&self, state: &State) {
        match state.ingest.list().await {
            Ok(list) => {
                *self.sources_mut() = list
                    .iter()
                    .map(|i| (i.id.clone(), SourceLite::of(i)))
                    .collect();
            }
            Err(e) => tracing::warn!(error = %e, "proactive: cannot list sources"),
        }
    }

    fn source(&self, id: &SourceId) -> Option<SourceLite> {
        self.sources_ref().get(id).cloned()
    }

    fn ctx(&self, source: &SourceId) -> ConnectorContext {
        ConnectorContext {
            source: source.clone(),
            paths: self.paths.clone(),
            egress: self.egress.clone(),
            secrets: self.secrets.clone(),
            known: Arc::new(StoreKnownItems::new(self.store.clone(), source.clone())),
        }
    }

    /// Gmail settings with the OAuth client secret injected from the keychain, as ingest does.
    fn gmail_settings(&self, id: &SourceId, settings: &Value) -> Value {
        let mut settings = settings.clone();
        if let (Value::Object(map), Ok(Some(secret))) =
            (&mut settings, self.secrets.get(&client_secret_account(id)))
            && !secret.is_empty()
        {
            map.insert(CLIENT_SECRET_FIELD.into(), Value::String(secret));
        }
        settings
    }

    async fn archive_in(&self, id: &SourceId, src: &SourceLite, keys: &[ItemKey]) -> Result<usize> {
        match src.kind.as_str() {
            nugget_connector_imap::KIND => {
                let conn = ImapConnector::new(ImapSettings::parse(&src.settings)?, self.ctx(id));
                conn.archive(keys, &self.cancel).await
            }
            "gmail" => {
                let settings = self.gmail_settings(id, &src.settings);
                let conn = GmailFactory::new().connector(&settings, self.ctx(id))?;
                let ids: Vec<String> = keys.iter().map(|k| k.as_str().to_owned()).collect();
                conn.archive(&ids, &self.cancel).await
            }
            k => Err(Error::Unsupported(format!(
                "{k} sources cannot archive mail"
            ))),
        }
    }

    /// The first enabled CalDAV source.
    fn caldav(&self) -> Option<(SourceId, Value)> {
        self.sources_ref()
            .iter()
            .find(|(_, s)| s.enabled && s.kind == nugget_connector_caldav::KIND)
            .map(|(id, s)| (id.clone(), s.settings.clone()))
    }

    async fn put_caldav(
        &self,
        id: &SourceId,
        settings: &Value,
        resource: &str,
        ics: &str,
    ) -> Result<String> {
        let conn = CalDavConnector::new(CalDavSettings::parse(settings)?, self.ctx(id))?;
        conn.put_event(resource, ics, &self.cancel).await
    }

    async fn write_ics(&self, title: &str, short: &str, ics: String) -> Result<String> {
        let dir = self.events_dir.clone();
        let name = format!("{}-{short}.ics", file_stem(title));
        blocking(move || {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(name);
            // A fresh name every time: never overwrite (or follow) an existing entry.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            std::io::Write::write_all(&mut f, ics.as_bytes())?;
            f.sync_all()?;
            Ok(path.display().to_string())
        })
        .await
    }
}

/// A file-name-safe stem: ASCII letters, digits, `-` and `_`; at most 60 characters.
fn file_stem(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if out.len() >= 60 {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if (c.is_whitespace() || c.is_ascii_punctuation()) && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() { "event".into() } else { out }
}

/// The note's Markdown: the body, then the items it was written from.
fn note_text(n: &ResearchNote) -> String {
    let mut text = format!("# {}\n\n{}\n", n.title.trim(), n.body.trim_end());
    if !n.sources.is_empty() {
        text.push_str("\n## Sources\n\n");
        for s in &n.sources {
            text.push_str(&format!("- {}: {}\n", s.source, s.key));
        }
    }
    text
}

#[async_trait]
impl MailExecutor for Inner {
    /// Kept in the encrypted store for the user to copy or open in their mail app. Never sent.
    async fn save_draft(&self, draft: &DraftReply) -> Result<String> {
        if draft.body.as_deref().is_none_or(|b| b.trim().is_empty()) {
            return Err(Error::Invalid("the draft has no body".into()));
        }
        self.drafts.save(draft, OffsetDateTime::now_utc())
    }

    async fn archive_thread(&self, archive: &ArchiveThread) -> Result<String> {
        let mut by_source: BTreeMap<SourceId, Vec<ItemKey>> = BTreeMap::new();
        for m in &archive.messages {
            by_source
                .entry(m.source.clone())
                .or_default()
                .push(m.key.clone());
        }
        let mut moved = 0;
        let mut tried = false;
        let mut first_err = None;
        for (id, keys) in by_source {
            let Some(src) = self.source(&id).filter(SourceLite::can_archive) else {
                continue;
            };
            tried = true;
            match self.archive_in(&id, &src, &keys).await {
                Ok(n) => moved += n,
                Err(e) => {
                    tracing::warn!(source = %id, error = %e, "proactive: archive failed");
                    first_err.get_or_insert(e);
                }
            }
        }
        if !tried {
            return Err(Error::Unsupported(
                "none of these messages is in a mailbox that can archive".into(),
            ));
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(format!("archived:{moved}"))
    }
}

#[async_trait]
impl CalendarExecutor for Inner {
    /// CalDAV when configured (falling back to a file if the server refuses), else an `.ics`
    /// file. Returns the event's URL or the file's path.
    async fn create_event(&self, event: &CreateEvent) -> Result<String> {
        let short = uuid::Uuid::new_v4().simple().to_string();
        let uid = format!("{short}@nugget");
        let now = OffsetDateTime::now_utc();
        if let Some((id, settings)) = self.caldav() {
            let ics = ics::render(event, &uid, now, None);
            match self
                .put_caldav(&id, &settings, &format!("{short}.ics"), &ics)
                .await
            {
                Ok(url) => return Ok(url),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => {
                    tracing::warn!(source = %id, error = %e, "proactive: CalDAV PUT failed; writing an .ics file");
                }
            }
        }
        let ics = ics::render(event, &uid, now, Some("PUBLISH"));
        self.write_ics(&event.title, &short[..8], ics).await
    }
}

#[async_trait]
impl ReminderExecutor for Inner {
    async fn create_reminder(&self, reminder: &Reminder) -> Result<String> {
        self.reminders.add(reminder, None)
    }
}

#[async_trait]
impl NoteExecutor for Inner {
    async fn write_note(&self, note: &ResearchNote) -> Result<String> {
        let vault = self
            .vault
            .clone()
            .ok_or_else(|| Error::Unsupported("the vault is disabled".into()))?;
        let (title, text) = (note.title.clone(), note_text(note));
        blocking(move || {
            vault
                .write_note(NOTES_FOLDER, &title, &text)
                .map(|p| p.display().to_string())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::ItemRef;
    use nugget_ingest::MemorySecrets;
    use nugget_policy::{ActionDescriptor, Decision, PolicyGate};
    use nugget_proactive::{ActionRunner, ExecOutcome};
    use serde_json::json;

    struct Allow;
    impl PolicyGate for Allow {
        fn evaluate(&self, _: &ActionDescriptor) -> Decision {
            Decision::Allow
        }
        fn halted(&self) -> bool {
            false
        }
    }

    struct Fixed;
    #[async_trait]
    impl Drafter for Fixed {
        async fn draft_reply(&self, _: &nugget_proactive::DraftRequest) -> Result<String> {
            Ok("Hi Ana,\n\nHere it is.".into())
        }
    }

    struct Fx {
        _dir: tempfile::TempDir,
        exec: Exec,
        store: Store,
        data: PathBuf,
    }

    fn fixture(with_vault: bool) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let paths = Paths::rooted(dir.path().join("home"));
        let vault = with_vault.then(|| {
            Arc::new(
                Vault::new(
                    dir.path().join("vault"),
                    Arc::new(store.graph()),
                    Arc::new(nugget_vault::NoItems),
                    nugget_vault::VaultConfig::default(),
                )
                .unwrap(),
            )
        });
        let data = paths.data.clone();
        let exec = Exec::from_parts(Parts {
            store: store.clone(),
            paths,
            vault,
            secrets: Arc::new(MemorySecrets::new()),
            egress: Arc::new(store.clone()),
            drafter: Some(Arc::new(Fixed)),
            cancel: CancellationToken::new(),
        });
        Fx {
            _dir: dir,
            exec,
            store,
            data,
        }
    }

    fn item(source: &str, key: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new(source),
            key: ItemKey::new(key),
        }
    }

    fn add_source(exec: &Exec, id: &str, kind: &str, enabled: bool, settings: Value) {
        exec.inner.sources_mut().insert(
            SourceId::new(id),
            SourceLite {
                kind: kind.into(),
                enabled,
                settings,
            },
        );
    }

    fn archive(sources: &[&str]) -> Action {
        Action::ArchiveThread(ArchiveThread {
            messages: sources.iter().map(|s| item(s, "INBOX/7/1")).collect(),
            subject: "Newsletter".into(),
        })
    }

    #[test]
    fn archive_is_supported_only_for_sources_that_can() {
        let f = fixture(false);
        let e = &f.exec;
        add_source(e, "imap-1", "imap", true, json!({}));
        add_source(e, "imap-off", "imap", false, json!({}));
        add_source(e, "gmail-ro", "gmail", true, json!({"client_id": "x"}));
        add_source(
            e,
            "gmail-rw",
            "gmail",
            true,
            json!({"client_id": "x", "allow_modify": true}),
        );
        add_source(e, "files", "files", true, json!({}));
        assert!(e.supports(&archive(&["imap-1"])));
        assert!(e.supports(&archive(&["files", "imap-1"])));
        assert!(e.supports(&archive(&["gmail-rw"])));
        assert!(!e.supports(&archive(&["gmail-ro"])));
        assert!(!e.supports(&archive(&["imap-off"])));
        assert!(!e.supports(&archive(&["files"])));
        assert!(!e.supports(&archive(&["unknown"])));
        assert!(!e.supports(&archive(&[])));
        // Events and reminders always; notes only with a vault.
        let note = Action::ResearchNote(ResearchNote {
            title: "t".into(),
            body: "b".into(),
            sources: vec![],
        });
        assert!(!e.supports(&note));
        assert!(fixture(true).exec.supports(&note));
    }

    #[tokio::test]
    async fn archiving_without_a_capable_source_is_unsupported() {
        let f = fixture(false);
        add_source(&f.exec, "files", "files", true, json!({}));
        let Action::ArchiveThread(a) = archive(&["files"]) else {
            unreachable!()
        };
        let err = f.exec.inner.archive_thread(&a).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err}");
    }

    #[tokio::test]
    async fn events_without_caldav_become_ics_files() {
        let f = fixture(false);
        let ev = CreateEvent {
            title: "Acme / pilot kickoff".into(),
            start: OffsetDateTime::now_utc(),
            end: OffsetDateTime::now_utc() + time::Duration::hours(1),
            attendees: vec!["ana@acme.io".into()],
            description: Some("Agenda".into()),
        };
        let path = f.exec.inner.create_event(&ev).await.unwrap();
        assert!(
            PathBuf::from(&path).starts_with(f.data.join("proactive").join("events")),
            "{path}"
        );
        assert!(path.ends_with(".ics"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(text.contains("SUMMARY:Acme / pilot kickoff\r\n"));
        assert!(text.contains("mailto:ana@acme.io"));
        let art = f.exec.artifact(&Action::CreateEvent(ev), &path).unwrap();
        assert_eq!(art.kind, "file");
        assert_eq!(art.path.as_deref(), Some(path.as_str()));
        let ev_art = f
            .exec
            .artifact(
                &Action::CreateEvent(CreateEvent {
                    title: "x".into(),
                    start: OffsetDateTime::now_utc(),
                    end: OffsetDateTime::now_utc(),
                    attendees: vec![],
                    description: None,
                }),
                "https://dav.example/cal/x.ics",
            )
            .unwrap();
        assert_eq!(ev_art.kind, "event");
        assert_eq!(ev_art.url.as_deref(), Some("https://dav.example/cal/x.ics"));
    }

    #[tokio::test]
    async fn notes_are_written_through_the_vault() {
        let f = fixture(true);
        let note = ResearchNote {
            title: "Prep: Acme kickoff".into(),
            body: "- agenda\n- pricing".into(),
            sources: vec![item("mail", "m1")],
        };
        let path = f.exec.inner.write_note(&note).await.unwrap();
        let vault_root = f.exec.inner.vault.as_ref().unwrap().root().to_path_buf();
        assert!(PathBuf::from(&path).starts_with(vault_root.join(NOTES_FOLDER)));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Prep: Acme kickoff\n\n- agenda\n- pricing\n"));
        assert!(text.contains("## Sources\n\n- mail: m1\n"));
        let art = f.exec.artifact(&Action::ResearchNote(note), &path).unwrap();
        assert_eq!(art.kind, "note");
        assert_eq!(art.path.as_deref(), Some(path.as_str()));
    }

    #[tokio::test]
    async fn the_runner_drafts_keeps_reminds_and_never_sends() {
        let f = fixture(true);
        let runner =
            ActionRunner::new(Arc::new(Allow), f.exec.executors()).with_audit(f.store.clone());
        let draft = Action::DraftReply(DraftReply {
            reply_to: item("mail", "m1"),
            in_reply_to_message_id: Some("<m1@acme.io>".into()),
            to: vec!["ana@acme.io".into()],
            cc: vec![],
            subject: "Re: Pricing".into(),
            intent: "send the sheet".into(),
            body: None,
        });
        assert!(f.exec.supports(&draft));
        let prepared = runner.prepare("card-1", &draft, &[]).await.unwrap();
        let out = runner
            .execute(&prepared, &prepared.approve(false))
            .await
            .unwrap();
        let ExecOutcome::Executed(id) = out else {
            panic!("{out:?}")
        };
        let stored = f.exec.inner.drafts.get(&id).unwrap().unwrap();
        assert_eq!(stored.body, "Hi Ana,\n\nHere it is.");
        let art = f.exec.artifact(&prepared.action, &id).unwrap();
        assert_eq!(art.kind, "draft");
        assert_eq!(art.text.as_deref(), Some("Hi Ana,\n\nHere it is."));
        assert!(
            art.url
                .as_deref()
                .unwrap()
                .starts_with("mailto:ana@acme.io?subject=Re%3A%20Pricing&body=Hi%20Ana")
        );
        // Nothing went over the network.
        assert!(f.store.recent_egress(10).unwrap().is_empty());

        let at = OffsetDateTime::now_utc() + time::Duration::minutes(5);
        let rem = Action::Reminder(Reminder {
            text: "Call Ana".into(),
            at,
            about: Some(item("mail", "m1")),
        });
        let prepared = runner.prepare("card-2", &rem, &[]).await.unwrap();
        let out = runner
            .execute(&prepared, &prepared.approve(false))
            .await
            .unwrap();
        let ExecOutcome::Executed(id) = out else {
            panic!("{out:?}")
        };
        let pending = f.exec.pending_reminders().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        let art = f.exec.artifact(&rem, &id).unwrap();
        assert_eq!((art.kind.as_str(), art.at), ("reminder", Some(at)));
        assert!(
            f.exec
                .take_due_reminders(OffsetDateTime::now_utc())
                .unwrap()
                .is_empty()
        );
        let due = f
            .exec
            .take_due_reminders(at + time::Duration::seconds(1))
            .unwrap();
        assert_eq!(due.len(), 1);
        assert!(
            f.exec
                .take_due_reminders(at + time::Duration::hours(1))
                .unwrap()
                .is_empty()
        );
        assert!(f.exec.pending_reminders().unwrap().is_empty());

        // Archiving is gated by confirmation before any executor runs.
        add_source(&f.exec, "imap-1", "imap", true, json!({}));
        let arch = archive(&["imap-1"]);
        let prepared = runner.prepare("card-3", &arch, &[]).await.unwrap();
        let out = runner
            .execute(&prepared, &prepared.approve(false))
            .await
            .unwrap();
        assert!(matches!(out, ExecOutcome::NeedsConfirmation(_)), "{out:?}");
    }

    #[test]
    fn file_stems_are_safe() {
        assert_eq!(file_stem("Acme / pilot: kickoff!"), "Acme-pilot-kickoff");
        assert_eq!(file_stem("../../etc"), "etc");
        assert_eq!(file_stem("日本"), "event");
        assert!(file_stem(&"x".repeat(200)).len() <= 60);
    }
}
