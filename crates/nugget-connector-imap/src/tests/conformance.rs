//! Conformance: the shared robustness properties (`nugget_connector::testkit`) against the
//! scripted IMAP server: many messages across UID ranges and batches, UIDVALIDITY changes,
//! dropped sessions, auth failures, expunges, moves and flag edits, huge messages, unicode,
//! odd dates, broken messages, and resumption after a crash at any send or checkpoint.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use nugget_connector::testkit::{
    MemSecrets, Replica, ReplicaSink, UNICODE_SAMPLES, VecEgress, assert_crash_resumable, context,
    html_mail, run_pass,
};
use nugget_connector::{Connector, Content, KnownItems, SecretStore};
use nugget_core::Error;
use nugget_core::paths::Paths;
use serde_json::{Value, json};

use super::server::{Mode, State, TestServer};
use crate::*;

const SOURCE: &str = "imap:conformance";

struct Env {
    server: TestServer,
    secrets: Arc<MemSecrets>,
    egress: Arc<VecEgress>,
    settings: Value,
}

impl Env {
    async fn new(state: State, extra: Value) -> Self {
        let server = TestServer::start(state, Mode::Implicit).await;
        let mut settings = json!({
            "host": "127.0.0.1",
            "port": server.port(),
            "tls": "implicit",
            "username": "me@example.com",
        });
        if let (Value::Object(s), Value::Object(e)) = (&mut settings, extra) {
            s.extend(e);
        }
        let secrets = Arc::new(MemSecrets::default());
        secrets.set(&password_account(SOURCE), "app-pass").unwrap();
        Self {
            server,
            secrets,
            egress: Arc::new(VecEgress::default()),
            settings,
        }
    }

    fn connector(&self, replica: Arc<Replica>) -> ImapConnector {
        let ctx = context(
            SOURCE,
            Paths::rooted(std::env::temp_dir().join("nugget-imap-conformance")),
            self.egress.clone(),
            self.secrets.clone(),
            replica as Arc<dyn KnownItems>,
        );
        ImapConnector::new(ImapSettings::parse(&self.settings).unwrap(), ctx)
            .with_tls_config(self.server.client_tls.clone())
            .with_timeouts(Duration::from_secs(10), Duration::from_secs(30))
    }

    async fn pass(&self, replica: &Arc<Replica>, cursor: Option<Value>) -> (Value, ReplicaSink) {
        let conn = self.connector(replica.clone());
        let sink = ReplicaSink::new(replica.clone());
        let c = run_pass(&conn, cursor, &sink).await.expect("sync pass");
        (c, sink)
    }

    /// The replica holds exactly the server's messages of the selected mailboxes.
    fn assert_mirrors(&self, replica: &Replica, mailboxes: &[&str]) {
        let want: Vec<String> = self.server.with(|s| {
            let mut v: Vec<String> = mailboxes
                .iter()
                .flat_map(|name| {
                    let mb = &s.mailboxes[*name];
                    mb.msgs
                        .keys()
                        .map(|u| item_key(name, mb.uidvalidity, *u))
                        .collect::<Vec<_>>()
                })
                .collect();
            v.sort();
            v
        });
        assert_eq!(replica.keys(), want);
    }
}

fn msg(subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: a@example.com\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         Message-ID: <{id}@example.com>\r\n\r\nHello {subject}\r\n"
    )
    .into_bytes()
}

fn inbox_and_sent() -> State {
    let mut s = State::new();
    s.mailbox("INBOX", 7);
    s.mailbox("Sent", 11).special = Some("\\Sent");
    s
}

