//! DOCX and XLSX: ZIP containers of XML parts, parsed with streaming readers.
//!
//! Zip-bomb guards: at most [`MAX_ENTRIES`] entries (checked from the end-of-central-directory
//! record before the archive is opened), every part is inflated through a reader that fails
//! once the part exceeds its cap, and all parts of one document share [`MAX_TOTAL_INFLATE`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor, Read};

use nugget_core::{Error, Result};
use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};
use serde_json::{Map, Value, json};
use zip::ZipArchive;

use crate::tabular::render_row;
use crate::util::{self, LINK_CAP, SectionTracker, TextBuf};
use crate::{Ctx, Extracted};

/// Most entries in an OOXML archive.
const MAX_ENTRIES: usize = 10_000;
/// Inflated size cap for metadata parts (rels, styles, workbook, core properties).
const MAX_SMALL_PART: u64 = 16 * 1024 * 1024;
/// Inflated size cap for the main document part and the shared-strings table.
const MAX_MAIN_PART: u64 = 128 * 1024 * 1024;
/// Inflated size cap for one worksheet (streamed; row caps usually stop reading much earlier).
const MAX_SHEET_PART: u64 = 256 * 1024 * 1024;
/// Inflated bytes across all parts of one document.
const MAX_TOTAL_INFLATE: u64 = 512 * 1024 * 1024;
/// Spreadsheet caps.
const MAX_SHEETS: usize = 256;
const MAX_ROWS_PER_SHEET: usize = 100_000;
const MAX_COLS: usize = 512;
const MAX_CELLS: usize = 2_000_000;
const MAX_SHARED_STRINGS: usize = 2_000_000;
const MAX_SHARED_CHARS: usize = 64 * 1024 * 1024;
/// Deepest nesting of paragraphs (text boxes inside paragraphs) tracked in DOCX.
const MAX_PARA_DEPTH: usize = 32;

const BOMB: &str = "nugget-extract: inflate limit exceeded";

/// A reader that errors once more than `left` bytes come through.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(std::io::Error::other(BOMB)),
            };
        }
        let max = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= n as u64;
        Ok(n)
    }
}

fn xml_err(part: &str, e: impl std::fmt::Display) -> Error {
    let msg = e.to_string();
    if msg.contains(BOMB) {
        Error::Invalid(format!(
            "{part}: inflates beyond its size cap (zip bomb guard)"
        ))
    } else {
        Error::Invalid(format!("{part}: malformed XML: {msg}"))
    }
}

/// Reject archives that declare more than [`MAX_ENTRIES`] entries, before the ZIP reader
/// allocates for them.
fn check_entry_count(data: &[u8]) -> Result<()> {
    let from = data.len().saturating_sub(22 + 65_535);
    let Some(rel) = data[from..].windows(4).rposition(|w| w == b"PK\x05\x06") else {
        return Ok(()); // the ZIP reader reports the missing directory
    };
    let p = from + rel;
    let u16_at = |i: usize| data.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u64_at = |i: usize| {
        data.get(i..i + 8)
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_le_bytes)
    };
    let mut total = u64::from(u16_at(p + 10).unwrap_or(0));
    if total == 0xFFFF
        && p >= 20
        && data[p - 20..].starts_with(b"PK\x06\x07")
        && let Some(off) = u64_at(p - 20 + 8).and_then(|o| usize::try_from(o).ok())
        && data
            .get(off..)
            .is_some_and(|d| d.starts_with(b"PK\x06\x06"))
    {
        total = u64_at(off + 32).unwrap_or(total);
    }
    if total > MAX_ENTRIES as u64 {
        return Err(Error::Invalid(format!(
            "archive declares {total} entries, over the {MAX_ENTRIES} limit"
        )));
    }
    Ok(())
}

struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    budget: u64,
}

impl<'a> Package<'a> {
    fn open(data: &'a [u8], what: &str) -> Result<Self> {
        check_entry_count(data)?;
        let zip = ZipArchive::new(Cursor::new(data))
            .map_err(|e| Error::Invalid(format!("{what}: not a valid ZIP archive: {e}")))?;
        if zip.len() > MAX_ENTRIES {
            return Err(Error::Invalid(format!(
                "{what}: archive has {} entries, over the {MAX_ENTRIES} limit",
                zip.len()
            )));
        }
        Ok(Self {
            zip,
            budget: MAX_TOTAL_INFLATE,
        })
    }

