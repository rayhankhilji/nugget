//! Telegram Desktop exports (machine-readable JSON, `result.json`).
//!
//! Two shapes: a single-chat export (`{name, type, id, messages}`) and a full-account export
//! (`{personal_information: {user_id}, chats: {list: [...]}, left_chats: {list: [...]}}`).
//! Media files are never read; their names, types and sizes become attachment metadata.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_full::notify::RecursiveMode;
use nugget_connector::CancellationToken;
use nugget_core::{Error, Result};
use serde::Deserialize;
use serde_json::Value;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

use crate::connector::{Found, Reader};
use crate::model::{
    Attachment, Conversation, Message, basename, clean_name, file_url, looks_like_phone,
    mime_from_name, normalise_phone, slug,
};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Export {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub id: Option<Value>,
    #[serde(deserialize_with = "lenient_messages")]
    pub messages: Vec<TgMessage>,
    pub personal_information: Option<PersonalInfo>,
    pub chats: Option<ChatList>,
    pub left_chats: Option<ChatList>,
    /// The full export's address book (`{list: [{user_id, first_name, last_name,
    /// phone_number}]}`); read leniently, since only phone numbers are taken from it.
    pub contacts: Option<Value>,
}

/// One entry of a full export's `contacts.list`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TgContact {
    pub user_id: Option<Value>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub phone_number: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PersonalInfo {
    pub user_id: Option<Value>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChatList {
    pub list: Vec<TgChat>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct TgChat {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub id: Option<Value>,
    #[serde(deserialize_with = "lenient_messages")]
    pub messages: Vec<TgMessage>,
}

/// `result.json` files larger than this are not read (the whole file is parsed in memory).
pub const MAX_RESULT_JSON_BYTES: u64 = 1024 * 1024 * 1024;

/// A message list where one message of an unexpected shape (a field of another type in a newer
/// export version) is skipped instead of failing the whole export. The skipped message's
/// content never reaches an error message or the log.
fn lenient_messages<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<TgMessage>, D::Error> {
    let raw: Option<Vec<Value>> = Deserialize::deserialize(d)?;
    let raw = raw.unwrap_or_default();
    let total = raw.len();
    let out: Vec<TgMessage> = raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    if out.len() < total {
        tracing::warn!(
            skipped = total - out.len(),
            "Telegram messages of an unknown shape skipped"
        );
    }
    Ok(out)
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct TgMessage {
    pub id: Option<Value>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub date: Option<String>,
    pub date_unixtime: Option<Value>,
    pub edited: Option<String>,
    pub edited_unixtime: Option<Value>,
    pub from: Option<String>,
    pub from_id: Option<Value>,
    pub actor: Option<String>,
    pub actor_id: Option<Value>,
    pub action: Option<String>,
    pub title: Option<String>,
    pub members: Option<Vec<Value>>,
    pub text: Value,
    pub photo: Option<String>,
    pub photo_file_size: Option<u64>,
    pub file: Option<String>,
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
    pub media_type: Option<String>,
    pub mime_type: Option<String>,
    pub duration_seconds: Option<u64>,
}

fn id_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `text` is a string or an array of strings and `{type, text}` entities.
pub fn flatten_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p {
                Value::String(s) => s.clone(),
                Value::Object(o) => o
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                _ => String::new(),
            })
            .collect(),
        _ => String::new(),
    }
}

const LOCAL_DATE: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");

/// A `*_unixtime` value (a string or a number).
fn unix_value(v: Option<&Value>) -> Option<i64> {
    v.and_then(|v| match v {
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    })
}

/// A local `date` / `edited` value (`2024-05-01T09:12:00`, no offset) as wall-clock time.
fn local_value(d: Option<&str>) -> Option<OffsetDateTime> {
    let d = d?;
    PrimitiveDateTime::parse(d.get(..19).unwrap_or(d), LOCAL_DATE)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
}

/// When the message was sent, as the user saw it: the export's local `date` (wall-clock time
/// without an offset, carried as UTC like WhatsApp's times), so the shown time and the month
/// split match the user's clock. Only when `date` is missing or unreadable is the absolute
/// `date_unixtime` used (shown in UTC).
fn message_time(m: &TgMessage) -> Option<OffsetDateTime> {
    local_value(m.date.as_deref()).or_else(|| {
        unix_value(m.date_unixtime.as_ref())
            .and_then(|u| OffsetDateTime::from_unix_timestamp(u).ok())
    })
}

