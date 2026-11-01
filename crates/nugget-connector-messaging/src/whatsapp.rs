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
//! 14/03/2026 à 09:06 - Alice: text            Android, French
//! ١٤/٠٣/٢٠٢٦، ٩:٠٦ م - Alice: text              Android, Arabic (with bidi marks)
//! ```
//!
//! Lines that do not start with a timestamp continue the previous message. A timestamp line
//! without `Name: ` is a system line (the encryption notice, "Alice added Bob"); iOS writes
//! system lines with an author and a leading U+200E mark instead. Day/month order is inferred
//! per conversation, over all its exports ([`infer_day_first`]). Timestamps carry no zone;
//! they are read as UTC (what the user saw on the phone). An export none of whose lines can be
//! read is reported (`Found::Problem`) and its chat's documents are kept.

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
    Attachment, Conversation, Message, basename, clean_name, file_url, is_invisible,
    looks_like_phone, mime_from_name, normalise_phone, slug,
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

/// What a folder listing found: entries `(path, name, is_dir)`, and whether some entry could
/// not be inspected (so the listing may be missing exports).
struct Listing {
    entries: Vec<(PathBuf, String, bool)>,
    incomplete: bool,
}

/// List `dir`: regular files and folders only (no FIFOs, devices or sockets, which could block
/// a read forever), and symlinks only when they resolve inside `root` (the watched folder).
fn list_dir(dir: &Path, root: &Path) -> std::io::Result<Listing> {
    let mut out = Listing {
        entries: Vec::new(),
        incomplete: false,
    };
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else {
            out.incomplete = true;
            continue;
        };
        let name = e.file_name().to_string_lossy().into_owned();
        if is_hidden(&name) {
            continue;
        }
        let Ok(ft) = e.file_type() else {
            out.incomplete = true;
            continue;
        };
        let path = e.path();
        let (is_dir, is_file) = if ft.is_symlink() {
            match std::fs::canonicalize(&path) {
                Ok(target) if target.starts_with(root) => match std::fs::metadata(&target) {
                    Ok(m) => (m.is_dir(), m.is_file()),
                    Err(_) => continue,
                },
                Ok(_) => {
                    tracing::debug!(link = %path.display(), "symlink leaves the export folder; skipped");
                    continue;
                }
                // Dangling link.
                Err(_) => continue,
            }
        } else {
            (ft.is_dir(), ft.is_file())
        };
        if is_dir || is_file {
            out.entries.push((path, name, is_dir));
        }
    }
    out.entries.sort();
    Ok(out)
}

/// `.zip` / `.txt` files in `folder` and its direct subfolders, sorted by path, and whether
/// the listing is incomplete (some subfolder or entry could not be read).
fn find_exports(folder: &Path) -> Result<(Vec<ExportFile>, bool)> {
    let not_found = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound(format!(
                "WhatsApp export folder {} does not exist",
                folder.display()
            ))
        } else {
            Error::Io(e)
        }
    };
    let root = std::fs::canonicalize(folder).map_err(not_found)?;
    let top = list_dir(folder, &root).map_err(not_found)?;
    let mut incomplete = top.incomplete;
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
    for (path, name, is_dir) in top.entries {
        if is_dir {
            match list_dir(&path, &root) {
                Ok(children) => {
                    incomplete |= children.incomplete;
                    for (p, n, d) in children.entries {
                        if !d {
                            take(p, &n, Some(&name));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(dir = %path.display(), error = %e, "cannot list folder; keeping what was imported");
                    incomplete = true;
                }
            }
        } else {
            take(path, &name, None);
        }
    }
    Ok((out, incomplete))
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
    /// The 12-hour marker is written in upper case (`PM`, as in the US), not `pm` or `p. m.`.
    pub upper_marker: bool,
    /// A word separates date and time (`14/03/2026 à 09:06`), not just `, ` or a space (an
    /// AM/PM marker there, as in `오후`, does not count).
    pub worded: bool,
}

/// AM/PM markers, longest first so `a.m.` wins over `am`.
const AM: &[&str] = &[
    "a. m.", "a.m.", "vorm.", "π.μ.", "午前", "上午", "오전", "am", "ص",
];
const PM: &[&str] = &[
    "p. m.", "p.m.", "nachm.", "μ.μ.", "午後", "下午", "오후", "pm", "م",
];

/// Bidi marks phones put around the numbers of a date in right-to-left locales (U+061C, the
/// Arabic letter mark, besides the marks every locale may carry).
fn is_mark(c: char) -> bool {
    is_invisible(c) || c == '\u{061C}'
}

/// Arabic-Indic (U+0660–0669) and Persian (U+06F0–06F9) digits as ASCII digits.
fn ascii_digit(c: char) -> char {
    let base = match c {
        '\u{0660}'..='\u{0669}' => 0x0660,
        '\u{06F0}'..='\u{06F9}' => 0x06F0,
        _ => return c,
    };
    char::from_digit(u32::from(c) - base, 10).unwrap_or(c)
}

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

/// Parse `31/12/2023, 21:15:42`, `12/31/23, 9:15 PM`, `2023. 12. 31. 오후 9:15`,
/// `14/03/2026 à 09:06`, `١٤/٠٣/٢٠٢٦، ٩:٠٦ م` ...
pub fn parse_stamp(raw: &str) -> Option<Stamp> {
    let s: String = raw
        .chars()
        .filter(|c| !is_mark(*c))
        .map(|c| {
            if c.is_whitespace() {
                ' '
            } else {
                ascii_digit(c)
            }
        })
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
    // Between date and time: a short separator (`, `, ` `, `، `, ` à `, ` um `, ` a las `,
    // ` в `), possibly holding the 12-hour marker (`오후 `, `午後`).
    let digit = rest.find(|c: char| c.is_ascii_digit())?;
    let (between, time_part) = rest.split_at(digit);
    if !between.starts_with([' ', ',', '،'])
        || between.chars().count() > 12
        || between.contains([':', '/', '[', ']', '<', '>'])
    {
        return None;
    }
    let (word, pre) = strip_marker(between.trim_matches([' ', ',', '،']));
    let word_ok = word.chars().count() <= 8
        && word
            .chars()
            .all(|c| c.is_alphabetic() || c == '.' || c == ' ');
    if !word_ok {
        return None;
    }
    let (time_s, post) = strip_marker(time_part.trim());
    let pm = match (pre, post) {
        (Some(_), Some(_)) => return None,
        (p, q) => p.or(q),
    };
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
        upper_marker: pm.is_some() && rest.chars().any(|c| c.is_ascii_uppercase()),
        worded: !word.trim().is_empty(),
    })
}