    /// The stored name matching `name` case-insensitively.
    fn find(&self, name: &str) -> Option<String> {
        let name = name.trim_start_matches('/');
        if self.zip.index_for_name(name).is_some() {
            return Some(name.to_owned());
        }
        self.zip
            .file_names()
            .flatten()
            .find(|n| n.eq_ignore_ascii_case(name))
            .map(|n| n.into_owned())
    }

    /// Stream part `name` through `f`, inflating at most `cap` bytes. `Ok(None)` if absent.
    fn with_part<T>(
        &mut self,
        name: &str,
        cap: u64,
        f: impl FnOnce(&mut Reader<BufReader<&mut dyn Read>>) -> Result<T>,
    ) -> Result<Option<T>> {
        let Some(stored) = self.find(name) else {
            return Ok(None);
        };
        let cap = cap.min(self.budget);
        let entry = self
            .zip
            .by_name(&stored)
            .map_err(|e| Error::Invalid(format!("{name}: unreadable ZIP entry: {e}")))?;
        if entry.size() > cap {
            return Err(Error::Invalid(format!(
                "{name}: declares {} inflated bytes, over the {cap} byte cap (zip bomb guard)",
                entry.size()
            )));
        }
        let mut capped = Capped {
            inner: entry,
            left: cap,
        };
        let result = {
            let dyn_read: &mut dyn Read = &mut capped;
            let mut reader = Reader::from_reader(BufReader::new(dyn_read));
            reader.config_mut().check_end_names = false;
            f(&mut reader)
        };
        self.budget -= cap - capped.left;
        result.map(Some)
    }
}

/// Text carried by an XML event (text, CDATA, entity and character references).
fn event_text<'e>(ev: &'e Event<'_>) -> Option<Cow<'e, str>> {
    match ev {
        Event::Text(t) => Some(t.xml10_content()),
        Event::CData(c) => Some(c.xml10_content()),
        Event::GeneralRef(r) => resolve_ref(r).map(|c| Cow::Owned(c.to_string())),
        _ => None,
    }
}

fn resolve_ref(r: &BytesRef<'_>) -> Option<char> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c);
    }
    match &**r {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

fn local(e: &BytesStart<'_>) -> String {
    e.local_name().as_ref().to_owned()
}

/// Attribute by local name (namespace prefix ignored).
fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().with_checks(false).flatten().find_map(|a| {
        (a.key.local_name().as_ref() == name).then(|| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(Cow::into_owned)
                .unwrap_or_else(|_| a.value.into_owned())
        })
    })
}

/// Drive `reader` to the end, calling `f` for each event.
fn for_each<R: BufRead>(
    reader: &mut Reader<R>,
    part: &str,
    mut f: impl FnMut(&Event<'_>) -> bool,
) -> Result<()> {
    let mut buf = Vec::new();
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| xml_err(part, e))?;
        if matches!(ev, Event::Eof) || !f(&ev) {
            return Ok(());
        }
        buf.clear();
    }
}

/// `Id → (Target, external)` from a relationships part.
fn read_rels(pkg: &mut Package<'_>, name: &str) -> Result<HashMap<String, (String, String)>> {
    Ok(pkg
        .with_part(name, MAX_SMALL_PART, |r| {
            let mut map = HashMap::new();
            for_each(r, name, |ev| {
                if let Event::Start(e) | Event::Empty(e) = ev
                    && local(e) == "Relationship"
                    && map.len() < 100_000
                    && let (Some(id), Some(target)) = (attr(e, "Id"), attr(e, "Target"))
                {
                    let ty = attr(e, "Type").unwrap_or_default();
                    map.insert(id, (target, ty));
                }
                true
            })?;
            Ok(map)
        })?
        .unwrap_or_default())
}

