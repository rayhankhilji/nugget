//! iMessage: a synthetic chat.db (and address book) built with rusqlite at runtime.

use std::path::{Path, PathBuf};

use nugget_connector::ConnectorFactory;
use nugget_connector::testkit::Replica;
use nugget_connector_messaging::IMessageFactory;
use nugget_connector_messaging::typedstream::encode_for_tests;
use nugget_core::Error;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::common::*;

const APPLE_EPOCH: i64 = 978_307_200;

/// Apple nanoseconds for a Unix time.
fn ns(unix: i64) -> i64 {
    (unix - APPLE_EPOCH) * 1_000_000_000
}

/// 2024-05-01T09:00:00Z
const MAY1: i64 = 1_714_554_000;

const SCHEMA: &str = "
CREATE TABLE handle (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL, service TEXT);
CREATE TABLE chat (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL, style INTEGER,
  chat_identifier TEXT, service_name TEXT, display_name TEXT);
CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
CREATE TABLE message (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT, text TEXT, attributedBody BLOB,
  handle_id INTEGER DEFAULT 0, date INTEGER, is_from_me INTEGER DEFAULT 0,
  associated_message_type INTEGER DEFAULT 0, item_type INTEGER DEFAULT 0, group_title TEXT,
  other_handle INTEGER DEFAULT 0, group_action_type INTEGER DEFAULT 0);
CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER, message_date INTEGER DEFAULT 0);
CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, filename TEXT, mime_type TEXT,
  transfer_name TEXT, total_bytes INTEGER);
CREATE TABLE message_attachment_join (message_id INTEGER, attachment_id INTEGER);
";

pub struct Db {
    pub conn: Connection,
    pub path: PathBuf,
}

