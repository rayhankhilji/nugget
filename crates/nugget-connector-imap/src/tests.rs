mod server;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::{
    AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory,
    Content, KnownItems, SecretStore,
};
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use serde_json::{Value, json};

use self::server::{Mode, State, TestServer};
use super::*;

// ---------------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------------

/// Applies changes like the pipeline would, and doubles as `KnownItems`.
/// Stored item: version, content bytes, metadata.
type Stored = (Option<String>, Vec<u8>, Value);

#[derive(Default)]
struct FakeStore {
    items: Mutex<HashMap<ItemKey, Stored>>,
    log: Mutex<Vec<Change>>,
    checkpoints: Mutex<Vec<Value>>,
}

impl FakeStore {
    fn take_log(&self) -> Vec<Change> {
        std::mem::take(&mut self.log.lock().unwrap())
    }
    fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self
            .items
            .lock()
            .unwrap()
            .keys()
            .map(|k| k.0.clone())
            .collect();
        k.sort();
        k
    }
}

#[async_trait]
impl ChangeSink for FakeStore {
    async fn send(&self, change: Change) -> Result<()> {
        match &change {
            Change::Upsert(d) => {
                let bytes = match &d.content {
                    Content::Bytes(b) => b.clone(),
                    _ => panic!("expected bytes"),
                };
                self.items.lock().unwrap().insert(
                    d.key.clone(),
                    (d.version.clone(), bytes, d.metadata.clone()),
                );
            }
            Change::Delete(k) => {
                self.items.lock().unwrap().remove(k);
            }
        }
        self.log.lock().unwrap().push(change);
        Ok(())
    }

    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        self.checkpoints.lock().unwrap().push(cursor);
        Ok(())
    }
}

struct Known(Arc<FakeStore>);

#[async_trait]
impl KnownItems for Known {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>> {
        Ok(self
            .0
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.0.clone()))
            .collect())
    }
}

#[derive(Default)]
struct Secrets(Mutex<HashMap<String, String>>);

impl SecretStore for Secrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

#[derive(Default)]
struct Egress(Mutex<Vec<EgressEvent>>);

impl EgressSink for Egress {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

struct Harness {
    server: TestServer,
    store: Arc<FakeStore>,
    secrets: Arc<Secrets>,
    egress: Arc<Egress>,
    settings: Value,
}

const SOURCE: &str = "imap:me@example.com";

impl Harness {
    async fn new(state: State, mode: Mode, extra: Value) -> Self {
        let server = TestServer::start(state, mode).await;
        let mut settings = json!({
            "host": "127.0.0.1",
            "port": server.port(),
            "tls": if matches!(mode, Mode::Starttls | Mode::StarttlsRefused) { "starttls" } else { "implicit" },
            "username": "me@example.com",
        });
        if let (Value::Object(s), Value::Object(e)) = (&mut settings, extra) {
            s.extend(e);
        }
        let secrets = Arc::new(Secrets::default());
        secrets.set(&password_account(SOURCE), "app-pass").unwrap();
        Self {
            server,
            store: Arc::new(FakeStore::default()),
            secrets,
            egress: Arc::new(Egress::default()),
            settings,
        }
    }

    fn connector(&self) -> ImapConnector {
        let ctx = ConnectorContext {
            source: SourceId::new(SOURCE),
            paths: Paths::rooted(std::env::temp_dir().join("nugget-imap-tests")),
            egress: self.egress.clone(),
            secrets: self.secrets.clone(),
            known: Arc::new(Known(self.store.clone())),
        };
        ImapConnector::new(ImapSettings::parse(&self.settings).unwrap(), ctx)
            .with_tls_config(self.server.client_tls.clone())
            .with_timeouts(Duration::from_secs(10), Duration::from_secs(30))
    }

    async fn sync(&self, cursor: Option<Value>) -> Result<Value> {
        self.connector()
            .sync(cursor, &*self.store, &CancellationToken::new())
            .await
    }
}

fn msg(subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: a@example.com\r\nTo: me@example.com\r\nSubject: {subject}\r\nMessage-ID: <{id}@example.com>\r\n\r\nHello {subject}\r\n"
    )
    .into_bytes()
}

/// INBOX (uidvalidity 7) with 3 messages, Sent (\Sent) with 1, Trash (\Trash) with 1.
fn mailbox_state() -> State {
    let mut s = State::new();
    s.mailbox("INBOX", 7);
    s.mailbox("Sent", 11).special = Some("\\Sent");
    s.mailbox("Trash", 13).special = Some("\\Trash");
    for i in 1..=3 {
        s.add("INBOX", &msg(&format!("in{i}"), &format!("in{i}")));
    }
    s.add("Sent", &msg("sent1", "sent1"));
    s.add("Trash", &msg("trash1", "trash1"));
    s
}