/// Core properties (`docProps/core.xml`): title and a few metadata fields.
fn read_core(pkg: &mut Package<'_>, meta: &mut Map<String, Value>) -> Result<Option<String>> {
    let fields = pkg
        .with_part("docProps/core.xml", MAX_SMALL_PART, |r| {
            let mut fields: Vec<(String, String)> = Vec::new();
            let mut cur: Option<(String, String)> = None;
            for_each(r, "docProps/core.xml", |ev| {
                match ev {
                    Event::Start(e) => {
                        let name = local(e);
                        if matches!(
                            name.as_str(),
                            "title"
                                | "creator"
                                | "subject"
                                | "description"
                                | "keywords"
                                | "created"
                                | "modified"
                                | "lastModifiedBy"
                        ) {
                            cur = Some((name, String::new()));
                        }
                    }
                    Event::End(_) => {
                        if let Some((k, v)) = cur.take() {
                            fields.push((k, v));
                        }
                    }
                    other => {
                        if let (Some((_, v)), Some(t)) = (cur.as_mut(), event_text(other))
                            && v.len() < 8_192
                        {
                            v.push_str(&t);
                        }
                    }
                }
                true
            })?;
            Ok(fields)
        })
        .unwrap_or_default()
        .unwrap_or_default();
    let mut title = None;
    for (k, v) in fields {
        let v = util::normalize_ws(&v);
        if v.is_empty() {
            continue;
        }
        if k == "title" {
            title = Some(v);
        } else {
            let key = match k.as_str() {
                "creator" => "author",
                "lastModifiedBy" => "last_modified_by",
                other => other,
            };
            meta.insert(key.to_owned(), Value::String(v));
        }
    }
    Ok(title)
}