impl Db {
    pub fn create(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("chat.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Self { conn, path }
    }

    pub fn handle(&self, id: &str) -> i64 {
        self.conn
            .execute(
                "INSERT INTO handle (id, service) VALUES (?1, 'iMessage')",
                [id],
            )
            .unwrap();
        self.conn.last_insert_rowid()
    }

    pub fn chat(
        &self,
        guid: &str,
        ident: &str,
        name: Option<&str>,
        style: i64,
        handles: &[i64],
    ) -> i64 {
        self.conn
            .execute(
                "INSERT INTO chat (guid, chat_identifier, display_name, style) VALUES (?1, ?2, ?3, ?4)",
                params![guid, ident, name, style],
            )
            .unwrap();
        let id = self.conn.last_insert_rowid();
        for h in handles {
            self.conn
                .execute("INSERT INTO chat_handle_join VALUES (?1, ?2)", [id, *h])
                .unwrap();
        }
        id
    }

    /// Insert a message; returns its ROWID.
    #[allow(clippy::too_many_arguments)]
    pub fn message(
        &self,
        chat: i64,
        unix: i64,
        handle: i64,
        from_me: bool,
        text: Option<&str>,
        body: Option<Vec<u8>>,
        assoc: i64,
        item_type: i64,
    ) -> i64 {
        self.conn
            .execute(
                "INSERT INTO message (text, attributedBody, handle_id, date, is_from_me, associated_message_type, item_type)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![text, body, handle, ns(unix), from_me as i64, assoc, item_type],
            )
            .unwrap();
        let id = self.conn.last_insert_rowid();
        self.conn
            .execute(
                "INSERT INTO chat_message_join (chat_id, message_id) VALUES (?1, ?2)",
                [chat, id],
            )
            .unwrap();
        id
    }

    pub fn text(&self, chat: i64, unix: i64, handle: i64, from_me: bool, text: &str) -> i64 {
        self.message(chat, unix, handle, from_me, Some(text), None, 0, 0)
    }

    pub fn attach(&self, message: i64, transfer: &str, mime: &str, size: i64) {
        self.conn
            .execute(
                "INSERT INTO attachment (filename, mime_type, transfer_name, total_bytes) VALUES (?1, ?2, ?3, ?4)",
                params![format!("~/Library/Messages/Attachments/ab/{transfer}"), mime, transfer, size],
            )
            .unwrap();
        let a = self.conn.last_insert_rowid();
        self.conn
            .execute(
                "INSERT INTO message_attachment_join VALUES (?1, ?2)",
                [message, a],
            )
            .unwrap();
    }
}

/// The standard scenario: a one-to-one chat and a named group over two months.
pub fn scenario(dir: &Path) -> Db {
    let db = Db::create(dir);
    let alice = db.handle("+15551234567");
    let bob = db.handle("bob@example.com");
    let carol = db.handle("+447700900123");
    let one = db.chat(
        "iMessage;-;+15551234567",
        "+15551234567",
        None,
        45,
        &[alice],
    );
    let group = db.chat(
        "iMessage;+;chat123",
        "chat123",
        Some("Weekend 🏕️"),
        43,
        &[alice, bob, carol],
    );

    db.text(one, MAY1, alice, false, "Hi from Alice\nline two");
    let m = db.message(
        one,
        MAY1 + 60,
        0,
        true,
        None,
        Some(encode_for_tests("typedstream reply ✓")),
        0,
        0,
    );
    let _ = m;
    // A tapback on the first message: skipped.
    db.message(
        one,
        MAY1 + 70,
        alice,
        false,
        Some("Loved “Hi from Alice”"),
        None,
        2000,
        0,
    );
    let pic = db.message(one, MAY1 + 120, alice, false, Some("\u{fffc}"), None, 0, 0);
    db.attach(pic, "IMG_0001.HEIC", "image/heic", 2_000_000);
    db.text(one, MAY1 + 31 * 86_400, alice, false, "June already");

    db.text(group, MAY1 + 300, bob, false, "Camping?");
    db.text(group, MAY1 + 360, carol, false, "Yes");
    db.text(group, MAY1 + 400, 0, true, "I'm in");
    // Group rename (item_type 2).
    db.conn
        .execute(
            "INSERT INTO message (handle_id, date, is_from_me, item_type, group_title) VALUES (?1, ?2, 0, 2, 'Weekend 🏕️')",
            params![bob, ns(MAY1 + 200)],
        )
        .unwrap();
    let id = db.conn.last_insert_rowid();
    db.conn
        .execute(
            "INSERT INTO chat_message_join (chat_id, message_id) VALUES (?1, ?2)",
            [group, id],
        )
        .unwrap();
    db
}

/// An address book naming Alice (by a differently formatted phone) and Bob (by e-mail).
pub fn address_book(dir: &Path) {
    let src = dir.join("Sources").join("ABC-123");
    std::fs::create_dir_all(&src).unwrap();
    let conn = Connection::open(src.join("AddressBook-v22.abcddb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE ZABCDRECORD (Z_PK INTEGER PRIMARY KEY, ZFIRSTNAME TEXT, ZLASTNAME TEXT, ZORGANIZATION TEXT);
         CREATE TABLE ZABCDPHONENUMBER (Z_PK INTEGER PRIMARY KEY, ZOWNER INTEGER, ZFULLNUMBER TEXT);
         CREATE TABLE ZABCDEMAILADDRESS (Z_PK INTEGER PRIMARY KEY, ZOWNER INTEGER, ZADDRESSNORMALIZED TEXT);
         INSERT INTO ZABCDRECORD VALUES (1, 'Alice', 'Smith', NULL), (2, NULL, NULL, 'Bob''s Bikes');
         INSERT INTO ZABCDPHONENUMBER VALUES (1, 1, '(555) 123-4567');
         INSERT INTO ZABCDEMAILADDRESS VALUES (1, 2, 'BOB@example.com');",
    )
    .unwrap();
}

fn settings(db: &Path, contacts: Option<&Path>) -> Value {
    match contacts {
        Some(c) => json!({ "db_path": db, "contacts_dir": c }),
        None => json!({ "db_path": db, "contacts": false }),
    }
}

#[tokio::test]
async fn reads_chats_messages_and_contacts() {
    let tmp = tempfile::tempdir().unwrap();
    let db = scenario(&tmp.path().join("Messages"));
    let ab = tmp.path().join("AddressBook");
    address_book(&ab);
    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(Kind::IMessage, &settings(&db.path, Some(&ab)), &replica)
        .await;
    assert_eq!(
        keys(&replica),
        set(&[
            "im:iMessage;+;chat123/2024-05",
            "im:iMessage;-;+15551234567/2024-05",
            "im:iMessage;-;+15551234567/2024-06",
        ])
    );
    for d in upserts(&sink) {
        assert_well_formed(&d);
        assert_eq!(d.url, None);
        assert_eq!(d.metadata["service"], "imessage");
    }
    let one = doc(&sink, "im:iMessage;-;+15551234567/2024-05");
    assert_eq!(one.title.as_deref(), Some("Alice Smith — May 2024"));
    let t = text(&one);
    assert!(
        t.contains("**Alice Smith** · 2024-05-01 09:00\n> Hi from Alice\n> line two\n"),
        "{t}"
    );
    assert!(
        t.contains("**Me** · 2024-05-01 09:01\n> typedstream reply ✓\n"),
        "{t}"
    );
    assert!(!t.contains("Loved"), "tapbacks are skipped: {t}");
    assert!(
        t.contains("**Alice Smith** · 2024-05-01 09:02\n> [photo: IMG_0001.HEIC]\n"),
        "{t}"
    );
    assert!(!t.contains('\u{fffc}'));
    assert_eq!(one.metadata["attachments"][0]["size"], 2_000_000);
    assert_eq!(one.metadata["contacts"][0]["name"], "Alice Smith");
    assert_eq!(one.metadata["contacts"][0]["phones"][0], "+15551234567");
    assert_eq!(one.metadata["is_group"], false);

    let g = doc(&sink, "im:iMessage;+;chat123/2024-05");
    assert_eq!(g.metadata["is_group"], true);
    assert_eq!(g.title.as_deref(), Some("Weekend 🏕️ — May 2024"));
    let t = text(&g);
    assert!(
        t.contains("Bob's Bikes named the conversation “Weekend 🏕️”"),
        "{t}"
    );
    assert!(
        t.contains("**Bob's Bikes** · 2024-05-01 09:05\n> Camping?"),
        "{t}"
    );
    // Carol has no contact card: her handle is shown.
    assert!(
        t.contains("**+447700900123** · 2024-05-01 09:06\n> Yes"),
        "{t}"
    );
    let contacts = g.metadata["contacts"].as_array().unwrap();
    assert!(contacts.iter().any(|c| c["emails"][0] == "bob@example.com"));

    // The copies are removed after the pass.
    let left: Vec<_> = std::fs::read_dir(env.scratch()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn without_contacts_names_fall_back_to_handles() {
    let tmp = tempfile::tempdir().unwrap();
    let db = scenario(&tmp.path().join("Messages"));
    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(Kind::IMessage, &settings(&db.path, None), &replica)
        .await;
    let one = doc(&sink, "im:iMessage;-;+15551234567/2024-05");
    assert_eq!(one.title.as_deref(), Some("+15551234567 — May 2024"));
    // A broken address book is not fatal either.
    let ab = tmp.path().join("AB");
    std::fs::create_dir_all(&ab).unwrap();
    std::fs::write(ab.join("AddressBook-v22.abcddb"), b"not a database").unwrap();
    let sink = env
        .pass(
            Kind::IMessage,
            &settings(&db.path, Some(&ab)),
            &Replica::new(),
        )
        .await;
    assert_eq!(sink.upserts().len(), 3);
}

#[tokio::test]
async fn reads_uncheckpointed_wal_and_old_schemas() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Messages");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("chat.db");
    // An old macOS schema: no attributedBody, item_type, associated_message_type, attachments.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
         CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT);
         CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, guid TEXT, chat_identifier TEXT);
         CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
         CREATE TABLE message (ROWID INTEGER PRIMARY KEY, text TEXT, handle_id INTEGER, date INTEGER, is_from_me INTEGER);
         CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);",
    )
    .unwrap();
    // Seconds (pre-High Sierra dates), written only to the WAL while the "app" keeps it open.
    conn.execute_batch(&format!(
        "INSERT INTO handle VALUES (1, 'dan@example.com');
         INSERT INTO chat VALUES (1, 'iMessage;-;dan@example.com', 'dan@example.com');
         INSERT INTO chat_handle_join VALUES (1, 1);
         INSERT INTO message VALUES (1, 'from the wal', 1, {}, 0);
         INSERT INTO chat_message_join VALUES (1, 1);",
        MAY1 - APPLE_EPOCH
    ))
    .unwrap();
    assert!(dir.join("chat.db-wal").exists());
    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(Kind::IMessage, &settings(&path, None), &replica)
        .await;
    let d = doc(&sink, "im:iMessage;-;dan@example.com/2024-05");
    assert!(text(&d).contains("**dan@example.com** · 2024-05-01 09:00\n> from the wal"));
    drop(conn);
}