impl Stamp {
    /// The instant, reading day-first (`31/12`) or month-first (`12/31`). Years before 2000
    /// are not WhatsApp dates (a Solar Hijri `1405` would otherwise land in the Middle Ages).
    pub fn to_datetime(self, day_first: bool) -> Option<OffsetDateTime> {
        let (y, m, d) = if self.year_first {
            (self.a, self.b, self.c)
        } else if day_first {
            (self.c, self.b, self.a)
        } else {
            (self.c, self.a, self.b)
        };
        let y = if y < 100 { 2000 + y } else { y };
        if y < 2000 {
            return None;
        }
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

/// How far past `now` a stamp may lie and still be plausible: stamps are local phone time read
/// as UTC, so a message just sent east of UTC looks up to 14 hours ahead.
const FUTURE_SLACK: time::Duration = time::Duration::days(1);

/// Day-first or month-first for one conversation, from the stamps of all its exports (so
/// re-exports of a chat agree). The order that makes fewer stamps impossible (`13/01` is not
/// month-first) wins, then the one that puts fewer messages after `now`. When both read alike,
/// an upper-case `AM/PM` clock suggests month-first (US); `am/pm`, `p. m.` or a 24-hour clock
/// suggest day-first (en_IN, en_GB, most of the world).
pub fn infer_day_first(stamps: &[Stamp], now: OffsetDateTime) -> bool {
    let dm: Vec<&Stamp> = stamps
        .iter()
        .filter(|s| !s.year_first && s.a != s.b)
        .collect();
    let limit = now + FUTURE_SLACK;
    let score = |day_first: bool| {
        let (mut invalid, mut future) = (0usize, 0usize);
        for s in &dm {
            match s.to_datetime(day_first) {
                None => invalid += 1,
                Some(t) if t > limit => future += 1,
                Some(_) => {}
            }
        }
        (invalid, future)
    };
    let (df, mf) = (score(true), score(false));
    if df != mf {
        return df < mf;
    }
    let twelve = dm.iter().filter(|s| s.pm.is_some()).count();
    let upper = dm.iter().filter(|s| s.upper_marker).count();
    !(upper > 0 && upper * 2 >= twelve)
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
    let line = line.trim_start_matches(|c: char| is_mark(c) || c == ' ');
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
    // Right-to-left dates carry bidi marks (3 bytes each) and 2-byte digits.
    let window = &line[..floor_boundary(line, 100)];
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
    s.chars().filter(|c| !is_mark(*c)).collect()
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
    "je hebt dit bericht verwijderd",
    "este mensaje fue eliminado",
    "данное сообщение удалено",
    "это сообщение удалено",
    "вы удалили данное сообщение",
    "вы удалили это сообщение",
    "bu mesaj silindi",
    "ta wiadomość została usunięta",
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
    "ocultada",
    "omitidos",
    "omitidas",
    "отсутствует",
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
    ("изображение отсутствует", "image/jpeg"),
    ("видео отсутствует", "video/mp4"),
    ("аудиофайл отсутствует", "audio/mp4"),
    ("стикер отсутствует", "image/webp"),
    ("gif отсутствует", "image/gif"),
    ("документ отсутствует", "application/octet-stream"),
    ("gif omitido", "image/gif"),
    ("gif weggelassen", "image/gif"),
];

/// The media kind named by the first word of a localized iOS `<kind> omitted` line.
fn omitted_kind_mime(word: &str) -> Option<&'static str> {
    Some(match word {
        "image"
        | "photo"
        | "bild"
        | "foto"
        | "imagen"
        | "imagem"
        | "immagine"
        | "изображение"
        | "фото" => "image/jpeg",
        "video" | "vidéo" | "vídeo" | "видео" => "video/mp4",
        "audio" | "áudio" | "аудио" | "аудиофайл" => "audio/mp4",
        "sticker" | "figurinha" | "стикер" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    })
}

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
        if (phrase.contains("document")
            || phrase.contains("dokument")
            || phrase.contains("документ"))
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
    // Other iOS locales: `‎<kind> <omitted>` behind the U+200E mark iOS puts on notices.
    if line.trim_start_matches(' ').starts_with('\u{200E}') {
        let words: Vec<&str> = lower.split_whitespace().collect();
        if (2..=4).contains(&words.len())
            && words
                .last()
                .is_some_and(|w| OMITTED.contains(&w.trim_end_matches('.')))
        {
            return Some(Media::Attachment(Attachment {
                name: None,
                mime: omitted_kind_mime(words[0]).map(str::to_owned),
                size: None,
            }));
        }
    }
    None
}

