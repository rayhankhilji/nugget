//! Against a scripted IMAP server: sync over QRESYNC / CONDSTORE / plain IMAP, UIDVALIDITY,
//! dropped sessions, auth, TLS, hostile responses, size caps, IDLE, archive, crash resumption.

mod server;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_connector::testkit::{
    MemSecrets, Replica, ReplicaSink, VecEgress, assert_crash_resumable, context, html_mail,
    run_pass,
};
use nugget_connector::{AuthKind, KnownItems, SecretStore};
use nugget_core::paths::Paths;

use self::server::{Mode, State, TestServer};
use super::*;

const SOURCE: &str = "imap:me@example.com";

struct Env {
    server: TestServer,
    secrets: Arc<MemSecrets>,
    egress: Arc<VecEgress>,
    settings: Value,
}

impl Env {
    async fn new(state: State, mode: Mode, extra: Value) -> Self {
        let server = TestServer::start(state, mode).await;
        let tls = match mode {
            Mode::Starttls | Mode::StarttlsRefused => "starttls",
            _ => "implicit",
        };
        let mut settings = json!({
            "host": "127.0.0.1",
            "port": server.port(),
            "tls": tls,
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

    async fn implicit(state: State, extra: Value) -> Self {
        Self::new(state, Mode::Implicit, extra).await
    }

    fn connector(&self, replica: Arc<Replica>) -> ImapConnector {
        let ctx = context(
            SOURCE,
            Paths::rooted(std::env::temp_dir().join("nugget-imap-tests")),
            self.egress.clone(),
            self.secrets.clone(),
            replica as Arc<dyn KnownItems>,
        );
        ImapConnector::new(ImapSettings::parse(&self.settings).unwrap(), ctx)
            .with_tls_config(self.server.client_tls.clone())
            .with_timeouts(Duration::from_secs(10), Duration::from_secs(30))
    }

    async fn pass(&self, replica: &Arc<Replica>, cursor: Option<Value>) -> (Value, ReplicaSink) {
        let sink = ReplicaSink::new(replica.clone());
        let c = run_pass(&self.connector(replica.clone()), cursor, &sink)
            .await
            .expect("sync pass");
        (c, sink)
    }

    async fn fail(&self, conn: &ImapConnector) -> Error {
        let sink = ReplicaSink::new(Replica::new());
        let err = run_pass(conn, None, &sink).await.unwrap_err();
        assert!(sink.committed().is_none());
        err
    }

    async fn sync_err(&self) -> Error {
        self.fail(&self.connector(Replica::new())).await
    }

    /// The replica holds exactly the server's messages of the selected mailboxes.
    fn assert_mirrors(&self, replica: &Replica, mailboxes: &[&str]) {
        let mut want: Vec<String> = self.server.with(|s| {
            let keys = |name: &&str| {
                let mb = &s.mailboxes[*name];
                let uids: Vec<_> = mb
                    .msgs
                    .keys()
                    .map(|u| item_key(name, mb.uidvalidity, *u))
                    .collect();
                uids
            };
            mailboxes.iter().flat_map(keys).collect()
        });
        want.sort();
        assert_eq!(replica.keys(), want);
    }

    fn logged(&self, needle: &str) -> bool {
        self.server.log().iter().any(|l| l.contains(needle))
    }
}

fn msg(subject: &str, id: &str) -> Vec<u8> {
    format!(
        "From: a@example.com\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         Message-ID: <{id}@example.com>\r\n\r\nHello {subject}\r\n"
    )
    .into_bytes()
}

/// INBOX (uidvalidity 7) with `n` messages, Sent (\Sent, 11) with one, Trash (\Trash, 13) with
/// one.
fn mailboxes(n: usize) -> State {
    let mut s = State::new();
    s.mailbox("INBOX", 7);
    s.mailbox("Sent", 11).special = Some("\\Sent");
    s.mailbox("Trash", 13).special = Some("\\Trash");
    for i in 1..=n {
        s.add("INBOX", &msg(&format!("m{i}"), &format!("m{i}")));
    }
    s.add("Sent", &msg("s1", "s1"));
    s.add("Trash", &msg("trash", "trash"));
    s
}

#[tokio::test]
async fn first_sync_is_read_only_and_unchanged_mailboxes_are_not_fetched() {
    let env = Env::implicit(mailboxes(3), json!({})).await;
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    assert_eq!(
        replica.keys(),
        ["INBOX/7/1", "INBOX/7/2", "INBOX/7/3", "Sent/11/1"]
    );
    let doc = &sink.upserts()[0];
    assert_eq!(doc.mime, "message/rfc822");
    assert_eq!(doc.content, Content::Bytes(msg("m1", "m1")));
    assert!(doc.version.as_deref().unwrap().starts_with("modseq:"));
    assert_eq!(doc.metadata["folder"], "INBOX");
    assert_eq!(doc.metadata["message_id"], "<m1@example.com>");
    assert_eq!(doc.metadata["flags"], json!([]));
    assert!(doc.modified_at.is_some());
    let url = format!(
        "imap://me%40example.com@127.0.0.1:{}/INBOX;UIDVALIDITY=7/;UID=1",
        env.server.port()
    );
    assert_eq!(doc.url.as_deref(), Some(url.as_str()));
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], 3);
    assert!(cursor["folders"]["INBOX"]["highest_modseq"].is_u64());
    assert!(cursor["folders"].get("Trash").is_none());
    assert!(env.connector(replica.clone()).poll_interval() > Duration::ZERO);
    let host = format!("127.0.0.1:{}", env.server.port());
    let events = env.egress.events();
    assert!(
        events
            .iter()
            .all(|e| e.feature == FEATURE && e.host == host)
    );
    assert!(events.iter().map(|e| e.bytes_received).sum::<u64>() > 500);

    // Nothing changed: no FETCH or SEARCH at all (HIGHESTMODSEQ unchanged).
    let before = env.server.log().len();
    let (cursor, quiet) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(quiet.sends(), 0);
    let log = env.server.log();
    assert!(
        !log[before..]
            .iter()
            .any(|l| l.contains("FETCH") || l.contains("SEARCH"))
    );

    let uid = env.server.with(|s| s.add("INBOX", &msg("m4", "m4")));
    let (cursor, sink) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(sink.summary(), [format!("+INBOX/7/{uid}")]);
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], uid);

    // \Seen is never set; mailboxes are EXAMINEd, bodies PEEKed.
    for uid in 1..=4 {
        assert!(env.server.with(|s| s.flags("INBOX", uid)).is_empty());
    }
    assert!(env.logged("EXAMINE"));
    for l in env.server.log() {
        let u = l.to_ascii_uppercase();
        assert!(!u.contains(" SELECT ") && !u.contains(" STORE "), "{l}");
        assert!(
            !(u.contains("FETCH") && u.contains("BODY")) || u.contains("BODY.PEEK["),
            "{l}"
        );
    }

    // A lost cursor relists without downloading what is held.
    env.server.with(|s| s.expunge("INBOX", 3));
    let (_, sink) = env.pass(&replica, None).await;
    assert_eq!(sink.summary(), ["-INBOX/7/3"]);
}

