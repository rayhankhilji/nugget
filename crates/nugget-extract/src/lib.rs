//! Text extraction and structure-aware chunking.
//!
//! **Contract (stable for M1):** [`extract`], [`detect_mime`], [`chunk`], [`estimate_tokens`] and
//! the types below. Additions since the M1 contract: [`extract_with_ocr`], [`is_supported`],
//! [`OcrModels`], [`set_ocr_models`], [`ocr_models`], [`ocr_available`] and the `OCR_*` constants;
//! [`Source::CheckedFile`], [`open_regular`], [`FileExpect`], [`is_file_changed`],
//! [`FILE_CHANGED`] and [`FILE_CHECK_KEY`].
//!
//! Files ([`Source::File`], [`Source::CheckedFile`]) are opened with [`open_regular`]: a final
//! symlink is not followed, FIFOs and devices never block, and anything but a regular file is
//! refused with an error [`is_file_changed`] recognises.
//!
//! Inputs are hostile: every function enforces the size caps in [`ExtractOptions`] and returns
//! `Error::Unsupported` / `Error::Invalid` rather than panicking. Extraction is CPU-bound and
//! synchronous; callers run it on a blocking thread with a timeout.
//!
//! # Formats
//!
//! | MIME | Notes |
//! |---|---|
//! | `text/plain`, `text/*`, source code (`text/x-rust`, `application/json`, ...) | `metadata.language` from the extension or MIME |
//! | `text/markdown` | frontmatter → `metadata.frontmatter`; headings → sections; wikilinks, links, `#tags` |
//! | `text/html` | readable text, `<title>`, links, headings → sections; script/style dropped |
//! | `application/pdf` | text per page; one section per page (`["Page 3"]`) |
//! | DOCX | paragraphs and tables; heading styles → sections; hyperlinks |
//! | XLSX, CSV/TSV | one row per line (`a \| b \| c`); one section per sheet; row/column caps |
//! | `message/rfc822` | headers → metadata; text/plain preferred; quotes and signature → metadata |
//! | `text/calendar` | events rendered as text; uid/times → metadata |
//! | `image/png`, `image/jpeg`, `image/webp` | OCR via `ocrs`, only with `ocr` set and models present |
//!
//! Markdown frontmatter is removed from [`Extracted::text`] (it is in `metadata.frontmatter`);
//! otherwise Markdown text is kept as-is, except for text the reader never sees (below).
//!
//! # Hidden text
//!
//! Text a reader never sees is not extracted, so it is neither indexed nor handed to a model:
//! Markdown `<!-- -->` comments and `[//]: # (...)`-style comment link definitions (outside code),
//! HTML comments and elements hidden with the `hidden` attribute, `aria-hidden="true"` or an
//! inline `display:none` / `visibility:hidden` / `font-size:0` style (HTML files and HTML mail),
//! and, in every format, invisible format characters (zero-width spaces and joiners, word joiner,
//! BOM, bidi embeddings / overrides / isolates / marks, Unicode tag characters). Zero-width
//! (non-)joiners between two non-ASCII characters (emoji sequences, Indic and Arabic-script
//! shaping) are kept.
//!
//! Mail text starts with `From: <sender>` and `Date: <YYYY-MM-DD>` lines (each only when the
//! header is present and parsable) and a blank line; `metadata.text_header` holds that exact
//! prefix so consumers that want the body alone can strip it.
//!
//! # Panics
//!
//! No extractor panics on any input. As a last line of defence (third-party parsers, notably the
//! PDF one, contain panicking paths), each extraction runs under `catch_unwind` and a caught panic
//! becomes `Error::Invalid`; this requires the default `panic = "unwind"` strategy.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};

mod chunker;
mod html;
mod ics;
mod mail;
mod markdown;
mod mime;
mod ocr;
mod ooxml;
mod open;
mod pdf;
mod tabular;
mod text;
mod util;
mod yaml;

pub use open::{FILE_CHANGED, FILE_CHECK_KEY, FileExpect, is_file_changed, open_regular};