/// When the message was last edited, as a Unix time (`edited_unixtime`, else the local
/// `edited`), for picking the newest version of a message present in several exports.
fn edited_time(m: &TgMessage) -> Option<i64> {
    unix_value(m.edited_unixtime.as_ref())
        .or_else(|| local_value(m.edited.as_deref()).map(OffsetDateTime::unix_timestamp))
}

fn is_group_type(kind: &str) -> bool {
    !matches!(
        kind,
        "personal_chat" | "bot_chat" | "saved_messages" | "" | "private"
    )
}

fn attachment(m: &TgMessage) -> Option<Attachment> {
    // `(File not included. Change data exporting settings to download.)` is not a name.
    let real = |s: &String| !s.starts_with('(') && !s.trim().is_empty();
    if let Some(p) = m.photo.as_ref() {
        let name = real(p).then(|| basename(p).to_owned());
        return Some(Attachment {
            mime: Some(
                name.as_deref()
                    .and_then(mime_from_name)
                    .unwrap_or("image/jpeg")
                    .to_owned(),
            ),
            name,
            size: m.photo_file_size.or(m.file_size),
        });
    }
    if m.file.is_some() || m.file_name.is_some() || m.media_type.is_some() {
        let name = m
            .file_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                m.file
                    .as_ref()
                    .filter(|f| real(f))
                    .map(|f| basename(f).to_owned())
            });
        let mime = m.mime_type.clone().or_else(|| {
            name.as_deref()
                .and_then(mime_from_name)
                .map(str::to_owned)
                .or_else(|| match m.media_type.as_deref() {
                    Some("sticker") => Some("image/webp".into()),
                    Some("animation") => Some("video/mp4".into()),
                    Some("video_file" | "video_message") => Some("video/mp4".into()),
                    Some("voice_message") => Some("audio/ogg".into()),
                    Some("audio_file") => Some("audio/mpeg".into()),
                    _ => None,
                })
        });
        return Some(Attachment {
            name: name.map(|n| basename(&n).to_owned()),
            mime,
            size: m.file_size,
        });
    }
    None
}

fn members(m: &TgMessage) -> String {
    m.members
        .iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A service message in words.
fn service_text(m: &TgMessage) -> String {
    let actor = m
        .actor
        .clone()
        .or_else(|| m.from.clone())
        .unwrap_or_else(|| "Someone".into());
    let title = m.title.clone().unwrap_or_default();
    let action = m.action.as_deref().unwrap_or("");
    let text = match action {
        "create_group" | "create_channel" | "migrate_from_group" => {
            format!("{actor} created the group “{title}”")
        }
        "invite_members" => format!("{actor} added {}", members(m)),
        "remove_members" => format!("{actor} removed {}", members(m)),
        "join_group_by_link" => format!("{actor} joined the group via invite link"),
        "edit_group_title" => format!("{actor} changed the group name to “{title}”"),
        "edit_group_photo" => format!("{actor} changed the group photo"),
        "delete_group_photo" => format!("{actor} removed the group photo"),
        "pin_message" => format!("{actor} pinned a message"),
        "phone_call" | "group_call" => match m.duration_seconds {
            Some(d) => format!("{actor} called ({d} s)"),
            None => format!("{actor} called"),
        },
        "" => actor,
        other => format!("{actor}: {}", other.replace('_', " ")),
    };
    let extra = flatten_text(&m.text);
    if extra.trim().is_empty() {
        text
    } else {
        format!("{text}: {extra}")
    }
}

fn chat_name(chat: &TgChat) -> String {
    chat.name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| match chat.kind.as_deref() {
            Some("saved_messages") => "Saved Messages".into(),
            _ => format!(
                "Telegram chat {}",
                chat.id.as_ref().and_then(id_string).unwrap_or_default()
            ),
        })
}