fn upserts(log: &[Change]) -> Vec<String> {
    log.iter()
        .filter_map(|c| match c {
            Change::Upsert(d) => Some(d.key.0.clone()),
            Change::Delete(_) => None,
        })
        .collect()
}

fn deletes(log: &[Change]) -> Vec<String> {
    log.iter()
        .filter_map(|c| match c {
            Change::Delete(k) => Some(k.0.clone()),
            Change::Upsert(_) => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Sync behaviour
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn initial_sync_fetches_inbox_and_sent_with_keys_versions_and_metadata() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();

    let log = h.store.take_log();
    assert_eq!(
        upserts(&log),
        vec!["INBOX/7/1", "INBOX/7/2", "INBOX/7/3", "Sent/11/1"]
    );
    let Change::Upsert(doc) = &log[0] else {
        panic!()
    };
    assert_eq!(doc.mime, "message/rfc822");
    assert_eq!(doc.content, Content::Bytes(msg("in1", "in1")));
    assert!(doc.version.as_deref().unwrap().starts_with("modseq:"));
    assert_eq!(doc.metadata["folder"], "INBOX");
    assert_eq!(doc.metadata["message_id"], "<in1@example.com>");
    assert_eq!(doc.metadata["flags"], json!([]));
    assert!(doc.modified_at.is_some());
    assert_eq!(
        doc.url.as_deref(),
        Some(
            format!(
                "imap://me%40example.com@127.0.0.1:{}/INBOX;UIDVALIDITY=7/;UID=1",
                h.server.port()
            )
            .as_str()
        )
    );

    assert_eq!(cursor["v"], 1);
    assert_eq!(cursor["folders"]["INBOX"]["uidvalidity"], 7);
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], 3);
    assert!(cursor["folders"]["INBOX"]["highest_modseq"].is_u64());
    assert_eq!(cursor["folders"]["Sent"]["last_uid"], 1);
    assert!(cursor["folders"].get("Trash").is_none());
    assert!(!h.store.checkpoints.lock().unwrap().is_empty());
    assert!(h.connector().poll_interval() > Duration::ZERO);
}

#[tokio::test]
async fn seen_is_never_set_and_mailboxes_are_read_only() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();
    h.server.with(|s| s.add("INBOX", &msg("in4", "in4")));
    h.sync(Some(cursor)).await.unwrap();

    for uid in 1..=4 {
        assert!(h.server.with(|s| s.flags("INBOX", uid)).is_empty());
    }
    let log = h.server.log();
    assert!(log.iter().any(|l| l.contains("EXAMINE")));
    for l in &log {
        let u = l.to_ascii_uppercase();
        assert!(!u.contains(" SELECT "), "{l}");
        assert!(!u.contains(" STORE "), "{l}");
        if u.contains("FETCH") && u.contains("BODY") {
            assert!(u.contains("BODY.PEEK["), "{l}");
        }
    }
}

#[tokio::test]
async fn incremental_sync_fetches_only_new_mail_and_skips_unchanged_mailboxes() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();
    h.store.take_log();

    // Nothing changed: no changes, and no FETCH at all (HIGHESTMODSEQ unchanged).
    let before = h.server.log().len();
    let cursor = h.sync(Some(cursor)).await.unwrap();
    assert!(h.store.take_log().is_empty());
    assert!(
        !h.server.log()[before..]
            .iter()
            .any(|l| l.contains("FETCH") || l.contains("SEARCH"))
    );

    let uid = h.server.with(|s| s.add("INBOX", &msg("in4", "in4")));
    let cursor = h.sync(Some(cursor)).await.unwrap();
    assert_eq!(upserts(&h.store.take_log()), vec![format!("INBOX/7/{uid}")]);
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], uid);
}

#[tokio::test]
async fn qresync_vanished_reports_deletes() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();
    h.store.take_log();
    h.server.with(|s| s.expunge("INBOX", 2));
    h.sync(Some(cursor)).await.unwrap();
    assert_eq!(deletes(&h.store.take_log()), vec!["INBOX/7/2"]);
    assert!(h.server.log().iter().any(|l| l.contains("VANISHED")));
    assert!(!h.store.keys().contains(&"INBOX/7/2".to_string()));
}

