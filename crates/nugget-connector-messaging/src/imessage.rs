//! iMessage / SMS from the macOS Messages database (`~/Library/Messages/chat.db`).
//!
//! The live database is never opened: `chat.db` with its `-wal` and `-shm` files is copied into
//! a fresh private temp dir under the cache dir, the copy is read and then removed. Columns
//! vary across macOS versions, so the queries are built from `PRAGMA table_info`.
//!
//! Polling is cheap while nothing changes: each pass first takes a [`Fingerprint`] of the
//! files it would copy (size, modification time and, on Unix, inode and change time of
//! `chat.db`, `chat.db-wal` and the address books). When it equals the one taken before the last
//! pass that succeeded end to end, the pass copies and reads nothing and keeps every known
//! document. Any change to any of these files (a WAL appearing or disappearing too) forces the
//! copy; so does a file modified in the last [`SETTLE`] (coarse timestamps could hide a second
//! write), and a full pass runs at least every [`FULL_PASS_EVERY`] whatever the fingerprint
//! says (`since_days` moves with time). The fingerprint lives in memory only: after a restart
//! the first pass copies.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

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
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
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
            .query_map([], |r| {
                Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?))
            })
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
            .query_map([], |r| {
                Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?))
            })
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

/// Read every address book under `dir` (copies in `tmp`), and whether every one could be
/// read. Failures are logged, never fatal.
fn load_contacts_checked(dir: &Path, tmp: &Path) -> (ContactBook, bool) {
    let mut complete = true;
    let mut book = ContactBook::default();
    for (i, path) in address_books(dir).into_iter().enumerate() {
        let sub = tmp.join(format!("ab{i}"));
        let res = std::fs::create_dir_all(&sub)
            .map_err(Error::Io)
            .and_then(|_| copy_db(&path, &sub, false))
            .and_then(|copy| open_copy(&copy))
            .and_then(|conn| read_book(&conn, &mut book));
        if let Err(e) = res {
            complete = false;
            tracing::warn!(book = %path.display(), error = %e, "cannot read address book; showing handles instead");
        }
    }
    (book, complete)
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
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
            })
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
                        guid: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        identifier: r.get(2)?,
                        display_name: r.get(3)?,
                        style: r.get(4)?,
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
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(sql_err)?;
        for row in rows {
            let (c, h) = row.map_err(sql_err)?;
            members.entry(c).or_default().push(h);
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
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
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
                text: r.get(2)?,
                body: r.get(3)?,
                date: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                from_me: r.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
                handle: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                associated: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                item_type: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                group_title: r.get(9)?,
                other_handle: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                group_action: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
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
        let m = row.map_err(sql_err)?;
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

/// A file modified more recently than this is not trusted to be unchanged by its stamp alone:
/// file systems with coarse timestamps could hide a second write within the same tick.
pub(crate) const SETTLE: Duration = Duration::from_secs(2);

/// Copy and read the database at least this often even when its fingerprint is unchanged.
pub(crate) const FULL_PASS_EVERY: Duration = Duration::from_secs(3600);

/// Cheap identity of one file: `None` when it does not exist (a missing WAL is a valid state).
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    /// Inode and change time (Unix): a file replaced by another, or rewritten with its old
    /// modification time restored, still differs.
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: (i64, i64),
}

/// Stamps of every file a pass copies, in a fixed order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint(Vec<(PathBuf, Option<FileStamp>)>);

/// `path` and its `-wal` companion.
fn with_wal(path: &Path) -> [PathBuf; 2] {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    [path.to_path_buf(), PathBuf::from(wal)]
}

/// The fingerprint of `db` and the address books in `contacts_dir`, if every file could be
/// stamped and none was modified within [`SETTLE`] of now. `None` means: copy.
fn fingerprint(db: &Path, contacts_dir: Option<&Path>) -> Option<Fingerprint> {
    let mut files: Vec<PathBuf> = with_wal(db).into();
    if let Some(dir) = contacts_dir {
        for book in address_books(dir) {
            files.extend(with_wal(&book));
        }
    }
    let now = SystemTime::now();
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let stamp = match std::fs::metadata(&f) {
            Ok(m) => {
                let modified = m.modified().ok();
                // Unknown or recent (or in the future): not settled.
                let settled = modified
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age >= SETTLE);
                if !settled {
                    return None;
                }
                #[cfg(unix)]
                let stamp = {
                    use std::os::unix::fs::MetadataExt;
                    FileStamp {
                        len: m.len(),
                        modified,
                        ino: m.ino(),
                        ctime: (m.ctime(), m.ctime_nsec()),
                    }
                };
                #[cfg(not(unix))]
                let stamp = FileStamp {
                    len: m.len(),
                    modified,
                };
                Some(stamp)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        out.push((f, stamp));
    }
    Some(Fingerprint(out))
}

#[derive(Default)]
struct PollState {
    /// Taken before the last pass that succeeded end to end, and when that pass ran.
    last: Option<(Fingerprint, SystemTime)>,
    /// Taken before the current pass's copy; becomes `last` if the pass succeeds.
    pending: Option<Fingerprint>,
}