/// Where the bytes come from.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
    /// A local file, opened with [`open_regular`] (no final symlink, never blocks, regular files
    /// only).
    File(&'a Path),
    /// Like [`File`](Self::File), and the opened file must still have the size and modification
    /// time a connector scanned ([`FileExpect::from_metadata`] reads them from a document's
    /// metadata). A mismatch fails with an error [`is_file_changed`] recognises.
    CheckedFile {
        path: &'a Path,
        expect: FileExpect,
    },
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

/// Environment variable naming a directory that holds the OCR models
/// ([`OCR_DETECTION_MODEL_FILE`] and [`OCR_RECOGNITION_MODEL_FILE`]).
pub const OCR_MODELS_ENV: &str = "NUGGET_OCR_MODELS";
/// File name of the `ocrs` text-detection model inside an OCR model directory.
pub const OCR_DETECTION_MODEL_FILE: &str = "text-detection.rten";
/// File name of the `ocrs` text-recognition model inside an OCR model directory.
pub const OCR_RECOGNITION_MODEL_FILE: &str = "text-recognition.rten";

/// Paths of the two `ocrs` models (`.rten` format). Nugget never downloads them; the user or the
/// model manager places them on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcrModels {
    pub detection: PathBuf,
    pub recognition: PathBuf,
}

impl OcrModels {
    /// The standard file names inside `dir`.
    pub fn from_dir(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        Self {
            detection: dir.join(OCR_DETECTION_MODEL_FILE),
            recognition: dir.join(OCR_RECOGNITION_MODEL_FILE),
        }
    }

    /// Models from the directory in [`OCR_MODELS_ENV`], if that variable is set and non-empty.
    pub fn from_env() -> Option<Self> {
        let dir = std::env::var_os(OCR_MODELS_ENV)?;
        if dir.is_empty() {
            return None;
        }
        Some(Self::from_dir(PathBuf::from(dir)))
    }

    /// Both model files exist.
    pub fn exist(&self) -> bool {
        self.detection.is_file() && self.recognition.is_file()
    }
}

static OCR_MODELS: RwLock<Option<OcrModels>> = RwLock::new(None);

/// Set (or clear) the process-wide OCR models used by [`extract`]. When unset, [`extract`] falls
/// back to [`OCR_MODELS_ENV`].
pub fn set_ocr_models(models: Option<OcrModels>) {
    match OCR_MODELS.write() {
        Ok(mut g) => *g = models,
        Err(poisoned) => *poisoned.into_inner() = models,
    }
}

/// The OCR models [`extract`] would use: those set with [`set_ocr_models`], else
/// [`OcrModels::from_env`]. Existence is not checked.
pub fn ocr_models() -> Option<OcrModels> {
    let set = match OCR_MODELS.read() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    set.or_else(OcrModels::from_env)
}

/// Whether [`extract`] can OCR images: the `ocr` feature is compiled in and the model files of
/// [`ocr_models`] exist.
pub fn ocr_available() -> bool {
    cfg!(feature = "ocr") && ocr_models().is_some_and(|m| m.exist())
}

/// Whether [`extract`] has an extractor for `mime` (parameters are ignored). Images count as
/// supported even though they also need `ExtractOptions::ocr` and OCR models.
pub fn is_supported(mime: &str) -> bool {
    Kind::route(&mime::base(mime)).is_some()
}

/// Extract text and structure from `source`, interpreted as `mime`.
pub fn extract(mime: &str, source: Source<'_>, opts: &ExtractOptions) -> Result<Extracted> {
    let models = if opts.ocr { ocr_models() } else { None };
    run(mime, source, opts, models.as_ref())
}

/// [`extract`] with explicit OCR models instead of the process-wide / environment ones.
pub fn extract_with_ocr(
    mime: &str,
    source: Source<'_>,
    opts: &ExtractOptions,
    models: &OcrModels,
) -> Result<Extracted> {
    run(mime, source, opts, Some(models))
}

/// Extractor families, chosen by MIME.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Markdown,
    Html,
    Pdf,
    Docx,
    Xlsx,
    Csv,
    Mail,
    Calendar,
    Image,
}