#[tokio::test]
async fn condstore_without_qresync_uses_uid_search_all_for_deletes() {
    let mut st = mailbox_state();
    st.qresync = false;
    let h = Harness::new(st, Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();
    h.store.take_log();
    h.server.with(|s| s.expunge("INBOX", 1));
    h.sync(Some(cursor)).await.unwrap();
    assert_eq!(deletes(&h.store.take_log()), vec!["INBOX/7/1"]);
    let log = h.server.log();
    assert!(log.iter().any(|l| l.contains("UID SEARCH ALL")));
    assert!(log.iter().any(|l| l.contains("CHANGEDSINCE")));
    assert!(!log.iter().any(|l| l.contains("VANISHED")));
}

#[tokio::test]
async fn flag_change_under_condstore_refetches_with_new_modseq_version() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();
    h.store.take_log();
    h.server.with(|s| s.set_flag("INBOX", 2, "\\Flagged"));
    h.sync(Some(cursor)).await.unwrap();
    let log = h.store.take_log();
    assert_eq!(upserts(&log), vec!["INBOX/7/2"]);
    let Change::Upsert(doc) = &log[0] else {
        panic!()
    };
    let ms = h.server.with(|s| s.mailboxes["INBOX"].msgs[&2].modseq);
    assert_eq!(
        doc.version.as_deref(),
        Some(format!("modseq:{ms}").as_str())
    );
    assert_eq!(doc.metadata["flags"], json!(["\\Flagged"]));
}

#[tokio::test]
async fn without_condstore_flags_hash_and_uid_search_all_drive_changes() {
    let mut st = mailbox_state();
    st.condstore = false;
    st.qresync = false;
    let h = Harness::new(st, Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();
    let log = h.store.take_log();
    let Change::Upsert(doc) = &log[0] else {
        panic!()
    };
    assert!(doc.version.as_deref().unwrap().starts_with("flags:"));
    assert!(cursor["folders"]["INBOX"].get("highest_modseq").is_none());

    // Unchanged: nothing emitted.
    let cursor = h.sync(Some(cursor)).await.unwrap();
    assert!(h.store.take_log().is_empty());

    h.server.with(|s| {
        s.set_flag("INBOX", 3, "\\Answered");
        s.expunge("INBOX", 1);
    });
    h.sync(Some(cursor)).await.unwrap();
    let log = h.store.take_log();
    assert_eq!(deletes(&log), vec!["INBOX/7/1"]);
    assert_eq!(upserts(&log), vec!["INBOX/7/3"]);
}

#[tokio::test]
async fn uidvalidity_change_resyncs_the_mailbox() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let cursor = h.sync(None).await.unwrap();
    h.store.take_log();
    h.server.with(|s| s.reset_uidvalidity("INBOX", 99));
    let cursor = h.sync(Some(cursor)).await.unwrap();
    let log = h.store.take_log();
    let mut d = deletes(&log);
    d.sort();
    assert_eq!(d, vec!["INBOX/7/1", "INBOX/7/2", "INBOX/7/3"]);
    assert_eq!(
        upserts(&log),
        vec!["INBOX/99/1", "INBOX/99/2", "INBOX/99/3"]
    );
    assert_eq!(cursor["folders"]["INBOX"]["uidvalidity"], 99);
    assert_eq!(
        h.store.keys(),
        vec!["INBOX/99/1", "INBOX/99/2", "INBOX/99/3", "Sent/11/1"]
    );
}

#[tokio::test]
async fn missing_or_rejected_password_is_unauthorized() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    h.secrets.delete(&password_account(SOURCE)).unwrap();
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    // Nothing touched the network.
    assert!(h.server.log().is_empty());

    h.secrets.set(&password_account(SOURCE), "wrong").unwrap();
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
}

#[tokio::test]
async fn egress_counts_wire_bytes_with_sync_imap_feature() {
    let mut st = mailbox_state();
    let big = vec![b'x'; 200_000];
    let mut body = msg("big", "big");
    body.extend_from_slice(&big);
    st.add("INBOX", &body);
    let h = Harness::new(st, Mode::Implicit, json!({})).await;
    h.sync(None).await.unwrap();
    let events = h.egress.0.lock().unwrap().clone();
    assert!(!events.is_empty());
    let host = format!("127.0.0.1:{}", h.server.port());
    for e in &events {
        assert_eq!(e.feature, "sync:imap");
        assert_eq!(e.host, host);
    }
    let received: u64 = events.iter().map(|e| e.bytes_received).sum();
    let sent: u64 = events.iter().map(|e| e.bytes_sent).sum();
    assert!(received > 200_000, "{received}");
    assert!(sent > 100, "{sent}");
}