#[tokio::test]
async fn many_messages_across_uid_ranges_and_batches() {
    let mut st = mailboxes(640);
    // Sparse UIDs: gaps make the UID sets non-contiguous.
    for uid in (1..=640).filter(|u| u % 7 == 0 || (300..340).contains(u)) {
        st.expunge("INBOX", uid);
    }
    let env = Env::implicit(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
    // Checkpoints per metadata batch while fetching new mail, plus the final ones.
    assert!(sink.checkpoint_calls() >= 4, "{}", sink.checkpoint_calls());
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], 640);

    env.server.with(|s| {
        s.body_fetches = 0;
        for i in 0..30 {
            s.add("INBOX", &msg(&format!("n{i}"), &format!("n{i}")));
        }
    });
    let (_, sink) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(sink.upserts().len(), 30);
    assert_eq!(env.server.with(|s| s.body_fetches), 2, "25 per FETCH");
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
}

/// Expunges, a move and flag edits, under each server capability level.
#[tokio::test]
async fn expunges_moves_and_flag_edits_under_qresync_condstore_and_plain_imap() {
    for (condstore, qresync) in [(true, true), (true, false), (false, false)] {
        let mut st = mailboxes(6);
        st.condstore = condstore;
        st.qresync = qresync;
        let env = Env::implicit(st, json!({})).await;
        let replica = Replica::new();
        let (cursor, _) = env.pass(&replica, None).await;
        let before = replica.get("INBOX/7/2").unwrap().version.unwrap();
        let prefix = if condstore { "modseq:" } else { "flags:" };
        assert!(before.starts_with(prefix), "{before}");
        assert_eq!(
            cursor["folders"]["INBOX"].get("highest_modseq").is_some(),
            condstore
        );
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
        let want = [
            "+INBOX/7/2",
            "+INBOX/7/3",
            "+Sent/11/2",
            "-INBOX/7/1",
            "-INBOX/7/4",
        ];
        assert_eq!(s, want, "condstore {condstore} qresync {qresync}");
        let flagged = sink
            .upserts()
            .into_iter()
            .find(|d| d.key.0 == "INBOX/7/2")
            .unwrap();
        assert_eq!(flagged.metadata["flags"], json!(["\\Flagged"]));
        assert_ne!(flagged.version.as_deref(), Some(before.as_str()));
        assert_eq!(
            replica.get("Sent/11/2").unwrap().content.as_bytes(),
            msg("m4", "m4")
        );
        assert_eq!(env.logged("VANISHED"), qresync);
        assert_eq!(env.logged("UID SEARCH ALL"), !qresync);
        assert_eq!(env.logged("CHANGEDSINCE"), condstore);
        let (_, quiet) = env.pass(&replica, Some(cursor)).await;
        assert_eq!(quiet.sends(), 0, "{:?}", quiet.summary());
    }
}