pub(crate) struct IMessageReader {
    pub db: PathBuf,
    pub contacts_dir: Option<PathBuf>,
    /// Parent of the per-pass temp dirs (under the cache dir).
    pub scratch: PathBuf,
    state: Mutex<PollState>,
    /// Passes that copied the database (for tests).
    copies: AtomicUsize,
    /// [`FULL_PASS_EVERY`] (shorter in tests).
    full_pass_every: Duration,
}

impl IMessageReader {
    pub fn new(db: PathBuf, contacts_dir: Option<PathBuf>, scratch: PathBuf) -> Self {
        Self {
            db,
            contacts_dir,
            scratch,
            state: Mutex::new(PollState::default()),
            copies: AtomicUsize::new(0),
            full_pass_every: FULL_PASS_EVERY,
        }
    }

    /// How many passes copied the database so far.
    #[cfg(test)]
    pub(crate) fn copies(&self) -> usize {
        self.copies.load(Ordering::SeqCst)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, PollState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether this pass may skip the copy; otherwise forget the last fingerprint (a failed
    /// pass must not let the next one skip) and remember this one as pending.
    fn unchanged(&self, fp: Option<Fingerprint>) -> bool {
        let mut st = self.state();
        st.pending = None;
        if let (Some(fp), Some((last, at))) = (&fp, &st.last)
            && fp == last
            // Wall-clock time, which (unlike `Instant` on macOS) also runs while the Mac
            // sleeps; a clock that went backwards makes a full pass due.
            && at.elapsed().is_ok_and(|age| age < self.full_pass_every)
        {
            return true;
        }
        st.last = None;
        st.pending = fp;
        false
    }
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
        if self.unchanged(fingerprint(&self.db, self.contacts_dir.as_deref())) {
            tracing::trace!("Messages database unchanged; skipping the copy");
            let _ = found(Found::Keep(String::new()));
            return Ok(());
        }
        self.copies.fetch_add(1, Ordering::SeqCst);
        // Removed (with the copies) when dropped, also on errors.
        let tmp = tempfile::Builder::new()
            .prefix("imessage-")
            .tempdir_in(&self.scratch)?;
        let copy = copy_db(&self.db, tmp.path(), true)?;
        let contacts = match &self.contacts_dir {
            Some(dir) => {
                let (book, complete) = load_contacts_checked(dir, tmp.path());
                if !complete {
                    // Names may be missing this time: do not let later passes skip the read.
                    self.state().pending = None;
                }
                book
            }
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

    fn remembers_passes(&self) -> bool {
        true
    }

    fn pass_succeeded(&self) {
        let mut st = self.state();
        if let Some(fp) = st.pending.take() {
            st.last = Some((fp, SystemTime::now()));
        }
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

    /// A WAL-mode chat.db (old schema) with one message, written while `conn` stays open.
    fn wal_db(dir: &Path) -> (PathBuf, Connection) {
        let path = dir.join("chat.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT);
             CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, guid TEXT, chat_identifier TEXT);
             CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
             CREATE TABLE message (ROWID INTEGER PRIMARY KEY, text TEXT, handle_id INTEGER, date INTEGER, is_from_me INTEGER);
             CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);
             INSERT INTO handle VALUES (1, 'dan@example.com');
             INSERT INTO chat VALUES (1, 'iMessage;-;dan@example.com', 'dan@example.com');
             INSERT INTO chat_handle_join VALUES (1, 1);",
        )
        .unwrap();
        add_message(&conn, 1);
        (path, conn)
    }

    fn add_message(conn: &Connection, id: i64) {
        conn.execute_batch(&format!(
            "INSERT INTO message VALUES ({id}, 'message {id}', 1, {}, 0);
             INSERT INTO chat_message_join VALUES (1, {id});",
            700_000_000 + id
        ))
        .unwrap();
    }

    /// Set the database files' modification time an hour back, so they count as settled.
    fn settle(db: &Path) {
        let past = std::time::SystemTime::now() - Duration::from_secs(3600);
        for f in with_wal(db) {
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

    /// One read: (messages read, whether everything known was kept without reading).
    fn read(r: &IMessageReader) -> (usize, bool) {
        let (mut messages, mut keep_all) = (0, false);
        r.read(&CancellationToken::new(), &mut |f| {
            match f {
                Found::Conversation(c) => messages += c.messages.len(),
                Found::Keep(p) => keep_all |= p.is_empty(),
            }
            true
        })
        .unwrap();
        (messages, keep_all)
    }

    fn reader(db: &Path, scratch: &Path) -> IMessageReader {
        IMessageReader::new(db.to_path_buf(), None, scratch.to_path_buf())
    }

    /// JJ2: a poll pass copies chat.db only when it or its WAL changed since the last pass
    /// that succeeded.
    #[test]
    fn unchanged_database_is_not_copied_again() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("scratch");
        let (db, conn) = wal_db(tmp.path());
        assert!(with_wal(&db)[1].exists());
        settle(&db);
        let r = reader(&db, &scratch);

        assert_eq!(read(&r), (1, false));
        r.pass_succeeded();
        assert_eq!(r.copies(), 1);
        assert_eq!(read(&r), (0, true), "unchanged: nothing read, all kept");
        assert_eq!(read(&r), (0, true));
        assert_eq!(r.copies(), 1);

        // A write to the WAL forces the copy.
        add_message(&conn, 2);
        settle(&db);
        assert_eq!(read(&r), (2, false));
        r.pass_succeeded();
        assert_eq!(read(&r), (0, true));
        assert_eq!(r.copies(), 2);

        // Closing checkpoints into chat.db and removes the WAL: a change, then a valid state.
        drop(conn);
        assert!(!with_wal(&db)[1].exists());
        settle(&db);
        assert_eq!(read(&r), (2, false));
        r.pass_succeeded();
        assert_eq!(
            read(&r),
            (0, true),
            "a missing WAL is a fingerprint like any other"
        );
        assert_eq!(r.copies(), 3);

        // A WAL appearing forces the copy too.
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
        add_message(&conn, 3);
        assert!(with_wal(&db)[1].exists());
        settle(&db);
        assert_eq!(read(&r), (3, false));
        assert_eq!(r.copies(), 4);
        let left: Vec<_> = std::fs::read_dir(&scratch).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// A fingerprint counts only once the whole pass succeeded: until then (a pass whose
    /// changes failed to reach the index) every pass copies again.
    #[test]
    fn only_a_successful_pass_records_the_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _conn) = wal_db(tmp.path());
        settle(&db);
        let r = reader(&db, &tmp.path().join("scratch"));
        assert_eq!(read(&r), (1, false));
        // No `pass_succeeded`.
        assert_eq!(read(&r), (1, false));
        r.pass_succeeded();
        assert_eq!(read(&r), (0, true));
        // A skipped pass's success refreshes nothing.
        r.pass_succeeded();
        assert_eq!(read(&r), (0, true));
        assert_eq!(r.copies(), 2);
        // A forced pass (here: a full pass is due) forgets the old fingerprint, so if it fails
        // the next pass copies again.
        let mut r = r;
        r.full_pass_every = Duration::ZERO;
        assert_eq!(read(&r), (1, false));
        assert!(r.state().last.is_none());
        r.full_pass_every = FULL_PASS_EVERY;
        assert_eq!(read(&r), (1, false));
        r.pass_succeeded();
        assert_eq!(read(&r), (0, true));
        assert_eq!(r.copies(), 4);
    }

    /// A pass with an address book that could not be read is not a complete pass: the next
    /// one reads again.
    #[test]
    fn an_unreadable_address_book_keeps_the_next_pass_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _conn) = wal_db(tmp.path());
        settle(&db);
        let ab = tmp.path().join("AB");
        std::fs::create_dir_all(&ab).unwrap();
        let book = ab.join("AddressBook-v22.abcddb");
        // Readable, but a row that cannot be read (a text primary key).
        Connection::open(&book)
            .unwrap()
            .execute_batch(
                "CREATE TABLE ZABCDRECORD (Z_PK TEXT, ZFIRSTNAME TEXT);
                 INSERT INTO ZABCDRECORD VALUES ('x', 'Ann');",
            )
            .unwrap();
        settle(&book);
        let r = IMessageReader::new(db.clone(), Some(ab), tmp.path().join("scratch"));
        for _ in 0..2 {
            assert_eq!(read(&r), (1, false));
            r.pass_succeeded();
        }
        assert_eq!(r.copies(), 2);
    }