/// Resolve a relationship target relative to the directory of its source part.
fn resolve_target(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_owned();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Main part from the package relationships, falling back to `default`.
fn main_part(pkg: &mut Package<'_>, default: &str) -> Result<String> {
    let rels = read_rels(pkg, "_rels/.rels")?;
    let mut targets: Vec<_> = rels
        .values()
        .filter(|(_, ty)| ty.ends_with("/officeDocument"))
        .map(|(t, _)| resolve_target("", t))
        .collect();
    targets.sort();
    Ok(targets
        .into_iter()
        .find(|t| pkg.find(t).is_some())
        .unwrap_or_else(|| default.to_owned()))
}

fn split_part(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn rels_path(part: &str) -> String {
    let (dir, file) = split_part(part);
    if dir.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{dir}/_rels/{file}.rels")
    }
}

// ---------------------------------------------------------------------------------------------
// DOCX
// ---------------------------------------------------------------------------------------------

/// Heading level by style id: 0 = Title, 1..=9 = headings.
fn read_styles(pkg: &mut Package<'_>, part: &str) -> Result<HashMap<String, u32>> {
    Ok(pkg
        .with_part(part, MAX_SMALL_PART, |r| {
            let mut map = HashMap::new();
            let mut cur: Option<String> = None;
            for_each(r, part, |ev| {
                match ev {
                    Event::Start(e) | Event::Empty(e) => match local(e).as_str() {
                        "style" => cur = attr(e, "styleId"),
                        "name" => {
                            if let (Some(id), Some(name)) = (cur.as_ref(), attr(e, "val"))
                                && let Some(level) = level_from_name(&name)
                            {
                                map.entry(id.clone()).or_insert(level);
                            }
                        }
                        "outlineLvl" => {
                            if let (Some(id), Some(v)) = (cur.as_ref(), attr(e, "val"))
                                && let Ok(v) = v.parse::<u32>()
                                && v < 9
                            {
                                map.entry(id.clone()).or_insert(v + 1);
                            }
                        }
                        _ => {}
                    },
                    Event::End(e) if e.local_name().as_ref() == "style" => cur = None,
                    _ => {}
                }
                map.len() < 100_000
            })?;
            Ok(map)
        })?
        .unwrap_or_default())
}

/// `Title` → 0, `heading 2` / `Heading2` → 2.
fn level_from_name(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if compact == "title" {
        return Some(0);
    }
    let n = compact.strip_prefix("heading")?.parse::<u32>().ok()?;
    (1..=9).contains(&n).then_some(n)
}

#[derive(Default)]
struct Para {
    text: String,
    style: Option<String>,
    outline: Option<u32>,
}

pub(crate) fn extract_docx(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    let mut pkg = Package::open(data, "docx")?;
    let doc_part = main_part(&mut pkg, "word/document.xml")?;
    if pkg.find(&doc_part).is_none() {
        return Err(Error::Invalid(
            "docx: no main document part (word/document.xml)".into(),
        ));
    }
    let (dir, _) = split_part(&doc_part);
    let rels = read_rels(&mut pkg, &rels_path(&doc_part))?;
    let styles_part = rels
        .values()
        .find(|(_, ty)| ty.ends_with("/styles"))
        .map(|(t, _)| resolve_target(dir, t))
        .unwrap_or_else(|| format!("{dir}/styles.xml"));
    let styles = read_styles(&mut pkg, &styles_part)?;
    let mut meta = util::obj();
    let core_title = read_core(&mut pkg, &mut meta)?;

    let mut buf = TextBuf::new(ctx.opts.max_text_chars);
    let mut tracker = SectionTracker::default();
    let mut links = Vec::new();
    let mut style_title: Option<String> = None;
    let mut paras: Vec<Para> = Vec::new();
    let mut in_t = false;
    let mut in_run = 0usize;
    let mut fallback = 0usize;
    let mut tbl = 0usize;
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut tables = 0usize;

    let level_of = |p: &Para| -> Option<u32> {
        if let Some(o) = p.outline {
            return Some(o);
        }
        let style = p.style.as_deref()?;
        styles
            .get(style)
            .copied()
            .or_else(|| level_from_name(style))
    };

    pkg.with_part(&doc_part, MAX_MAIN_PART, |r| {
        for_each(r, &doc_part, |ev| {
            if fallback > 0 {
                match ev {
                    Event::Start(e) if e.local_name().as_ref() == "Fallback" => fallback += 1,
                    Event::End(e) if e.local_name().as_ref() == "Fallback" => fallback -= 1,
                    _ => {}
                }
                return true;
            }
            match ev {
                Event::Start(e) => match e.local_name().as_ref() {
                    "Fallback" => fallback = 1,
                    "p" => {
                        if paras.len() < MAX_PARA_DEPTH {
                            paras.push(Para::default());
                        }
                    }
                    "r" => in_run += 1,
                    "t" => in_t = true,
                    "tbl" => {
                        tbl += 1;
                        if tbl == 1 {
                            tables += 1;
                            buf.paragraph();
                        }
                    }
                    "tr" if tbl == 1 => row.clear(),
                    "tc" if tbl == 1 => cell.clear(),
                    "hyperlink" => {
                        if let Some(target) = attr(e, "id").and_then(|id| rels.get(&id)) {
                            util::push_unique(&mut links, &target.0, LINK_CAP);
                        }
                    }
                    "pStyle" | "outlineLvl" => style_attr(e, &mut paras),
                    _ => {}
                },
                Event::Empty(e) => match e.local_name().as_ref() {
                    "pStyle" | "outlineLvl" => style_attr(e, &mut paras),
                    "tab" if in_run > 0 => {
                        if let Some(p) = paras.last_mut() {
                            p.text.push('\t');
                        }
                    }
                    "br" | "cr" if in_run > 0 => {
                        if let Some(p) = paras.last_mut() {
                            p.text.push('\n');
                        }
                    }
                    _ => {}
                },
                Event::End(e) => match e.local_name().as_ref() {
                    "t" => in_t = false,
                    "r" => in_run = in_run.saturating_sub(1),
                    "p" => {
                        let Some(p) = paras.pop() else { return true };
                        let text = p.text.trim();
                        if text.is_empty() {
                            return true;
                        }
                        if tbl > 0 {
                            if cell.len() < 64 * 1024 {
                                if !cell.is_empty() {
                                    cell.push(' ');
                                }
                                cell.push_str(text);
                            }
                            return true;
                        }
                        match level_of(&p) {
                            Some(0) => {
                                if style_title.is_none() {
                                    style_title = Some(util::normalize_ws(text));
                                }
                                buf.paragraph();
                                buf.push(text);
                            }
                            Some(level) => {
                                buf.paragraph();
                                tracker.heading(level, text, buf.len());
                                buf.push(text);
                            }
                            None => {
                                buf.paragraph();
                                buf.push(text);
                            }
                        }
                    }
                    "tc" if tbl == 1 => {
                        if row.len() < MAX_COLS {
                            row.push(std::mem::take(&mut cell));
                        }
                    }
                    "tr" if tbl == 1 => {
                        if let Some(line) = render_row(&row) {
                            buf.newline();
                            buf.push(&line);
                        }
                        row.clear();
                    }
                    "tbl" => {
                        tbl = tbl.saturating_sub(1);
                        if tbl == 0 {
                            buf.paragraph();
                        }
                    }
                    _ => {}
                },
                other => {
                    if in_t && let (Some(p), Some(t)) = (paras.last_mut(), event_text(other)) {
                        p.text.push_str(&t);
                    }
                }
            }
            !buf.is_full()
        })
    })?;

    if tables > 0 {
        meta.insert("tables".into(), Value::from(tables));
    }
    let truncated = buf.is_full();
    let (text, cut) = buf.finish();
    let sections = tracker.finish(text.len());
    let links = links
        .into_iter()
        .filter(|l: &String| !l.is_empty())
        .collect();
    Ok(Extracted {
        title: style_title.or(core_title),
        text,
        sections,
        links,
        tags: Vec::new(),
        metadata: Value::Object(meta),
        truncated: truncated || cut,
    })
}

fn style_attr(e: &BytesStart<'_>, paras: &mut [Para]) {
    let Some(p) = paras.last_mut() else { return };
    let Some(val) = attr(e, "val") else { return };
    if e.local_name().as_ref() == "pStyle" {
        p.style = Some(val);
    } else if let Ok(v) = val.parse::<u32>()
        && v < 9
    {
        p.outline = Some(v + 1);
    }
}

// ---------------------------------------------------------------------------------------------
// XLSX
// ---------------------------------------------------------------------------------------------

struct SharedStrings {
    strings: Vec<String>,
    truncated: bool,
}

fn read_shared_strings(pkg: &mut Package<'_>, part: &str) -> Result<SharedStrings> {
    Ok(pkg
        .with_part(part, MAX_MAIN_PART, |r| {
            let mut out = SharedStrings {
                strings: Vec::new(),
                truncated: false,
            };
            let mut chars = 0usize;
            let mut cur = String::new();
            let mut in_t = false;
            let mut in_rph = 0usize;
            for_each(r, part, |ev| {
                match ev {
                    Event::Start(e) => match e.local_name().as_ref() {
                        "si" => cur.clear(),
                        "t" => in_t = true,
                        "rPh" => in_rph += 1,
                        _ => {}
                    },
                    Event::Empty(e) if e.local_name().as_ref() == "si" => {
                        out.strings.push(String::new());
                    }
                    Event::End(e) => match e.local_name().as_ref() {
                        "t" => in_t = false,
                        "rPh" => in_rph = in_rph.saturating_sub(1),
                        "si" => {
                            chars += cur.len();
                            out.strings.push(std::mem::take(&mut cur));
                        }
                        _ => {}
                    },
                    other => {
                        if in_t
                            && in_rph == 0
                            && cur.len() < 32 * 1024
                            && let Some(t) = event_text(other)
                        {
                            cur.push_str(&t);
                        }
                    }
                }
                if out.strings.len() >= MAX_SHARED_STRINGS || chars >= MAX_SHARED_CHARS {
                    out.truncated = true;
                    return false;
                }
                true
            })?;
            Ok(out)
        })?
        .unwrap_or(SharedStrings {
            strings: Vec::new(),
            truncated: false,
        }))
}

/// Zero-based column of a cell reference like `AB12`.
fn column_of(r: &str) -> Option<usize> {
    let mut col = 0usize;
    let mut any = false;
    for c in r.chars() {
        if !c.is_ascii_alphabetic() {
            break;
        }
        any = true;
        col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
        if col > 16_384 {
            return None;
        }
    }
    any.then(|| col - 1)
}

fn format_number(v: &str) -> String {
    match v.trim().parse::<f64>() {
        Ok(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
        Ok(f) if f.is_finite() => format!("{f}"),
        _ => v.trim().to_owned(),
    }
}

struct SheetInfo {
    name: String,
    rid: Option<String>,
    hidden: bool,
}

pub(crate) fn extract_xlsx(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    let mut pkg = Package::open(data, "xlsx")?;
    let wb_part = main_part(&mut pkg, "xl/workbook.xml")?;
    let sheets: Vec<SheetInfo> = pkg
        .with_part(&wb_part, MAX_SMALL_PART, |r| {
            let mut sheets = Vec::new();
            for_each(r, &wb_part, |ev| {
                if let Event::Start(e) | Event::Empty(e) = ev
                    && e.local_name().as_ref() == "sheet"
                    && sheets.len() < MAX_SHEETS * 4
                {
                    sheets.push(SheetInfo {
                        name: attr(e, "name").unwrap_or_default(),
                        rid: attr(e, "id"),
                        hidden: attr(e, "state").is_some_and(|s| s != "visible"),
                    });
                }
                true
            })?;
            Ok(sheets)
        })?
        .ok_or_else(|| Error::Invalid("xlsx: no workbook part (xl/workbook.xml)".into()))?;
    let (dir, _) = split_part(&wb_part);
    let rels = read_rels(&mut pkg, &rels_path(&wb_part))?;
    let ss_part = rels
        .values()
        .find(|(_, ty)| ty.ends_with("/sharedStrings"))
        .map(|(t, _)| resolve_target(dir, t))
        .unwrap_or_else(|| format!("{dir}/sharedStrings.xml"));
    let shared = read_shared_strings(&mut pkg, &ss_part)?;
    let mut meta = util::obj();
    let title = read_core(&mut pkg, &mut meta)?;

    let mut buf = TextBuf::new(ctx.opts.max_text_chars);
    let mut tracker = SectionTracker::default();
    let mut sheet_meta = Vec::new();
    let mut cells_left = MAX_CELLS;
    let mut any_truncated = shared.truncated || sheets.len() > MAX_SHEETS;
    for (i, sheet) in sheets.iter().take(MAX_SHEETS).enumerate() {
        if buf.is_full() || cells_left == 0 {
            any_truncated = true;
            break;
        }
        let target = sheet
            .rid
            .as_ref()
            .and_then(|id| rels.get(id))
            .map(|(t, _)| resolve_target(dir, t))
            .unwrap_or_else(|| format!("{dir}/worksheets/sheet{}.xml", i + 1));
        let name = if sheet.name.trim().is_empty() {
            format!("Sheet{}", i + 1)
        } else {
            util::normalize_ws(&sheet.name)
        };
        buf.paragraph();
        tracker.start(vec![name.clone()], buf.len());
        buf.push(&name);
        let stats = pkg.with_part(&target, MAX_SHEET_PART, |r| {
            read_sheet(r, &target, &shared.strings, &mut buf, &mut cells_left)
        })?;
        let Some((rows, cols, truncated)) = stats else {
            continue;
        };
        any_truncated |= truncated;
        let mut m = json!({"name": name, "rows": rows, "columns": cols});
        if truncated {
            m["truncated"] = Value::Bool(true);
        }
        if sheet.hidden {
            m["hidden"] = Value::Bool(true);
        }
        sheet_meta.push(m);
    }
    meta.insert("sheets".into(), Value::Array(sheet_meta));
    if any_truncated {
        meta.insert("rows_truncated".into(), Value::Bool(true));
    }
    let truncated = buf.is_full();
    let (text, cut) = buf.finish();
    let sections = tracker.finish(text.len());
    let mut links = Vec::new();
    util::find_urls(&text, |_, u| util::push_unique(&mut links, u, LINK_CAP));
    Ok(Extracted {
        title,
        text,
        sections,
        links,
        tags: Vec::new(),
        metadata: Value::Object(meta),
        truncated: truncated || cut,
    })
}

/// Stream one worksheet into `buf`. Returns `(rows, max columns, truncated)`.
fn read_sheet<R: BufRead>(
    r: &mut Reader<R>,
    part: &str,
    shared: &[String],
    buf: &mut TextBuf,
    cells_left: &mut usize,
) -> Result<(usize, usize, bool)> {
    let mut rows = 0usize;
    let mut max_cols = 0usize;
    let mut truncated = false;
    let mut cells: Vec<String> = Vec::new();
    let mut col = 0usize;
    let mut ty = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut in_cell = false;
    for_each(r, part, |ev| {
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    "row" => {
                        cells.clear();
                        col = 0;
                    }
                    "c" => {
                        if let Some(c) = attr(e, "r").and_then(|r| column_of(&r)) {
                            col = c;
                        }
                        ty = attr(e, "t").unwrap_or_default();
                        value.clear();
                        in_cell = !empty;
                        if empty {
                            col += 1;
                        }
                    }
                    "v" | "t" if in_cell && !empty => in_value = true,
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "v" | "t" => in_value = false,
                "c" => {
                    in_cell = false;
                    let v = match ty.as_str() {
                        "s" => value
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i))
                            .cloned()
                            .unwrap_or_default(),
                        "b" => match value.trim() {
                            "1" => "TRUE".to_owned(),
                            "0" => "FALSE".to_owned(),
                            other => other.to_owned(),
                        },
                        "str" | "inlineStr" | "e" => std::mem::take(&mut value),
                        _ => format_number(&value),
                    };
                    if col < MAX_COLS && *cells_left > 0 {
                        if cells.len() <= col {
                            cells.resize(col + 1, String::new());
                        }
                        cells[col] = v;
                        *cells_left -= 1;
                    } else {
                        truncated = true;
                    }
                    col += 1;
                }
                "row" => {
                    if let Some(line) = render_row(&cells) {
                        max_cols = max_cols.max(cells.len());
                        buf.newline();
                        buf.push(&line);
                        rows += 1;
                    }
                    cells.clear();
                    if rows >= MAX_ROWS_PER_SHEET || *cells_left == 0 {
                        truncated = true;
                        return false;
                    }
                }
                _ => {}
            },
            other => {
                if in_value
                    && value.len() < 64 * 1024
                    && let Some(t) = event_text(other)
                {
                    value.push_str(&t);
                }
            }
        }
        !buf.is_full()
    })?;
    Ok((rows, max_cols, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_reader_stops_inflation() {
        let mut ok = Capped {
            inner: Cursor::new(vec![b'a'; 10]),
            left: 10,
        };
        let mut out = Vec::new();
        assert_eq!(ok.read_to_end(&mut out).unwrap(), 10);
        let mut over = Capped {
            inner: Cursor::new(vec![b'a'; 11]),
            left: 10,
        };
        let err = over.read_to_end(&mut Vec::new()).unwrap_err();
        let mapped = xml_err("word/document.xml", err);
        assert!(matches!(mapped, Error::Invalid(m) if m.contains("zip bomb")));
    }

    #[test]
    fn columns() {
        assert_eq!(column_of("A1"), Some(0));
        assert_eq!(column_of("Z9"), Some(25));
        assert_eq!(column_of("AB12"), Some(27));
        assert_eq!(column_of("12"), None);
    }

    #[test]
    fn targets() {
        assert_eq!(
            resolve_target("xl", "worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(resolve_target("xl", "/xl/a.xml"), "xl/a.xml");
        assert_eq!(
            resolve_target("xl/worksheets", "../sharedStrings.xml"),
            "xl/sharedStrings.xml"
        );
        assert_eq!(
            rels_path("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(format_number("3"), "3");
        assert_eq!(format_number("3.1400000000000001"), "3.14");
        assert_eq!(format_number("abc"), "abc");
    }

    #[test]
    fn style_names() {
        assert_eq!(level_from_name("heading 2"), Some(2));
        assert_eq!(level_from_name("Heading1"), Some(1));
        assert_eq!(level_from_name("Title"), Some(0));
        assert_eq!(level_from_name("Normal"), None);
    }
}