#[tokio::test]
async fn starttls_upgrades_before_login() {
    let h = Harness::new(mailbox_state(), Mode::Starttls, json!({})).await;
    h.sync(None).await.unwrap();
    assert_eq!(upserts(&h.store.take_log()).len(), 4);
    let log = h.server.log();
    assert!(log[0].ends_with("STARTTLS"));
}

#[tokio::test]
async fn refused_starttls_and_plaintext_servers_fail_closed() {
    let h = Harness::new(mailbox_state(), Mode::StarttlsRefused, json!({})).await;
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
    assert!(!h.server.log().iter().any(|l| l.contains("LOGIN")));

    let h = Harness::new(mailbox_state(), Mode::Plaintext, json!({})).await;
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert_eq!(h.server.with(|s| s.logins), 0);
}

#[tokio::test]
async fn window_days_limits_the_backfill() {
    let mut st = mailbox_state();
    st.add_dated("INBOX", &msg("old", "old"), "1-Jan-2015 10:00:00 +0000");
    let h = Harness::new(st, Mode::Implicit, json!({"window_days": 30})).await;
    let cursor = h.sync(None).await.unwrap();
    assert_eq!(upserts(&h.store.take_log()).len(), 4);
    // The old message is behind last_uid and never fetched.
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], 4);
    assert!(
        h.server
            .log()
            .iter()
            .any(|l| l.contains("UID SEARCH SINCE"))
    );
}

#[tokio::test]
async fn star_selects_all_but_trash_and_junk_and_dropped_folders_are_deleted() {
    let mut st = mailbox_state();
    st.mailbox("Spam", 17);
    st.add("Spam", &msg("spam", "spam"));
    st.mailbox("Archive", 19);
    st.add("Archive", &msg("arch", "arch"));
    let h = Harness::new(st, Mode::Implicit, json!({"folders": ["*"]})).await;
    let cursor = h.sync(None).await.unwrap();
    let mut keys = upserts(&h.store.take_log());
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "Archive/19/1",
            "INBOX/7/1",
            "INBOX/7/2",
            "INBOX/7/3",
            "Sent/11/1"
        ]
    );

    // Narrow the selection: Archive and Sent items go away.
    let mut h2 = h;
    h2.settings["folders"] = json!(["INBOX"]);
    let cursor = h2.sync(Some(cursor)).await.unwrap();
    let mut d = deletes(&h2.store.take_log());
    d.sort();
    assert_eq!(d, vec!["Archive/19/1", "Sent/11/1"]);
    assert_eq!(
        cursor["folders"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["INBOX"]
    );
}

#[tokio::test]
async fn large_mailboxes_checkpoint_per_batch() {
    let mut st = State::new();
    st.mailbox("INBOX", 3);
    for i in 0..450 {
        st.add(
            "INBOX",
            format!("Subject: {i}\r\n\r\nbody {i}\r\n").as_bytes(),
        );
    }
    let h = Harness::new(st, Mode::Implicit, json!({})).await;
    h.sync(None).await.unwrap();
    assert_eq!(upserts(&h.store.take_log()).len(), 450);
    let cps = h.store.checkpoints.lock().unwrap().clone();
    let last_uids: Vec<u64> = cps
        .iter()
        .map(|c| c["folders"]["INBOX"]["last_uid"].as_u64().unwrap())
        .collect();
    assert_eq!(last_uids, vec![200, 400, 450, 450]);
}

#[tokio::test]
async fn resync_after_lost_cursor_skips_unchanged_items() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    h.sync(None).await.unwrap();
    h.store.take_log();
    h.server.with(|s| s.expunge("INBOX", 3));
    h.sync(None).await.unwrap();
    // Only the delete; nothing re-downloaded.
    let log = h.store.take_log();
    assert_eq!(deletes(&log), vec!["INBOX/7/3"]);
    assert!(upserts(&log).is_empty());
}

