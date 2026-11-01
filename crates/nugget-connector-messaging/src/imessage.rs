//! iMessage / SMS from the macOS Messages database (`~/Library/Messages/chat.db`).
//!
//! The live database is never opened: `chat.db` with its `-wal` and `-shm` files is copied into
//! a fresh private temp dir under the cache dir, the copy is read and then removed. Columns
//! vary across macOS versions, so the queries are built from `PRAGMA table_info`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use nugget_connector::CancellationToken;
use nugget_core::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use time::OffsetDateTime;

use crate::connector::{Found, Reader};
use crate::model::{
    Attachment, Conversation, Message, basename, clean_name, mime_from_name, normalise_phone,
    phone_key,
};
use crate::settings::map_db_error;
use crate::typedstream::attributed_body_text;

/// Seconds between the Unix epoch and Apple's (2001-01-01T00:00:00Z).
const APPLE_EPOCH: i64 = 978_307_200;

/// A Messages `date` value: nanoseconds since 2001 on macOS 10.13+, seconds before.
pub fn apple_time(v: i64) -> Option<OffsetDateTime> {
    let secs = if v.unsigned_abs() > 1_000_000_000_000 {
        v / 1_000_000_000
    } else {
        v
    };
    let nanos = if v.unsigned_abs() > 1_000_000_000_000 {
        v % 1_000_000_000
    } else {
        0
    };
    let t = OffsetDateTime::from_unix_timestamp(secs.checked_add(APPLE_EPOCH)?).ok()?;
    Some(t + time::Duration::nanoseconds(nanos))
}

fn sql_err(e: rusqlite::Error) -> Error {
    Error::Storage(format!("Messages database: {e}"))
}

/// A text column, whatever SQLite holds there: invalid UTF-8 (which Messages does store) is
/// decoded lossily instead of failing the whole read; a blob is read as text, a number as its
/// digits.
fn text_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<String>> {
    use rusqlite::types::ValueRef;
    Ok(match r.get_ref(i)? {
        ValueRef::Null => None,
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
        ValueRef::Integer(n) => Some(n.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
    })
}

/// A blob column (also when stored as text).
fn bytes_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<Vec<u8>>> {
    use rusqlite::types::ValueRef;
    Ok(match r.get_ref(i)? {
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(b.to_vec()),
        _ => None,
    })
}

/// An integer column; anything else (a stray text or blob) reads as `None`.
fn int_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<i64>> {
    use rusqlite::types::ValueRef;
    Ok(match r.get_ref(i)? {
        ValueRef::Integer(n) => Some(n),
        ValueRef::Real(f) => Some(f as i64),
        _ => None,
    })
}

/// Copy `db` (and its `-wal` / `-shm` companions) into `dir`; returns the copy's path.
fn copy_db(db: &Path, dir: &Path, map_perm: bool) -> Result<PathBuf> {
    let name = db.file_name().unwrap_or_else(|| "db".as_ref());
    let dest = dir.join(name);
    std::fs::copy(db, &dest).map_err(|e| {
        if map_perm {
            map_db_error(e, db)
        } else {
            Error::Io(e)
        }
    })?;
    for suffix in ["-wal", "-shm"] {
        let mut src = db.as_os_str().to_owned();
        src.push(suffix);
        let mut dst = dest.as_os_str().to_owned();
        dst.push(suffix);
        match std::fs::copy(&src, &dst) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) if map_perm => return Err(map_db_error(e, db)),
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(dest)
}

fn open_copy(path: &Path) -> Result<Connection> {
    // Read-write on our private copy so SQLite can replay the copied WAL.
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql_err)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(sql_err)?;
    Ok(conn)
}

fn columns(conn: &Connection, table: &str) -> HashSet<String> {
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info(\"{table}\")")) else {
        return HashSet::new();
    };
    st.query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// `alias.col` when the table has the column, else `default` (SQL literal).
fn col(cols: &HashSet<String>, alias: &str, name: &str, default: &str) -> String {
    if cols.contains(name) {
        format!("{alias}.\"{name}\"")
    } else {
        default.to_owned()
    }
}

// ---------------------------------------------------------------------------------------------
// Contacts

/// Names from the address book, by phone key (last ten digits) and lower-case e-mail.
#[derive(Debug, Default, Clone)]
pub struct ContactBook {
    pub phones: HashMap<String, String>,
    pub emails: HashMap<String, String>,
}

impl ContactBook {
    pub fn name_for(&self, handle: &str) -> Option<&str> {
        if handle.contains('@') {
            self.emails
                .get(&handle.trim().to_lowercase())
                .map(String::as_str)
        } else {
            phone_key(handle).and_then(|k| self.phones.get(&k).map(String::as_str))
        }
    }
}