/// An Android media placeholder in any language: a whole message that is one `<…>` phrase
/// (`<Media omitted>`, `<Medien ausgeschlossen>`, `<Без медиафайлов>`, `<Multimedia omitido>`).
fn is_android_placeholder(line: &str) -> bool {
    let t = strip_invisible(line);
    let Some(inner) = t.trim().strip_prefix('<').and_then(|t| t.strip_suffix('>')) else {
        return false;
    };
    let inner = inner.trim();
    // Placeholders in cased scripts are capitalised phrases (`<Media omitted>`); a single or
    // lower-case word in angle brackets is something a person typed (`<sigh>`, `<evil laugh>`).
    // Caseless scripts (`<メディアは含まれていません>`) need no space.
    let shaped = match inner.chars().next() {
        Some(c) if c.is_uppercase() => inner.contains(' '),
        Some(c) => !c.is_lowercase(),
        None => false,
    };
    shaped
        && inner.chars().count() <= 64
        && inner.chars().any(char::is_alphabetic)
        && !inner.contains(['<', '>'])
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

/// Whether `name` (the text before the first `: `) can be an author. A system line that quotes
/// a group subject containing `: ` (`Alice changed the subject to "a: b"`) splits inside the
/// quotes, leaving an unbalanced quote; a contact name with balanced quotes (`Mike "Ace" Li`)
/// is a real author.
fn valid_author(name: &str) -> bool {
    let n = name.trim();
    let straight = n.matches('"').count().is_multiple_of(2);
    let curly = n.matches('“').count() == n.matches('”').count();
    !n.is_empty() && n.chars().count() <= 80 && straight && curly && !n.contains('\u{201E}')
}

struct Entry<'a> {
    stamp: Stamp,
    rest: &'a str,
    ios: bool,
    /// The whole header line, kept for when its timestamp turns out not to be a date.
    line: &'a str,
    more: Vec<&'a str>,
}

/// One export's message headers, each with its continuation lines.
fn split_entries(text: &str) -> Vec<Entry<'_>> {
    let lines: Vec<(&str, Option<Header<'_>>)> = text
        .split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            (line, parse_header(line))
        })
        .collect();
    // One export is written by one phone, so all its headers share one style (`[...] ` on iOS,
    // `... - ` on Android). A line of the other style is message text that merely looks like a
    // header (a pasted message), not a new message.
    let ios = lines
        .iter()
        .filter(|(_, h)| h.as_ref().is_some_and(|h| h.ios))
        .count();
    let android = lines
        .iter()
        .filter(|(_, h)| h.as_ref().is_some_and(|h| !h.ios))
        .count();
    let ios_file = ios >= android;
    // Likewise its headers all separate date and time the same way: in a file of
    // `14/03/2026, 09:06` headers, a line `14/03/2026 at 09:06 - Bob: hi` is pasted text, and
    // in a file of `14/03/2026 à 09:06` headers, `14/03/2026, 09:06 - …` is.
    let (worded, plain) = lines
        .iter()
        .filter_map(|(_, h)| h.as_ref().filter(|h| h.ios == ios_file))
        .fold((0usize, 0usize), |(w, p), h| {
            if h.stamp.worded {
                (w + 1, p)
            } else {
                (w, p + 1)
            }
        });
    let style_ok = |h: &Header<'_>| {
        h.ios == ios_file
            && match worded.cmp(&plain) {
                std::cmp::Ordering::Greater => h.stamp.worded,
                std::cmp::Ordering::Less => !h.stamp.worded,
                std::cmp::Ordering::Equal => true,
            }
    };
    let mut entries: Vec<Entry<'_>> = Vec::new();
    for (line, header) in lines {
        match header.filter(|h| style_ok(h)) {
            Some(h) => entries.push(Entry {
                stamp: h.stamp,
                rest: h.rest,
                ios: h.ios,
                line,
                more: Vec::new(),
            }),
            None => {
                if let Some(last) = entries.last_mut() {
                    last.more.push(line);
                }
            }
        }
    }
    entries
}

