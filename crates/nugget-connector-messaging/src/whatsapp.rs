//! WhatsApp "Export chat" files (`.txt`, or the `.zip` with media WhatsApp shares).
//!
//! The text format differs per phone OS and locale:
//!
//! ```text
//! [31/12/2023, 21:15:42] Alice: text          iOS, 24 h
//! [12/31/23, 9:15:42 PM] Alice: text          iOS, 12 h
//! 31/12/2023, 21:15 - Alice: text             Android
//! 12/31/23, 9:15 PM - Alice: text             Android, US
//! 31.12.23, 21:15 - Alice: text               Android, German
//! 2023-12-31, 21:15 - Alice: text             Android, ISO
//! 2023. 12. 31. 오후 9:15 - Alice: text        Android, Korean
//! ```
//!
//! Lines that do not start with a timestamp continue the previous message. A timestamp line
//! without `Name: ` is a system line (the encryption notice, "Alice added Bob"); iOS writes
//! system lines with an author and a leading U+200E mark instead. Day/month order is inferred
//! per file. Timestamps carry no zone; they are read as UTC (what the user saw on the phone).

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_full::notify::RecursiveMode;
use nugget_connector::CancellationToken;
use nugget_core::{Error, Result};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use crate::connector::{Found, Reader};
use crate::model::{
    Attachment, Conversation, Message, basename, clean_name, is_invisible, looks_like_phone,
    mime_from_name, normalise_phone, slug,
};

/// The chat text inside an export is never read beyond this size.
pub const MAX_CHAT_TEXT_BYTES: u64 = 256 * 1024 * 1024;
/// Archives with more entries than this are refused.
const MAX_ZIP_ENTRIES: usize = 200_000;

pub(crate) struct WhatsAppReader {
    pub folder: PathBuf,
    pub me: Option<String>,
}

/// An export file found in the folder.
#[derive(Debug, Clone)]
struct ExportFile {
    path: PathBuf,
    name: String,
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.') || name.starts_with("~$")
}

fn ext_is(name: &str, ext: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
}

fn list_dir(dir: &Path) -> std::io::Result<Vec<(PathBuf, String, bool)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if is_hidden(&name) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let is_dir = if ft.is_symlink() {
            std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir())
        } else {
            ft.is_dir()
        };
        out.push((e.path(), name, is_dir));
    }
    out.sort();
    Ok(out)
}

/// `.zip` / `.txt` files in `folder` and its direct subfolders, sorted by path.
fn find_exports(folder: &Path) -> Result<Vec<ExportFile>> {
    let top = list_dir(folder).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound(format!(
                "WhatsApp export folder {} does not exist",
                folder.display()
            ))
        } else {
            Error::Io(e)
        }
    })?;
    let mut out = Vec::new();
    let mut take = |path: PathBuf, name: &str, parent: Option<&str>| {
        if !(ext_is(name, "zip") || ext_is(name, "txt")) {
            return;
        }
        let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
        // An unzipped iOS export is a folder holding `_chat.txt`: the folder names the chat.
        let chat = match parent {
            Some(p) if stem.eq_ignore_ascii_case("_chat") => p,
            _ => stem,
        };
        out.push(ExportFile {
            path,
            name: chat_name(chat),
        });
    };
    for (path, name, is_dir) in top {
        if is_dir {
            match list_dir(&path) {
                Ok(children) => {
                    for (p, n, d) in children {
                        if !d {
                            take(p, &n, Some(&name));
                        }
                    }
                }
                Err(e) => tracing::warn!(dir = %path.display(), error = %e, "cannot list folder"),
            }
        } else {
            take(path, &name, None);
        }
    }
    Ok(out)
}