/// The conversation id of a chat: `tg:<chat id>`, else from its name.
fn conv_id(chat: &TgChat) -> String {
    match chat.id.as_ref().and_then(id_string) {
        Some(i) => format!("tg:{i}"),
        None => format!("tg:{}", slug(&chat_name(chat))),
    }
}

/// Convert one chat. `me` is the account's `user<id>`, when known.
pub fn convert_chat(chat: &TgChat, me: Option<&str>, url: Option<String>) -> Conversation {
    let kind = chat.kind.as_deref().unwrap_or("");
    let chat_id = chat.id.as_ref().and_then(id_string);
    let name = chat_name(chat);
    let mut conv = Conversation::new(conv_id(chat), name);
    conv.is_group = is_group_type(kind);
    conv.url = url;
    // In a one-to-one chat the chat id is the other person's user id: anyone else is me.
    let other = (kind == "personal_chat" || kind == "bot_chat")
        .then(|| chat_id.as_ref().map(|i| format!("user{i}")))
        .flatten();
    for m in &chat.messages {
        let Some(at) = message_time(m) else { continue };
        let from_id = m
            .from_id
            .as_ref()
            .or(m.actor_id.as_ref())
            .and_then(id_string);
        let is_me = kind == "saved_messages"
            || match (&from_id, me, &other) {
                (Some(f), Some(me), _) => f == me,
                (Some(f), None, Some(o)) => f != o,
                _ => false,
            };
        let author_name = m.from.clone().or_else(|| m.actor.clone());
        let author = if author_name.is_some() || from_id.is_some() || is_me {
            let n = author_name
                .clone()
                .unwrap_or_else(|| from_id.clone().unwrap_or_default());
            Some(conv.participant(&n, from_id.as_deref(), is_me))
        } else {
            None
        };
        let msg = if m.kind.as_deref() == Some("service") {
            Message::system(at, service_text(m))
        } else {
            let mut msg = Message::new(at, author, flatten_text(&m.text));
            msg.attachments.extend(attachment(m));
            msg
        };
        conv.messages.push(msg);
    }
    conv.normalise();
    conv
}

/// `result.json` files under `path`: the file itself, or in the folder and its direct
/// subfolders (`ChatExport_*`, `DataExport_*`), sorted. Inside a folder, only regular files and
/// folders whose real path stays inside the folder count: a symlink leading out of it (or to a
/// FIFO or device) is skipped. A `path` that is itself a symlink is the user's choice and is
/// followed.
fn find_results(path: &Path) -> Result<Vec<PathBuf>> {
    let not_found = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound(format!("Telegram export {} does not exist", path.display()))
        } else {
            Error::Io(e)
        }
    };
    let meta = std::fs::metadata(path).map_err(not_found)?;
    if meta.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !meta.is_dir() {
        return Err(Error::Invalid(format!(
            "Telegram export {} is not a file or folder",
            path.display()
        )));
    }
    let root = std::fs::canonicalize(path).map_err(not_found)?;
    let inside = |p: &Path, dir: bool| match std::fs::canonicalize(p) {
        Ok(real) if real.starts_with(&root) => {
            std::fs::metadata(&real).is_ok_and(|m| if dir { m.is_dir() } else { m.is_file() })
        }
        Ok(_) => {
            tracing::debug!(link = %p.display(), "symlink leaves the export folder; skipped");
            false
        }
        Err(_) => false,
    };
    let mut out = Vec::new();
    let top = path.join("result.json");
    if inside(&top, false) {
        out.push(top);
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| inside(p, true))
        .collect();
    dirs.sort();
    for d in dirs {
        let f = d.join("result.json");
        if inside(&f, false) {
            out.push(f);
        }
    }
    Ok(out)
}

pub fn parse_export(path: &Path) -> Result<Export> {
    let f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if len > MAX_RESULT_JSON_BYTES {
        return Err(Error::Invalid(format!(
            "{} is too large ({len} bytes)",
            path.display()
        )));
    }
    // `take` bounds a file that grows while it is read.
    serde_json::from_reader(BufReader::new(f.take(MAX_RESULT_JSON_BYTES)))
        // Only the position: serde's message can quote values from the file (message text).
        .map_err(|e| {
            Error::Invalid(format!(
                "cannot read {}: invalid JSON ({:?}) at line {} column {}",
                path.display(),
                e.classify(),
                e.line(),
                e.column()
            ))
        })
}