// ---------------------------------------------------------------------------------------------
// Volume, UID ranges, UIDVALIDITY
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn many_messages_across_uid_ranges_and_batches() {
    let mut st = inbox_and_sent();
    for i in 0..640 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    // Sparse UIDs: gaps make the UID sets non-contiguous.
    for uid in (1..=640).filter(|u| u % 7 == 0 || (300..340).contains(u)) {
        st.expunge("INBOX", uid);
    }
    for i in 0..30 {
        st.add("Sent", &msg(&format!("s{i}"), &format!("s{i}")));
    }
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
    // Checkpoints per metadata batch while fetching new mail, plus the final ones.
    assert!(sink.checkpoint_calls() >= 4, "{}", sink.checkpoint_calls());
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], 640);

    // New mail only: no refetch of what is held.
    env.server.with(|s| {
        s.body_fetches = 0;
        for i in 0..30 {
            s.add("INBOX", &msg(&format!("n{i}"), &format!("n{i}")));
        }
    });
    let (_, sink) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(sink.upserts().len(), 30);
    assert_eq!(
        env.server.with(|s| s.body_fetches),
        2,
        "30 new messages, 25 per FETCH"
    );
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
}

#[tokio::test]
async fn uidvalidity_change_triggers_a_reconciling_resync() {
    let mut st = inbox_and_sent();
    for i in 0..12 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    st.add("Sent", &msg("s", "s"));
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    env.server.with(|s| {
        s.expunge("INBOX", 3);
        s.reset_uidvalidity("INBOX", 4242);
    });
    let (cursor, sink) = env.pass(&replica, Some(cursor)).await;
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
    assert!(replica.keys().iter().all(|k| !k.starts_with("INBOX/7/")));
    assert_eq!(
        sink.summary().iter().filter(|s| s.starts_with('-')).count(),
        12
    );
    assert_eq!(cursor["folders"]["INBOX"]["uidvalidity"], 4242);
    // Settled: the next pass is silent.
    let (_, quiet) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(quiet.sends(), 0, "{:?}", quiet.summary());
}