/// The chat name from an export's file name (`WhatsApp Chat with Alice (1)` → `Alice`).
pub fn chat_name(stem: &str) -> String {
    let mut s = clean_name(stem);
    // ` (1)`, `(2)`: duplicate downloads.
    loop {
        let t = s.trim_end();
        let Some(open) = t.rfind('(') else { break };
        let inner = &t[open + 1..];
        if t.ends_with(')')
            && (2..=3).contains(&inner.len())
            && inner[..inner.len() - 1].chars().all(|c| c.is_ascii_digit())
            && open > 0
        {
            s = t[..open].trim_end().to_owned();
        } else {
            break;
        }
    }
    const PREFIXES: &[&str] = &[
        "WhatsApp Chat with ",
        "WhatsApp Chat - ",
        "WhatsApp Chat mit ",
        "WhatsApp-Chat mit ",
        "Chat de WhatsApp con ",
        "Chat de WhatsApp com ",
        "Conversa do WhatsApp com ",
        "Conversa de WhatsApp com ",
        "Discussion WhatsApp avec ",
        "Chat WhatsApp con ",
        "Chat di WhatsApp con ",
        "WhatsApp-chat met ",
        "WhatsApp Chat met ",
        "WhatsApp-chatt med ",
        "WhatsApp Chat med ",
        "WhatsApp-keskustelu: ",
        "Czat WhatsApp z ",
        "WhatsApp Sohbeti: ",
        "WhatsApp Chat: ",
    ];
    for p in PREFIXES {
        if s.len() > p.len() && s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)) {
            let rest = s[p.len()..].trim();
            if !rest.is_empty() {
                return rest.to_owned();
            }
        }
    }
    if s.is_empty() {
        "WhatsApp chat".into()
    } else {
        s
    }
}

// ---------------------------------------------------------------------------------------------
// Reading files

fn decode(bytes: &[u8]) -> String {
    let utf16 = |be: bool| {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if be {
                    u16::from_be_bytes(*c)
                } else {
                    u16::from_le_bytes(*c)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn read_bounded(r: impl Read, declared: u64, what: &str) -> Result<Vec<u8>> {
    if declared > MAX_CHAT_TEXT_BYTES {
        return Err(Error::Invalid(format!(
            "{what} is too large ({declared} bytes)"
        )));
    }
    let mut buf = Vec::with_capacity(declared.min(16 * 1024 * 1024) as usize);
    r.take(MAX_CHAT_TEXT_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_CHAT_TEXT_BYTES {
        return Err(Error::Invalid(format!("{what} is too large")));
    }
    Ok(buf)
}

/// The chat text of an export and the sizes of the media files packed with it.
struct ExportText {
    text: String,
    media: HashMap<String, u64>,
}

fn zip_err(path: &Path, e: zip::result::ZipError) -> Error {
    Error::Invalid(format!("cannot read {}: {e}", path.display()))
}

fn read_zip(path: &Path) -> Result<Option<ExportText>> {
    let f = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| zip_err(path, e))?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(Error::Invalid(format!(
            "{} has too many entries",
            path.display()
        )));
    }
    let mut media = HashMap::new();
    let mut texts: Vec<(usize, String)> = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(|e| zip_err(path, e))?;
        if entry.is_dir() {
            continue;
        }
        let Ok(name) = entry.name() else { continue };
        let name = name.into_owned();
        let base = basename(&name).to_owned();
        if ext_is(&base, "txt") && !name.trim_start_matches('/').contains('/') {
            texts.push((i, base));
        } else {
            media.insert(base, entry.size());
        }
    }
    let pick = texts
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case("_chat.txt"))
        .or_else(|| {
            if texts.len() == 1 {
                texts.first()
            } else {
                texts
                    .iter()
                    .find(|(_, n)| n.to_ascii_lowercase().starts_with("whatsapp"))
            }
        });
    let Some((index, _)) = pick else {
        return Ok(None);
    };
    let entry = zip.by_index(*index).map_err(|e| zip_err(path, e))?;
    let declared = entry.size();
    let bytes = read_bounded(entry, declared, &format!("chat text in {}", path.display()))?;
    Ok(Some(ExportText {
        text: decode(&bytes),
        media,
    }))
}

fn read_export(path: &Path) -> Result<Option<ExportText>> {
    if ext_is(&path.to_string_lossy(), "zip") {
        return read_zip(path);
    }
    let f = std::fs::File::open(path)?;
    let declared = f.metadata()?.len();
    let bytes = read_bounded(f, declared, &path.display().to_string())?;
    Ok(Some(ExportText {
        text: decode(&bytes),
        media: HashMap::new(),
    }))
}

// ---------------------------------------------------------------------------------------------
// Timestamps