#[tokio::test]
async fn uidvalidity_change_triggers_a_reconciling_resync() {
    let env = Env::implicit(mailboxes(12), json!({})).await;
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
    let (_, quiet) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(quiet.sends(), 0, "{:?}", quiet.summary());
}

#[tokio::test]
async fn folder_selection_window_and_dropped_folders() {
    let mut st = mailboxes(3);
    st.add_dated("INBOX", &msg("old", "old"), "1-Jan-2015 10:00:00 +0000");
    st.mailbox("Spam", 17);
    st.add("Spam", &msg("spam", "spam"));
    st.mailbox("Archive", 19);
    st.add("Archive", &msg("arch", "arch"));
    for (name, uv, special) in [
        ("AllMail", 21, "\\All"),
        ("Starred", 23, "\\Flagged"),
        ("Important", 25, "\\Important"),
    ] {
        st.mailbox(name, uv).special = Some(special);
        st.add(name, &msg(name, name));
    }
    // `*`: everything but trash, junk and virtual views; the old message is outside the window.
    let mut env = Env::implicit(st, json!({"folders": ["*"], "window_days": 30})).await;
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    let want = [
        "Archive/19/1",
        "INBOX/7/1",
        "INBOX/7/2",
        "INBOX/7/3",
        "Sent/11/1",
    ];
    assert_eq!(replica.keys(), want);
    assert_eq!(cursor["folders"]["INBOX"]["last_uid"], 4);
    assert!(env.logged("UID SEARCH SINCE"));

    // Narrowed: Archive and Sent items go away.
    env.settings["folders"] = json!(["INBOX"]);
    let (cursor, sink) = env.pass(&replica, Some(cursor)).await;
    let mut d = sink.summary();
    d.sort();
    assert_eq!(d, ["-Archive/19/1", "-Sent/11/1"]);
    assert_eq!(cursor["folders"].as_object().unwrap().len(), 1);
    // Named explicitly, a virtual view is honoured.
    env.settings["folders"] = json!(["AllMail"]);
    env.pass(&replica, None).await;
    assert!(replica.get("AllMail/21/1").is_some());
}

