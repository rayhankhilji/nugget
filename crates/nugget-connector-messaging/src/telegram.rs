//! Telegram Desktop exports (machine-readable JSON, `result.json`).
//!
//! Two shapes: a single-chat export (`{name, type, id, messages}`) and a full-account export
//! (`{personal_information: {user_id}, chats: {list: [...]}, left_chats: {list: [...]}}`).
//! Media files are never read; their names, types and sizes become attachment metadata.

use std::collections::BTreeMap;
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
use crate::model::{Attachment, Conversation, Message, basename, mime_from_name, slug};

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

#[derive(Debug, Default, Deserialize)]
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

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TgMessage {
    pub id: Option<Value>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub date: Option<String>,
    pub date_unixtime: Option<Value>,
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

fn message_time(m: &TgMessage) -> Option<OffsetDateTime> {
    let unix = m.date_unixtime.as_ref().and_then(|v| match v {
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    });
    if let Some(t) = unix.and_then(|u| OffsetDateTime::from_unix_timestamp(u).ok()) {
        return Some(t);
    }
    // Older exports: local time without an offset, read as UTC.
    let d = m.date.as_deref()?;
    PrimitiveDateTime::parse(d.get(..19).unwrap_or(d), LOCAL_DATE)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
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

/// Convert one chat. `me` is the account's `user<id>`, when known.
pub fn convert_chat(chat: &TgChat, me: Option<&str>, url: Option<String>) -> Conversation {
    let kind = chat.kind.as_deref().unwrap_or("");
    let chat_id = chat.id.as_ref().and_then(id_string);
    let name = chat
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| match kind {
            "saved_messages" => "Saved Messages".into(),
            _ => format!("Telegram chat {}", chat_id.as_deref().unwrap_or("")),
        });
    let id = match &chat_id {
        Some(i) => format!("tg:{i}"),
        None => format!("tg:{}", slug(&name)),
    };
    let mut conv = Conversation::new(id, name);
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

fn file_url(path: &Path) -> Option<String> {
    let abs = std::path::absolute(path).ok()?;
    url::Url::from_file_path(abs).ok().map(String::from)
}

/// `result.json` files under `path`: the file itself, or in the folder and its direct
/// subfolders (`ChatExport_*`, `DataExport_*`), sorted.
fn find_results(path: &Path) -> Result<Vec<PathBuf>> {
    let meta = std::fs::metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound(format!("Telegram export {} does not exist", path.display()))
        } else {
            Error::Io(e)
        }
    })?;
    if meta.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut out = Vec::new();
    let top = path.join("result.json");
    if top.is_file() {
        out.push(top);
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let f = d.join("result.json");
        if f.is_file() {
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

/// Every chat of one export.
pub fn conversations(export: Export, url: Option<String>) -> Vec<Conversation> {
    let me = export
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
        });
    let mut out = Vec::new();
    if !export.messages.is_empty() || export.chats.is_none() {
        let chat = TgChat {
            name: export.name,
            kind: export.kind,
            id: export.id,
            messages: export.messages,
        };
        if !chat.messages.is_empty() {
            out.push(convert_chat(&chat, me.as_deref(), url.clone()));
        }
    }
    for list in [export.chats, export.left_chats].into_iter().flatten() {
        for chat in &list.list {
            out.push(convert_chat(chat, me.as_deref(), url.clone()));
        }
    }
    out
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
        let mut convs: BTreeMap<String, Conversation> = BTreeMap::new();
        for f in files {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match parse_export(&f) {
                Ok(export) => {
                    for conv in conversations(export, file_url(&f)) {
                        match convs.get_mut(&conv.id) {
                            Some(existing) => existing.merge(conv),
                            None => {
                                convs.insert(conv.id.clone(), conv);
                            }
                        }
                    }
                }
                Err(e) => {
                    // Its chats are unknown: keep everything imported so far.
                    tracing::warn!(file = %f.display(), error = %e, "cannot read Telegram export; keeping what was imported");
                    if !found(Found::Keep(String::new())) {
                        return Ok(());
                    }
                }
            }
        }
        for (_, conv) in convs {
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

    #[test]
    fn text_entities_flatten() {
        let v = json!(["Hi ", {"type": "bold", "text": "there"}, {"type": "link", "text": "https://example.com"}, 3]);
        assert_eq!(flatten_text(&v), "Hi therehttps://example.com");
        assert_eq!(flatten_text(&json!("plain")), "plain");
        assert_eq!(flatten_text(&Value::Null), "");
    }

    #[test]
    fn dates_prefer_unixtime() {
        let m: TgMessage = serde_json::from_value(
            json!({"date": "2024-05-01T12:00:00", "date_unixtime": "1714557600"}),
        )
        .unwrap();
        assert_eq!(message_time(&m).unwrap().unix_timestamp(), 1_714_557_600);
        let m: TgMessage = serde_json::from_value(json!({"date": "2024-05-01T12:00:00"})).unwrap();
        assert_eq!(
            message_time(&m).unwrap(),
            time::macros::datetime!(2024-05-01 12:00 UTC)
        );
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

    #[test]
    fn personal_chat_infers_me() {
        let chat: TgChat = serde_json::from_value(json!({
            "name": "Alice", "type": "personal_chat", "id": 42,
            "messages": [
                {"id": 1, "type": "message", "date_unixtime": "1714557600", "from": "Alice", "from_id": "user42", "text": "hi"},
                {"id": 2, "type": "message", "date_unixtime": "1714557660", "from": "Tanuj", "from_id": "user7", "text": "",
                 "photo": "photos/photo_1@01-05-2024_12-01-00.jpg", "photo_file_size": 1234},
                {"id": 3, "type": "service", "date_unixtime": "1714557700", "actor": "Alice", "actor_id": "user42", "action": "phone_call", "duration_seconds": 5, "text": ""},
                {"id": 4, "type": "message", "date_unixtime": "1714557800", "from": "Alice", "from_id": "user42", "text": "x", "unknown_field": {"a": 1}}
            ]
        }))
        .unwrap();
        let c = convert_chat(&chat, None, None);
        assert_eq!(c.id, "tg:42");
        assert!(!c.is_group);
        assert_eq!(c.messages.len(), 4);
        assert!(c.participants[c.messages[1].author.unwrap()].me);
        assert!(!c.participants[c.messages[0].author.unwrap()].me);
        let a = &c.messages[1].attachments[0];
        assert_eq!(a.name.as_deref(), Some("photo_1@01-05-2024_12-01-00.jpg"));
        assert_eq!(a.size, Some(1234));
        assert!(c.messages[2].system);
        assert_eq!(c.messages[2].text, "Alice called (5 s)");
    }
}