#[test]
fn validation() {
    let tmp = tempfile::tempdir().unwrap();
    if !cfg!(target_os = "macos") {
        match IMessageFactory.validate(&json!({})) {
            Err(Error::Unsupported(m)) => assert_eq!(m, "iMessage is only available on macOS"),
            other => panic!("{other:?}"),
        }
    }
    let missing = tmp.path().join("chat.db");
    assert!(matches!(
        IMessageFactory.validate(&json!({ "db_path": missing })),
        Err(Error::NotFound(_))
    ));
    let db = scenario(&tmp.path().join("M"));
    assert!(
        IMessageFactory
            .validate(&json!({ "db_path": db.path }))
            .is_ok()
    );
    let info = IMessageFactory.info();
    assert_eq!((info.kind, info.display_name), ("imessage", "iMessage"));
}

/// Permission errors (what macOS privacy protection returns) ask for Full Disk Access, from
/// both `validate` and `sync`.
#[cfg(unix)]
#[tokio::test]
async fn permission_denied_asks_for_full_disk_access() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let db = scenario(&tmp.path().join("Messages"));
    let path = db.path.clone();
    drop(db);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&path).is_ok() {
        eprintln!("running with privileges that ignore file modes (root?); skipping");
        return;
    }
    let check = |r: Result<(), Error>| match r {
        Err(Error::Unauthorized(m)) => {
            assert!(m.contains("Full Disk Access"));
            assert_eq!(m, nugget_connector_messaging::FULL_DISK_ACCESS_MESSAGE);
        }
        other => panic!("{other:?}"),
    };
    check(IMessageFactory.validate(&json!({ "db_path": path })));
    let env = Env::new();
    let replica = Replica::new();
    let conn = env.build(Kind::IMessage, &settings(&path, None), replica.clone());
    let sink = nugget_connector::testkit::ReplicaSink::new(replica);
    check(
        conn.sync(None, &sink, &nugget_connector::CancellationToken::new())
            .await
            .map(|_| ()),
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Set `db`'s (and its WAL's) modification time an hour back, so the reader trusts its stamp.
fn settle(db: &Path) {
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    for f in [db.to_path_buf(), PathBuf::from(wal)] {
        if f.exists() {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&f)
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
    }
}

