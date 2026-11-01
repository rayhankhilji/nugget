//! The normalised chat model every reader produces, and small helpers shared by the readers.

use std::collections::{BTreeMap, HashMap};

use time::OffsetDateTime;

/// A file attached to a message. Metadata only: attachment content is never read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attachment {
    /// File name (basename only), when the export names it.
    pub name: Option<String>,
    pub mime: Option<String>,
    pub size: Option<u64>,
}

/// Someone in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Participant {
    /// Display name (contact name, profile name, or the handle itself).
    pub name: String,
    /// Phone number, e-mail address or service user id.
    pub handle: Option<String>,
    /// The user of this device.
    pub me: bool,
    pub phones: Vec<String>,
    pub emails: Vec<String>,
}

/// One message, already normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub at: OffsetDateTime,
    /// Index into [`Conversation::participants`]; `None` for system lines without an author.
    pub author: Option<usize>,
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// A service line ("Alice added Bob", the encryption notice, ...).
    pub system: bool,
    /// The message was deleted ("This message was deleted").
    pub deleted: bool,
}

impl Message {
    pub fn new(at: OffsetDateTime, author: Option<usize>, text: impl Into<String>) -> Self {
        Self {
            at,
            author,
            text: text.into(),
            attachments: Vec::new(),
            system: false,
            deleted: false,
        }
    }

    pub fn system(at: OffsetDateTime, text: impl Into<String>) -> Self {
        Self {
            system: true,
            ..Self::new(at, None, text)
        }
    }

    fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty() && !self.deleted
    }
}

/// One conversation (thread) with its messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// `im:<guid>`, `wa:<slug>`, `tg:<id>`.
    pub id: String,
    pub name: String,
    pub is_group: bool,
    pub participants: Vec<Participant>,
    pub messages: Vec<Message>,
    /// Where the user can open the original (a `file://` URL of the export).
    pub url: Option<String>,
}

impl Conversation {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            is_group: false,
            participants: Vec::new(),
            messages: Vec::new(),
            url: None,
        }
    }

    /// The participant index for `name` (and handle), adding it when new. Participants are
    /// matched by handle when both have one, else by name.
    pub fn participant(&mut self, name: &str, handle: Option<&str>, me: bool) -> usize {
        let name = clean_name(name);
        let found = self.participants.iter().position(|p| {
            if me {
                return p.me;
            }
            if p.me {
                return false;
            }
            match (handle, p.handle.as_deref()) {
                (Some(h), Some(ph)) => h == ph,
                _ => p.name == name,
            }
        });
        if let Some(i) = found {
            return i;
        }
        let mut p = Participant {
            name,
            handle: handle.map(str::to_owned),
            me,
            ..Default::default()
        };
        if p.name.is_empty() {
            p.name = if me {
                "Me".into()
            } else {
                handle.unwrap_or("Unknown").to_owned()
            };
        }
        if let Some(h) = handle {
            if h.contains('@') {
                p.emails.push(h.to_owned());
            } else if looks_like_phone(h) {
                p.phones.push(h.to_owned());
            }
        }
        self.participants.push(p);
        self.participants.len() - 1
    }

    /// Sort messages by time (stable, so same-instant messages keep their order) and drop empty
    /// ones. Identical messages within one export are kept: two photos (or two "ok"s) sent in
    /// the same minute are distinct messages in a minute-resolution WhatsApp export.
    pub fn normalise(&mut self) {
        self.messages.retain(|m| !m.is_empty());
        self.messages.sort_by_key(|m| m.at);
    }

    /// What makes two messages from overlapping exports "the same message".
    fn dedupe_key(&self, m: &Message) -> MessageKey {
        let who = m.author.and_then(|i| self.participants.get(i)).map(|p| {
            if p.me {
                "\u{0}me".to_owned()
            } else {
                p.name.clone()
            }
        });
        let files = m
            .attachments
            .iter()
            .map(|a| a.name.clone().unwrap_or_default())
            .collect();
        (m.at, who, m.text.clone(), m.system, m.deleted, files)
    }

    /// Merge `other` (the same conversation from another export) into `self`. Overlapping
    /// exports are combined as multisets: a message that occurs `n` times in one export and
    /// `k` times in the other ends up `max(n, k)` times, so the overlap is not duplicated and
    /// repeated identical messages are not collapsed.
    pub fn merge(&mut self, other: Conversation) {
        let mut map = BTreeMap::new();
        for (i, p) in other.participants.iter().enumerate() {
            let j = self.participant(&p.name, p.handle.as_deref(), p.me);
            map.insert(i, j);
        }
        let mut have: HashMap<MessageKey, usize> = HashMap::new();
        for m in &self.messages {
            *have.entry(self.dedupe_key(m)).or_default() += 1;
        }
        for mut m in other.messages {
            m.author = m.author.and_then(|i| map.get(&i).copied());
            let key = self.dedupe_key(&m);
            match have.get_mut(&key) {
                Some(n) if *n > 0 => *n -= 1,
                _ => self.messages.push(m),
            }
        }
        self.is_group |= other.is_group;
        if self.url.is_none() {
            self.url = other.url;
        }
        self.normalise();
    }
}

