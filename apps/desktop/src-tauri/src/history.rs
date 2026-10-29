//! Chat history, kept by the desktop shell in `chat-history.json` under the app's data directory.
//!
//! The daemon has no conversation store, so the shell keeps recent chats itself. The file is
//! plain JSON and **not encrypted** (the UI says so next to "Clear history"); on Unix it is
//! readable by the user only. Everything is bounded: conversations, messages per conversation,
//! bytes per message and the file as a whole. A corrupt file, or a corrupt conversation inside
//! it, is skipped rather than failing the Chat page.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::daemon::CmdError;

/// Most recent conversations kept; older ones are dropped on save.
pub const MAX_CONVERSATIONS: usize = 50;
/// Messages kept per conversation (the newest ones).
pub const MAX_MESSAGES: usize = 200;
/// A save carrying more messages than this is a UI bug, not a long chat.
const MAX_INPUT_MESSAGES: usize = 2_000;
/// Bytes of text per message.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// Upper bound for the whole file; the oldest conversations go first when it is exceeded.
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ID_LEN: usize = 64;
const MAX_TITLE_BYTES: usize = 1024;
const MAX_SHORT_BYTES: usize = 256;
const MAX_ERROR_BYTES: usize = 4 * 1024;
const MAX_CITATIONS: usize = 200;

const FILE_VERSION: u32 = 1;

/// One saved message. Assistant messages carry how the turn ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
    /// Provider that answered (assistant only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// `done`, `error` or `cancelled` (assistant only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Sources the reply cites as `[n]`, when the reply came with any (kept opaque here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<Value>>,
    /// The reply is a general answer: "Use my sources" found no matching passages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub general_answer: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    /// Milliseconds since the Unix epoch, set by the shell on save.
    #[serde(default)]
    pub updated_at: u64,
    pub messages: Vec<StoredMessage>,
}

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    /// Most recent first. Kept as raw values so one bad entry doesn't lose the rest.
    conversations: Vec<Value>,
}

pub fn file_in(data_dir: &Path) -> PathBuf {
    data_dir.join("chat-history.json")
}

/// Saved conversations, most recent first. Missing or unreadable files mean "no history".
pub fn load(path: &Path) -> Vec<Conversation> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read chat history");
            return Vec::new();
        }
    };
    if bytes.len() > MAX_FILE_BYTES * 2 {
        tracing::warn!(path = %path.display(), "ignoring oversized chat history");
        return Vec::new();
    }
    let file: HistoryFile = match serde_json::from_slice(&bytes) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable chat history");
            return Vec::new();
        }
    };
    let mut out: Vec<Conversation> = file
        .conversations
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Conversation>(v).ok())
        .filter(|c| validate(c).is_ok())
        .collect();
    out.truncate(MAX_CONVERSATIONS);
    out
}

/// Check a conversation from the webview before it is written.
pub fn validate(c: &Conversation) -> Result<(), String> {
    if c.id.is_empty()
        || c.id.len() > MAX_ID_LEN
        || !c
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid conversation id".into());
    }
    if c.title.len() > MAX_TITLE_BYTES {
        return Err("conversation title is too long".into());
    }
    if c.messages.len() > MAX_INPUT_MESSAGES {
        return Err("too many messages".into());
    }
    for m in &c.messages {
        if m.role != "user" && m.role != "assistant" {
            return Err("invalid message role".into());
        }
        if m.text.len() > MAX_MESSAGE_BYTES {
            return Err("message is too long to save".into());
        }
        if let Some(s) = &m.state
            && !matches!(s.as_str(), "done" | "error" | "cancelled")
        {
            return Err("invalid message state".into());
        }
        if m.provider
            .as_ref()
            .is_some_and(|p| p.len() > MAX_SHORT_BYTES)
            || m.error.as_ref().is_some_and(|e| e.len() > MAX_ERROR_BYTES)
        {
            return Err("message metadata is too long".into());
        }
        if let Some(cs) = &m.citations
            && (cs.len() > MAX_CITATIONS
                || serde_json::to_vec(cs).map_or(true, |b| b.len() > MAX_MESSAGE_BYTES))
        {
            return Err("too many citations".into());
        }
    }
    Ok(())
}

/// Put `conv` first (replacing any conversation with its id) and apply the caps.
pub fn upsert(mut list: Vec<Conversation>, mut conv: Conversation) -> Vec<Conversation> {
    if conv.messages.len() > MAX_MESSAGES {
        let extra = conv.messages.len() - MAX_MESSAGES;
        conv.messages.drain(..extra);
    }
    list.retain(|c| c.id != conv.id);
    list.insert(0, conv);
    list.truncate(MAX_CONVERSATIONS);
    fit(&mut list);
    list
}