/// A timestamp as written, before the day/month order is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub a: u32,
    pub b: u32,
    pub c: u32,
    /// The first field is a four-digit year (`2023-12-31`, `2023. 12. 31.`).
    pub year_first: bool,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// `Some(true)` for PM, `Some(false)` for AM, `None` for a 24-hour clock.
    pub pm: Option<bool>,
}

/// AM/PM markers, longest first so `a.m.` wins over `am`.
const AM: &[&str] = &[
    "a. m.", "a.m.", "vorm.", "π.μ.", "午前", "上午", "오전", "am", "ص",
];
const PM: &[&str] = &[
    "p. m.", "p.m.", "nachm.", "μ.μ.", "午後", "下午", "오후", "pm", "م",
];

fn take_digits(s: &str, max: usize) -> Option<(u32, usize, &str)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || n > max {
        return None;
    }
    Some((s[..n].parse().ok()?, n, &s[n..]))
}

fn strip_marker(t: &str) -> (String, Option<bool>) {
    let lower = t.to_lowercase();
    for (list, pm) in [(AM, false), (PM, true)] {
        for m in list {
            if let Some(rest) = lower.strip_prefix(m) {
                return (rest.trim().to_owned(), Some(pm));
            }
            if let Some(rest) = lower.strip_suffix(m) {
                return (rest.trim().to_owned(), Some(pm));
            }
        }
    }
    (lower, None)
}

/// Parse `31/12/2023, 21:15:42`, `12/31/23, 9:15 PM`, `2023. 12. 31. 오후 9:15` ...
pub fn parse_stamp(raw: &str) -> Option<Stamp> {
    let s: String = raw
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let s = s.trim();
    let (a, ad, rest) = take_digits(s, 4)?;
    let sep = rest
        .chars()
        .next()
        .filter(|c| matches!(c, '/' | '.' | '-'))?;
    let rest = rest[1..].trim_start();
    let (b, _, rest) = take_digits(rest, 2)?;
    let rest = rest.strip_prefix(sep)?.trim_start();
    let (c, cd, rest) = take_digits(rest, 4)?;
    let shape_ok = if ad == 4 {
        cd <= 2
    } else {
        ad <= 2 && (cd == 2 || cd == 4)
    };
    if !shape_ok {
        return None;
    }
    let rest = rest.strip_prefix('.').unwrap_or(rest);
    let time_part = rest.trim_start_matches([',', ' ']);
    if time_part.len() == rest.len() {
        return None; // no separator between date and time
    }
    let (time_s, pm) = strip_marker(time_part.trim());
    let mut fields = time_s.split([':', '.']);
    let hour: u32 = fields.next()?.trim().parse().ok()?;
    let minute: u32 = fields.next()?.trim().parse().ok()?;
    let second: u32 = match fields.next() {
        Some(f) => f.trim().parse().ok()?,
        None => 0,
    };
    if fields.next().is_some() || minute > 59 || second > 59 {
        return None;
    }
    match pm {
        Some(_) if !(1..=12).contains(&hour) => return None,
        None if hour > 23 => return None,
        _ => {}
    }
    Some(Stamp {
        a,
        b,
        c,
        year_first: ad == 4,
        hour,
        minute,
        second,
        pm,
    })
}

impl Stamp {
    /// The instant, reading day-first (`31/12`) or month-first (`12/31`).
    pub fn to_datetime(self, day_first: bool) -> Option<OffsetDateTime> {
        let (y, m, d) = if self.year_first {
            (self.a, self.b, self.c)
        } else if day_first {
            (self.c, self.b, self.a)
        } else {
            (self.c, self.a, self.b)
        };
        let y = if y < 100 { 2000 + y } else { y };
        let hour = match self.pm {
            Some(true) if self.hour < 12 => self.hour + 12,
            Some(false) if self.hour == 12 => 0,
            _ => self.hour,
        };
        let date = Date::from_calendar_date(
            i32::try_from(y).ok()?,
            Month::try_from(u8::try_from(m).ok()?).ok()?,
            u8::try_from(d).ok()?,
        )
        .ok()?;
        let time = Time::from_hms(
            u8::try_from(hour).ok()?,
            u8::try_from(self.minute).ok()?,
            u8::try_from(self.second).ok()?,
        )
        .ok()?;
        Some(PrimitiveDateTime::new(date, time).assume_utc())
    }
}