impl Kind {
    fn route(base: &str) -> Option<Kind> {
        Some(match base {
            "text/markdown" | "text/x-markdown" => Kind::Markdown,
            "text/html" | "application/xhtml+xml" => Kind::Html,
            "text/calendar" | "application/ics" | "text/x-vcalendar" => Kind::Calendar,
            "text/csv" | "application/csv" | "text/tab-separated-values" | "text/tsv" => Kind::Csv,
            "message/rfc822" | "message/global" => Kind::Mail,
            "application/pdf" | "application/x-pdf" => Kind::Pdf,
            mime::DOCX => Kind::Docx,
            mime::XLSX | mime::XLSM => Kind::Xlsx,
            "image/png" | "image/jpeg" | "image/jpg" | "image/pjpeg" | "image/webp" => Kind::Image,
            _ if base.starts_with("text/") => Kind::Text,
            _ if mime::language_for_mime(base).is_some() => Kind::Text,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Markdown => "markdown",
            Kind::Html => "html",
            Kind::Pdf => "pdf",
            Kind::Docx => "docx",
            Kind::Xlsx => "xlsx",
            Kind::Csv => "csv",
            Kind::Mail => "mail",
            Kind::Calendar => "calendar",
            Kind::Image => "image",
        }
    }
}

/// Everything an extractor needs besides the bytes.
pub(crate) struct Ctx<'a> {
    pub opts: &'a ExtractOptions,
    /// Lower-cased MIME without parameters.
    pub mime: &'a str,
    /// `charset` parameter of the MIME type, if any.
    pub charset: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub ocr: Option<&'a OcrModels>,
}

fn run(
    mime: &str,
    source: Source<'_>,
    opts: &ExtractOptions,
    ocr: Option<&OcrModels>,
) -> Result<Extracted> {
    let base = mime::base(mime);
    let Some(kind) = Kind::route(&base) else {
        return Err(Error::Unsupported(format!("no extractor for {base}")));
    };
    let charset = mime::param(mime, "charset");
    let (data, path) = load(source, opts)?;
    let ctx = Ctx {
        opts,
        mime: &base,
        charset: charset.as_deref(),
        path,
        ocr: if opts.ocr { ocr } else { None },
    };
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(kind, &data, &ctx)));
    match result {
        Ok(Ok(out)) => Ok(finalize(out, opts)),
        Ok(Err(e)) => Err(e),
        Err(payload) => {
            let msg = util::panic_message(payload.as_ref());
            tracing::warn!(kind = kind.name(), "extractor panicked: {msg}");
            Err(Error::Invalid(format!(
                "{} extractor panicked on malformed input: {msg}",
                kind.name()
            )))
        }
    }
}

fn dispatch(kind: Kind, data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    match kind {
        Kind::Text => text::extract(data, ctx),
        Kind::Markdown => markdown::extract(data, ctx),
        Kind::Html => html::extract(data, ctx),
        Kind::Pdf => pdf::extract(data, ctx),
        Kind::Docx => ooxml::extract_docx(data, ctx),
        Kind::Xlsx => ooxml::extract_xlsx(data, ctx),
        Kind::Csv => tabular::extract_csv(data, ctx),
        Kind::Mail => mail::extract(data, ctx),
        Kind::Calendar => ics::extract(data, ctx),
        Kind::Image => ocr::extract(data, ctx),
    }
}

fn load<'a>(
    source: Source<'a>,
    opts: &ExtractOptions,
) -> Result<(std::borrow::Cow<'a, [u8]>, Option<&'a Path>)> {
    use std::borrow::Cow;
    Ok(match source {
        Source::Text(t) => {
            check_size(t.len() as u64, opts)?;
            (Cow::Borrowed(t.as_bytes()), None)
        }
        Source::Bytes(b) => {
            check_size(b.len() as u64, opts)?;
            (Cow::Borrowed(b), None)
        }
        Source::File(p) => (Cow::Owned(load_file(p, None, opts)?), Some(p)),
        Source::CheckedFile { path, expect } => {
            (Cow::Owned(load_file(path, Some(expect), opts)?), Some(path))
        }
    })
}