#[tokio::test]
async fn dropped_sessions_reconnect_a_few_times_then_fail_without_deleting() {
    let mut st = mailboxes(120);
    // The third body FETCH gets `* BYE` and a closed connection.
    st.bye_after_body_fetches = Some(2);
    let env = Env::implicit(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
    assert_eq!(env.server.with(|s| s.logins), 2, "one reconnect");
    // Nothing applied before the drop was fetched again.
    assert_eq!(sink.upserts().len(), 121);

    env.server.with(|s| {
        for i in 0..200 {
            s.add("INBOX", &msg(&format!("x{i}"), &format!("x{i}")));
        }
        s.logins = 0;
        s.bye_every_body_fetch = true;
    });
    let conn = env.connector(replica.clone());
    let err = run_pass(&conn, Some(cursor), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert_eq!(env.server.with(|s| s.logins), 1 + MAX_RECONNECTS);
    assert_eq!(replica.len(), 121);
}

#[tokio::test]
async fn missing_or_rejected_password_is_unauthorized() {
    let env = Env::implicit(mailboxes(1), json!({})).await;
    env.secrets.delete(&password_account(SOURCE)).unwrap();
    let err = env.sync_err().await;
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert!(env.server.log().is_empty(), "nothing touched the network");
    env.secrets
        .set(&password_account(SOURCE), "wrong-password")
        .unwrap();
    let err = env.sync_err().await;
    assert!(
        matches!(&err, Error::Unauthorized(m) if m.contains("AUTHENTICATIONFAILED") || m.contains("rejected")),
        "{err:?}"
    );
    assert!(!err.to_string().contains("wrong-password"));
}

#[tokio::test]
async fn starttls_upgrades_before_login_and_downgrades_fail_closed() {
    let env = Env::new(mailboxes(3), Mode::Starttls, json!({})).await;
    let replica = Replica::new();
    env.pass(&replica, None).await;
    assert_eq!(replica.len(), 4);
    assert!(env.server.log()[0].ends_with("STARTTLS"));

    let env = Env::new(mailboxes(3), Mode::StarttlsRefused, json!({})).await;
    let err = env.sync_err().await;
    assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
    assert!(!env.logged("LOGIN"));

    let env = Env::new(mailboxes(3), Mode::Plaintext, json!({})).await;
    let err = env.sync_err().await;
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert_eq!(env.server.with(|s| s.logins), 0);
}

/// A server that accepts the TCP connection and never greets still gets its egress row (GG8-11).
#[tokio::test]
async fn a_connection_that_exchanged_no_bytes_is_still_in_the_egress_ledger() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        drop(sock);
    });
    let mut env = Env::new(mailboxes(1), Mode::Starttls, json!({})).await;
    env.settings["port"] = json!(port);
    let conn = env
        .connector(Replica::new())
        .with_timeouts(Duration::from_millis(200), Duration::from_secs(30));
    let err = env.fail(&conn).await;
    assert!(matches!(&err, Error::Network(_)), "{err:?}");
    let events = env.egress.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].host, format!("127.0.0.1:{port}"));
    assert_eq!((events[0].bytes_sent, events[0].bytes_received), (0, 0));
    held.abort();
}

#[test]
fn egress_host_carries_only_a_non_default_port() {
    let ctx = context(
        SOURCE,
        Paths::rooted(std::env::temp_dir().join("nugget-imap-tests")),
        Arc::new(VecEgress::default()),
        Arc::new(MemSecrets::default()),
        Replica::new(),
    );
    for (mut settings, host) in [
        (json!({"host": " imap.example "}), "imap.example"),
        (
            json!({"host": "imap.example", "tls": "starttls"}),
            "imap.example",
        ),
        (
            json!({"host": "imap.example", "tls": "starttls", "port": 993}),
            "imap.example:993",
        ),
        (
            json!({"host": "imap.example", "port": 143}),
            "imap.example:143",
        ),
        (json!({"host": "::1", "port": 1993}), "[::1]:1993"),
    ] {
        settings["username"] = json!("me@example.com");
        let c = ImapConnector::new(ImapSettings::parse(&settings).unwrap(), ctx.clone());
        assert_eq!(c.egress_host(), host, "{settings}");
    }
}