/// Day-first or month-first for a whole file: a first field above 12 means day-first, a second
/// above 12 month-first; otherwise 12-hour clocks suggest month-first (US) and 24-hour clocks
/// day-first.
pub fn infer_day_first(stamps: &[Stamp]) -> bool {
    let dm: Vec<&Stamp> = stamps.iter().filter(|s| !s.year_first).collect();
    let first = dm.iter().filter(|s| s.a > 12).count();
    let second = dm.iter().filter(|s| s.b > 12).count();
    if first > 0 || second > 0 {
        return first >= second;
    }
    !stamps.iter().any(|s| s.pm.is_some())
}

// ---------------------------------------------------------------------------------------------
// Lines

/// One message header: timestamp, the rest of the line, and whether it used `[...]` (iOS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header<'a> {
    pub stamp: Stamp,
    pub rest: &'a str,
    pub ios: bool,
}

/// Split a line into timestamp and remainder when it starts a message.
pub fn parse_header(line: &str) -> Option<Header<'_>> {
    let line = line.trim_start_matches(|c: char| is_invisible(c) || c == ' ');
    if let Some(inner) = line.strip_prefix('[') {
        let end = inner.find(']')?;
        if end > 48 {
            return None;
        }
        let stamp = parse_stamp(&inner[..end])?;
        let rest = inner[end + 1..]
            .strip_prefix(' ')
            .unwrap_or(&inner[end + 1..]);
        return Some(Header {
            stamp,
            rest,
            ios: true,
        });
    }
    let window = &line[..floor_boundary(line, 64)];
    let (i, sep_len) = [" - ", " – ", " — "]
        .iter()
        .filter_map(|sep| window.find(sep).map(|i| (i, sep.len())))
        .min()?;
    let stamp = parse_stamp(&line[..i])?;
    Some(Header {
        stamp,
        rest: &line[i + sep_len..],
        ios: false,
    })
}

fn floor_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `t` without `suffix` (given in lower case), compared case-insensitively.
fn strip_suffix_ci<'a>(t: &'a str, suffix: &str) -> Option<&'a str> {
    let n = suffix.len();
    if t.len() < n || !t.is_char_boundary(t.len() - n) {
        return None;
    }
    let (head, tail) = t.split_at(t.len() - n);
    (tail.to_lowercase() == suffix).then_some(head)
}

fn strip_invisible(s: &str) -> String {
    s.chars().filter(|c| !is_invisible(*c)).collect()
}

/// What a message's first line turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    Attachment(Attachment),
    Deleted,
}

const DELETED: &[&str] = &[
    "this message was deleted",
    "you deleted this message",
    "diese nachricht wurde gelöscht",
    "du hast diese nachricht gelöscht",
    "se eliminó este mensaje",
    "eliminaste este mensaje",
    "ce message a été supprimé",
    "vous avez supprimé ce message",
    "mensagem apagada",
    "esta mensagem foi apagada",
    "você apagou esta mensagem",
    "questo messaggio è stato eliminato",
    "hai eliminato questo messaggio",
    "dit bericht is verwijderd",
];

const OMITTED: &[&str] = &[
    "omitted",
    "ausgelassen",
    "weggelassen",
    "omitido",
    "omitida",
    "omis",
    "absente",
    "weggelaten",
    "oculta",
    "ocultado",
    "omessi",
    "omesso",
];

/// iOS `image omitted` lines (also localized): (phrase, mime).
const IOS_OMITTED: &[(&str, &str)] = &[
    ("image omitted", "image/jpeg"),
    ("photo omitted", "image/jpeg"),
    ("video omitted", "video/mp4"),
    ("audio omitted", "audio/mp4"),
    ("sticker omitted", "image/webp"),
    ("gif omitted", "image/gif"),
    ("contact card omitted", "text/vcard"),
    ("document omitted", "application/octet-stream"),
    ("imagen omitida", "image/jpeg"),
    ("video omitido", "video/mp4"),
    ("audio omitido", "audio/mp4"),
    ("sticker omitido", "image/webp"),
    ("documento omitido", "application/octet-stream"),
    ("bild weggelassen", "image/jpeg"),
    ("video weggelassen", "video/mp4"),
    ("audio weggelassen", "audio/mp4"),
    ("sticker weggelassen", "image/webp"),
    ("dokument weggelassen", "application/octet-stream"),
    ("image absente", "image/jpeg"),
    ("vidéo absente", "video/mp4"),
    ("audio omis", "audio/mp4"),
    ("document omis", "application/octet-stream"),
    ("imagem ocultada", "image/jpeg"),
    ("vídeo ocultado", "video/mp4"),
    ("áudio ocultado", "audio/mp4"),
];