/// `AddressBook-v22.abcddb` files in `dir` and `dir/Sources/*/`.
fn address_books(dir: &Path) -> Vec<PathBuf> {
    const NAME: &str = "AddressBook-v22.abcddb";
    let mut out = Vec::new();
    let top = dir.join(NAME);
    if top.is_file() {
        out.push(top);
    }
    if let Ok(rd) = std::fs::read_dir(dir.join("Sources")) {
        let mut v: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path().join(NAME))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

fn read_book(conn: &Connection, book: &mut ContactBook) -> Result<()> {
    let rec = columns(conn, "ZABCDRECORD");
    if rec.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "SELECT r.Z_PK, {}, {}, {} FROM ZABCDRECORD r",
        col(&rec, "r", "ZFIRSTNAME", "NULL"),
        col(&rec, "r", "ZLASTNAME", "NULL"),
        col(&rec, "r", "ZORGANIZATION", "NULL"),
    );
    let mut names: HashMap<i64, String> = HashMap::new();
    let mut st = conn.prepare(&sql).map_err(sql_err)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                text_col(r, 1)?,
                text_col(r, 2)?,
                text_col(r, 3)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (pk, first, last, org) = row.map_err(sql_err)?;
        let full = [first, last]
            .into_iter()
            .flatten()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let name = if full.is_empty() {
            org.map(|o| o.trim().to_owned()).unwrap_or_default()
        } else {
            full
        };
        let name = clean_name(&name);
        if !name.is_empty() {
            names.insert(pk, name);
        }
    }

    let phone_cols = columns(conn, "ZABCDPHONENUMBER");
    if phone_cols.contains("ZOWNER") && phone_cols.contains("ZFULLNUMBER") {
        let mut st = conn
            .prepare("SELECT ZOWNER, ZFULLNUMBER FROM ZABCDPHONENUMBER")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok((int_col(r, 0)?, text_col(r, 1)?)))
            .map_err(sql_err)?;
        for row in rows {
            if let (Some(owner), Some(num)) = row.map_err(sql_err)?
                && let (Some(name), Some(key)) = (names.get(&owner), phone_key(&num))
            {
                book.phones.entry(key).or_insert_with(|| name.clone());
            }
        }
    }
    let mail_cols = columns(conn, "ZABCDEMAILADDRESS");
    let addr = if mail_cols.contains("ZADDRESS") {
        Some("ZADDRESS")
    } else if mail_cols.contains("ZADDRESSNORMALIZED") {
        Some("ZADDRESSNORMALIZED")
    } else {
        None
    };
    if let Some(addr) = addr.filter(|_| mail_cols.contains("ZOWNER")) {
        let mut st = conn
            .prepare(&format!("SELECT ZOWNER, \"{addr}\" FROM ZABCDEMAILADDRESS"))
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok((int_col(r, 0)?, text_col(r, 1)?)))
            .map_err(sql_err)?;
        for row in rows {
            if let (Some(owner), Some(mail)) = row.map_err(sql_err)?
                && let Some(name) = names.get(&owner)
            {
                book.emails
                    .entry(mail.trim().to_lowercase())
                    .or_insert_with(|| name.clone());
            }
        }
    }
    Ok(())
}

/// Read every address book under `dir` (copies in `tmp`). Failures are logged, never fatal.
pub fn load_contacts(dir: &Path, tmp: &Path) -> ContactBook {
    let mut book = ContactBook::default();
    for (i, path) in address_books(dir).into_iter().enumerate() {
        let sub = tmp.join(format!("ab{i}"));
        let res = std::fs::create_dir_all(&sub)
            .map_err(Error::Io)
            .and_then(|_| copy_db(&path, &sub, false))
            .and_then(|copy| open_copy(&copy))
            .and_then(|conn| read_book(&conn, &mut book));
        if let Err(e) = res {
            tracing::warn!(book = %path.display(), error = %e, "cannot read address book; showing handles instead");
        }
    }
    book
}

// ---------------------------------------------------------------------------------------------
// Messages

struct ChatRow {
    guid: String,
    identifier: Option<String>,
    display_name: Option<String>,
    style: Option<i64>,
}

struct MsgRow {
    chat: i64,
    rowid: i64,
    text: Option<String>,
    body: Option<Vec<u8>>,
    date: i64,
    from_me: bool,
    handle: i64,
    associated: i64,
    item_type: i64,
    group_title: Option<String>,
    other_handle: i64,
    group_action: i64,
}

