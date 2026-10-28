//! Text extraction and structure-aware chunking.
//!
//! **Contract (stable for M1):** [`extract`], [`detect_mime`], [`chunk`], [`estimate_tokens`] and
//! the types below. Work package M1-A replaces the minimal implementation behind them.
//!
//! Inputs are hostile: every function enforces the size caps in [`ExtractOptions`] and returns
//! `Error::Unsupported` / `Error::Invalid` rather than panicking. Extraction is CPU-bound and
//! synchronous; callers run it on a blocking thread with a timeout.

use std::path::Path;

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// Where the bytes come from.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
    File(&'a Path),
}

/// Limits for one extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Refuse inputs larger than this.
    pub max_input_bytes: u64,
    /// Truncate extracted text beyond this many chars (`Extracted::truncated` is set).
    pub max_text_chars: usize,
    /// Run OCR on images when OCR models are available.
    pub ocr: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_text_chars: 2_000_000,
            ocr: true,
        }
    }
}

/// A heading-delimited region of [`Extracted::text`] (byte offsets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// Heading titles from the outermost to this section's own, e.g. `["Guide", "Install"]`.
    pub heading_path: Vec<String>,
    pub start: usize,
    pub end: usize,
}

/// Result of extraction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Extracted {
    pub title: Option<String>,
    /// Plain text (Markdown kept as-is for Markdown inputs).
    pub text: String,
    /// Structure, if the format has any. May be empty.
    pub sections: Vec<Section>,
    /// Outgoing links (URLs, `[[wikilinks]]` targets).
    pub links: Vec<String>,
    /// Tags (`#tag`, frontmatter `tags`, mail labels...).
    pub tags: Vec<String>,
    /// Format-specific fields (frontmatter, email headers, event times...). JSON object.
    pub metadata: serde_json::Value,
    /// Set when `text` was cut at `max_text_chars`.
    pub truncated: bool,
}

/// Extract text and structure from `source`, interpreted as `mime`.
pub fn extract(mime: &str, source: Source<'_>, opts: &ExtractOptions) -> Result<Extracted> {
    let base = mime.split(';').next().unwrap_or("").trim();
    if !(base.starts_with("text/") || base == "application/json") {
        return Err(Error::Unsupported(format!("no extractor for {base}")));
    }
    let raw = match source {
        Source::Text(t) => t.to_owned(),
        Source::Bytes(b) => {
            check_size(b.len() as u64, opts)?;
            String::from_utf8_lossy(b).into_owned()
        }
        Source::File(p) => {
            check_size(std::fs::metadata(p)?.len(), opts)?;
            String::from_utf8_lossy(&std::fs::read(p)?).into_owned()
        }
    };
    let mut out = Extracted {
        text: raw,
        metadata: serde_json::Value::Object(Default::default()),
        ..Default::default()
    };
    if out.text.chars().count() > opts.max_text_chars {
        out.text = out.text.chars().take(opts.max_text_chars).collect();
        out.truncated = true;
    }
    Ok(out)
}

fn check_size(len: u64, opts: &ExtractOptions) -> Result<()> {
    if len > opts.max_input_bytes {
        return Err(Error::Invalid(format!(
            "input is {len} bytes, over the {} byte limit",
            opts.max_input_bytes
        )));
    }
    Ok(())
}

/// Best-effort MIME type from a file name and its first bytes.
pub fn detect_mime(path: Option<&Path>, head: &[u8]) -> String {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("md" | "markdown") => "text/markdown",
        Some("txt") => "text/plain",
        Some("pdf") => "application/pdf",
        _ if head.starts_with(b"%PDF") => "application/pdf",
        _ if std::str::from_utf8(head).is_ok() => "text/plain",
        _ => "application/octet-stream",
    }
    .to_owned()
}

/// Chunking parameters, in estimated tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkOptions {
    pub target_tokens: usize,
    pub max_tokens: usize,
    pub overlap_tokens: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            target_tokens: 400,
            max_tokens: 512,
            overlap_tokens: 48,
        }
    }
}

/// One chunk of an extracted document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkOut {
    /// Chunk text as indexed (may include a heading-path prefix for context).
    pub text: String,
    pub heading_path: Vec<String>,
    /// Byte range in [`Extracted::text`] this chunk covers.
    pub start: usize,
    pub end: usize,
    pub tokens: usize,
}

/// Split an extracted document into retrieval-sized chunks. Deterministic: the same input always
/// yields the same chunks.
pub fn chunk(doc: &Extracted, opts: &ChunkOptions) -> Vec<ChunkOut> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut cur = String::new();
    let mut pos = 0;
    for para in doc.text.split_inclusive("\n\n") {
        if !cur.is_empty() && estimate_tokens(&cur) + estimate_tokens(para) > opts.target_tokens {
            out.push(make(&cur, start, pos));
            cur.clear();
            start = pos;
        }
        cur.push_str(para);
        pos += para.len();
    }
    if !cur.trim().is_empty() {
        out.push(make(&cur, start, pos));
    }
    out
}

fn make(text: &str, start: usize, end: usize) -> ChunkOut {
    ChunkOut {
        text: text.trim().to_owned(),
        heading_path: Vec::new(),
        start,
        end,
        tokens: estimate_tokens(text),
    }
}

/// Rough token count (about 4 chars per token for Latin text).
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}