/// `IMG-....jpg (file attached)` on Android, localized.
const FILE_ATTACHED: &[&str] = &[
    "(file attached)",
    "(datei angehängt)",
    "(archivo adjunto)",
    "(fichier joint)",
    "(arquivo anexado)",
    "(bestand bijgevoegd)",
    "(file allegato)",
    "(fil bifogad)",
    "(plik załączony)",
    "(dosya ekli)",
];

fn named(name: &str) -> Attachment {
    let name = basename(name.trim()).to_owned();
    Attachment {
        mime: mime_from_name(&name).map(str::to_owned),
        name: Some(name),
        size: None,
    }
}

/// Recognise media placeholders and deletion notices on a message's first line.
pub fn classify_media(line: &str) -> Option<Media> {
    let t = strip_invisible(line);
    let t = t.trim();
    if t.is_empty() || t.len() > 300 {
        return None;
    }
    let lower = t.to_lowercase();
    let bare = lower
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim_end_matches('.')
        .trim();
    if DELETED.contains(&bare) {
        return Some(Media::Deleted);
    }
    // `<attached: 00000012-PHOTO-2023-12-31-21-15-42.jpg>`
    if t.starts_with('<') && t.ends_with('>') {
        let inner = &t[1..t.len() - 1];
        if let Some((label, name)) = inner.split_once(':')
            && label.chars().count() <= 24
            && !name.trim().is_empty()
            && name.contains('.')
        {
            return Some(Media::Attachment(named(name)));
        }
        let words = inner.to_lowercase();
        if words
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| OMITTED.contains(&w))
        {
            return Some(Media::Attachment(Attachment::default()));
        }
        return None;
    }
    for (phrase, mime) in IOS_OMITTED {
        if lower == *phrase {
            return Some(Media::Attachment(Attachment {
                name: None,
                mime: Some((*mime).to_owned()),
                size: None,
            }));
        }
        // `Report.pdf • 3 pages document omitted`
        if (phrase.contains("document") || phrase.contains("dokument"))
            && let Some(head) = strip_suffix_ci(t, phrase)
        {
            let name = head.trim();
            let name = name.split(" • ").next().unwrap_or(name).trim();
            if !name.is_empty() {
                return Some(Media::Attachment(named(name)));
            }
        }
    }
    for marker in FILE_ATTACHED {
        if let Some(head) = strip_suffix_ci(t, marker) {
            let name = head.trim();
            if !name.is_empty() && name.contains('.') {
                return Some(Media::Attachment(named(name)));
            }
        }
    }
    None
}

const GROUP_HINTS: &[&str] = &[
    "created group",
    "created this group",
    "created the group",
    "changed the group",
    "changed this group",
    "changed the subject",
    "group's",
    " added ",
    "joined using",
    "hat die gruppe",
    "creó el grupo",
    "a créé le groupe",
    "criou o grupo",
];

fn valid_author(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n.chars().count() <= 80 && !n.contains(['"', '“', '”', '\u{201E}'])
}

struct Entry<'a> {
    stamp: Stamp,
    rest: &'a str,
    ios: bool,
    more: Vec<&'a str>,
}