/// Everything read from one chat.db copy.
pub fn read_conversations(
    conn: &Connection,
    contacts: &ContactBook,
    cancel: &CancellationToken,
) -> Result<Vec<Conversation>> {
    // Handles.
    let mut handles: HashMap<i64, String> = HashMap::new();
    {
        let mut st = conn
            .prepare("SELECT ROWID, id FROM handle")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, text_col(r, 1)?)))
            .map_err(sql_err)?;
        for row in rows {
            let (id, h) = row.map_err(sql_err)?;
            if let Some(h) = h {
                handles.insert(id, h);
            }
        }
    }

    // Chats.
    let chat_cols = columns(conn, "chat");
    let sql = format!(
        "SELECT c.ROWID, {}, {}, {}, {} FROM chat c ORDER BY c.ROWID",
        col(&chat_cols, "c", "guid", "CAST(c.ROWID AS TEXT)"),
        col(&chat_cols, "c", "chat_identifier", "NULL"),
        col(&chat_cols, "c", "display_name", "NULL"),
        col(&chat_cols, "c", "style", "NULL"),
    );
    let mut chats: BTreeMap<i64, ChatRow> = BTreeMap::new();
    {
        let mut st = conn.prepare(&sql).map_err(sql_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    ChatRow {
                        guid: text_col(r, 1)?.unwrap_or_default(),
                        identifier: text_col(r, 2)?,
                        display_name: text_col(r, 3)?,
                        style: int_col(r, 4)?,
                    },
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (id, c) = row.map_err(sql_err)?;
            chats.insert(id, c);
        }
    }

    // Members.
    let mut members: HashMap<i64, Vec<i64>> = HashMap::new();
    if !columns(conn, "chat_handle_join").is_empty() {
        let mut st = conn
            .prepare("SELECT chat_id, handle_id FROM chat_handle_join ORDER BY chat_id, handle_id")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok((int_col(r, 0)?, int_col(r, 1)?)))
            .map_err(sql_err)?;
        for row in rows {
            if let (Some(c), Some(h)) = row.map_err(sql_err)? {
                members.entry(c).or_default().push(h);
            }
        }
    }

    // Attachments (metadata only).
    let mut attachments: HashMap<i64, Vec<Attachment>> = HashMap::new();
    let att_cols = columns(conn, "attachment");
    if !att_cols.is_empty() && !columns(conn, "message_attachment_join").is_empty() {
        let sql = format!(
            "SELECT j.message_id, {}, {}, {}, {} FROM message_attachment_join j \
             JOIN attachment a ON a.ROWID = j.attachment_id ORDER BY j.message_id, a.ROWID",
            col(&att_cols, "a", "transfer_name", "NULL"),
            col(&att_cols, "a", "filename", "NULL"),
            col(&att_cols, "a", "mime_type", "NULL"),
            col(&att_cols, "a", "total_bytes", "NULL"),
        );
        let mut st = conn.prepare(&sql).map_err(sql_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    text_col(r, 1)?,
                    text_col(r, 2)?,
                    text_col(r, 3)?,
                    int_col(r, 4)?,
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (mid, transfer, filename, mime, size) = row.map_err(sql_err)?;
            let name = transfer
                .filter(|s| !s.trim().is_empty())
                .or(filename)
                .map(|n| basename(&n).to_owned());
            let mime = mime.or_else(|| name.as_deref().and_then(mime_from_name).map(str::to_owned));
            attachments.entry(mid).or_default().push(Attachment {
                name,
                mime,
                size: size.and_then(|s| u64::try_from(s).ok()),
            });
        }
    }

    // Messages.
    let mc = columns(conn, "message");
    let sql = format!(
        "SELECT j.chat_id, m.ROWID, {text}, {body}, {date}, {from_me}, {handle}, {assoc}, {item}, \
         {title}, {other}, {action} \
         FROM message m JOIN chat_message_join j ON j.message_id = m.ROWID \
         ORDER BY j.chat_id, {date}, m.ROWID",
        text = col(&mc, "m", "text", "NULL"),
        body = col(&mc, "m", "attributedBody", "NULL"),
        date = col(&mc, "m", "date", "0"),
        from_me = col(&mc, "m", "is_from_me", "0"),
        handle = col(&mc, "m", "handle_id", "0"),
        assoc = col(&mc, "m", "associated_message_type", "0"),
        item = col(&mc, "m", "item_type", "0"),
        title = col(&mc, "m", "group_title", "NULL"),
        other = col(&mc, "m", "other_handle", "0"),
        action = col(&mc, "m", "group_action_type", "0"),
    );
    let mut st = conn.prepare(&sql).map_err(sql_err)?;
    let rows = st
        .query_map([], |r| {
            Ok(MsgRow {
                chat: r.get(0)?,
                rowid: r.get(1)?,
                text: text_col(r, 2)?,
                body: bytes_col(r, 3)?,
                date: int_col(r, 4)?.unwrap_or(0),
                from_me: int_col(r, 5)?.unwrap_or(0) != 0,
                handle: int_col(r, 6)?.unwrap_or(0),
                associated: int_col(r, 7)?.unwrap_or(0),
                item_type: int_col(r, 8)?.unwrap_or(0),
                group_title: text_col(r, 9)?,
                other_handle: int_col(r, 10)?.unwrap_or(0),
                group_action: int_col(r, 11)?.unwrap_or(0),
            })
        })
        .map_err(sql_err)?;

    let display = |h: &str| -> String {
        contacts
            .name_for(h)
            .map(str::to_owned)
            .unwrap_or_else(|| h.to_owned())
    };
    let handle_value = |h: &str| -> String {
        if h.contains('@') {
            h.trim().to_lowercase()
        } else if h.chars().any(|c| c.is_ascii_digit())
            && h.chars()
                .all(|c| c.is_ascii_digit() || " +-().".contains(c))
        {
            normalise_phone(h)
        } else {
            h.to_owned()
        }
    };

    let mut convs: BTreeMap<i64, Conversation> = BTreeMap::new();
    let mut n = 0usize;
    for row in rows {
        n += 1;
        if n.is_multiple_of(2000) && cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let m = match row {
            Ok(m) => m,
            // SQLite itself failed (corrupt, busy): the pass fails, nothing is deleted.
            Err(e @ rusqlite::Error::SqliteFailure(..)) => return Err(sql_err(e)),
            // One row of an unexpected shape: skip it, keep the rest.
            Err(e) => {
                tracing::warn!(error = %e, "unreadable Messages row skipped");
                continue;
            }
        };
        let Some(chat) = chats.get(&m.chat) else {
            continue;
        };
        // Tapbacks, reactions and other annotations of another message.
        if m.associated != 0 {
            continue;
        }
        let Some(at) = apple_time(m.date) else {
            continue;
        };
        let conv = convs.entry(m.chat).or_insert_with(|| {
            let mut c = Conversation::new(format!("im:{}", chat.guid), String::new());
            for h in members.get(&m.chat).into_iter().flatten() {
                if let Some(handle) = handles.get(h) {
                    c.participant(&display(handle), Some(&handle_value(handle)), false);
                }
            }
            c
        });
        let author_of = |conv: &mut Conversation, from_me: bool, handle: i64| {
            if from_me {
                Some(conv.participant("Me", None, true))
            } else {
                handles
                    .get(&handle)
                    .map(|h| conv.participant(&display(h), Some(&handle_value(h)), false))
            }
        };
        if m.item_type != 0 {
            let who = author_of(conv, m.from_me, m.handle);
            let who_name = who
                .and_then(|i| conv.participants.get(i))
                .map(|p| {
                    if p.me {
                        "You".to_owned()
                    } else {
                        p.name.clone()
                    }
                })
                .unwrap_or_else(|| "Someone".into());
            let other = handles
                .get(&m.other_handle)
                .map(|h| display(h))
                .unwrap_or_else(|| "someone".into());
            let text = match (m.item_type, m.group_action) {
                (1, 0) => format!("{who_name} added {other}"),
                (1, 1) => format!("{who_name} removed {other}"),
                (2, _) => match &m.group_title {
                    Some(t) => format!("{who_name} named the conversation “{t}”"),
                    None => format!("{who_name} removed the conversation name"),
                },
                (3, _) => format!("{who_name} left the conversation"),
                _ => continue,
            };
            conv.messages.push(Message::system(at, text));
            continue;
        }
        let author = author_of(conv, m.from_me, m.handle);
        let mut text = m
            .text
            .filter(|t| !t.trim().is_empty())
            .or_else(|| m.body.as_deref().and_then(attributed_body_text))
            .unwrap_or_default();
        // U+FFFC marks where an attachment sat in the text.
        text.retain(|c| c != '\u{FFFC}');
        let mut msg = Message::new(at, author, text.trim().to_owned());
        if let Some(a) = attachments.get(&m.rowid) {
            msg.attachments = a.clone();
        }
        conv.messages.push(msg);
    }

    let mut out = Vec::with_capacity(convs.len());
    for (id, mut conv) in convs {
        let Some(chat) = chats.get(&id) else { continue };
        let others: Vec<String> = conv
            .participants
            .iter()
            .filter(|p| !p.me)
            .map(|p| p.name.clone())
            .collect();
        conv.is_group = chat.style == Some(43) || others.len() > 1;
        conv.name = match chat.display_name.as_deref().map(str::trim) {
            Some(n) if !n.is_empty() => n.to_owned(),
            _ if !others.is_empty() => others.join(", "),
            _ => chat.identifier.clone().unwrap_or_else(|| chat.guid.clone()),
        };
        conv.normalise();
        out.push(conv);
    }
    Ok(out)
}