// ---------------------------------------------------------------------------------------------
// Sessions and auth
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn regression_server_bye_mid_fetch_reconnects_within_the_pass() {
    let mut st = inbox_and_sent();
    for i in 0..120 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    st.add("Sent", &msg("s", "s"));
    // The third body FETCH gets `* BYE` and a closed connection.
    st.bye_after_body_fetches = Some(2);
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let (_, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
    assert_eq!(env.server.with(|s| s.logins), 2, "one reconnect");
    // Nothing applied before the drop was fetched again.
    assert_eq!(sink.upserts().len(), 121);
}

#[tokio::test]
async fn a_server_that_keeps_dropping_the_session_fails_the_pass_without_deleting() {
    let mut st = inbox_and_sent();
    for i in 0..60 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    env.server.with(|s| {
        for i in 0..200 {
            s.add("INBOX", &msg(&format!("x{i}"), &format!("x{i}")));
        }
    });
    env.server.with(|s| {
        s.logins = 0;
        s.bye_every_body_fetch = true;
    });
    // Dropped on every body fetch: bounded reconnects, then a transient error (never
    // "reconnect the account"), with nothing deleted.
    let conn = env.connector(replica.clone());
    let err = run_pass(&conn, Some(cursor), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert_eq!(env.server.with(|s| s.logins), 1 + MAX_RECONNECTS);
    assert_eq!(replica.len(), 60);
}

#[tokio::test]
async fn authentication_failure_is_unauthorized() {
    let env = Env::new(inbox_and_sent(), json!({})).await;
    env.secrets
        .set(&password_account(SOURCE), "wrong-password")
        .unwrap();
    let replica = Replica::new();
    let conn = env.connector(replica.clone());
    let err = run_pass(&conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Unauthorized(m) if m.contains("AUTHENTICATIONFAILED") || m.contains("rejected")),
        "{err:?}"
    );
    // The password is never echoed into the error.
    assert!(!err.to_string().contains("wrong-password"));
    env.secrets.delete(&password_account(SOURCE)).unwrap();
    let err = run_pass(&conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
}

// ---------------------------------------------------------------------------------------------
// Deltas: expunges, moves, flags
// ---------------------------------------------------------------------------------------------

async fn deltas(condstore: bool, qresync: bool) {
    let mut st = inbox_and_sent();
    st.condstore = condstore;
    st.qresync = qresync;
    for i in 1..=6 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    st.add("Sent", &msg("s1", "s1"));
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    let before = replica.get("INBOX/7/2").unwrap().version;
    env.server.with(|s| {
        s.expunge("INBOX", 1);
        // Moved between folders (as MOVE does: expunge here, append there).
        let moved = s.mailboxes["INBOX"].msgs[&4].body.clone();
        s.expunge("INBOX", 4);
        s.add("Sent", &moved);
        s.set_flag("INBOX", 2, "\\Flagged");
        s.set_flag("INBOX", 3, "\\Seen");
    });
    let (cursor, sink) = env.pass(&replica, Some(cursor)).await;
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
    let mut s = sink.summary();
    s.sort();
    assert_eq!(
        s,
        vec![
            "+INBOX/7/2",
            "+INBOX/7/3",
            "+Sent/11/2",
            "-INBOX/7/1",
            "-INBOX/7/4"
        ],
        "condstore {condstore} qresync {qresync}"
    );
    assert_ne!(replica.get("INBOX/7/2").unwrap().version, before);
    assert_eq!(
        replica.get("Sent/11/2").unwrap().content,
        String::from_utf8(msg("m4", "m4")).unwrap()
    );
    let (_, quiet) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(quiet.sends(), 0, "{:?}", quiet.summary());
}

#[tokio::test]
async fn expunges_moves_and_flag_edits_with_qresync() {
    deltas(true, true).await;
}

#[tokio::test]
async fn expunges_moves_and_flag_edits_with_condstore_only() {
    deltas(true, false).await;
}

#[tokio::test]
async fn expunges_moves_and_flag_edits_without_condstore() {
    deltas(false, false).await;
}

// ---------------------------------------------------------------------------------------------
// Content: size, unicode, HTML, dates, broken messages
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn huge_messages_and_attachments_are_capped_without_aborting() {
    let mut st = inbox_and_sent();
    st.add("INBOX", &msg("before", "before"));
    let attachment = "QUJD".repeat(400_000); // 1.6 MB of base64
    let big = format!(
        "From: a@example.com\r\nSubject: big attachment\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
         see attached\r\n--b\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\r\n\
         {attachment}\r\n--b--\r\n"
    );
    st.add("INBOX", big.as_bytes());
    st.add("INBOX", &msg("after", "after"));
    let env = Env::new(st, json!({"max_message_mb": 1})).await;
    let replica = Replica::new();
    let (_, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica, &["INBOX"]);
    let docs = sink.upserts();
    let big_doc = docs.iter().find(|d| d.key.0 == "INBOX/7/2").unwrap();
    assert_eq!(big_doc.metadata["truncated"], true);
    let Content::Bytes(b) = &big_doc.content else {
        panic!("bytes")
    };
    assert!(b.len() < 1024, "headers only: {}", b.len());
    assert!(String::from_utf8_lossy(b).contains("big attachment"));
    assert_eq!(big_doc.metadata["size"], big.len());
}

#[tokio::test]
async fn unicode_subjects_utf7_folders_and_html_only_mail() {
    let folders = ["Entwürfe", "日本語/メモ", "R&D"];
    let wire: Vec<String> = folders.iter().map(|f| conn::encode_mutf7(f)).collect();
    assert_eq!(wire[0], "Entw&APw-rfe");
    assert_eq!(wire[2], "R&-D");
    let mut st = State::new();
    st.mailbox("INBOX", 1);
    for (i, w) in wire.iter().enumerate() {
        st.mailbox(w, 100 + i as u32);
    }
    // RFC 2047 encoded words (B and Q), raw UTF-8 headers (RFC 6532), HTML-only.
    let encoded = b"From: =?UTF-8?B?w4lsb8Ovc2U=?= <e@example.com>\r\n\
        Subject: =?UTF-8?Q?Caf=C3=A9_=F0=9F=8E=89?= =?ISO-8859-1?Q?_na=EFve?=\r\n\r\nbody\r\n"
        .to_vec();
    let raw_utf8 = format!(
        "From: a@example.com\r\nSubject: {}\r\n\r\n{}\r\n",
        UNICODE_SAMPLES[1], UNICODE_SAMPLES[2]
    );
    let html = format!(
        "From: a@example.com\r\nSubject: html only\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n{}\r\n",
        html_mail("Hello <b>there</b>")
    );
    st.add(&wire[0], &encoded);
    st.add(&wire[1], raw_utf8.as_bytes());
    st.add(&wire[2], html.as_bytes());
    let env = Env::new(
        st,
        json!({"folders": ["INBOX", folders[0], folders[1], folders[2]]}),
    )
    .await;
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    assert_eq!(
        replica.keys(),
        vec![
            format!("{}/101/1", wire[1]),
            format!("{}/102/1", wire[2]),
            format!("{}/100/1", wire[0]),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    // Bytes reach extraction untouched (it decodes encoded words and HTML).
    for d in sink.upserts() {
        let Content::Bytes(b) = &d.content else {
            panic!("bytes")
        };
        assert_eq!(d.mime, "message/rfc822");
        let want: &[u8] = if d.key.0.starts_with(&wire[0]) {
            &encoded
        } else if d.key.0.starts_with(&wire[1]) {
            raw_utf8.as_bytes()
        } else {
            html.as_bytes()
        };
        assert_eq!(b.as_slice(), want);
        // The imap:// URL stays ASCII (the wire name is already 7-bit).
        assert!(d.url.as_deref().unwrap().is_ascii());
    }
    assert!(cursor["folders"][&wire[0]].is_object());
    let (_, quiet) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(quiet.sends(), 0);
}

#[tokio::test]
async fn skewed_and_garbage_dates_never_stop_the_sync() {
    let mut st = State::new();
    st.mailbox("INBOX", 1);
    let future = time::OffsetDateTime::now_utc() + time::Duration::days(3 * 365);
    let future_date = format!(
        "{}-{}-{} 10:00:00 -1200",
        future.day(),
        [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
        ][usize::from(u8::from(future.month())) - 1],
        future.year()
    );
    let bodies = [
        // Date header in the future, garbage, and missing; INTERNALDATE likewise odd.
        (
            "From: a@example.com\r\nDate: Mon, 1 Jan 2099 00:00:00 +1400\r\nSubject: future\r\n\r\nx\r\n",
            future_date.clone(),
        ),
        (
            "From: a@example.com\r\nDate: not a date at all\r\nSubject: garbage\r\n\r\nx\r\n",
            "31-Feb-2026 25:61:61 +9999".to_string(),
        ),
        (
            "From: a@example.com\r\nSubject: no date\r\n\r\nx\r\n",
            server_now(),
        ),
    ];
    for (b, d) in &bodies {
        st.add_dated("INBOX", b.as_bytes(), d);
    }
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let (_, sink) = env.pass(&replica, None).await;
    assert_eq!(replica.len(), 3, "{:?}", replica.keys());
    let docs = sink.upserts();
    let by = |uid: u32| {
        docs.iter()
            .find(|d| d.key.0 == format!("INBOX/1/{uid}"))
            .unwrap()
    };
    assert!(
        by(1)
            .modified_at
            .is_some_and(|t| t > time::OffsetDateTime::now_utc())
    );
    assert_eq!(
        by(2).modified_at,
        None,
        "an impossible INTERNALDATE is dropped"
    );
    assert!(by(3).modified_at.is_some());
}

fn server_now() -> String {
    super::server::now_internal_date()
}

#[tokio::test]
async fn regression_one_undeliverable_message_never_stops_the_sync() {
    let mut st = inbox_and_sent();
    for i in 1..=40 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    // The server cannot deliver message 7 (a corrupt file on its side): its FETCH fails NO.
    st.broken.insert(7);
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    assert_eq!(replica.len(), 39);
    assert!(replica.get("INBOX/7/7").is_none());
    assert!(replica.get("INBOX/7/8").is_some() && replica.get("INBOX/7/40").is_some());
    assert_eq!(cursor["folders"]["INBOX"]["retry"], json!([[7, 1]]));
    // The server recovers: the next pass picks up message 7 (and new mail keeps flowing).
    env.server.with(|s| {
        s.broken.clear();
        s.add("INBOX", &msg("later", "later"));
    });
    let (cursor, _) = env.pass(&replica, Some(cursor)).await;
    assert!(
        replica.get("INBOX/7/7").is_some(),
        "refused message retried"
    );
    assert!(replica.get("INBOX/7/41").is_some());
    assert!(
        cursor["folders"]["INBOX"].get("retry").is_none(),
        "{cursor}"
    );
}

#[tokio::test]
async fn regression_a_message_refused_on_every_pass_is_given_up_after_a_cap() {
    let mut st = inbox_and_sent();
    for i in 1..=3 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    st.broken.insert(2);
    // No CONDSTORE: every pass looks at the mailbox.
    st.condstore = false;
    st.qresync = false;
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let mut cursor = None;
    for pass in 1..=MAX_FETCH_ATTEMPTS {
        let (c, _) = env.pass(&replica, cursor).await;
        let retry = &c["folders"]["INBOX"]["retry"];
        if pass < MAX_FETCH_ATTEMPTS {
            assert_eq!(*retry, json!([[2, pass]]), "pass {pass}");
        } else {
            assert!(retry.is_null(), "given up after {pass} passes: {c}");
        }
        cursor = Some(c);
    }
    assert_eq!(replica.len(), 2);
}

#[tokio::test]
async fn regression_a_server_refusing_every_message_fails_the_pass() {
    let mut st = inbox_and_sent();
    for i in 1..=5 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    st.broken.extend(1..=5);
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let conn = env.connector(replica.clone());
    let sink = ReplicaSink::new(replica.clone());
    let err = run_pass(&conn, None, &sink).await.unwrap_err();
    assert!(matches!(err, Error::Provider(_)), "{err:?}");
    assert!(
        sink.committed().is_none(),
        "no progress committed past them"
    );
}

#[tokio::test]
async fn regression_bad_on_a_body_fetch_is_an_error_not_a_skipped_message() {
    let mut st = inbox_and_sent();
    for i in 1..=3 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    st.bad_body_fetch = true;
    let env = Env::new(st, json!({})).await;
    let replica = Replica::new();
    let conn = env.connector(replica.clone());
    let sink = ReplicaSink::new(replica.clone());
    let err = run_pass(&conn, None, &sink).await.unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert!(sink.committed().is_none());
}

// ---------------------------------------------------------------------------------------------
// Crash resumption
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn crash_at_every_send_and_checkpoint_resumes_to_the_same_index() {
    let mut st = inbox_and_sent();
    for i in 1..=8 {
        st.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    st.add("Sent", &msg("s1", "s1"));
    let env = Arc::new(Env::new(st, json!({})).await);
    let build = {
        let env = env.clone();
        move |r: Arc<Replica>| {
            let c: Arc<dyn Connector> = Arc::new(env.connector(r));
            async move { c }
        }
    };
    let report = assert_crash_resumable(&build, None, BTreeMap::new()).await;
    assert_eq!(report.sends, 9);

    // Incremental: expunge, flag edit, move, new mail.
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    env.server.with(|s| {
        s.expunge("INBOX", 2);
        s.set_flag("INBOX", 3, "\\Flagged");
        let moved = s.mailboxes["INBOX"].msgs[&5].body.clone();
        s.expunge("INBOX", 5);
        s.add("Sent", &moved);
        for i in 0..3 {
            s.add("INBOX", &msg(&format!("n{i}"), &format!("n{i}")));
        }
    });
    let report = assert_crash_resumable(&build, Some(cursor), replica.snapshot()).await;
    assert_eq!(report.sends, 7, "2 deletes, 1 flag edit, 1 moved in, 3 new");
}