/// The account's `user<id>`, when the export says.
fn export_me(export: &Export) -> Option<String> {
    export
        .personal_information
        .as_ref()
        .and_then(|p| p.user_id.as_ref())
        .and_then(id_string)
        .map(|id| {
            if id.starts_with("user") {
                id
            } else {
                format!("user{id}")
            }
        })
}

/// Every chat of one export (the single-chat shape as one chat).
fn export_chats(export: &mut Export) -> Vec<TgChat> {
    let mut out = Vec::new();
    if !export.messages.is_empty() {
        out.push(TgChat {
            name: export.name.take(),
            kind: export.kind.take(),
            id: export.id.take(),
            messages: std::mem::take(&mut export.messages),
        });
    }
    for list in [export.chats.take(), export.left_chats.take()]
        .into_iter()
        .flatten()
    {
        out.extend(list.list);
    }
    out
}

/// The entries of a full export's `contacts.list` (entries of an unknown shape are skipped).
fn export_contacts(export: &Export) -> Vec<TgContact> {
    export
        .contacts
        .as_ref()
        .and_then(|c| c.get("list"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect()
}

/// Attach phone numbers from `contacts.list` to the participants they belong to (by user id,
/// else by full name). Entries that are not participants are ignored.
fn apply_contacts(conv: &mut Conversation, contacts: &[TgContact]) {
    for c in contacts {
        let Some(phone) = c
            .phone_number
            .as_deref()
            .filter(|p| looks_like_phone(p))
            .map(normalise_phone)
        else {
            continue;
        };
        let handle = c.user_id.as_ref().and_then(id_string).map(|id| {
            if id.starts_with("user") {
                id
            } else {
                format!("user{id}")
            }
        });
        let name = clean_name(
            &[c.first_name.as_deref(), c.last_name.as_deref()]
                .into_iter()
                .flatten()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        );
        for p in conv.participants.iter_mut().filter(|p| !p.me) {
            let by_id = handle.is_some() && p.handle == handle;
            let by_name = handle.is_none() && !name.is_empty() && p.name == name;
            if (by_id || by_name) && !p.phones.contains(&phone) {
                p.phones.push(phone.clone());
            }
        }
    }
}

/// One parsed export file and where it ranks among the others: by modification time, then by
/// position in the (sorted) list. A later rank is a newer export.
pub struct Source {
    pub export: Export,
    pub url: Option<String>,
    pub modified: Option<std::time::SystemTime>,
    pub index: usize,
}

/// A message's identity within its chat: its `id`, else its date, sender and text.
fn message_key(m: &TgMessage) -> String {
    match m.id.as_ref().and_then(id_string) {
        Some(id) => id,
        None => format!(
            "~{}\u{0}{}\u{0}{}\u{0}{}",
            m.date.as_deref().unwrap_or_default(),
            unix_value(m.date_unixtime.as_ref()).unwrap_or_default(),
            m.from_id.as_ref().and_then(id_string).unwrap_or_default(),
            m.text
        ),
    }
}

type FileRank = (Option<std::time::SystemTime>, usize);

struct Group {
    chat: TgChat,
    chat_rank: FileRank,
    me: Option<String>,
    url: Option<String>,
    /// By message key: the newest version seen, with its rank.
    messages: HashMap<String, ((Option<i64>, FileRank), TgMessage)>,
}

/// Every chat of several exports, combined: a chat in several exports becomes one
/// conversation, and a message in several exports (the same chat and message id) appears
/// once, in its newest version: the latest `edited` time, else the newest export.
pub fn combine(sources: Vec<Source>) -> Vec<Conversation> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut contacts = Vec::new();
    for mut src in sources {
        let me = export_me(&src.export);
        contacts.extend(export_contacts(&src.export));
        let rank: FileRank = (src.modified, src.index);
        for mut chat in export_chats(&mut src.export) {
            let messages = std::mem::take(&mut chat.messages);
            let g = groups.entry(conv_id(&chat)).or_insert_with(|| Group {
                chat: chat.clone(),
                chat_rank: rank,
                me: None,
                url: src.url.clone(),
                messages: HashMap::new(),
            });
            if rank >= g.chat_rank {
                // The newest export names the chat.
                g.chat = chat;
                g.chat_rank = rank;
            }
            if g.me.is_none() {
                g.me.clone_from(&me);
            }
            for m in messages {
                let r = (edited_time(&m), rank);
                match g.messages.entry(message_key(&m)) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        if r >= e.get().0 {
                            e.insert((r, m));
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert((r, m));
                    }
                }
            }
        }
    }
    let mut out = Vec::with_capacity(groups.len());
    for (_, g) in groups {
        let mut messages: Vec<TgMessage> = g.messages.into_values().map(|(_, m)| m).collect();
        // Export order (by id) for messages of the same minute.
        messages.sort_by_key(|m| {
            (
                message_time(m),
                m.id.as_ref().and_then(Value::as_i64),
                message_key(m),
            )
        });
        if messages.is_empty() {
            continue;
        }
        let chat = TgChat { messages, ..g.chat };
        let mut conv = convert_chat(&chat, g.me.as_deref(), g.url);
        apply_contacts(&mut conv, &contacts);
        out.push(conv);
    }
    out
}