/// Parse one export's text into a conversation (`None` when it holds no WhatsApp messages).
pub fn parse_chat(
    text: &str,
    id: &str,
    name: &str,
    me: Option<&str>,
    media_sizes: &HashMap<String, u64>,
) -> Option<Conversation> {
    let mut entries: Vec<Entry<'_>> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match parse_header(line) {
            Some(h) => entries.push(Entry {
                stamp: h.stamp,
                rest: h.rest,
                ios: h.ios,
                more: Vec::new(),
            }),
            None => {
                if let Some(last) = entries.last_mut() {
                    last.more.push(line);
                }
            }
        }
    }
    if entries.is_empty() {
        return None;
    }
    let stamps: Vec<Stamp> = entries.iter().map(|e| e.stamp).collect();
    let day_first = infer_day_first(&stamps);
    let me_norm = me.map(|m| clean_name(m).to_lowercase());

    let mut conv = Conversation::new(id, name);
    let mut group_hint = false;
    let mut last: Option<usize> = None;
    for e in entries {
        let Some(at) = e.stamp.to_datetime(day_first) else {
            // Not a real date after all: keep the line as text of the previous message.
            if let Some(i) = last {
                let m = &mut conv.messages[i];
                m.text.push('\n');
                m.text.push_str(&strip_invisible(e.rest));
                for l in e.more {
                    m.text.push('\n');
                    m.text.push_str(l);
                }
            }
            continue;
        };
        let rest = e.rest;
        let split = rest
            .split_once(": ")
            .or_else(|| rest.strip_suffix(':').map(|n| (n, "")));
        let (author, first) = match split {
            Some((n, t)) if valid_author(&strip_invisible(n)) => (Some(strip_invisible(n)), t),
            _ => (None, rest),
        };
        let body_more = e.more.iter().map(|l| strip_invisible(l));
        let mut msg = match author {
            None => {
                let mut text = strip_invisible(first);
                for l in body_more {
                    text.push('\n');
                    text.push_str(&l);
                }
                Message::system(at, text.trim().to_owned())
            }
            Some(author) => {
                let author = clean_name(&author);
                let lrm_system = e.ios && first.starts_with('\u{200E}');
                let media = classify_media(first);
                let mut msg = if lrm_system && media.is_none() {
                    let mut m = Message::system(at, String::new());
                    if author != clean_name(name) {
                        let is_me = me_norm.as_deref() == Some(author.to_lowercase().as_str());
                        let handle = looks_like_phone(&author).then(|| normalise_phone(&author));
                        m.author = Some(conv.participant(&author, handle.as_deref(), is_me));
                    }
                    m
                } else {
                    let is_me = me_norm.as_deref() == Some(author.to_lowercase().as_str());
                    let handle = looks_like_phone(&author).then(|| normalise_phone(&author));
                    let idx = conv.participant(&author, handle.as_deref(), is_me);
                    Message::new(at, Some(idx), String::new())
                };
                let mut lines: Vec<String> = Vec::new();
                match media {
                    Some(Media::Attachment(mut a)) => {
                        if let Some(n) = &a.name {
                            a.size = media_sizes.get(n).copied();
                        }
                        msg.attachments.push(a);
                    }
                    Some(Media::Deleted) => msg.deleted = true,
                    None => lines.push(strip_invisible(first)),
                }
                lines.extend(body_more);
                msg.text = lines.join("\n").trim_end().to_owned();
                msg
            }
        };
        if msg.system {
            let lower = msg.text.to_lowercase();
            group_hint |= GROUP_HINTS.iter().any(|h| lower.contains(h));
            msg.text = msg.text.trim().to_owned();
        }
        conv.messages.push(msg);
        last = Some(conv.messages.len() - 1);
    }
    let authors: std::collections::BTreeSet<usize> = conv
        .messages
        .iter()
        .filter(|m| !m.system)
        .filter_map(|m| m.author)
        .filter(|i| !conv.participants[*i].me)
        .collect();
    conv.is_group = group_hint || authors.len() > 2;
    conv.normalise();
    Some(conv)
}

fn file_url(path: &Path) -> Option<String> {
    let abs = std::path::absolute(path).ok()?;
    url::Url::from_file_path(abs).ok().map(String::from)
}