#[tokio::test]
async fn oversized_messages_are_indexed_by_headers_only() {
    let mut st = State::new();
    st.mailbox("INBOX", 5);
    let mut body = msg("huge", "huge");
    body.extend(std::iter::repeat_n(b'y', 1_200_000));
    st.add("INBOX", &body);
    let h = Harness::new(st, Mode::Implicit, json!({"max_message_mb": 1})).await;
    h.sync(None).await.unwrap();
    let log = h.store.take_log();
    let Change::Upsert(doc) = &log[0] else {
        panic!()
    };
    assert_eq!(doc.metadata["truncated"], true);
    let Content::Bytes(b) = &doc.content else {
        panic!()
    };
    assert!(b.len() < 1000 && b.ends_with(b"\r\n\r\n"));
    assert!(
        h.server
            .log()
            .iter()
            .any(|l| l.contains("BODY.PEEK[HEADER]"))
    );
}

#[tokio::test]
async fn malformed_server_responses_error_without_panicking() {
    let mut st = mailbox_state();
    st.garbage = true;
    let h = Harness::new(st, Mode::Implicit, json!({})).await;
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
}

#[tokio::test]
async fn a_silent_server_times_out() {
    let h = Harness::new(mailbox_state(), Mode::Silent, json!({})).await;
    let c = h
        .connector()
        .with_timeouts(Duration::from_millis(300), Duration::from_secs(30));
    let started = std::time::Instant::now();
    let err = c
        .sync(None, &*h.store, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Network(m) if m.contains("timed out")),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn cancelled_sync_returns_cancelled() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = h
        .connector()
        .sync(None, &*h.store, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

#[tokio::test]
async fn idle_watch_delivers_new_mail_and_stops_on_cancel() {
    let h = Harness::new(mailbox_state(), Mode::Implicit, json!({})).await;
    let c = Arc::new(h.connector());
    assert!(!c.supports_watch());
    c.sync(None, &*h.store, &CancellationToken::new())
        .await
        .unwrap();
    assert!(c.supports_watch());
    h.store.take_log();

    let cancel = CancellationToken::new();
    let task = {
        let (c, sink, cancel) = (
            c.clone(),
            h.store.clone() as Arc<dyn ChangeSink>,
            cancel.clone(),
        );
        tokio::spawn(async move { c.watch(sink, cancel).await })
    };
    // Wait until the client is idling.
    for _ in 0..200 {
        if h.server.log().iter().any(|l| l.ends_with("IDLE")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let uid = h.server.with(|s| s.add("INBOX", &msg("live", "live")));
    h.server.push(&format!("* {uid} EXISTS"));
    let mut got = Vec::new();
    for _ in 0..300 {
        got = upserts(&h.store.log.lock().unwrap());
        if !got.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(got, vec![format!("INBOX/7/{uid}")]);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(h.server.log().iter().any(|l| l == "DONE"));
    assert!(h.server.with(|s| s.flags("INBOX", uid)).is_empty());
}

#[tokio::test]
async fn watch_without_idle_is_unsupported() {
    let mut st = mailbox_state();
    st.idle = false;
    let h = Harness::new(st, Mode::Implicit, json!({})).await;
    let c = h.connector();
    c.sync(None, &*h.store, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!c.supports_watch());
    let err = c
        .watch(h.store.clone(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

// ---------------------------------------------------------------------------------------------
// Factory, settings and helpers
// ---------------------------------------------------------------------------------------------

#[test]
fn factory_describes_kind_and_validates_settings() {
    let f = ImapFactory::new();
    let info = f.info();
    assert_eq!(info.kind, "imap");
    assert_eq!(info.auth, AuthKind::Password);
    assert_eq!(
        info.settings_schema["required"],
        json!(["host", "username"])
    );

    let ok = json!({"host": "imap.fastmail.com", "username": "me@fastmail.com"});
    f.validate(&ok).unwrap();
    let s = ImapSettings::parse(&ok).unwrap();
    assert_eq!(
        (s.port(), s.tls, s.window_days),
        (993, TlsMode::Implicit, 365)
    );
    let s = ImapSettings::parse(&json!({"host": "h", "username": "u", "tls": "starttls"})).unwrap();
    assert_eq!(s.port(), 143);

    for bad in [
        json!({"username": "u"}),
        json!({"host": "", "username": "u"}),
        json!({"host": "h/x", "username": "u"}),
        json!({"host": "h", "username": "u", "tls": "none"}),
        json!({"host": "h", "username": "u", "port": 0}),
        json!({"host": "h", "username": "u\r\nx"}),
        json!({"host": "h", "username": "u", "folders": []}),
        json!({"host": "h", "username": "u", "folders": ["a\nb"]}),
        json!({"host": "h", "username": "u", "password": "nope"}),
        json!({"host": "h", "username": "u", "max_message_mb": 0}),
        json!("not an object"),
    ] {
        assert!(f.validate(&bad).is_err(), "{bad}");
    }
}

#[test]
fn keys_round_trip_with_slashes_in_mailbox_names() {
    assert_eq!(item_key("Archive/2024", 9, 42), "Archive/2024/9/42");
    assert_eq!(
        parse_key("Archive/2024/9/42"),
        Some(("Archive/2024", 9, 42))
    );
    assert_eq!(parse_key("INBOX/1/x"), None);
    assert_eq!(parse_key("nope"), None);
}

#[test]
fn uid_sets_compress_and_split() {
    assert_eq!(conn::uid_sets(&[1, 2, 3, 5, 7, 8], 100), vec!["1:3,5,7:8"]);
    assert_eq!(conn::uid_sets(&[], 100), Vec::<String>::new());
    let many: Vec<u32> = (0..1000).map(|i| i * 2).collect();
    let sets = conn::uid_sets(&many, 50);
    assert!(sets.len() > 1 && sets.iter().all(|s| s.len() <= 50));
    let rejoined: Vec<u32> = sets
        .iter()
        .flat_map(|s| s.split(','))
        .map(|x| x.parse().unwrap())
        .collect();
    assert_eq!(rejoined, many);
    assert_eq!(conn::uid_sets(&[u32::MAX], 100), vec![u32::MAX.to_string()]);
}

#[test]
fn mailbox_names_encode_and_quote_safely() {
    assert_eq!(conn::encode_mutf7("INBOX"), "INBOX");
    assert_eq!(conn::encode_mutf7("Entwürfe"), "Entw&APw-rfe");
    assert_eq!(conn::encode_mutf7("A&B"), "A&-B");
    assert_eq!(conn::encode_mutf7("日本語"), "&ZeVnLIqe-");
    assert_eq!(conn::quote("a\"b\\c").unwrap(), r#""a\"b\\c""#);
    assert!(conn::quote("a\r\nb").is_err());
    assert!(conn::quote("é").is_err());
}

#[test]
fn internal_dates_parse_and_reject_garbage() {
    let d = conn::parse_internal_date("17-Jul-1996 02:44:25 -0700").unwrap();
    assert_eq!(d.unix_timestamp(), 837_596_665);
    assert!(conn::parse_internal_date(" 7-Jul-1996 02:44:25 +0000").is_some());
    for bad in [
        "",
        "garbage",
        "32-Jul-1996 02:44:25 +0000",
        "1-Foo-1996 00:00:00 +0000",
        "1-Jan-1996 00:00:00 0000",
        "1-Jan-1996 00:00:00 +ab00",
    ] {
        assert!(conn::parse_internal_date(bad).is_none(), "{bad}");
    }
}

#[test]
fn headers_are_found_unfolded_and_bounded() {
    let m = b"Subject: x\r\nMessage-Id:\r\n <abc@x>\r\n\r\nMessage-ID: <body@x>\r\n";
    assert_eq!(header_value(m, "message-id").as_deref(), Some("<abc@x>"));
    assert_eq!(header_value(b"no headers here", "message-id"), None);
    assert_eq!(header_value(b"", "message-id"), None);
    assert_eq!(header_value(&[0xff, 0xfe, b':', b'\n'], "message-id"), None);
}

#[test]
fn versions_are_stable_and_ignore_recent() {
    let mut m = MessageMeta {
        uid: 1,
        flags: vec!["\\Seen".into(), "\\Flagged".into()],
        ..Default::default()
    };
    let a = version_of(&m, false);
    m.flags = vec!["\\Flagged".into(), "\\Recent".into(), "\\SEEN".into()];
    assert_eq!(version_of(&m, false), a);
    m.modseq = Some(5);
    assert_eq!(version_of(&m, true), "modseq:5");
    assert_eq!(version_of(&m, false), a);
}

#[test]
fn unreadable_cursors_start_fresh() {
    assert_eq!(Cursor::load(Some(json!({"v": 2}))), Cursor::fresh());
    assert_eq!(Cursor::load(Some(json!("junk"))), Cursor::fresh());
    assert_eq!(Cursor::load(None), Cursor::fresh());
    let c = json!({"v": 1, "folders": {"INBOX": {"uidvalidity": 1, "last_uid": 2}}});
    assert_eq!(Cursor::load(Some(c.clone())).to_value(), c);
}

#[test]
fn production_tls_uses_the_platform_verifier() {
    // Builds without a test root; the connector never falls back to anything weaker.
    platform_tls_config().unwrap();
}