/// Every chat of one export.
#[cfg(test)]
pub fn conversations(export: Export, url: Option<String>) -> Vec<Conversation> {
    combine(vec![Source {
        export,
        url,
        modified: None,
        index: 0,
    }])
}

pub(crate) struct TelegramReader {
    pub path: PathBuf,
}

impl Reader for TelegramReader {
    fn kind(&self) -> &'static str {
        crate::TELEGRAM_KIND
    }

    fn service(&self) -> &'static str {
        "telegram"
    }

    fn read(&self, cancel: &CancellationToken, found: &mut dyn FnMut(Found) -> bool) -> Result<()> {
        let files = find_results(&self.path)?;
        let mut sources = Vec::with_capacity(files.len());
        for (index, f) in files.into_iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match parse_export(&f) {
                Ok(export) => sources.push(Source {
                    export,
                    url: file_url(&f),
                    modified: std::fs::metadata(&f).and_then(|m| m.modified()).ok(),
                    index,
                }),
                Err(e) => {
                    // Its chats are unknown: keep everything imported so far.
                    tracing::warn!(file = %f.display(), error = %e, "cannot read Telegram export; keeping what was imported");
                    let rel = f
                        .strip_prefix(&self.path)
                        .ok()
                        .filter(|r| !r.as_os_str().is_empty());
                    let rel = rel.unwrap_or(&f).to_string_lossy().replace('\\', "/");
                    let problem = Found::Problem {
                        key: format!("telegram:{rel}"),
                        message: format!(
                            "Couldn't read the Telegram export {rel} ({e}). If Telegram is still \
                             exporting, this clears once it finishes; otherwise export again in \
                             JSON format. Anything imported before is kept."
                        ),
                    };
                    if !found(problem) || !found(Found::Keep(String::new())) {
                        return Ok(());
                    }
                }
            }
        }
        for conv in combine(sources) {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !found(Found::Conversation(conv)) {
                return Ok(());
            }
        }
        Ok(())
    }

    fn watch_target(&self) -> Option<(PathBuf, RecursiveMode)> {
        if self.path.is_file() {
            let parent = self.path.parent()?.to_path_buf();
            Some((parent, RecursiveMode::NonRecursive))
        } else {
            Some((self.path.clone(), RecursiveMode::Recursive))
        }
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Regression (JJ1-12): times were taken from `date_unixtime` and shown in UTC, so local
    /// 09:12 at UTC+2 showed as 07:12 and 00:30 on 1 June was filed under May. The local
    /// `date` is the time the user saw; `date_unixtime` is only a fallback.
    #[test]
    fn dates_are_local_wall_clock_time() {
        // 2026-05-03T09:12:00+02:00 and 2026-06-01T00:30:00+02:00.
        let m: TgMessage = serde_json::from_value(
            json!({"date": "2026-05-03T09:12:00", "date_unixtime": "1777792320"}),
        )
        .unwrap();
        assert_eq!(
            message_time(&m).unwrap(),
            time::macros::datetime!(2026-05-03 09:12 UTC)
        );
        let chat: TgChat = serde_json::from_value(json!({
            "name": "Sam Lee", "type": "personal_chat", "id": 5,
            "messages": [{"id": 1, "type": "message", "date": "2026-06-01T00:30:00",
                          "date_unixtime": "1780266600", "from": "Sam Lee", "from_id": "user5", "text": "late"}]
        }))
        .unwrap();
        let c = convert_chat(&chat, None, None);
        let docs = crate::render::conversation_docs(
            &c,
            crate::render::RenderOptions {
                service: "telegram",
                private: true,
            },
        );
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].key.0, "tg:5/2026-06");
        let nugget_connector::Content::Text(t) = &docs[0].content else {
            panic!()
        };
        assert!(t.contains("**Sam Lee** · 2026-06-01 00:30"), "{t}");

        // Only `date_unixtime`: used as is (UTC).
        let m: TgMessage = serde_json::from_value(json!({"date_unixtime": 1714557600})).unwrap();
        assert_eq!(message_time(&m).unwrap().unix_timestamp(), 1_714_557_600);
        let m: TgMessage =
            serde_json::from_value(json!({"date": "garbage", "date_unixtime": "1714557600"}))
                .unwrap();
        assert_eq!(message_time(&m).unwrap().unix_timestamp(), 1_714_557_600);
    }

    fn source(v: Value, index: usize, secs: u64) -> Source {
        Source {
            export: serde_json::from_value(v).unwrap(),
            url: None,
            modified: Some(std::time::UNIX_EPOCH + Duration::from_secs(secs)),
            index,
        }
    }

    /// Regression (JJ1-17): a message edited between two exports showed twice (old and new
    /// text). Messages are the same by (chat id, message id); the newest version wins.
    #[test]
    fn a_message_edited_between_exports_appears_once_newest_wins() {
        let export = |text: &str, edited: Option<&str>| {
            let mut m = json!({"id": 9, "type": "message", "date": "2026-05-03T09:12:00",
                "from": "Sam Lee", "from_id": "user5", "text": text});
            if let Some(e) = edited {
                m["edited"] = json!(e);
                m["edited_unixtime"] = json!("1777800000");
            }
            json!({"name": "Sam Lee", "type": "personal_chat", "id": 5, "messages": [
                {"id": 8, "type": "message", "date": "2026-05-03T09:10:00", "from": "Sam Lee",
                 "from_id": "user5", "text": "unchanged"},
                m
            ]})
        };
        let old = || export("lands at 18:40", None);
        let new = || export("19:10 now", Some("2026-05-03T11:20:00"));
        let texts = |convs: Vec<Conversation>| -> Vec<String> {
            assert_eq!(convs.len(), 1);
            convs[0].messages.iter().map(|m| m.text.clone()).collect()
        };
        let want = vec!["unchanged".to_owned(), "19:10 now".to_owned()];
        // Either file order or modification time: the edit wins.
        assert_eq!(
            texts(combine(vec![source(old(), 0, 10), source(new(), 1, 20)])),
            want
        );
        assert_eq!(
            texts(combine(vec![source(new(), 0, 10), source(old(), 1, 20)])),
            want
        );
        // Without edit times, the newer file wins; equal times fall back to file order.
        let plain = |t: &str| export(t, None);
        assert_eq!(
            texts(combine(vec![
                source(plain("b"), 0, 20),
                source(plain("a"), 1, 10)
            ]))[1],
            "b"
        );
        assert_eq!(
            texts(combine(vec![
                source(plain("a"), 0, 10),
                source(plain("b"), 1, 10)
            ]))[1],
            "b"
        );
    }

    /// Regression (JJ1-13): `contacts.list` phone numbers were ignored, so Telegram people
    /// could never be matched by phone. Participants get them by user id (or name); entries
    /// that are not participants are ignored.
    #[test]
    fn contact_list_phones_attach_to_participants() {
        let convs = conversations(
            serde_json::from_value(json!({
                "personal_information": {"user_id": 7},
                "contacts": {"list": [
                    {"user_id": 5, "first_name": "Sam", "last_name": "Lee", "phone_number": "+49 151 23456789"},
                    {"first_name": "Bea", "last_name": "", "phone_number": "+44 7700 900123"},
                    {"user_id": 99, "first_name": "Not", "last_name": "Here", "phone_number": "+1 555 0199"},
                    {"user_id": 7, "first_name": "Me", "phone_number": "+1 555 0100"},
                    "odd"
                ]},
                "chats": {"list": [{"name": "Group", "type": "private_group", "id": 1, "messages": [
                    {"id": 1, "type": "message", "date": "2026-05-03T09:10:00", "from": "Sam Lee", "from_id": "user5", "text": "a"},
                    {"id": 2, "type": "message", "date": "2026-05-03T09:11:00", "from": "Bea", "from_id": "user11", "text": "b"},
                    {"id": 3, "type": "message", "date": "2026-05-03T09:12:00", "from": "Me", "from_id": "user7", "text": "c"}
                ]}]}
            }))
            .unwrap(),
            None,
        );
        let ps = &convs[0].participants;
        let get = |n: &str| ps.iter().find(|p| p.name == n).unwrap();
        assert_eq!(get("Sam Lee").phones, vec!["+4915123456789"]);
        assert_eq!(get("Bea").phones, vec!["+447700900123"]);
        assert!(ps.iter().find(|p| p.me).unwrap().phones.is_empty());
        assert_eq!(ps.len(), 3);
    }

    /// Regression (JJ1-15): a symlinked export folder (or `result.json`) leading outside the
    /// chosen folder was imported. Only real paths inside the folder count; a FIFO is skipped;
    /// a chosen path that is itself a symlink is followed.
    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_folder_and_special_files_are_skipped() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("ChatExport_x")).unwrap();
        std::fs::write(outside.join("ChatExport_x/result.json"), "{}").unwrap();
        std::fs::write(outside.join("result.json"), "{}").unwrap();
        let root = tmp.path().join("chosen");
        std::fs::create_dir_all(root.join("ChatExport_in")).unwrap();
        std::fs::create_dir_all(root.join("ChatExport_file")).unwrap();
        std::fs::write(root.join("ChatExport_in/result.json"), "{}").unwrap();
        symlink(outside.join("ChatExport_x"), root.join("ChatExport_link")).unwrap();
        symlink(outside.join("result.json"), root.join("result.json")).unwrap();
        symlink(
            outside.join("result.json"),
            root.join("ChatExport_file/result.json"),
        )
        .unwrap();
        // A link inside the folder is fine.
        symlink(root.join("ChatExport_in"), root.join("ChatExport_alias")).unwrap();
        let fifo = root.join("ChatExport_fifo");
        std::fs::create_dir_all(&fifo).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(fifo.join("result.json"))
            .status()
            .is_ok_and(|s| s.success());

        let found = find_results(&root).unwrap();
        assert_eq!(
            found,
            vec![
                root.join("ChatExport_alias/result.json"),
                root.join("ChatExport_in/result.json"),
            ],
            "fifo made: {made}"
        );
        // The user chose a link: followed.
        let chosen_link = tmp.path().join("link-to-outside");
        symlink(&outside, &chosen_link).unwrap();
        assert_eq!(find_results(&chosen_link).unwrap().len(), 2);
    }

    /// Regression: one message of an unexpected shape failed the whole export (so it was
    /// never imported), and the error quoted values from the file into the log.
    #[test]
    fn odd_messages_are_skipped_and_errors_do_not_quote_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("result.json");
        std::fs::write(
            &p,
            json!({
                "name": "Alice", "type": "personal_chat", "id": 42,
                "messages": [
                    {"id": 1, "type": "message", "date_unixtime": "1714557600", "from": "Alice", "from_id": "user42", "text": "kept"},
                    {"id": 2, "type": "message", "date_unixtime": "1714557660", "from": {"secret": "SECRET TEXT"}, "text": "odd"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let convs = conversations(parse_export(&p).unwrap(), None);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].messages.len(), 1);
        assert_eq!(convs[0].messages[0].text, "kept");

        std::fs::write(&p, r#"{"name": "x", "chats": "SECRET TEXT"}"#).unwrap();
        let err = parse_export(&p).unwrap_err().to_string();
        assert!(!err.contains("SECRET"), "{err}");
        assert!(err.contains("line 1"), "{err}");
    }
}