impl Reader for WhatsAppReader {
    fn kind(&self) -> &'static str {
        crate::WHATSAPP_KIND
    }

    fn service(&self) -> &'static str {
        "whatsapp"
    }

    fn read(&self, cancel: &CancellationToken, found: &mut dyn FnMut(Found) -> bool) -> Result<()> {
        let files = find_exports(&self.folder)?;
        let mut convs: BTreeMap<String, Conversation> = BTreeMap::new();
        for f in files {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let id = format!("wa:{}", slug(&f.name));
            let parsed = read_export(&f.path).map(|t| {
                t.and_then(|t| parse_chat(&t.text, &id, &f.name, self.me.as_deref(), &t.media))
            });
            match parsed {
                Ok(Some(mut conv)) => {
                    conv.url = file_url(&f.path);
                    match convs.get_mut(&id) {
                        Some(existing) => existing.merge(conv),
                        None => {
                            convs.insert(id, conv);
                        }
                    }
                }
                Ok(None) => {
                    tracing::debug!(file = %f.path.display(), "not a WhatsApp export; skipped");
                }
                Err(e) => {
                    tracing::warn!(file = %f.path.display(), error = %e, "cannot read WhatsApp export; keeping what was imported");
                    if !found(Found::Keep(format!("{id}/"))) {
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
        Some((self.folder.clone(), RecursiveMode::Recursive))
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn dt(line: &str, day_first: bool) -> OffsetDateTime {
        parse_header(line)
            .unwrap()
            .stamp
            .to_datetime(day_first)
            .unwrap()
    }

    #[test]
    fn header_variants() {
        let want = datetime!(2023-12-31 21:15 UTC);
        assert_eq!(
            dt("[31/12/2023, 21:15:42] Alice: hi", true),
            want + time::Duration::seconds(42)
        );
        assert_eq!(
            dt("[12/31/23, 9:15:42 PM] Alice: hi", false),
            want + time::Duration::seconds(42)
        );
        assert_eq!(dt("31/12/2023, 21:15 - Alice: hi", true), want);
        assert_eq!(dt("12/31/23, 9:15 PM - Alice: hi", false), want);
        assert_eq!(dt("31.12.23, 21:15 - Alice: hi", true), want);
        assert_eq!(dt("2023-12-31, 21:15 - Alice: hi", true), want);
        assert_eq!(dt("2023. 12. 31. 오후 9:15 - Alice: hi", true), want);
        assert_eq!(dt("2023/12/31 午後9:15 - Alice: hi", true), want);
        assert_eq!(dt("2023/12/31 下午9:15 - Alice: hi", true), want);
        assert_eq!(dt("31/12/2023, 9:15 p.m. - Alice: hi", true), want);
        assert_eq!(dt("31/12/2023, 9:15\u{202f}p. m. - Alice: hi", true), want);
        assert_eq!(
            dt("\u{feff}\u{200e}[31.12.23, 21:15:00] Alice: hi", true),
            want
        );
        assert_eq!(
            dt("12/31/23, 12:05\u{a0}AM - Alice: hi", false),
            datetime!(2023-12-31 0:05 UTC)
        );
        assert_eq!(
            dt("12/31/23, 12:05 pm - Alice: hi", false),
            datetime!(2023-12-31 12:05 UTC)
        );
        assert_eq!(
            parse_header("[31/12/2023, 21:15:42] Alice: hi")
                .unwrap()
                .rest,
            "Alice: hi"
        );
        assert!(parse_header("31/12/2023, 21:15 - ").is_some());
    }

    #[test]
    fn non_headers() {
        for l in [
            "just text",
            "see you at 9:15 - ok",
            "1/2 - half",
            "31/12/2023 - no time",
            "[note] Alice: x",
            "12/31/23, 25:15 - Alice: x",
            "12/31/23, 13:15 PM - Alice: x",
            "123/12/2023, 21:15 - x",
        ] {
            assert!(parse_header(l).is_none(), "{l}");
        }
    }

    #[test]
    fn day_month_inference() {
        let s = |l: &str| parse_header(l).unwrap().stamp;
        assert!(infer_day_first(&[s("13/01/2024, 10:00 - a: b")]));
        assert!(!infer_day_first(&[s("01/13/2024, 10:00 - a: b")]));
        assert!(!infer_day_first(&[s("01/02/2024, 10:00 AM - a: b")]));
        assert!(infer_day_first(&[s("01/02/2024, 10:00 - a: b")]));
        // Year-first stamps say nothing about the order.
        assert!(infer_day_first(&[s("2024-01-02, 10:00 - a: b")]));
        // Feb 30 is not a date.
        assert!(s("30/02/2024, 10:00 - a: b").to_datetime(true).is_none());
    }

    #[test]
    fn media_placeholders() {
        let att = |l: &str| match classify_media(l) {
            Some(Media::Attachment(a)) => a,
            other => panic!("{l}: {other:?}"),
        };
        assert_eq!(att("<Media omitted>"), Attachment::default());
        assert_eq!(att("<Medien ausgelassen>"), Attachment::default());
        let a = att("\u{200e}<attached: 00000012-PHOTO-2023-12-31-21-15-42.jpg>");
        assert_eq!(
            a.name.as_deref(),
            Some("00000012-PHOTO-2023-12-31-21-15-42.jpg")
        );
        assert_eq!(a.mime.as_deref(), Some("image/jpeg"));
        let a = att("IMG-20231231-WA0001.jpg (file attached)");
        assert_eq!(a.name.as_deref(), Some("IMG-20231231-WA0001.jpg"));
        let a = att("\u{200e}image omitted");
        assert_eq!((a.name, a.mime.as_deref()), (None, Some("image/jpeg")));
        let a = att("Report final.pdf • 3 pages \u{200e}document omitted");
        assert_eq!(a.name.as_deref(), Some("Report final.pdf"));
        assert_eq!(a.mime.as_deref(), Some("application/pdf"));
        assert_eq!(
            classify_media("<This message was deleted>"),
            Some(Media::Deleted)
        );
        assert_eq!(
            classify_media("\u{200e}This message was deleted."),
            Some(Media::Deleted)
        );
        assert_eq!(classify_media("I love you <3"), None);
        assert_eq!(classify_media("<3>"), None);
        assert_eq!(classify_media("hello"), None);
    }

    #[test]
    fn decodes_utf16_and_bom() {
        let mut le = vec![0xFF, 0xFE];
        le.extend(
            "31/12/2023, 21:15 - Zoé: é"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        assert_eq!(decode(&le), "31/12/2023, 21:15 - Zoé: é");
        let mut be = vec![0xFE, 0xFF];
        be.extend("ok".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode(&be), "ok");
        assert_eq!(decode(b"\xEF\xBB\xBFhi"), "hi");
    }

    #[test]
    fn crlf_line_endings() {
        let text = "\u{feff}31/12/2023 21:15 - Zo: a\r\nb\r\n31/12/2023 21:16 - Zo: c\r\n";
        let c = parse_chat(text, "wa:zo", "Zo", None, &HashMap::new()).unwrap();
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].text, "a\nb");
        assert_eq!(c.messages[1].text, "c");
    }

    #[test]
    fn chat_names() {
        assert_eq!(chat_name("WhatsApp Chat with Alice"), "Alice");
        assert_eq!(chat_name("WhatsApp Chat with Alice (1)"), "Alice");
        assert_eq!(chat_name("WhatsApp Chat - Family"), "Family");
        assert_eq!(chat_name("WhatsApp-Chat mit Oma"), "Oma");
        assert_eq!(chat_name("Chat de WhatsApp con Juan"), "Juan");
        assert_eq!(chat_name("Discussion WhatsApp avec Zoé"), "Zoé");
        assert_eq!(chat_name("whatsapp chat with bob(2)"), "bob");
        assert_eq!(chat_name("Team (2024)"), "Team (2024)");
        assert_eq!(chat_name("random"), "random");
    }

    #[test]
    fn parses_a_small_chat() {
        let text = "\u{feff}12/31/23, 9:00 PM - Messages and calls are end-to-end encrypted.\n\
12/31/23, 9:01 PM - Alice: Hello\nsecond line\n\
12/31/23, 9:02 PM - +1 (555) 123-4567: <Media omitted>\n\
12/31/23, 9:03 PM - Tanuj: me here\n";
        let c = parse_chat(text, "wa:x", "X", Some("tanuj"), &HashMap::new()).unwrap();
        assert_eq!(c.messages.len(), 4);
        assert!(c.messages[0].system);
        assert_eq!(c.messages[1].text, "Hello\nsecond line");
        let p = &c.participants[c.messages[2].author.unwrap()];
        assert_eq!(p.handle.as_deref(), Some("+15551234567"));
        assert_eq!(c.messages[2].attachments.len(), 1);
        assert!(c.participants[c.messages[3].author.unwrap()].me);
        assert!(!c.is_group);
        assert_eq!(c.messages[3].at, datetime!(2023-12-31 21:03 UTC));
    }
}