/// JJ2: polling an unchanged database sends nothing and deletes nothing (the copy is skipped);
/// a change still arrives, also after a pass that failed to deliver it.
#[tokio::test]
async fn unchanged_polls_keep_everything_and_changes_still_arrive() {
    use nugget_connector::testkit::{ReplicaSink, run_pass};
    let tmp = tempfile::tempdir().unwrap();
    let db = scenario(&tmp.path().join("Messages"));
    settle(&db.path);
    let env = Env::new();
    let replica = Replica::new();
    let conn = env.build(Kind::IMessage, &settings(&db.path, None), replica.clone());

    let sink = ReplicaSink::new(replica.clone());
    run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(sink.upserts().len(), 3);
    let sink = ReplicaSink::new(replica.clone());
    run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(sink.sends(), 0);
    assert_eq!(replica.len(), 3, "an unchanged pass keeps every document");

    // A new message in June, and a pass that fails before delivering it.
    db.text(1, MAY1 + 31 * 86_400 + 60, 1, false, "Still June");
    settle(&db.path);
    let sink = ReplicaSink::new(replica.clone()).crash_at_send(0);
    assert!(run_pass(&*conn, None, &sink).await.is_err());
    // A pass whose changes were sent (queued) but could not be stored: the checkpoint at the
    // end of `sync` fails, and the store does not have them.
    let before = replica.snapshot();
    let sink = ReplicaSink::new(replica.clone()).crash_at_checkpoint(0);
    assert!(
        conn.sync(None, &sink, &nugget_connector::CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(sink.sends(), 1, "the change was sent");
    replica.restore(before);
    let sink = ReplicaSink::new(replica.clone());
    run_pass(&*conn, None, &sink).await.unwrap();
    let june = doc(&sink, "im:iMessage;-;+15551234567/2024-06");
    assert!(text(&june).contains("Still June"));
    assert_eq!(sink.sends(), 1);
    let sink = ReplicaSink::new(replica.clone());
    run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(sink.sends(), 0);
    assert_eq!(replica.len(), 3);
    let left: Vec<_> = std::fs::read_dir(env.scratch()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}