type MessageKey = (
    OffsetDateTime,
    Option<String>,
    String,
    bool,
    bool,
    Vec<String>,
);

/// A display name on one line: control characters and line breaks become spaces, runs of
/// whitespace collapse, and very long names are cut.
pub fn clean_name(s: &str) -> String {
    truncate_chars(&one_line(s), 200)
}

/// `s` on one line: control characters and line breaks become spaces, runs of whitespace
/// collapse, invisible format characters are dropped. Unlike [`clean_name`], never cut.
pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if is_invisible(c) {
            continue;
        }
        if c.is_whitespace() || c.is_control() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// Bidi marks, zero-width no-break space (BOM) and similar format characters exports sprinkle
/// around names and timestamps.
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

/// Phone-number-like: only digits, spaces and `+-().`, with at least 5 digits.
pub fn looks_like_phone(s: &str) -> bool {
    let digits = s.chars().filter(char::is_ascii_digit).count();
    digits >= 5
        && s.chars()
            .all(|c| c.is_ascii_digit() || " +-().\u{a0}".contains(c))
}

/// `+15551234567` / `15551234567`: digits only, keeping a leading `+`.
pub fn normalise_phone(s: &str) -> String {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if s.trim_start().starts_with('+') {
        format!("+{digits}")
    } else {
        digits
    }
}

/// The last ten digits of a phone number (enough to match national and international forms).
pub fn phone_key(s: &str) -> Option<String> {
    let digits: Vec<char> = s.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 5 {
        return None;
    }
    let start = digits.len().saturating_sub(10);
    Some(digits[start..].iter().collect())
}

/// A key-safe slug: lower-case letters and digits (any script), other runs become `-`.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.extend(c.to_lowercase());
        } else {
            dash = true;
        }
    }
    let out = truncate_chars(&out, 80);
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        // Names of only symbols / emoji: a short stable hash keeps them apart.
        let h = crate::render::sha256_hex(s.as_bytes());
        format!("chat-{}", &h[..12])
    } else {
        out
    }
}

/// MIME type from a file extension (common chat media only).
pub fn mime_from_name(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "tif" | "tiff" => "image/tiff",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "3gp" => "video/3gpp",
        "webm" => "video/webm",
        "opus" => "audio/opus",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "aac" => "audio/aac",
        "amr" => "audio/amr",
        "caf" => "audio/x-caf",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "txt" => "text/plain",
        "vcf" => "text/vcard",
        "zip" => "application/zip",
        _ => return None,
    })
}

/// The basename of a `/`- or `\`-separated path.
/// `file://` URL of `path` (made absolute).
pub fn file_url(path: &std::path::Path) -> Option<String> {
    let abs = std::path::absolute(path).ok()?;
    url::Url::from_file_path(abs).ok().map(String::from)
}

pub fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn slugs() {
        assert_eq!(slug("Family Group!"), "family-group");
        assert_eq!(slug("  Alice & Bob  "), "alice-bob");
        assert_eq!(slug("日本語 チャット"), "日本語-チャット");
        assert!(slug("👩‍👩‍👧").starts_with("chat-"));
        assert_eq!(slug("👩‍👩‍👧"), slug("👩‍👩‍👧"));
    }

    #[test]
    fn phones() {
        assert!(looks_like_phone("+1 (555) 123-4567"));
        assert!(!looks_like_phone("Alice 5551234"));
        assert_eq!(normalise_phone("+1 (555) 123-4567"), "+15551234567");
        assert_eq!(phone_key("+1 (555) 123-4567").unwrap(), "5551234567");
        assert_eq!(phone_key("(555) 123-4567").unwrap(), "5551234567");
    }

    /// Regression: identical messages in one export (two photos in the same minute) were
    /// collapsed into one; overlapping exports must still not duplicate them.
    #[test]
    fn repeated_identical_messages_survive_normalise_and_merge() {
        let at = datetime!(2024-05-01 9:00 UTC);
        let export = || {
            let mut c = Conversation::new("wa:x", "X");
            let alice = c.participant("Alice", None, false);
            for _ in 0..3 {
                let mut m = Message::new(at, Some(alice), "");
                m.attachments.push(Attachment::default());
                c.messages.push(m);
            }
            c.messages.push(Message::new(at, Some(alice), "ok"));
            c.messages.push(Message::new(at, Some(alice), "ok"));
            c.normalise();
            c
        };
        let mut a = export();
        assert_eq!(a.messages.len(), 5);
        a.merge(export());
        assert_eq!(a.messages.len(), 5, "the overlap is not duplicated");
        let mut longer = export();
        longer
            .messages
            .push(Message::new(at, longer.messages[0].author, "ok"));
        a.merge(longer);
        assert_eq!(a.messages.len(), 6, "max of the two counts");
    }
}