/// Drop the oldest conversations, then the oldest messages of the newest one, until the file
/// fits in `MAX_FILE_BYTES`.
fn fit(list: &mut Vec<Conversation>) {
    let size = |c: &Conversation| serde_json::to_vec(c).map_or(0, |b| b.len() + 1);
    let mut total: usize = 64 + list.iter().map(size).sum::<usize>();
    while total > MAX_FILE_BYTES && list.len() > 1 {
        if let Some(c) = list.pop() {
            total -= size(&c);
        }
    }
    if let Some(first) = list.first_mut() {
        while total > MAX_FILE_BYTES && !first.messages.is_empty() {
            let m = first.messages.remove(0);
            total = total.saturating_sub(serde_json::to_vec(&m).map_or(0, |b| b.len() + 1));
        }
    }
}

/// Write via a temporary file and rename, so a crash never leaves half a file. On Unix the file
/// is created readable and writable by the user only.
pub fn save(path: &Path, list: &[Conversation]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = HistoryFile {
        version: FILE_VERSION,
        conversations: list
            .iter()
            .filter_map(|c| serde_json::to_value(c).ok())
            .collect(),
    };
    let json = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // `mode` applies only on create; tighten a leftover temp file too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)?;
    f.sync_all()
}

pub fn clear(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Serialises read-modify-write cycles between concurrent commands.
static LOCK: Mutex<()> = Mutex::new(());

fn history_path(app: &AppHandle) -> Result<PathBuf, CmdError> {
    app.path()
        .app_data_dir()
        .map(|d| file_in(&d))
        .map_err(|e| CmdError::Other(format!("no app data folder: {e}")))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, CmdError> + Send + 'static,
) -> Result<T, CmdError> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        f()
    })
    .await
    .map_err(|e| CmdError::Other(e.to_string()))?
}

/// Saved conversations, most recent first.
#[tauri::command]
pub async fn chat_history_load(app: AppHandle) -> Result<Vec<Conversation>, CmdError> {
    let path = history_path(&app)?;
    blocking(move || Ok(load(&path))).await
}

/// Save (insert or replace) one conversation; it becomes the most recent.
#[tauri::command]
pub async fn chat_history_save(app: AppHandle, conversation: Conversation) -> Result<(), CmdError> {
    validate(&conversation).map_err(CmdError::Other)?;
    let path = history_path(&app)?;
    blocking(move || {
        let mut conversation = conversation;
        conversation.updated_at = now_ms();
        let list = upsert(load(&path), conversation);
        save(&path, &list).map_err(|e| CmdError::Other(format!("couldn't save chat history: {e}")))
    })
    .await
}

/// Forget one conversation.
#[tauri::command]
pub async fn chat_history_delete(app: AppHandle, id: String) -> Result<(), CmdError> {
    if id.len() > MAX_ID_LEN {
        return Err(CmdError::Other("invalid conversation id".into()));
    }
    let path = history_path(&app)?;
    blocking(move || {
        let mut list = load(&path);
        list.retain(|c| c.id != id);
        save(&path, &list).map_err(|e| CmdError::Other(format!("couldn't save chat history: {e}")))
    })
    .await
}