    /// A full pass is due once `full_pass_every` has passed by the wall clock, also when the
    /// clock went backwards.
    #[test]
    fn a_full_pass_is_due_after_the_interval_or_a_clock_jump() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _conn) = wal_db(tmp.path());
        settle(&db);
        let r = reader(&db, &tmp.path().join("scratch"));
        assert_eq!(read(&r), (1, false));
        r.pass_succeeded();
        assert_eq!(read(&r), (0, true));
        r.state().last.as_mut().unwrap().1 = SystemTime::now() - FULL_PASS_EVERY;
        assert_eq!(read(&r), (1, false));
        r.pass_succeeded();
        assert_eq!(read(&r), (0, true));
        r.state().last.as_mut().unwrap().1 = SystemTime::now() + Duration::from_secs(3600);
        assert_eq!(read(&r), (1, false));
        assert_eq!(r.copies(), 3);
    }

    /// A file modified just now is not trusted to be unchanged by its stamp alone.
    #[test]
    fn recently_modified_files_are_always_copied() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _conn) = wal_db(tmp.path());
        let r = reader(&db, &tmp.path().join("scratch"));
        for _ in 0..3 {
            assert_eq!(read(&r), (1, false));
            r.pass_succeeded();
        }
        assert_eq!(r.copies(), 3);
    }
}
