//! PDF text per page via `pdf-extract` (on `lopdf`). Runs on a dedicated thread with a large
//! stack, so deep object graphs cannot overflow the caller's stack and panics in the parser are
//! contained.

use nugget_core::{Error, Result};
use pdf_extract::{Document, Object, PlainTextOutput, output_doc_page};
use serde_json::Value;

use crate::util::{self, SectionTracker, TextBuf};
use crate::{Ctx, Extracted};

/// Pages read at most.
const MAX_PAGES: usize = 5_000;
/// Stack for the parser thread.
const PDF_STACK: usize = 64 * 1024 * 1024;

pub(crate) fn extract(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    let head = &data[..data.len().min(1024)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(Error::Invalid("not a PDF (no %PDF- header)".into()));
    }
    let max_chars = ctx.opts.max_text_chars;
    std::thread::scope(|s| {
        let handle = std::thread::Builder::new()
            .name("nugget-extract-pdf".into())
            .stack_size(PDF_STACK)
            .spawn_scoped(s, || run(data, max_chars))
            .map_err(|e| Error::Other(format!("cannot start PDF thread: {e}")))?;
        match handle.join() {
            Ok(r) => r,
            Err(p) => Err(Error::Invalid(format!(
                "pdf extractor panicked on malformed input: {}",
                util::panic_message(p.as_ref())
            ))),
        }
    })
}

/// Decode a PDF text string (UTF-16 with BOM, UTF-8 with BOM, else PDFDocEncoding ≈ Latin-1).
fn text_string(bytes: &[u8]) -> String {
    if bytes.starts_with(b"\xFE\xFF") || bytes.starts_with(b"\xFF\xFE") {
        return util::decode_text(bytes, None);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn info_string(doc: &Document, key: &[u8]) -> Option<String> {
    let info = doc.trailer.get(b"Info").ok()?;
    let (_, info) = doc.dereference(info).ok()?;
    let v = info.as_dict().ok()?.get(key).ok()?;
    let (_, v) = doc.dereference(v).ok()?;
    let s = match v {
        Object::String(bytes, _) => text_string(bytes),
        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
        _ => return None,
    };
    let s = util::truncate_chars(s.trim().trim_matches('\0'), 2_000);
    (!s.is_empty()).then_some(s)
}

/// Trim line ends and collapse runs of blank lines.
fn clean(page: &str) -> String {
    let mut out = String::with_capacity(page.len());
    let mut blank = 0;
    for line in page.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

fn run(data: &[u8], max_chars: usize) -> Result<Extracted> {
    let mut doc =
        Document::load_mem(data).map_err(|e| Error::Invalid(format!("malformed PDF: {e}")))?;
    if doc.is_encrypted() && doc.decrypt("").is_err() {
        return Err(Error::Unsupported("password-protected PDF".into()));
    }
    let mut meta = util::obj();
    for (key, field) in [
        (&b"Author"[..], "author"),
        (b"Subject", "subject"),
        (b"Keywords", "keywords"),
        (b"Creator", "creator"),
        (b"Producer", "producer"),
        (b"CreationDate", "created"),
        (b"ModDate", "modified"),
    ] {
        if let Some(v) = info_string(&doc, key) {
            meta.insert(field.into(), Value::String(v));
        }
    }
    let title = info_string(&doc, b"Title");
    let pages = doc.get_pages();
    meta.insert("pages".into(), Value::from(pages.len()));

    let mut buf = TextBuf::new(max_chars);
    let mut tracker = SectionTracker::default();
    let mut errors = 0usize;
    let mut first_error = None;
    for &num in pages.keys().take(MAX_PAGES) {
        if buf.is_full() {
            break;
        }
        let mut raw = String::new();
        // A malformed page can make the parser panic; lose that page, not the document.
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut out = PlainTextOutput::new(&mut raw);
            output_doc_page(&doc, &mut out, num).map_err(|e| format!("{e:?}"))
        }))
        .unwrap_or_else(|p| {
            Err(format!(
                "parser panicked: {}",
                util::panic_message(p.as_ref())
            ))
        });
        if let Err(e) = res {
            errors += 1;
            first_error.get_or_insert(e);
            continue;
        }
        let page = clean(&raw);
        if page.is_empty() {
            continue;
        }
        buf.paragraph();
        tracker.start(vec![format!("Page {num}")], buf.len());
        buf.push(&page);
    }
    if pages.len() > MAX_PAGES {
        meta.insert("pages_truncated".into(), Value::Bool(true));
    }
    if errors > 0 {
        meta.insert("page_errors".into(), Value::from(errors));
        if buf.len() == 0 {
            return Err(Error::Invalid(format!(
                "no readable PDF page: {}",
                first_error.unwrap_or_default()
            )));
        }
    }
    let truncated = buf.is_full() || pages.len() > MAX_PAGES;
    let (text, cut) = buf.finish();
    if text.is_empty() {
        // Typically a scanned PDF without a text layer.
        meta.insert("no_text_layer".into(), Value::Bool(true));
    }
    let sections = tracker.finish(text.len());
    let mut links = Vec::new();
    util::find_urls(&text, |_, u| {
        util::push_unique(&mut links, u, util::LINK_CAP)
    });
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