/// Delete the history file.
#[tauri::command]
pub async fn chat_history_clear(app: AppHandle) -> Result<(), CmdError> {
    let path = history_path(&app)?;
    blocking(move || {
        clear(&path).map_err(|e| CmdError::Other(format!("couldn't clear chat history: {e}")))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "nugget-desktop-history-{tag}-{}-{}",
                std::process::id(),
                now_ms()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn msg(role: &str, text: &str) -> StoredMessage {
        StoredMessage {
            role: role.into(),
            text: text.into(),
            provider: None,
            state: (role == "assistant").then(|| "done".into()),
            error: None,
            output_tokens: None,
            citations: None,
            general_answer: None,
        }
    }

    fn conv(id: &str, n: usize) -> Conversation {
        Conversation {
            id: id.into(),
            title: format!("chat {id}"),
            updated_at: 0,
            messages: (0..n)
                .map(|i| {
                    msg(
                        if i % 2 == 0 { "user" } else { "assistant" },
                        &format!("m{i}"),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn round_trips_most_recent_first() {
        let dir = TempDir::new("rt");
        let path = file_in(&dir.0);
        assert!(load(&path).is_empty());
        let list = upsert(Vec::new(), conv("a", 2));
        let list = upsert(list, conv("b", 2));
        save(&path, &list).unwrap();
        let back = load(&path);
        assert_eq!(
            back.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        // Saving "a" again moves it to the front and replaces it.
        let list = upsert(back, conv("a", 4));
        save(&path, &list).unwrap();
        let back = load(&path);
        assert_eq!(back[0].id, "a");
        assert_eq!(back[0].messages.len(), 4);
        assert_eq!(back.len(), 2);
        clear(&path).unwrap();
        assert!(load(&path).is_empty());
        clear(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("mode");
        let path = file_in(&dir.0);
        save(&path, &[conv("a", 1)]).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn caps_conversations_messages_and_bytes() {
        let mut list = Vec::new();
        for i in 0..(MAX_CONVERSATIONS + 5) {
            list = upsert(list, conv(&format!("c{i}"), 2));
        }
        assert_eq!(list.len(), MAX_CONVERSATIONS);
        assert_eq!(list[0].id, format!("c{}", MAX_CONVERSATIONS + 4));

        let list = upsert(Vec::new(), conv("long", MAX_MESSAGES + 10));
        assert_eq!(list[0].messages.len(), MAX_MESSAGES);
        // The newest messages are kept.
        assert_eq!(
            list[0].messages.last().unwrap().text,
            format!("m{}", MAX_MESSAGES + 9)
        );

        // Big conversations: older ones are dropped to fit the file cap.
        let big = |id: &str| Conversation {
            messages: vec![msg("user", &"x".repeat(MAX_MESSAGE_BYTES - 10)); 6],
            ..conv(id, 0)
        };
        let mut list = Vec::new();
        for i in 0..5 {
            list = upsert(list, big(&format!("b{i}")));
        }
        let size: usize = list
            .iter()
            .map(|c| serde_json::to_vec(c).unwrap().len())
            .sum();
        assert!(size <= MAX_FILE_BYTES, "{size}");
        assert_eq!(list[0].id, "b4");
        assert!(list.len() < 5);

        // A single conversation over the cap loses its oldest messages.
        let huge = Conversation {
            messages: (0..MAX_MESSAGES)
                .map(|i| msg("user", &format!("{i}{}", "y".repeat(MAX_MESSAGE_BYTES / 4))))
                .collect(),
            ..conv("huge", 0)
        };
        let list = upsert(Vec::new(), huge);
        let size = serde_json::to_vec(&list[0]).unwrap().len();
        assert!(size <= MAX_FILE_BYTES);
        assert!(
            list[0]
                .messages
                .last()
                .unwrap()
                .text
                .starts_with(&format!("{}", MAX_MESSAGES - 1))
        );
    }

    #[test]
    fn tolerates_corruption() {
        let dir = TempDir::new("corrupt");
        let path = file_in(&dir.0);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load(&path).is_empty());
        std::fs::write(&path, b"[1,2,3]").unwrap();
        assert!(load(&path).is_empty());
        // One bad conversation doesn't lose the others.
        let good = serde_json::to_value(conv("ok", 2)).unwrap();
        let file = serde_json::json!({
            "version": 1,
            "conversations": [
                { "id": "broken" },
                good,
                { "id": "bad id!", "title": "", "messages": [] },
                { "id": "role", "title": "", "messages": [{ "role": "system", "text": "x" }] },
            ]
        });
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let back = load(&path);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "ok");
        // Saving over a corrupt file works.
        std::fs::write(&path, b"\0\0garbage").unwrap();
        let list = upsert(load(&path), conv("new", 1));
        save(&path, &list).unwrap();
        assert_eq!(load(&path)[0].id, "new");
    }

    #[test]
    fn validates_input() {
        assert!(validate(&conv("a-b_1", 2)).is_ok());
        assert!(validate(&conv("", 0)).is_err());
        assert!(validate(&conv("../x", 0)).is_err());
        assert!(validate(&conv(&"a".repeat(MAX_ID_LEN + 1), 0)).is_err());
        let mut c = conv("a", 1);
        c.messages[0].text = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(validate(&c).is_err());
        let mut c = conv("a", 1);
        c.messages[0].role = "system".into();
        assert!(validate(&c).is_err());
        let mut c = conv("a", 2);
        c.messages[1].state = Some("streaming".into());
        assert!(validate(&c).is_err());
        let mut c = conv("a", 0);
        c.title = "t".repeat(MAX_TITLE_BYTES + 1);
        assert!(validate(&c).is_err());
        assert!(validate(&conv("a", MAX_INPUT_MESSAGES + 1)).is_err());
    }
}