/// The timestamps of one export's messages, for [`infer_day_first`].
pub fn chat_stamps(text: &str) -> Vec<Stamp> {
    split_entries(text).iter().map(|e| e.stamp).collect()
}

/// Parse one export's text into a conversation, reading dates day-first or month-first
/// (`None` when it holds no readable WhatsApp message).
pub fn parse_chat(
    text: &str,
    id: &str,
    name: &str,
    me: Option<&str>,
    media_sizes: &HashMap<String, u64>,
    day_first: bool,
) -> Option<Conversation> {
    let entries = split_entries(text);
    if entries.is_empty() {
        return None;
    }
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
                m.text.push_str(&strip_invisible(e.line));
                for l in e.more {
                    m.text.push('\n');
                    m.text.push_str(&strip_invisible(l));
                }
                m.text.truncate(m.text.trim_end().len());
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
                let whole = e.more.iter().all(|l| l.trim().is_empty());
                let media = classify_media(first).or_else(|| {
                    (!e.ios && whole && is_android_placeholder(first))
                        .then(|| Media::Attachment(Attachment::default()))
                });
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
    (!conv.messages.is_empty()).then_some(conv)
}

/// An export's path relative to the watched folder, with `/` separators.
fn relative_name(path: &Path, folder: &Path) -> String {
    let rel = path.strip_prefix(folder).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether a file none of whose lines could be read is still meant as a WhatsApp export (so
/// it is reported, not skipped as a stray text file): its name says so, or some line looks
/// like a message header in a format this parser does not know (`<digits…> - Name: text`).
fn looks_like_export(path: &Path, text: &str) -> bool {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if file.contains("whatsapp") || file.starts_with("_chat.") {
        return true;
    }
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .take(200)
        .any(|l| {
            let l = l.trim_start_matches(|c: char| is_mark(c) || c.is_whitespace() || c == '[');
            let head = &l[..floor_boundary(l, 100)];
            l.chars().next().is_some_and(char::is_numeric)
                && head.contains(": ")
                && (head.contains(" - ") || head.contains(']'))
        })
}

impl WhatsAppReader {
    /// One read, judging future dates against `now`.
    fn read_at(
        &self,
        cancel: &CancellationToken,
        found: &mut dyn FnMut(Found) -> bool,
        now: OffsetDateTime,
    ) -> Result<()> {
        let (files, incomplete) = find_exports(&self.folder)?;
        // Exports in an unreadable subfolder are unknown: do not delete anything this pass.
        if incomplete && !found(Found::Keep(String::new())) {
            return Ok(());
        }
        // A chat's exports (re-exports, duplicate downloads) are read together, so they agree
        // on the day/month order and merge without duplicates.
        let mut chats: BTreeMap<String, Vec<ExportFile>> = BTreeMap::new();
        for f in files {
            chats
                .entry(format!("wa:{}", slug(&f.name)))
                .or_default()
                .push(f);
        }
        for (id, files) in chats {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut texts: Vec<(ExportFile, ExportText)> = Vec::new();
            for f in files {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                match read_export(&f.path) {
                    Ok(Some(t)) => texts.push((f, t)),
                    Ok(None) => {
                        tracing::debug!(file = %f.path.display(), "no chat text in archive; skipped");
                    }
                    Err(e) => {
                        tracing::warn!(file = %f.path.display(), error = %e, "cannot read WhatsApp export; keeping what was imported");
                        if !found(Found::Keep(format!("{id}/"))) {
                            return Ok(());
                        }
                    }
                }
            }
            let stamps: Vec<Stamp> = texts
                .iter()
                .flat_map(|(_, t)| chat_stamps(&t.text))
                .collect();
            let day_first = infer_day_first(&stamps, now);
            let mut merged: Option<Conversation> = None;
            for (f, t) in texts {
                match parse_chat(
                    &t.text,
                    &id,
                    &f.name,
                    self.me.as_deref(),
                    &t.media,
                    day_first,
                ) {
                    Some(mut conv) => {
                        conv.url = file_url(&f.path);
                        match &mut merged {
                            Some(existing) => existing.merge(conv),
                            None => merged = Some(conv),
                        }
                    }
                    None if t.text.trim().is_empty() => {
                        // Being written: keep what was imported, the next pass reads it.
                        tracing::debug!(file = %f.path.display(), "empty WhatsApp export; keeping what was imported");
                        if !found(Found::Keep(format!("{id}/"))) {
                            return Ok(());
                        }
                    }
                    None if looks_like_export(&f.path, &t.text) => {
                        let rel = relative_name(&f.path, &self.folder);
                        tracing::warn!(file = %f.path.display(), "no readable message in WhatsApp export; keeping what was imported");
                        let problem = Found::Problem {
                            key: format!("whatsapp:{rel}"),
                            message: format!(
                                "Couldn't read any messages in {rel}: its date format isn't \
                                 recognised. Anything imported from this chat before is kept."
                            ),
                        };
                        if !found(problem) || !found(Found::Keep(format!("{id}/"))) {
                            return Ok(());
                        }
                    }
                    None => {
                        tracing::debug!(file = %f.path.display(), "not a WhatsApp export; skipped");
                    }
                }
            }
            if let Some(conv) = merged
                && !found(Found::Conversation(conv))
            {
                return Ok(());
            }
        }
        Ok(())
    }
}

impl Reader for WhatsAppReader {
    fn kind(&self) -> &'static str {
        crate::WHATSAPP_KIND
    }

    fn service(&self) -> &'static str {
        "whatsapp"
    }

    fn read(&self, cancel: &CancellationToken, found: &mut dyn FnMut(Found) -> bool) -> Result<()> {
        self.read_at(cancel, found, OffsetDateTime::now_utc())
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
        assert_eq!(dt("31/12/2023, 9:15 p.m. - Alice: hi", true), want);
        assert_eq!(
            dt("\u{feff}\u{200e}[31.12.23, 21:15:00] Alice: hi", true),
            want
        );
        assert_eq!(
            dt("12/31/23, 12:05\u{a0}AM - Alice: hi", false),
            datetime!(2023-12-31 0:05 UTC)
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
        let now = datetime!(2026-11-01 12:00 UTC);
        assert!(infer_day_first(&[s("13/01/2024, 10:00 - a: b")], now));
        assert!(!infer_day_first(&[s("01/13/2024, 10:00 - a: b")], now));
        assert!(!infer_day_first(&[s("01/02/2024, 10:00 AM - a: b")], now));
        assert!(infer_day_first(&[s("01/02/2024, 10:00 - a: b")], now));
        // Year-first stamps say nothing about the order.
        assert!(infer_day_first(&[s("2024-01-02, 10:00 - a: b")], now));
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
        let c = parse_chat(text, "wa:zo", "Zo", None, &HashMap::new(), true).unwrap();
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].text, "a\nb");
        assert_eq!(c.messages[1].text, "c");
    }

    /// Regression: in an iOS export, a message line pasted from an Android export (or vice
    /// versa) started a new message with the wrong author.
    #[test]
    fn other_style_header_lines_are_message_text() {
        let text = "[31/12/2023, 21:15:42] Alice: forwarding this\n\
31/12/2023, 20:00 - Bob: original\n\
[31/12/2023, 21:16:00] Zo: ok\n";
        let c = parse_chat(text, "wa:x", "X", None, &HashMap::new(), true).unwrap();
        assert_eq!(c.messages.len(), 2);
        assert_eq!(
            c.messages[0].text,
            "forwarding this\n31/12/2023, 20:00 - Bob: original"
        );
        assert_eq!(c.participants.len(), 2);
    }

    /// Regression: a continuation line that parsed as a header with an impossible date lost
    /// its timestamp part when folded back into the previous message.
    #[test]
    fn impossible_date_lines_keep_their_full_text() {
        let text = "31/12/2023, 21:15 - Alice: plan\n30/02/2024, 10:00 - pay rent\n";
        let c = parse_chat(text, "wa:x", "X", None, &HashMap::new(), true).unwrap();
        assert_eq!(c.messages.len(), 1);
        assert_eq!(c.messages[0].text, "plan\n30/02/2024, 10:00 - pay rent");
    }

    /// Regression: a FIFO named `*.txt` blocked the reader forever, and a symlink could pull
    /// files from outside the watched folder into the index.
    #[cfg(unix)]
    #[test]
    fn listing_skips_special_files_and_escaping_symlinks() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("WhatsApp Chat with Eve.txt");
        std::fs::write(&secret, "31/12/2023, 21:15 - Eve: secret\n").unwrap();
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("WhatsApp Chat with Ann.txt");
        std::fs::write(&inside, "31/12/2023, 21:15 - Ann: hi\n").unwrap();
        std::os::unix::fs::symlink(&secret, root.path().join("WhatsApp Chat with Eve.txt"))
            .unwrap();
        std::os::unix::fs::symlink(&inside, root.path().join("WhatsApp Chat with Ann2.txt"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let fifo = root.path().join("WhatsApp Chat with Pipe.txt");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        let (files, incomplete) = find_exports(root.path()).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Ann", "Ann2"]);
        assert!(!incomplete);
        if made {
            assert!(fifo.exists());
        }
    }

    /// Regression: an unreadable subfolder was skipped silently, so the exports in it looked
    /// removed and their documents were deleted.
    #[cfg(unix)]
    #[test]
    fn unreadable_subfolder_marks_the_listing_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let sub = root.path().join("old");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("WhatsApp Chat with Ann.txt"), "x").unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = std::fs::read_dir(&sub).is_ok(); // running as root
        let (_, incomplete) = find_exports(root.path()).unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !readable_anyway {
            assert!(incomplete);
        }
    }

    /// Regression: a contact name with quotes turned every message of that person into a
    /// one-line system line; subject changes quoting `: ` must stay system lines.
    #[test]
    fn quoted_names_are_authors_quoted_subjects_are_not() {
        let text = "31/12/2023, 21:15 - Mike \"Ace\" Li: line one\nline two\n\
31/12/2023, 21:16 - Alice changed the subject to \"plans: 2024\"\n\
31/12/2023, 21:17 - Bob changed the subject from “a” to “b: c”\n\
31/12/2023, 21:18 - Mike \"Ace\" Li: <Media omitted>\n\
31/12/2023, 21:18 - Mike \"Ace\" Li: <Media omitted>\n";
        let c = parse_chat(text, "wa:x", "X", None, &HashMap::new(), true).unwrap();
        assert_eq!(c.messages.len(), 5);
        assert!(!c.messages[0].system);
        assert_eq!(
            c.participants[c.messages[0].author.unwrap()].name,
            "Mike \"Ace\" Li"
        );
        assert_eq!(c.messages[0].text, "line one\nline two");
        assert!(c.messages[1].system);
        assert!(c.messages[2].system);
        // Two photos in the same minute are two messages (regression: deduplicated to one).
        assert_eq!(c.messages[3].attachments.len(), 1);
        assert_eq!(c.messages[4].attachments.len(), 1);
    }

    #[test]
    fn parses_a_small_chat() {
        let text = "\u{feff}12/31/23, 9:00 PM - Messages and calls are end-to-end encrypted.\n\
12/31/23, 9:01 PM - Alice: Hello\nsecond line\n\
12/31/23, 9:02 PM - +1 (555) 123-4567: <Media omitted>\n\
12/31/23, 9:03 PM - Tanuj: me here\n";
        let c = parse_chat(text, "wa:x", "X", Some("tanuj"), &HashMap::new(), false).unwrap();
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

    /// Regression (JJ1-5): date lines with a word, an Arabic comma, Eastern Arabic or Persian
    /// digits, or bidi marks between the fields were not headers, so the export vanished.
    #[test]
    fn localized_date_time_separators_and_digits() {
        let want = datetime!(2026-03-14 09:06 UTC);
        for l in [
            "14/03/2026 à 09:06 - Chloé: salut",
            "14.03.26 um 09:06 - Oma: hallo",
            "14/03/2026 a las 9:06 a. m. - Yusuf: hola",
            "14.03.2026 в 9:06 - Иван: привет",
            "14/03/2026، 9:06 ص - Omar: مرحبا",
            "١٤/٠٣/٢٠٢٦، ٩:٠٦ ص - Omar: مرحبا",
            "\u{200f}١٤\u{200f}/٠٣\u{200f}/٢٠٢٦، \u{61c}٩:٠٦ ص - Omar: مرحبا",
            "۱۴/۰۳/۲۰۲۶، ۹:۰۶ - Sara: سلام",
            "[١٤/٠٣/٢٠٢٦، ٩:٠٦:٠٠ ص] Omar: مرحبا",
        ] {
            assert_eq!(dt(l, true), want, "{l}");
        }
        let pm = dt("١٤/٠٣/٢٠٢٦، ٩:٠٦ م - Omar: مرحبا", true);
        assert_eq!(pm, datetime!(2026-03-14 21:06 UTC));
        for l in [
            "14/03/2026 something long here 09:06 - x: y",
            "14/03/2026 - 09:06 - x: y",
            "14/03/202609:06 - x: y",
            // A Solar Hijri year is not a date this parser can place.
            "۱۴۰۵/۰۱/۰۱، ۹:۰۶ - Sara: سلام",
        ] {
            assert!(
                parse_header(l)
                    .and_then(|h| h.stamp.to_datetime(true))
                    .is_none(),
                "{l}"
            );
        }
        let c = parse_chat(
            "\u{200f}١٤/٠٣/٢٠٢٦، ٩:٠٦ م - Omar: مرحبا\nسطر ثان\n",
            "wa:omar",
            "Omar",
            None,
            &HashMap::new(),
            true,
        )
        .unwrap();
        assert_eq!(c.messages.len(), 1);
        assert_eq!(c.messages[0].text, "مرحبا\nسطر ثان");
        assert_eq!(c.participants[c.messages[0].author.unwrap()].name, "Omar");
    }

    /// Regression (JJ1-6): day-first 12-hour exports (en_IN, all days ≤ 12) were read
    /// month-first, some into the future.
    #[test]
    fn day_order_rejects_future_dates_and_reads_lower_case_am_pm_day_first() {
        let s = |l: &str| parse_header(l).unwrap().stamp;
        let now = datetime!(2026-11-01 12:00 UTC);
        // `pm` in lower case: en_IN / en_GB, day-first.
        assert!(infer_day_first(
            &[
                s("03/05/26, 9:00 pm - Priya: hi"),
                s("04/05/26, 9:05 am - Priya: x")
            ],
            now
        ));
        assert!(infer_day_first(&[s("03/05/26, 9:00 p. m. - Ana: hi")], now));
        // Upper-case AM/PM and nothing else to go by: US, month-first.
        assert!(!infer_day_first(&[s("03/05/26, 9:00 PM - Al: hi")], now));
        // Month-first would put 11 June on 6 November, after `now`.
        assert!(infer_day_first(
            &[
                s("03/05/26, 9:00 PM - Al: hi"),
                s("11/06/26, 9:30 AM - Al: x")
            ],
            now
        ));
        // A day above 12 still decides over everything else.
        assert!(!infer_day_first(
            &[
                s("10/09/26, 9:00 pm - Al: hi"),
                s("10/13/26, 9:30 pm - Al: x")
            ],
            datetime!(2026-12-01 0:00 UTC)
        ));
    }

    fn reader_pass(folder: &Path, now: OffsetDateTime) -> Vec<Found> {
        let reader = WhatsAppReader {
            folder: folder.to_path_buf(),
            me: None,
        };
        let mut out = Vec::new();
        reader
            .read_at(
                &CancellationToken::new(),
                &mut |f| {
                    out.push(f);
                    true
                },
                now,
            )
            .unwrap();
        out
    }

    /// Regression (JJ1-6): an old export with only ambiguous dates and a re-export with a day
    /// above 12 were dated with different orders, so the shared messages showed up twice.
    #[test]
    fn reexports_share_one_day_order() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("WhatsApp Chat with Priya.txt"),
            "03/05/26, 9:00 PM - Priya: dinner at 9?\n04/05/26, 8:15 AM - Priya: running late\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("WhatsApp Chat with Priya (1).txt"),
            "03/05/26, 9:00 PM - Priya: dinner at 9?\n04/05/26, 8:15 AM - Priya: running late\n\
25/06/26, 7:00 PM - Priya: back home\n",
        )
        .unwrap();
        let found = reader_pass(tmp.path(), datetime!(2026-11-01 12:00 UTC));
        let convs: Vec<&Conversation> = found
            .iter()
            .filter_map(|f| match f {
                Found::Conversation(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(convs.len(), 1);
        let at: Vec<OffsetDateTime> = convs[0].messages.iter().map(|m| m.at).collect();
        assert_eq!(
            at,
            [
                datetime!(2026-05-03 21:00 UTC),
                datetime!(2026-05-04 08:15 UTC),
                datetime!(2026-06-25 19:00 UTC),
            ]
        );
    }

    /// Regression (JJ1-5): an export with no readable line was skipped at debug level and the
    /// source said "Up to date"; now it is reported and its chat's documents are kept.
    #[test]
    fn unreadable_export_is_reported_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("old")).unwrap();
        std::fs::write(
            tmp.path().join("old").join("WhatsApp Chat with Kai.txt"),
            "2026年3月14日 9:06 - Kai: hi\n",
        )
        .unwrap();
        // Not a WhatsApp name, but its lines look like headers in an unknown format.
        std::fs::write(tmp.path().join("export.txt"), "14|03|2026 9:06 - Lu: hi\n").unwrap();
        // Stray text and a file still being written are not reported.
        std::fs::write(tmp.path().join("notes.txt"), "Just some notes.\n").unwrap();
        std::fs::write(tmp.path().join("WhatsApp Chat with Mo.txt"), "").unwrap();
        let found = reader_pass(tmp.path(), datetime!(2026-11-01 12:00 UTC));
        let mut problems = Vec::new();
        let mut keep = Vec::new();
        for f in &found {
            match f {
                Found::Problem { key, message } => problems.push((key.clone(), message.clone())),
                Found::Keep(p) => keep.push(p.clone()),
                Found::Conversation(c) => panic!("{}", c.id),
            }
        }
        let keys: Vec<&str> = problems.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "whatsapp:export.txt",
                "whatsapp:old/WhatsApp Chat with Kai.txt"
            ]
        );
        assert!(
            problems[1].1.starts_with(
                "Couldn't read any messages in old/WhatsApp Chat with Kai.txt: its date format isn't recognised."
            ),
            "{}",
            problems[1].1
        );
        assert_eq!(keep, ["wa:export/", "wa:kai/", "wa:mo/"]);
    }

    /// Regression (JJ1-16): media placeholders and deletion notices in other languages were
    /// kept as text, or turned into an authorless system line.
    #[test]
    fn localized_placeholders() {
        let android = "14/03/2026, 09:06 - Oma: <Medien ausgeschlossen>\n\
14/03/2026, 09:07 - Иван: <Без медиафайлов>\n\
14/03/2026, 09:08 - Ana: <Multimedia omitido>\n\
14/03/2026, 09:09 - Ana: <sarcasm>\nnot a placeholder\n\
14/03/2026, 09:10 - Иван: Данное сообщение удалено\n";
        let c = parse_chat(android, "wa:x", "X", None, &HashMap::new(), true).unwrap();
        assert_eq!(c.messages.len(), 5);
        for m in &c.messages[..3] {
            assert_eq!(m.attachments, [Attachment::default()]);
            assert!(m.text.is_empty() && !m.system && m.author.is_some());
        }
        assert_eq!(c.messages[3].text, "<sarcasm>\nnot a placeholder");
        assert!(c.messages[4].deleted && c.messages[4].text.is_empty());

        // iOS 1:1 chat named after its contact: the placeholder stays Иван's.
        let ios = "[14.03.26, 09:06:00] Иван: \u{200e}изображение отсутствует\n\
[14.03.26, 09:07:00] Иван: \u{200e}видео отсутствует\n\
[14.03.26, 09:08:00] Иван: \u{200e}Данное сообщение удалено.\n\
[14.03.26, 09:09:00] Иван: \u{200e}Вы удалили это сообщение.\n\
[14.03.26, 09:10:00] Иван: \u{200e}Пропущенный звонок\n";
        let c = parse_chat(ios, "wa:ivan", "Иван", None, &HashMap::new(), true).unwrap();
        let ivan = c.participants.iter().position(|p| p.name == "Иван");
        let mimes: Vec<Option<&str>> = c.messages[..2]
            .iter()
            .map(|m| {
                assert!(!m.system && m.author == ivan, "{m:?}");
                m.attachments[0].mime.as_deref()
            })
            .collect();
        assert_eq!(mimes, [Some("image/jpeg"), Some("video/mp4")]);
        assert!(c.messages[2].deleted && c.messages[3].deleted);
        assert!(!c.messages[2].system && c.messages[2].author == ivan);
        // A real notice still is one.
        assert!(c.messages[4].system);

        let att = |l: &str| match classify_media(l) {
            Some(Media::Attachment(a)) => a.mime,
            other => panic!("{l}: {other:?}"),
        };
        for (l, mime) in [
            ("\u{200e}Bild weggelassen", "image/jpeg"),
            ("\u{200e}GIF omitido", "image/gif"),
            ("\u{200e}figurinha omitida", "image/webp"),
        ] {
            assert_eq!(att(l).as_deref(), Some(mime), "{l}");
        }
        for l in [
            "Diese Nachricht wurde gelöscht",
            "Se eliminó este mensaje.",
            "Ce message a été supprimé",
            "Mensagem apagada",
            "\u{200e}You deleted this message.",
            "Вы удалили данное сообщение",
        ] {
            assert_eq!(classify_media(l), Some(Media::Deleted), "{l}");
        }
        // Without the iOS mark, a sentence ending in "omitted" stays text.
        assert_eq!(classify_media("nothing important omitted"), None);
    }

    /// Review regression: the `<…>` placeholder rule turned a typed `<sigh>` or `<evil laugh>`
    /// into an attachment and dropped its text.
    #[test]
    fn typed_angle_bracket_messages_stay_text() {
        let text = "14/03/2026, 09:06 - Ana: <sigh>\n\
14/03/2026, 09:07 - Ana: <evil laugh>\n\
14/03/2026, 09:08 - Ana: <Sigh>\n\
14/03/2026, 09:09 - Ana: <メディアは含まれていません>\n\
14/03/2026, 09:10 - Ana: <Media weggelaten>\n";
        let c = parse_chat(text, "wa:x", "X", None, &HashMap::new(), true).unwrap();
        let texts: Vec<&str> = c.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["<sigh>", "<evil laugh>", "<Sigh>", "", ""]);
        assert!(c.messages[3].attachments.len() == 1 && c.messages[4].attachments.len() == 1);
    }

    /// Review regression: with words allowed between date and time, a pasted line such as
    /// `14/03/2026 at 09:06 - Bob: hi` in a file of `14/03/2026, 09:06` headers became a
    /// message from Bob. Headers must share the file's separator style.
    #[test]
    fn pasted_header_of_another_separator_style_stays_text() {
        let text = "14/03/2026, 09:06 - Ana: forwarding:\n\
14/03/2026 at 08:00 - Bob: the plan\n\
14/03/2026, 09:07 - Ana: ok\n";
        let c = parse_chat(text, "wa:x", "X", None, &HashMap::new(), true).unwrap();
        let texts: Vec<&str> = c.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            ["forwarding:\n14/03/2026 at 08:00 - Bob: the plan", "ok"]
        );
        // And the other way round, in a French export.
        let text = "14/03/2026 à 09:06 - Chloé: regarde\n\
14/03/2026, 08:00 - Bob: the plan\n\
14/03/2026 à 09:07 - Chloé: ok\n";
        let c = parse_chat(text, "wa:x", "X", None, &HashMap::new(), true).unwrap();
        assert_eq!(c.messages.len(), 2);
        assert_eq!(
            c.messages[0].text,
            "regarde\n14/03/2026, 08:00 - Bob: the plan"
        );
        // Korean markers between date and time are not words.
        assert!(
            !parse_header("2023. 12. 31. 오후 9:15 - a: b")
                .unwrap()
                .stamp
                .worded
        );
    }
}