#[tokio::test]
async fn hostile_or_broken_servers_error_without_panicking() {
    let mut st = mailboxes(3);
    st.garbage = true;
    let err = Env::implicit(st, json!({})).await.sync_err().await;
    assert!(matches!(err, Error::Network(_)), "{err:?}");

    // A huge announced literal is refused from its header, before any buffer is sized from it:
    // in a body fetch (then a stall), in metadata, and with leading zeros.
    let mut st = mailboxes(3);
    st.lie_literal = Some(400_000_000);
    let env = Env::implicit(st, json!({"max_message_mb": 1})).await;
    let started = Instant::now();
    let conn = env
        .connector(Replica::new())
        .with_timeouts(Duration::from_secs(2), Duration::from_secs(30));
    let err = env.fail(&conn).await;
    assert!(
        matches!(&err, Error::Network(m) if m.contains("400000000-byte literal")),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "no timeout wait"
    );
    for raw in [
        &b"* 1 FETCH (UID 1 BODY[] {300000000}\r\nabc"[..],
        b"* 1 FETCH (UID 1 BODY[] {00000000000000000000000300000000}\r\nabc",
    ] {
        let mut st = mailboxes(3);
        st.raw_examine = Some(raw.to_vec());
        let err = Env::implicit(st, json!({})).await.sync_err().await;
        assert!(
            matches!(&err, Error::Network(m) if m.contains("300000000-byte literal")),
            "{err:?}"
        );
    }

    // A parse error keeps only a short, escaped excerpt of the receive buffer.
    let mut raw = b"A9 FOO \x00\x01\x1b[31m ".to_vec();
    raw.extend(std::iter::repeat_n(b'S', 50_000));
    raw.extend_from_slice(b" secret-tail\r\n");
    let mut st = mailboxes(3);
    st.raw_examine = Some(raw);
    let err = Env::implicit(st, json!({})).await.sync_err().await;
    let Error::Network(m) = &err else {
        panic!("{err:?}")
    };
    assert!(m.contains("malformed response") && m.len() < 400, "{m}");
    assert!(
        !m.chars().any(char::is_control) && !m.contains("secret-tail"),
        "{m:?}"
    );

    // A silent server times out; a cancelled pass stops.
    let env = Env::new(mailboxes(3), Mode::Silent, json!({})).await;
    let conn = env
        .connector(Replica::new())
        .with_timeouts(Duration::from_millis(300), Duration::from_secs(30));
    let err = env.fail(&conn).await;
    assert!(
        matches!(&err, Error::Network(m) if m.contains("timed out")),
        "{err:?}"
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let sink = ReplicaSink::new(Replica::new());
    let err = conn.sync(None, &sink, &cancel).await.unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

#[tokio::test]
async fn huge_messages_are_indexed_by_headers_only() {
    let mut st = mailboxes(1);
    let attachment = "QUJD".repeat(400_000); // 1.6 MB of base64
    let big = format!(
        "From: a@example.com\r\nSubject: big attachment\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
         see attached\r\n--b\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\r\n\
         {attachment}\r\n--b--\r\n"
    );
    st.add("INBOX", big.as_bytes());
    st.add("INBOX", &msg("after", "after"));
    let env = Env::implicit(st, json!({"max_message_mb": 1})).await;
    let replica = Replica::new();
    let (_, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica, &["INBOX", "Sent"]);
    let docs = sink.upserts();
    let big_doc = docs.iter().find(|d| d.key.0 == "INBOX/7/2").unwrap();
    assert_eq!(big_doc.metadata["truncated"], true);
    assert_eq!(big_doc.metadata["size"], big.len());
    let Content::Bytes(b) = &big_doc.content else {
        panic!("bytes")
    };
    assert!(
        b.len() < 1024 && b.ends_with(b"\r\n\r\n"),
        "headers only: {}",
        b.len()
    );
    assert!(String::from_utf8_lossy(b).contains("big attachment"));
    assert!(env.logged("BODY.PEEK[HEADER]"));
}

#[tokio::test]
async fn utf7_folders_raw_bytes_and_odd_dates_pass_through() {
    let folders = ["Entwürfe", "日本語/メモ"];
    let wire: Vec<String> = folders.iter().map(|f| conn::encode_mutf7(f)).collect();
    let mut st = State::new();
    st.mailbox("INBOX", 1);
    st.mailbox(&wire[0], 100);
    st.mailbox(&wire[1], 101);
    // RFC 2047 encoded words, HTML-only mail.
    let encoded = b"From: =?UTF-8?B?w4lsb8Ovc2U=?= <e@example.com>\r\n\
        Subject: =?UTF-8?Q?Caf=C3=A9_=F0=9F=8E=89?=\r\n\r\nbody\r\n"
        .to_vec();
    let html = format!(
        "From: a@example.com\r\nSubject: html only\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{}\r\n",
        html_mail("Hello <b>there</b>")
    );
    st.add(&wire[0], &encoded);
    st.add(&wire[1], html.as_bytes());
    // A future Date and INTERNALDATE, a garbage one, and none.
    let future = time::OffsetDateTime::now_utc() + time::Duration::days(3 * 365);
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let future_date = format!(
        "{}-{}-{} 10:00:00 -1200",
        future.day(),
        month[usize::from(u8::from(future.month())) - 1],
        future.year()
    );
    for (body, date) in [
        (
            "Date: Mon, 1 Jan 2099 00:00:00 +1400\r\nSubject: future",
            future_date,
        ),
        (
            "Date: not a date\r\nSubject: garbage",
            "31-Feb-2026 25:61:61 +9999".to_string(),
        ),
        ("Subject: no date", server::now_internal_date()),
    ] {
        st.add_dated(
            "INBOX",
            format!("From: a@example.com\r\n{body}\r\n\r\nx\r\n").as_bytes(),
            &date,
        );
    }
    let env = Env::implicit(st, json!({"folders": ["INBOX", folders[0], folders[1]]})).await;
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    assert_eq!(replica.len(), 5, "{:?}", replica.keys());
    let docs = sink.upserts();
    let by = |key: &str| docs.iter().find(|d| d.key.0 == key).unwrap();
    // Bytes reach extraction untouched; the imap:// URL stays ASCII.
    assert_eq!(
        by(&format!("{}/100/1", wire[0])).content,
        Content::Bytes(encoded)
    );
    assert_eq!(
        by(&format!("{}/101/1", wire[1])).content,
        Content::Bytes(html.into_bytes())
    );
    assert!(docs.iter().all(|d| d.url.as_deref().unwrap().is_ascii()));
    assert!(
        by("INBOX/1/1")
            .modified_at
            .is_some_and(|t| t > time::OffsetDateTime::now_utc())
    );
    assert_eq!(
        by("INBOX/1/2").modified_at,
        None,
        "an impossible INTERNALDATE is dropped"
    );
    assert!(by("INBOX/1/3").modified_at.is_some());
    let (_, quiet) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(quiet.sends(), 0);
}

#[tokio::test]
async fn regression_one_undeliverable_message_never_stops_the_sync() {
    let mut st = mailboxes(40);
    // The server cannot deliver message 7 (a corrupt file on its side): its FETCH fails NO.
    st.broken.insert(7);
    let env = Env::implicit(st, json!({})).await;
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    assert_eq!(replica.len(), 40);
    assert!(replica.get("INBOX/7/7").is_none());
    assert_eq!(cursor["folders"]["INBOX"]["retry"], json!([[7, 1]]));
    // The server recovers: the next pass picks up message 7, and new mail keeps flowing.
    env.server.with(|s| {
        s.broken.clear();
        s.add("INBOX", &msg("later", "later"));
    });
    let (cursor, _) = env.pass(&replica, Some(cursor)).await;
    assert!(replica.get("INBOX/7/7").is_some() && replica.get("INBOX/7/41").is_some());
    assert!(
        cursor["folders"]["INBOX"].get("retry").is_none(),
        "{cursor}"
    );
}

#[tokio::test]
async fn regression_a_message_refused_on_every_pass_is_given_up_after_a_cap() {
    let mut st = mailboxes(3);
    st.broken.insert(2);
    // No CONDSTORE: every pass looks at the mailbox.
    st.condstore = false;
    st.qresync = false;
    let env = Env::implicit(st, json!({})).await;
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
    assert_eq!(replica.len(), 3);
}

#[tokio::test]
async fn regression_refusing_every_message_or_bad_on_a_body_fetch_fails_the_pass() {
    let mut st = mailboxes(5);
    st.broken.extend(1..=5);
    let err = Env::implicit(st, json!({})).await.sync_err().await;
    assert!(matches!(err, Error::Provider(_)), "{err:?}");
    let mut st = mailboxes(3);
    st.bad_body_fetch = true;
    let err = Env::implicit(st, json!({})).await.sync_err().await;
    assert!(matches!(err, Error::Network(_)), "{err:?}");
}

#[tokio::test]
async fn crash_at_every_send_and_checkpoint_resumes_to_the_same_index() {
    let env = Arc::new(Env::implicit(mailboxes(8), json!({})).await);
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

#[tokio::test]
async fn idle_watches_inbox_or_the_first_selected_mailbox() {
    for (folder, uv) in [("INBOX", 7), ("Archive", 19)] {
        let mut st = mailboxes(3);
        st.mailbox("Archive", 19);
        st.add("Archive", &msg("arch", "arch"));
        let env = Env::implicit(st, json!({"folders": [folder, "Sent"]})).await;
        let replica = Replica::new();
        let c = Arc::new(env.connector(replica.clone()));
        assert!(!c.supports_watch());
        run_pass(&*c, None, &ReplicaSink::new(replica.clone()))
            .await
            .unwrap();
        assert!(c.supports_watch());
        let start = env.server.log().len();
        let sink = Arc::new(ReplicaSink::new(replica.clone()));
        let cancel = CancellationToken::new();
        let task = {
            let (c, sink, cancel) = (
                c.clone(),
                sink.clone() as Arc<dyn ChangeSink>,
                cancel.clone(),
            );
            tokio::spawn(async move { c.watch(sink, cancel).await })
        };
        for _ in 0..200 {
            if env.server.log()[start..]
                .iter()
                .any(|l| l.ends_with("IDLE"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let log = env.server.log();
        let examined: Vec<_> = log[start..]
            .iter()
            .filter(|l| l.contains("EXAMINE"))
            .collect();
        assert_eq!(examined.len(), 1, "{examined:?}");
        assert!(
            examined[0].contains(&format!("\"{folder}\"")),
            "{examined:?}"
        );
        let uid = env.server.with(|s| s.add(folder, &msg("live", "live")));
        env.server.push(&format!("* {uid} EXISTS"));
        for _ in 0..300 {
            if sink.sends() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(sink.summary(), [format!("+{folder}/{uv}/{uid}")]);
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(env.server.log().iter().any(|l| l == "DONE"));
        assert!(env.server.with(|s| s.flags(folder, uid)).is_empty());
    }

    // Without IDLE, watching is unsupported.
    let mut st = mailboxes(1);
    st.idle = false;
    let env = Env::implicit(st, json!({})).await;
    let replica = Replica::new();
    let c = env.connector(replica.clone());
    let sink = Arc::new(ReplicaSink::new(replica));
    run_pass(&c, None, &sink).await.unwrap();
    assert!(!c.supports_watch());
    let err = c.watch(sink, CancellationToken::new()).await.unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}

fn key(folder: &str, uv: u32, uid: u32) -> ItemKey {
    ItemKey::new(item_key(folder, uv, uid))
}

#[tokio::test]
async fn archive_moves_messages_to_the_archive_or_all_mail_mailbox() {
    let mut st = mailboxes(3);
    st.move_cap = true;
    st.mailbox("Archive", 21).special = Some("\\Archive");
    let env = Env::implicit(st, json!({})).await;
    let cancel = CancellationToken::new();
    let keys = [key("INBOX", 7, 1), key("INBOX", 7, 3), key("INBOX", 99, 2)];
    let moved = env
        .connector(Replica::new())
        .archive(&keys, &cancel)
        .await
        .unwrap();
    assert_eq!(moved, 2);
    env.server.with(|s| {
        let inbox: Vec<u32> = s.mailboxes["INBOX"].msgs.keys().copied().collect();
        assert_eq!(inbox, vec![2], "the stale-uidvalidity key is left alone");
        assert_eq!(s.mailboxes["Archive"].msgs.len(), 2);
    });
    assert!(env.logged("SELECT \"INBOX\"") && env.logged("UID MOVE 1,3 \"Archive\""));
    assert!(!env.logged("EXPUNGE") && !env.logged("STORE"));
    assert!(
        env.egress
            .events()
            .iter()
            .any(|e| e.feature == ACTION_FEATURE)
    );

    let mut st = mailboxes(3);
    st.move_cap = true;
    st.mailbox("[Gmail]/All Mail", 31).special = Some("\\All");
    let env = Env::implicit(st, json!({})).await;
    let moved = env
        .connector(Replica::new())
        .archive(&[key("INBOX", 7, 2)], &cancel)
        .await
        .unwrap();
    assert_eq!(moved, 1);
    env.server
        .with(|s| assert_eq!(s.mailboxes["[Gmail]/All Mail"].msgs.len(), 1));
}

#[tokio::test]
async fn archive_without_move_or_archive_mailbox_is_unsupported() {
    let cancel = CancellationToken::new();
    // No MOVE capability: nothing changes.
    let mut st = mailboxes(3);
    st.mailbox("Archive", 21).special = Some("\\Archive");
    let env = Env::implicit(st, json!({})).await;
    let err = env
        .connector(Replica::new())
        .archive(&[key("INBOX", 7, 1)], &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");
    env.server
        .with(|s| assert_eq!(s.mailboxes["INBOX"].msgs.len(), 3));
    // MOVE but nowhere to archive to.
    let mut st = mailboxes(3);
    st.move_cap = true;
    let env = Env::implicit(st, json!({})).await;
    let conn = env.connector(Replica::new());
    let err = conn
        .archive(&[key("INBOX", 7, 1)], &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");
    // Malformed keys are refused before connecting.
    let err = conn
        .archive(&[ItemKey::new("nope")], &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
}

#[test]
fn factory_validates_settings() {
    let f = ImapFactory::new();
    assert_eq!(f.info().auth, AuthKind::Password);
    let s = ImapSettings::parse(&json!({"host": "imap.fastmail.com", "username": "me"})).unwrap();
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
    // Builds without a test root; the connector never falls back to anything weaker.
    platform_tls_config().unwrap();
}

#[test]
fn keys_uid_sets_and_mailbox_names_are_safe_on_the_wire() {
    assert_eq!(item_key("Archive/2024", 9, 42), "Archive/2024/9/42");
    assert_eq!(
        parse_key("Archive/2024/9/42"),
        Some(("Archive/2024", 9, 42))
    );
    assert_eq!(parse_key("INBOX/1/x"), None);
    assert_eq!(conn::uid_sets(&[1, 2, 3, 5, 7, 8], 100), vec!["1:3,5,7:8"]);
    let many: Vec<u32> = (0..1000).map(|i| i * 2).collect();
    let sets = conn::uid_sets(&many, 50);
    assert!(sets.len() > 1 && sets.iter().all(|s| s.len() <= 50));
    let rejoined: Vec<u32> = sets
        .iter()
        .flat_map(|s| s.split(','))
        .map(|x| x.parse().unwrap())
        .collect();
    assert_eq!(rejoined, many);
    assert_eq!(conn::encode_mutf7("Entwürfe"), "Entw&APw-rfe");
    assert_eq!(conn::encode_mutf7("A&B"), "A&-B");
    assert_eq!(conn::encode_mutf7("日本語"), "&ZeVnLIqe-");
    assert_eq!(conn::quote("a\"b\\c").unwrap(), r#""a\"b\\c""#);
    assert!(conn::quote("a\r\nb").is_err());
    assert!(conn::quote("é").is_err());
}

#[test]
fn dates_headers_versions_and_cursors_parse_defensively() {
    let d = conn::parse_internal_date("17-Jul-1996 02:44:25 -0700").unwrap();
    assert_eq!(d.unix_timestamp(), 837_596_665);
    assert!(conn::parse_internal_date(" 7-Jul-1996 02:44:25 +0000").is_some());
    for bad in [
        "garbage",
        "32-Jul-1996 02:44:25 +0000",
        "1-Jan-1996 00:00:00 +ab00",
    ] {
        assert!(conn::parse_internal_date(bad).is_none(), "{bad}");
    }
    let m = b"Subject: x\r\nMessage-Id:\r\n <abc@x>\r\n\r\nMessage-ID: <body@x>\r\n";
    assert_eq!(header_value(m, "message-id").as_deref(), Some("<abc@x>"));
    assert_eq!(header_value(b"no headers here", "message-id"), None);
    assert_eq!(header_value(&[0xff, 0xfe, b':', b'\n'], "message-id"), None);
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
    assert_eq!(Cursor::load(Some(json!({"v": 2}))), Cursor::fresh());
    assert_eq!(Cursor::load(Some(json!("junk"))), Cursor::fresh());
}