/// A per-pass temp dir older than this was left behind by a crashed process.
const STALE_COPY_AGE: Duration = Duration::from_secs(6 * 3600);

/// Remove `imessage-*` temp dirs a killed process left in `scratch`: each holds a full copy of
/// the user's Messages database. Recent ones may belong to a pass still running (another
/// iMessage source shares the scratch dir), so only old ones go.
pub fn remove_stale_copies(scratch: &Path) {
    let Ok(rd) = std::fs::read_dir(scratch) else {
        return;
    };
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().starts_with("imessage-") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > STALE_COPY_AGE);
        if meta.is_dir()
            && old
            && let Err(err) = std::fs::remove_dir_all(e.path())
        {
            tracing::warn!(dir = %e.path().display(), error = %err, "cannot remove a stale Messages copy");
        }
    }
}

pub(crate) struct IMessageReader {
    pub db: PathBuf,
    pub contacts_dir: Option<PathBuf>,
    /// Parent of the per-pass temp dirs (under the cache dir).
    pub scratch: PathBuf,
}

impl Reader for IMessageReader {
    fn kind(&self) -> &'static str {
        crate::IMESSAGE_KIND
    }

    fn service(&self) -> &'static str {
        "imessage"
    }

    fn read(&self, cancel: &CancellationToken, found: &mut dyn FnMut(Found) -> bool) -> Result<()> {
        crate::settings::check_db_readable(&self.db)?;
        std::fs::create_dir_all(&self.scratch)?;
        remove_stale_copies(&self.scratch);
        // Removed (with the copies) when dropped, also on errors.
        let tmp = tempfile::Builder::new()
            .prefix("imessage-")
            .tempdir_in(&self.scratch)?;
        let copy = copy_db(&self.db, tmp.path(), true)?;
        let contacts = match &self.contacts_dir {
            Some(dir) => load_contacts(dir, tmp.path()),
            None => ContactBook::default(),
        };
        let conn = open_copy(&copy)?;
        let convs = read_conversations(&conn, &contacts, cancel)?;
        drop(conn);
        for conv in convs {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !found(Found::Conversation(conv)) {
                break;
            }
        }
        Ok(())
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(120)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_dates() {
        let t = apple_time(0).unwrap();
        assert_eq!(t.unix_timestamp(), APPLE_EPOCH);
        let secs = 700_000_000i64;
        assert_eq!(
            apple_time(secs).unwrap().unix_timestamp(),
            APPLE_EPOCH + secs
        );
        let ns = secs * 1_000_000_000 + 5;
        let t = apple_time(ns).unwrap();
        assert_eq!(t.unix_timestamp(), APPLE_EPOCH + secs);
        assert_eq!(t.nanosecond(), 5);
    }

    /// Regression: a pass killed mid-read left a full copy of chat.db in the cache forever.
    #[test]
    fn stale_copies_are_removed_recent_ones_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let old = scratch.path().join("imessage-old");
        let fresh = scratch.path().join("imessage-fresh");
        let other = scratch.path().join("unrelated");
        for d in [&old, &fresh, &other] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("chat.db"), b"x").unwrap();
        }
        let day_ago = std::time::SystemTime::now() - Duration::from_secs(24 * 3600);
        for d in [&old, &other] {
            std::fs::File::open(d)
                .unwrap()
                .set_modified(day_ago)
                .unwrap();
        }
        remove_stale_copies(scratch.path());
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(other.exists());
    }

    #[test]
    fn contact_lookup() {
        let mut b = ContactBook::default();
        b.phones.insert("5551234567".into(), "Alice".into());
        b.emails.insert("bob@example.com".into(), "Bob".into());
        assert_eq!(b.name_for("+1 555-123-4567"), Some("Alice"));
        assert_eq!(b.name_for("(555) 123 4567"), Some("Alice"));
        assert_eq!(b.name_for("Bob@Example.com"), Some("Bob"));
        assert_eq!(b.name_for("+44 20 7946 0000"), None);
    }
}