fn load_file(p: &Path, expect: Option<FileExpect>, opts: &ExtractOptions) -> Result<Vec<u8>> {
    let file = open_regular(p, expect)?;
    let len = file.metadata()?.len();
    check_size(len, opts)?;
    let cap = usize::try_from(len.min(opts.max_input_bytes)).unwrap_or(usize::MAX);
    let mut buf = Vec::with_capacity(cap.min(64 * 1024 * 1024));
    // The file may grow between `metadata` and `read`: read at most one byte too many.
    file.take(opts.max_input_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    check_size(buf.len() as u64, opts)?;
    Ok(buf)
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

/// Caps applied to every result, whatever the extractor did.
const MAX_LINKS: usize = 2_000;
const MAX_TAGS: usize = 500;

fn finalize(mut out: Extracted, opts: &ExtractOptions) -> Extracted {
    // Invisible format characters (zero-width, bidi controls, tag characters) are never shown to
    // the user, so they are not indexed or handed to a model either.
    if let Some((clean, removed)) = util::strip_invisible(&out.text) {
        for s in &mut out.sections {
            s.start = removed.map(s.start);
            s.end = removed.map(s.end);
        }
        out.text = clean;
    }
    if let Some((clean, _)) = out.title.as_deref().and_then(util::strip_invisible) {
        out.title = Some(clean);
    }
    for h in out.sections.iter_mut().flat_map(|s| &mut s.heading_path) {
        if let Some((clean, _)) = util::strip_invisible(h) {
            *h = clean;
        }
    }
    if let Some((cut, _)) = out.text.char_indices().nth(opts.max_text_chars) {
        out.text.truncate(cut);
        out.truncated = true;
    }
    let len = out.text.len();
    for s in &mut out.sections {
        s.end = util::floor_char_boundary(&out.text, s.end.min(len));
        s.start = util::floor_char_boundary(&out.text, s.start.min(s.end));
    }
    out.sections.retain(|s| s.start < s.end);
    out.links.truncate(MAX_LINKS);
    out.tags.truncate(MAX_TAGS);
    if let Some(t) = &out.title {
        let t = util::normalize_ws(t);
        out.title = (!t.is_empty()).then(|| util::truncate_chars(&t, 500));
    }
    if !out.metadata.is_object() {
        out.metadata = serde_json::Value::Object(Default::default());
    }
    out
}

/// Best-effort MIME type from a file name and its first bytes.
///
/// Binary signatures (PDF, PNG, JPEG, WebP, GIF, ZIP/OOXML, OLE) win over the extension; then the
/// extension decides (source files map to `text/x-<language>`, e.g. `text/x-rust`); then the
/// content is sniffed for HTML, iCalendar, RFC 822 mail and plain text.
pub fn detect_mime(path: Option<&Path>, head: &[u8]) -> String {
    mime::detect(path, head)
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
///
/// Splits by section, then paragraph (blank line), then sentence or line, then word, then
/// character, and packs the pieces greedily up to `target_tokens`. Guarantees:
/// - every non-whitespace byte of `doc.text` lies in some chunk's `start..end`;
/// - `text` is the heading-path prefix (`"Guide > Install\n"`, empty outside sections) followed
///   by exactly `doc.text[start..end]`;
/// - `tokens == estimate_tokens(&text) <= max_tokens` (`max_tokens` is treated as at least 1);
/// - consecutive chunks of one section overlap by at most `overlap_tokens` (capped at half of
///   `max_tokens`), starting at a word boundary; chunks never overlap across sections.
pub fn chunk(doc: &Extracted, opts: &ChunkOptions) -> Vec<ChunkOut> {
    chunker::chunk(doc, opts)
}

/// Rough token count: about 4 chars per token for Latin-script text, and 1 token per CJK
/// ideograph, kana or Hangul syllable. Cheap, deterministic and additive over concatenation up
/// to rounding (`estimate_tokens(a + b) <= estimate_tokens(a) + estimate_tokens(b)`).
pub fn estimate_tokens(text: &str) -> usize {
    chunker::Counts::of(text).tokens()
}
