//! Content of one Google Drive file: Google Docs, Sheets and Slides exported to text, other
//! supported files downloaded for the extraction pipeline, everything else refused with a
//! human-readable reason.
//!
//! CONTRACT (owned by the content workstream; `drive.rs` calls only what is below).
//!
//! Every response is untrusted: bodies are size-capped per request, JSON is parsed into bounded
//! structures, text is stripped of control characters and capped, and skip reasons never
//! contain file content.

use nugget_connector::{CancellationToken, Content};
use nugget_core::{Error, Result};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::GoogleEndpoints;
use crate::api::{GoogleApi, Media, api_url};

const DOC: &str = "application/vnd.google-apps.document";
const SHEET: &str = "application/vnd.google-apps.spreadsheet";
const SLIDES: &str = "application/vnd.google-apps.presentation";
const GOOGLE_APPS: &str = "application/vnd.google-apps.";

const MIB: u64 = 1024 * 1024;
/// Largest Sheets or Slides JSON response read (spreadsheet metadata, one sheet's values, one
/// presentation).
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;
/// Most sheets read from one spreadsheet.
const MAX_SHEETS: usize = 50;
/// Rows requested per sheet (`'<title>'!1:<ROW_CAP>`).
const SHEET_ROW_CAP: u32 = 20_000;
/// Most CSV text kept per sheet.
const SHEET_TEXT_CAP: usize = 512 * 1024;
/// Most text kept for one spreadsheet or presentation (never above the file size limit).
const TOTAL_TEXT_CAP: usize = 2 * 1024 * 1024;
/// Longest sheet title rendered as a heading.
const MAX_TITLE: usize = 200;
/// Longest sheet title put in a range (Sheets allows 100 characters).
const MAX_RANGE_TITLE: usize = 1024;
/// Deepest nesting of Slides element groups walked.
const MAX_ELEMENT_DEPTH: usize = 16;
/// Longest file id accepted into a URL.
const MAX_ID_LEN: usize = 256;

const SHEETS_FIELDS: &str =
    "sheets.properties(sheetId,title,sheetType,gridProperties(rowCount,columnCount))";
const SLIDES_FIELDS: &str = "slides(objectId,pageElements,slideProperties/notesPage/pageElements)";
const JSON: &str = "application/json";
const ANY: &str = "*/*";

/// Shown above a Sheets fallback export.
const FIRST_SHEET_ONLY: &str =
    "Only the first sheet was exported; enable the Google Sheets API for every sheet.";

/// Files downloaded for the extraction pipeline: (extension, extractor mime). Mirrors the
/// OneDrive connector.
const SUPPORTED: &[(&str, &str)] = &[
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("rst", "text/plain"),
    ("org", "text/plain"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("json", "application/json"),
    ("eml", "message/rfc822"),
    ("ics", "text/calendar"),
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
];

/// Other spellings Drive uses for supported types: (Drive mime, extractor mime).
const MIME_ALIASES: &[(&str, &str)] = &[
    ("text/x-markdown", "text/markdown"),
    ("application/xhtml+xml", "text/html"),
    ("text/x-csv", "text/csv"),
    ("application/csv", "text/csv"),
    ("application/ics", "text/calendar"),
];

/// What `drive.rs` knows about a file when asking for its content.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileRef<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// Drive `mimeType` (e.g. `application/vnd.google-apps.document`, `application/pdf`).
    pub mime_type: &'a str,
    /// Drive `size` (absent for Google Docs, Sheets, Slides).
    pub size: Option<u64>,
    /// The file lives in a shared drive (adds `supportsAllDrives=true`).
    pub shared_drive: bool,
}

/// The outcome for one file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Fetched {
    /// Indexable content. `mime` is what the pipeline extracts it as (`text/markdown` for Docs,
    /// Sheets and Slides; the file's own type for downloads). `via` names how it was obtained
    /// (`"export:markdown"`, `"export:plain"`, `"sheets:csv"`, `"export:csv"`, `"slides"`,
    /// `"export:slides-plain"`, `"download"`) and goes into document metadata.
    Ready {
        mime: String,
        content: Content,
        via: &'static str,
    },
    /// Not indexed; the reason is shown to the user (one short English sentence, no file
    /// content in it), e.g. "Larger than the 25 MB limit." or "Images are not indexed.".
    Skipped(String),
    /// The file no longer exists or is no longer readable (404/410): drop it.
    Gone,
}

/// Whether `mime_type` is a Drive folder or shortcut: not a document at all, never fetched.
pub(crate) fn is_container(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "application/vnd.google-apps.folder" | "application/vnd.google-apps.shortcut"
    )
}

/// Fetches file content through [`GoogleApi`] (so every request is metered and retried).
#[derive(Debug, Clone)]
pub(crate) struct ContentFetcher {
    drive_api: String,
    sheets_api: String,
    slides_api: String,
    max_bytes: u64,
}

impl ContentFetcher {
    pub(crate) fn new(endpoints: &GoogleEndpoints, max_bytes: u64) -> Self {
        Self {
            drive_api: endpoints.drive_api.clone(),
            sheets_api: endpoints.sheets_api.clone(),
            slides_api: endpoints.slides_api.clone(),
            max_bytes,
        }
    }

    /// The content of `file`. Transient failures (network, 5xx after retries, rate limits) and
    /// auth failures are `Err` (the pass fails and resumes from its checkpoint); permanent
    /// per-file refusals are `Ok(Fetched::Skipped)`.
    pub(crate) async fn fetch(
        &self,
        api: &GoogleApi,
        file: FileRef<'_>,
        cancel: &CancellationToken,
    ) -> Result<Fetched> {
        if !valid_file_id(file.id) {
            return Ok(Fetched::Skipped(
                "Google Drive returned a file id that cannot be used.".into(),
            ));
        }
        match kind(file.mime_type, file.name) {
            Kind::Doc => self.doc(api, file.id, cancel).await,
            Kind::Sheet => self.sheet(api, file.id, cancel).await,
            Kind::Slides => self.slides(api, file.id, cancel).await,
            Kind::Download(mime) => self.download(api, file, mime, cancel).await,
            Kind::Skip(reason) => Ok(Fetched::Skipped(reason)),
        }
    }

    fn max_body(&self) -> usize {
        usize::try_from(self.max_bytes).unwrap_or(usize::MAX)
    }

    fn text_cap(&self) -> usize {
        TOTAL_TEXT_CAP.min(self.max_body())
    }

    fn too_large(&self) -> Fetched {
        Fetched::Skipped(format!(
            "Larger than the {} MB limit.",
            (self.max_bytes / MIB).max(1)
        ))
    }

    fn export_url(&self, id: &str, mime: &str) -> Result<Url> {
        api_url(
            &self.drive_api,
            &["files", id, "export"],
            &[("mimeType", mime)],
        )
    }

    /// One request through [`GoogleApi::get_media`].
    async fn get(
        &self,
        api: &GoogleApi,
        url: &Url,
        accept: &str,
        max_body: usize,
        cancel: &CancellationToken,
    ) -> Result<Got> {
        Ok(match api.get_media(url, accept, max_body, cancel).await? {
            Media::Body(resp) if resp.status.is_success() => Got::Body(resp.body),
            Media::Body(resp) if matches!(resp.status.as_u16(), 404 | 410) => Got::Gone,
            Media::Body(resp) => Got::Refused(format!("HTTP {}", resp.status.as_u16())),
            Media::TooLarge => Got::TooLarge,
            Media::Refused {
                status,
                reason,
                message,
            } => {
                tracing::debug!(%status, %reason, %message, "google drive: request refused");
                Got::Refused(if reason.is_empty() {
                    format!("HTTP {status}")
                } else {
                    reason
                })
            }
        })
    }

    /// A Drive API request: a refusal for lack of scope fails the pass (every file would be
    /// refused), anything else stays per-file.
    async fn drive_get(
        &self,
        api: &GoogleApi,
        url: &Url,
        accept: &str,
        cancel: &CancellationToken,
    ) -> Result<Got> {
        let got = self.get(api, url, accept, self.max_body(), cancel).await?;
        if let Got::Refused(reason) = &got
            && SCOPE_REASONS.contains(&reason.as_str())
        {
            return Err(Error::Unauthorized(format!(
                "Google Drive denied access ({reason}); reconnect the account"
            )));
        }
        Ok(got)
    }

    async fn doc(&self, api: &GoogleApi, id: &str, cancel: &CancellationToken) -> Result<Fetched> {
        let url = self.export_url(id, "text/markdown")?;
        match self.drive_get(api, &url, ANY, cancel).await? {
            Got::Body(b) => {
                return Ok(Fetched::Ready {
                    mime: "text/markdown".into(),
                    content: Content::Text(clean_text(&b, usize::MAX)),
                    via: "export:markdown",
                });
            }
            Got::Gone => return Ok(Fetched::Gone),
            Got::TooLarge => return Ok(self.too_large()),
            Got::Refused(_) => {}
        }
        let url = self.export_url(id, "text/plain")?;
        Ok(match self.drive_get(api, &url, ANY, cancel).await? {
            Got::Body(b) => Fetched::Ready {
                mime: "text/plain".into(),
                content: Content::Text(clean_text(&b, usize::MAX)),
                via: "export:plain",
            },
            Got::Gone => Fetched::Gone,
            Got::TooLarge => self.too_large(),
            Got::Refused(r) => refused("export", &r),
        })
    }

    async fn sheet(
        &self,
        api: &GoogleApi,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Fetched> {
        let url = api_url(
            &self.sheets_api,
            &["spreadsheets", id],
            &[("fields", SHEETS_FIELDS)],
        )?;
        let meta: SpreadsheetMeta = match self.get(api, &url, JSON, MAX_JSON_BODY, cancel).await? {
            Got::Gone => return Ok(Fetched::Gone),
            Got::Body(b) => match serde_json::from_slice(&b) {
                Ok(m) => m,
                Err(_) => return self.sheet_fallback(api, id, cancel).await,
            },
            Got::TooLarge | Got::Refused(_) => return self.sheet_fallback(api, id, cancel).await,
        };
        let total_cap = self.text_cap();
        let grid: Vec<&SheetProps> = meta
            .sheets
            .iter()
            .map(|s| &s.properties)
            .filter(|p| p.sheet_type.as_deref().is_none_or(|t| t == "GRID"))
            .collect();
        let mut out = String::new();
        for (n, props) in grid.iter().enumerate() {
            if n >= MAX_SHEETS || out.len() >= total_cap {
                out.push_str(&format!(
                    "(truncated: {} more sheets are not included)\n",
                    grid.len() - n
                ));
                break;
            }
            let mut title = clean_line(&props.title, MAX_TITLE);
            if title.is_empty() {
                title = format!("Sheet {}", n + 1);
            }
            out.push_str(&format!("## {title}\n\n"));
            let got = if props.title.len() > MAX_RANGE_TITLE {
                Got::Refused(String::new())
            } else {
                let range = sheet_range(&props.title);
                let url = api_url(
                    &self.sheets_api,
                    &["spreadsheets", id, "values", &range],
                    &[
                        ("majorDimension", "ROWS"),
                        ("valueRenderOption", "FORMATTED_VALUE"),
                    ],
                )?;
                self.get(api, &url, JSON, MAX_JSON_BODY, cancel).await?
            };
            match got {
                Got::Body(b) => match serde_json::from_slice::<SheetValues>(&b) {
                    Ok(v) => {
                        let cap = SHEET_TEXT_CAP.min(total_cap.saturating_sub(out.len()));
                        let (csv, cut) = rows_to_csv(&v.values, cap);
                        out.push_str(&csv);
                        if let Some(rows) = cut {
                            out.push_str(&format!(
                                "(truncated: only the first {rows} rows of this sheet are included)\n"
                            ));
                        } else if props
                            .grid
                            .as_ref()
                            .and_then(|g| g.row_count)
                            .is_some_and(|r| r > u64::from(SHEET_ROW_CAP))
                        {
                            out.push_str(&format!(
                                "(truncated: only the first {SHEET_ROW_CAP} rows of this sheet are included)\n"
                            ));
                        }
                    }
                    Err(_) => out.push_str("(this sheet could not be read)\n"),
                },
                Got::TooLarge => out.push_str("(truncated: this sheet is too large to read)\n"),
                Got::Gone | Got::Refused(_) => out.push_str("(this sheet could not be read)\n"),
            }
            out.push('\n');
        }
        Ok(Fetched::Ready {
            mime: "text/markdown".into(),
            content: Content::Text(out),
            via: "sheets:csv",
        })
    }

    /// The Sheets API is unavailable: Drive's CSV export (first sheet only).
    async fn sheet_fallback(
        &self,
        api: &GoogleApi,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Fetched> {
        let url = self.export_url(id, "text/csv")?;
        Ok(match self.drive_get(api, &url, ANY, cancel).await? {
            Got::Body(b) => {
                let cap = self.text_cap();
                let csv = clean_text(&b, cap);
                let mut text = format!("{FIRST_SHEET_ONLY}\n\n{csv}");
                if csv.len() >= cap {
                    text.push_str("\n(truncated: the sheet is longer than shown)\n");
                }
                Fetched::Ready {
                    mime: "text/markdown".into(),
                    content: Content::Text(text),
                    via: "export:csv",
                }
            }
            Got::Gone => Fetched::Gone,
            Got::TooLarge => self.too_large(),
            Got::Refused(r) => refused("export", &r),
        })
    }

    async fn slides(
        &self,
        api: &GoogleApi,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Fetched> {
        let url = api_url(
            &self.slides_api,
            &["presentations", id],
            &[("fields", SLIDES_FIELDS)],
        )?;
        match self.get(api, &url, JSON, MAX_JSON_BODY, cancel).await? {
            Got::Gone => return Ok(Fetched::Gone),
            Got::Body(b) => {
                if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                    return Ok(Fetched::Ready {
                        mime: "text/markdown".into(),
                        content: Content::Text(render_slides(&v, self.text_cap())),
                        via: "slides",
                    });
                }
            }
            Got::TooLarge | Got::Refused(_) => {}
        }
        let url = self.export_url(id, "text/plain")?;
        Ok(match self.drive_get(api, &url, ANY, cancel).await? {
            Got::Body(b) => Fetched::Ready {
                mime: "text/plain".into(),
                content: Content::Text(clean_text(&b, usize::MAX)),
                via: "export:slides-plain",
            },
            Got::Gone => Fetched::Gone,
            Got::TooLarge => self.too_large(),
            Got::Refused(r) => refused("export", &r),
        })
    }

    async fn download(
        &self,
        api: &GoogleApi,
        file: FileRef<'_>,
        mime: &'static str,
        cancel: &CancellationToken,
    ) -> Result<Fetched> {
        if file.size.is_some_and(|s| s > self.max_bytes) {
            return Ok(self.too_large());
        }
        let mut query = vec![("alt", "media")];
        if file.shared_drive {
            query.push(("supportsAllDrives", "true"));
        }
        let url = api_url(&self.drive_api, &["files", file.id], &query)?;
        Ok(match self.drive_get(api, &url, ANY, cancel).await? {
            Got::Body(b) => Fetched::Ready {
                mime: mime.into(),
                content: Content::Bytes(b),
                via: "download",
            },
            Got::Gone => Fetched::Gone,
            Got::TooLarge => self.too_large(),
            Got::Refused(r) => refused("download", &r),
        })
    }
}

/// Google 403 reasons that mean the token lacks the scope: every file would fail the same way.
const SCOPE_REASONS: &[&str] = &[
    "insufficientPermissions",
    "ACCESS_TOKEN_SCOPE_INSUFFICIENT",
    "authError",
];

/// One content request's outcome.
enum Got {
    Body(Vec<u8>),
    Gone,
    TooLarge,
    /// Google's reason code (letters, digits, `_`), or `HTTP <status>`.
    Refused(String),
}

fn refused(what: &str, reason: &str) -> Fetched {
    Fetched::Skipped(format!("Google refused the {what} ({reason})."))
}

/// How a file is handled, decided from its Drive mime type, then its extension.
#[derive(Debug, PartialEq)]
enum Kind {
    Doc,
    Sheet,
    Slides,
    /// Download it; the pipeline extracts it as this mime type.
    Download(&'static str),
    Skip(String),
}

fn kind(mime_type: &str, name: &str) -> Kind {
    let mime = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        DOC => return Kind::Doc,
        SHEET => return Kind::Sheet,
        SLIDES => return Kind::Slides,
        _ => {}
    }
    if let Some(app) = mime.strip_prefix(GOOGLE_APPS) {
        return Kind::Skip(match google_label(app) {
            Some(label) => format!("{label} files cannot be exported as text."),
            None => "Google files of this kind cannot be exported as text.".into(),
        });
    }
    if let Some(m) = supported_mime(&mime) {
        return Kind::Download(m);
    }
    let media = ["image/", "video/", "audio/"]
        .iter()
        .any(|p| mime.starts_with(p));
    if !media && let Some(m) = mime_for_name(name) {
        return Kind::Download(m);
    }
    Kind::Skip(unsupported_reason(&mime))
}

fn google_label(app: &str) -> Option<&'static str> {
    Some(match app {
        "form" => "Google Forms",
        "drawing" => "Google Drawings",
        "site" => "Google Sites",
        "map" => "Google My Maps",
        "jam" => "Jamboard",
        "script" => "Apps Script",
        "fusiontable" => "Fusion Tables",
        "vid" => "Google Vids",
        "folder" => "Folder",
        "shortcut" => "Shortcut",
        _ => return None,
    })
}

fn supported_mime(mime: &str) -> Option<&'static str> {
    SUPPORTED
        .iter()
        .map(|(_, m)| *m)
        .find(|m| *m == mime)
        .or_else(|| {
            MIME_ALIASES
                .iter()
                .find(|(a, _)| *a == mime)
                .map(|(_, m)| *m)
        })
}

fn mime_for_name(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    if ext.is_empty() || ext.len() > 10 {
        return None;
    }
    let ext = ext.to_ascii_lowercase();
    SUPPORTED.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

fn unsupported_reason(mime: &str) -> String {
    let sub = mime.split_once('/').map_or("", |(_, s)| s);
    if mime.starts_with("image/") {
        return "Images are not indexed.".into();
    }
    if mime.starts_with("video/") {
        return "Videos are not indexed.".into();
    }
    if mime.starts_with("audio/") {
        return "Audio files are not indexed.".into();
    }
    if mime.starts_with("application/")
        && matches!(
            sub,
            "zip"
                | "x-zip-compressed"
                | "x-tar"
                | "gzip"
                | "x-gzip"
                | "x-7z-compressed"
                | "x-rar-compressed"
                | "vnd.rar"
                | "x-bzip2"
                | "x-xz"
        )
    {
        return "Archives are not indexed.".into();
    }
    if mime.starts_with("application/")
        && matches!(sub, "msword" | "vnd.ms-excel" | "vnd.ms-powerpoint")
    {
        return "Older Office formats (.doc, .xls, .ppt) are not indexed.".into();
    }
    if mime.starts_with("application/")
        && matches!(
            sub,
            "x-msdownload"
                | "x-msdos-program"
                | "x-executable"
                | "x-msi"
                | "x-apple-diskimage"
                | "vnd.android.package-archive"
        )
    {
        return "Programs and installers are not indexed.".into();
    }
    let clean: String = mime
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '+' | '-' | '_'))
        .take(80)
        .collect();
    if clean.is_empty() {
        "This file type is not indexed.".into()
    } else {
        format!("This file type ({clean}) is not indexed.")
    }
}

/// Drive file ids: letters, digits, `-` and `_`.
fn valid_file_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The A1 range for the first [`SHEET_ROW_CAP`] rows of a sheet: `'<title>'!1:<cap>`, with
/// quotes in the title doubled. `api_url` percent-encodes it as a path segment.
fn sheet_range(title: &str) -> String {
    format!("'{}'!1:{SHEET_ROW_CAP}", title.replace('\'', "''"))
}

#[derive(Deserialize, Default)]
struct SpreadsheetMeta {
    #[serde(default)]
    sheets: Vec<SheetMeta>,
}

#[derive(Deserialize, Default)]
struct SheetMeta {
    #[serde(default)]
    properties: SheetProps,
}

#[derive(Deserialize, Default)]
struct SheetProps {
    #[serde(default)]
    title: String,
    #[serde(default, rename = "sheetType")]
    sheet_type: Option<String>,
    #[serde(default, rename = "gridProperties")]
    grid: Option<GridProps>,
}

#[derive(Deserialize, Default)]
struct GridProps {
    #[serde(default, rename = "rowCount")]
    row_count: Option<u64>,
}

#[derive(Deserialize, Default)]
struct SheetValues {
    #[serde(default)]
    values: Vec<Vec<Value>>,
}

/// Rows as CSV (RFC 4180 quoting, `\n` line ends) within `cap` bytes. The second value is the
/// number of rows included when the rest did not fit.
fn rows_to_csv(rows: &[Vec<Value>], cap: usize) -> (String, Option<usize>) {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        let line = row
            .iter()
            .map(|v| csv_field(&cell_text(v)))
            .collect::<Vec<_>>()
            .join(",");
        if out.len() + line.len() + 1 > cap {
            return (out, Some(i));
        }
        out.push_str(&line);
        out.push('\n');
    }
    (out, None)
}

fn cell_text(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    };
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// A presentation as Markdown: `## Slide N`, its text (shapes, tables, groups), then `Notes:`
/// with the speaker notes when there are any. At most `cap` bytes plus a truncation line.
fn render_slides(v: &Value, cap: usize) -> String {
    let empty = Vec::new();
    let slides = v.get("slides").and_then(Value::as_array).unwrap_or(&empty);
    let mut out = String::new();
    for (i, slide) in slides.iter().enumerate() {
        let mut body = String::new();
        walk_elements(slide.get("pageElements"), 0, &mut body, cap);
        let mut notes = String::new();
        walk_elements(
            slide.pointer("/slideProperties/notesPage/pageElements"),
            0,
            &mut notes,
            cap,
        );
        let mut section = format!("## Slide {}\n\n", i + 1);
        let body = body.trim();
        if !body.is_empty() {
            section.push_str(body);
            section.push_str("\n\n");
        }
        let notes = notes.trim();
        if !notes.is_empty() {
            section.push_str("Notes:\n");
            section.push_str(notes);
            section.push_str("\n\n");
        }
        if !push_capped(&mut out, &section, cap) {
            out.push_str(&format!(
                "\n(truncated: {} more slides are not included)\n",
                slides.len() - i - 1
            ));
            break;
        }
    }
    out
}

fn walk_elements(elems: Option<&Value>, depth: usize, out: &mut String, cap: usize) {
    let Some(elems) = elems.and_then(Value::as_array) else {
        return;
    };
    if depth > MAX_ELEMENT_DEPTH {
        return;
    }
    for el in elems {
        if out.len() > cap {
            return;
        }
        if let Some(shape) = el.get("shape") {
            // The notes page's slide thumbnail.
            if shape.pointer("/placeholder/type").and_then(Value::as_str) == Some("SLIDE_IMAGE") {
                continue;
            }
            push_text(shape.get("text"), out);
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        } else if let Some(rows) = el.pointer("/table/tableRows").and_then(Value::as_array) {
            for row in rows {
                let Some(cells) = row.get("tableCells").and_then(Value::as_array) else {
                    continue;
                };
                let cells: Vec<String> = cells
                    .iter()
                    .map(|c| {
                        let mut s = String::new();
                        push_text(c.get("text"), &mut s);
                        s.split_whitespace().collect::<Vec<_>>().join(" ")
                    })
                    .collect();
                if cells.iter().any(|c| !c.is_empty()) {
                    out.push_str(&cells.join(" | "));
                    out.push('\n');
                }
                if out.len() > cap {
                    return;
                }
            }
        } else if let Some(group) = el.get("elementGroup") {
            walk_elements(group.get("children"), depth + 1, out, cap);
        }
    }
}

/// The text runs of a Slides `text` object, control characters removed (vertical tab, the
/// Slides soft line break, becomes a newline).
fn push_text(text: Option<&Value>, out: &mut String) {
    let Some(elements) = text
        .and_then(|t| t.get("textElements"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for te in elements {
        if let Some(run) = te.pointer("/textRun/content").and_then(Value::as_str) {
            out.extend(run.chars().filter_map(|c| match c {
                '\u{b}' => Some('\n'),
                '\n' | '\t' => Some(c),
                c if c.is_control() => None,
                c => Some(c),
            }));
        }
    }
}

/// Append `s` if it fits in `cap` bytes; otherwise as much as fits (on a character boundary)
/// and `false`.
fn push_capped(out: &mut String, s: &str, cap: usize) -> bool {
    let room = cap.saturating_sub(out.len());
    if s.len() <= room {
        out.push_str(s);
        return true;
    }
    out.push_str(&s[..floor_boundary(s, room)]);
    false
}

fn floor_boundary(s: &str, max: usize) -> usize {
    let mut i = max.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Exported text: lossy UTF-8, no byte-order mark, control characters other than newline and
/// tab removed, at most `cap` bytes.
fn clean_text(b: &[u8], cap: usize) -> String {
    let s = String::from_utf8_lossy(b);
    let s = s.strip_prefix('\u{feff}').unwrap_or(&s);
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let end = floor_boundary(&out, cap);
    out.truncate(end);
    out
}

/// One line of text: control characters become spaces, whitespace collapsed, at most `max`
/// characters.
fn clean_line(s: &str, max: usize) -> String {
    s.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// Fuzzing entry point (`fuzz/fuzz_targets/drive_export.rs`); not part of the API.
///
/// Runs every parser a Drive, Sheets or Slides response body goes through, offline: the
/// mime/extension decision for `mime_type` and `name`, the text clean-up of a Docs, Slides or
/// CSV export capped at `cap`, Sheets metadata (titles and the values range URL), Sheets values
/// as CSV, Slides JSON as Markdown, and a `files.list` entry. Returns `(what, output, max_len)`
/// for the caller to check: `what` is `"text"` (only `\n` and `\t` as control characters),
/// `"csv"` (also `\r`), `"line"` (no control characters), `"version"` (a line without `|`),
/// `"json"` (an object) or `"url"` (a Sheets values URL that must stay one path segment on the
/// Sheets host).
#[doc(hidden)]
pub fn fuzz_export(
    mime_type: &str,
    name: &str,
    body: &[u8],
    cap: usize,
) -> Vec<(&'static str, String, usize)> {
    let mut out = Vec::new();
    if let Kind::Skip(reason) = kind(mime_type, name) {
        out.push(("line", reason, 200));
    }
    out.push(("text", clean_text(body, cap), cap));
    if let Ok(meta) = serde_json::from_slice::<SpreadsheetMeta>(body) {
        for props in meta.sheets.iter().map(|s| &s.properties).take(MAX_SHEETS) {
            out.push(("line", clean_line(&props.title, MAX_TITLE), MAX_TITLE * 4));
            if props.title.len() <= MAX_RANGE_TITLE {
                let range = sheet_range(&props.title);
                if let Ok(url) = api_url(
                    "https://sheets.googleapis.com/v4",
                    &["spreadsheets", "s1", "values", &range],
                    &[("majorDimension", "ROWS")],
                ) {
                    out.push(("url", url.to_string(), usize::MAX));
                }
            }
        }
    }
    if let Ok(v) = serde_json::from_slice::<SheetValues>(body) {
        let (csv, _) = rows_to_csv(&v.values, cap);
        out.push(("csv", csv, cap));
    }
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        // The truncation line after the capped slides.
        out.push(("text", render_slides(&v, cap), cap.saturating_add(80)));
    }
    out.extend(crate::drive::fuzz_file_entry(body));
    out
}

#[cfg(test)]
mod unit {
    use serde_json::json;
    use wiremock::matchers::{method, path, path_regex, query_param, query_param_is_missing};
    use wiremock::{Mock, ResponseTemplate};

    use super::*;
    use crate::api::OAuthSettings;
    use crate::tests::{Harness, fast_retry};

    const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

    #[test]
    fn kinds_by_mime_then_extension() {
        assert_eq!(kind(DOC, "x"), Kind::Doc);
        assert_eq!(kind(SHEET, "x"), Kind::Sheet);
        assert_eq!(kind(SLIDES, "x"), Kind::Slides);
        assert_eq!(
            kind("application/vnd.google-apps.form", "f"),
            Kind::Skip("Google Forms files cannot be exported as text.".into())
        );
        assert_eq!(
            kind("application/vnd.google-apps.unknownthing", "f"),
            Kind::Skip("Google files of this kind cannot be exported as text.".into())
        );
        assert_eq!(
            kind("application/pdf", "a"),
            Kind::Download("application/pdf")
        );
        assert_eq!(
            kind("text/plain; charset=utf-8", "a"),
            Kind::Download("text/plain")
        );
        assert_eq!(
            kind("text/x-markdown", "a"),
            Kind::Download("text/markdown")
        );
        assert_eq!(kind(XLSX, "a"), Kind::Download(XLSX));
        assert_eq!(
            kind("application/octet-stream", "Notes.MD"),
            Kind::Download("text/markdown")
        );
        assert_eq!(kind("", "report.pdf"), Kind::Download("application/pdf"));
        // An image never becomes text because of its name.
        assert_eq!(
            kind("image/png", "x.txt"),
            Kind::Skip("Images are not indexed.".into())
        );
        assert_eq!(
            kind("video/mp4", "v"),
            Kind::Skip("Videos are not indexed.".into())
        );
        assert_eq!(
            kind("audio/mpeg", "v"),
            Kind::Skip("Audio files are not indexed.".into())
        );
        assert_eq!(
            kind("application/zip", "a.zip"),
            Kind::Skip("Archives are not indexed.".into())
        );
        assert_eq!(
            kind("application/msword", "a.doc"),
            Kind::Skip("Older Office formats (.doc, .xls, .ppt) are not indexed.".into())
        );
        assert_eq!(
            kind("application/x-msdownload", "setup.exe"),
            Kind::Skip("Programs and installers are not indexed.".into())
        );
        assert_eq!(
            kind("application/x-weird\"<script>; q=1", "noext"),
            Kind::Skip("This file type (application/x-weirdscript) is not indexed.".into())
        );
        assert_eq!(
            kind("", "noext"),
            Kind::Skip("This file type is not indexed.".into())
        );
        let long = format!("application/{}", "x".repeat(500));
        let Kind::Skip(r) = kind(&long, "a") else {
            panic!("expected a skip")
        };
        assert!(r.len() < 140, "{r}");
        assert!(is_container("application/vnd.google-apps.folder"));
        assert!(!is_container(DOC));
    }

    #[test]
    fn file_ids() {
        assert!(valid_file_id("1A2b_c-D3"));
        assert!(!valid_file_id(""));
        assert!(!valid_file_id(".."));
        assert!(!valid_file_id("a/b"));
        assert!(!valid_file_id("a?b"));
        assert!(!valid_file_id(&"a".repeat(MAX_ID_LEN + 1)));
    }

    #[test]
    fn sheet_ranges_are_quoted_and_encoded() {
        assert_eq!(sheet_range("Q1"), "'Q1'!1:20000");
        assert_eq!(sheet_range("It's"), "'It''s'!1:20000");
        let url = api_url(
            "http://h/sheets/v4",
            &["spreadsheets", "s1", "values", &sheet_range("a/b #?%'")],
            &[("majorDimension", "ROWS")],
        )
        .unwrap();
        assert_eq!(
            url.path(),
            "/sheets/v4/spreadsheets/s1/values/'a%2Fb%20%23%3F%25'''!1:20000"
        );
        assert_eq!(url.query(), Some("majorDimension=ROWS"));
    }

    #[test]
    fn csv_quoting_and_caps() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
        assert_eq!(csv_field("cr\r"), "\"cr\r\"");
        let rows = vec![
            vec![json!("Name"), json!("Qty"), json!(true)],
            vec![json!("a,b"), json!(3), Value::Null, json!({"x": 1})],
            vec![json!("bell\u{7}gone")],
        ];
        let (csv, cut) = rows_to_csv(&rows, 1000);
        assert_eq!(csv, "Name,Qty,true\n\"a,b\",3,,\nbellgone\n");
        assert_eq!(cut, None);
        let (csv, cut) = rows_to_csv(&rows, 20);
        assert_eq!(csv, "Name,Qty,true\n");
        assert_eq!(cut, Some(1));
    }

    fn shape(text: &str) -> Value {
        json!({"shape": {"text": {"textElements": [
            {"paragraphMarker": {}},
            {"textRun": {"content": text}},
        ]}}})
    }

    #[test]
    fn slides_text_tables_groups_and_notes() {
        let deck = json!({"slides": [
            {
                "objectId": "p1",
                "pageElements": [
                    shape("Title\u{b}line two\n"),
                    {"table": {"tableRows": [
                        {"tableCells": [
                            {"text": {"textElements": [{"textRun": {"content": "A\n"}}]}},
                            {"text": {"textElements": [{"textRun": {"content": "B  b\n"}}]}}
                        ]},
                        {"tableCells": [{}, {}]}
                    ]}},
                    {"elementGroup": {"children": [
                        {"elementGroup": {"children": [shape("Deep\u{1b}")]}}
                    ]}},
                    {"image": {"contentUrl": "https://example.com/x.png"}}
                ],
                "slideProperties": {"notesPage": {"pageElements": [
                    {"shape": {"placeholder": {"type": "SLIDE_IMAGE"},
                        "text": {"textElements": [{"textRun": {"content": "thumb"}}]}}},
                    shape("Say hello\n")
                ]}}
            },
            {"objectId": "p2", "pageElements": []},
            "garbage"
        ]});
        let md = render_slides(&deck, 10_000);
        assert_eq!(
            md,
            "## Slide 1\n\nTitle\nline two\nA | B b\nDeep\n\nNotes:\nSay hello\n\n\
             ## Slide 2\n\n## Slide 3\n\n"
        );
        // Not an object: nothing, no panic.
        assert_eq!(render_slides(&json!([1, 2]), 100), "");
    }

    #[test]
    fn slides_depth_and_size_are_bounded() {
        let mut el = shape("too deep");
        for _ in 0..(MAX_ELEMENT_DEPTH + 4) {
            el = json!({"elementGroup": {"children": [el]}});
        }
        let deck = json!({"slides": [{"pageElements": [el]}]});
        assert_eq!(render_slides(&deck, 1000), "## Slide 1\n\n");

        let many: Vec<Value> = (0..100)
            .map(|i| json!({"pageElements": [shape(&format!("slide é {i}"))]}))
            .collect();
        let md = render_slides(&json!({ "slides": many }), 200);
        assert!(md.len() < 300, "{}", md.len());
        assert!(md.contains("more slides are not included"), "{md}");
    }

    #[test]
    fn text_cleaning() {
        assert_eq!(
            clean_text("\u{feff}a\r\nb\u{0}\tc".as_bytes(), 100),
            "a\nb\tc"
        );
        assert_eq!(clean_text("ééé".as_bytes(), 3), "é");
        assert_eq!(clean_text(&[0xff, b'a'], 100), "\u{fffd}a");
        assert_eq!(clean_line(" Q1\n\tsales\u{7} ", 100), "Q1 sales");
        assert_eq!(clean_line("abcdef", 3), "abc");
        let mut s = String::from("ab");
        assert!(!push_capped(&mut s, "cdé", 5));
        assert_eq!(s, "abcd");
    }

    /// The fuzz entry point keeps the contract its target checks (and does not panic).
    #[test]
    fn fuzz_entry_outputs_are_bounded() {
        let bodies: [&[u8]; 4] = [
            br#"{"sheets":[{"properties":{"title":"../a/?#%2F'x'"}},{"properties":{"title":".."}}]}"#,
            br#"{"values":[["a,b","\u0007\r\n"],[1,true,null]]}"#,
            br#"{"slides":[{"pageElements":[{"shape":{"text":{"textElements":[{"textRun":{"content":"h\u000bi\u0000"}}]}}}]},{}]}"#,
            br#"{"id":"x_1","name":"n\u202e\n","version":"1|2","owners":[{"displayName":"\u0000"}]}"#,
        ];
        for cap in [0, 1, 5, 4096] {
            for body in bodies {
                for (what, out, max) in fuzz_export("application/x-y\u{0}", "a.zip", body, cap) {
                    assert!(out.len() <= max, "{what} {out:?}");
                    match what {
                        "text" => assert!(
                            !out.chars()
                                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
                        ),
                        "csv" => assert!(
                            !out.chars()
                                .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
                        ),
                        "line" | "version" => {
                            assert!(!out.chars().any(|c| c.is_control()), "{out:?}");
                            assert!(what == "line" || !out.contains('|'), "{out:?}");
                        }
                        "json" => assert!(serde_json::from_str::<Value>(&out).unwrap().is_object()),
                        "url" => {
                            let u = Url::parse(&out).unwrap();
                            assert_eq!(u.host_str(), Some("sheets.googleapis.com"));
                            assert_eq!(u.path_segments().unwrap().count(), 5, "{out}");
                            assert_eq!(u.query(), Some("majorDimension=ROWS"));
                        }
                        other => panic!("{other}"),
                    }
                }
            }
        }
    }

    // ---- against a mock server ----

    const MB: u64 = 1024 * 1024;

    fn file<'a>(id: &'a str, name: &'a str, mime: &'a str, size: Option<u64>) -> FileRef<'a> {
        FileRef {
            id,
            name,
            mime_type: mime,
            size,
            shared_drive: false,
        }
    }

    async fn setup() -> (Harness, GoogleApi, ContentFetcher) {
        let h = Harness::new().await;
        h.seed_tokens("at", false);
        let api = GoogleApi::new(
            "sync:gdrive",
            "https://www.googleapis.com/auth/drive.readonly",
            OAuthSettings {
                client_id: "cid".into(),
                client_secret: None,
                login_hint: None,
            },
            &h.endpoints(),
            fast_retry(1),
            h.ctx(),
        )
        .unwrap();
        let fetcher = ContentFetcher::new(&h.endpoints(), MB);
        (h, api, fetcher)
    }

    fn google_error(status: u16, reason: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(json!({"error": {
            "code": status, "message": "nope",
            "errors": [{"reason": reason}]
        }}))
    }

    async fn fetch(api: &GoogleApi, f: &ContentFetcher, file: FileRef<'_>) -> Result<Fetched> {
        f.fetch(api, file, &CancellationToken::new()).await
    }

    #[tokio::test]
    async fn docs_export_markdown_then_plain() {
        let (h, api, f) = setup().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/d1/export"))
            .and(query_param("mimeType", "text/markdown"))
            .respond_with(ResponseTemplate::new(200).set_body_string("# Hi\r\n\u{0}body"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/d2/export"))
            .and(query_param("mimeType", "text/markdown"))
            .respond_with(google_error(400, "badRequest"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/d2/export"))
            .and(query_param("mimeType", "text/plain"))
            .respond_with(ResponseTemplate::new(200).set_body_string("plain"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex("^/drive/v3/files/d3/export$"))
            .respond_with(google_error(403, "exportSizeLimitExceeded"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/d4/export"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&h.server)
            .await;

        assert_eq!(
            fetch(&api, &f, file("d1", "Doc", DOC, None)).await.unwrap(),
            Fetched::Ready {
                mime: "text/markdown".into(),
                content: Content::Text("# Hi\nbody".into()),
                via: "export:markdown"
            }
        );
        assert_eq!(
            fetch(&api, &f, file("d2", "Doc", DOC, None)).await.unwrap(),
            Fetched::Ready {
                mime: "text/plain".into(),
                content: Content::Text("plain".into()),
                via: "export:plain"
            }
        );
        assert_eq!(
            fetch(&api, &f, file("d3", "Doc", DOC, None)).await.unwrap(),
            Fetched::Skipped("Google refused the export (exportSizeLimitExceeded).".into())
        );
        assert_eq!(
            fetch(&api, &f, file("d4", "Doc", DOC, None)).await.unwrap(),
            Fetched::Gone
        );
        h.assert_egress("sync:gdrive").await;
    }

    #[tokio::test]
    async fn downloads_sizes_and_refusals() {
        let (h, api, f) = setup().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/p1"))
            .and(query_param("alt", "media"))
            .and(query_param_is_missing("supportsAllDrives"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7".to_vec()))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/p2"))
            .and(query_param("alt", "media"))
            .and(query_param("supportsAllDrives", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; (MB + 1) as usize]))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/p3"))
            .respond_with(google_error(403, "cannotDownloadAbusiveFile"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/p4"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&h.server)
            .await;

        assert_eq!(
            fetch(&api, &f, file("p1", "a.pdf", "application/pdf", Some(8)))
                .await
                .unwrap(),
            Fetched::Ready {
                mime: "application/pdf".into(),
                content: Content::Bytes(b"%PDF-1.7".to_vec()),
                via: "download"
            }
        );
        let mut shared = file("p2", "a.txt", "text/plain", None);
        shared.shared_drive = true;
        assert_eq!(
            fetch(&api, &f, shared).await.unwrap(),
            Fetched::Skipped("Larger than the 1 MB limit.".into())
        );
        assert_eq!(
            fetch(&api, &f, file("p3", "a.pdf", "application/pdf", None))
                .await
                .unwrap(),
            Fetched::Skipped("Google refused the download (cannotDownloadAbusiveFile).".into())
        );
        assert_eq!(
            fetch(&api, &f, file("p4", "a.pdf", "application/pdf", None))
                .await
                .unwrap(),
            Fetched::Gone
        );
        let before = h.server.received_requests().await.unwrap().len();
        // Too large by its listed size, unsupported, or a bad id: no request.
        assert_eq!(
            fetch(
                &api,
                &f,
                file("p5", "a.pdf", "application/pdf", Some(MB + 1))
            )
            .await
            .unwrap(),
            Fetched::Skipped("Larger than the 1 MB limit.".into())
        );
        assert_eq!(
            fetch(&api, &f, file("p6", "a.png", "image/png", Some(10)))
                .await
                .unwrap(),
            Fetched::Skipped("Images are not indexed.".into())
        );
        assert!(matches!(
            fetch(&api, &f, file("../x", "a.pdf", "application/pdf", None))
                .await
                .unwrap(),
            Fetched::Skipped(_)
        ));
        assert_eq!(h.server.received_requests().await.unwrap().len(), before);
    }

    #[tokio::test]
    async fn rate_limits_and_scope_stay_errors() {
        let (h, api, f) = setup().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/r1"))
            .respond_with(google_error(403, "userRateLimitExceeded"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/r2"))
            .respond_with(google_error(403, "insufficientPermissions"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/r3"))
            .respond_with(google_error(403, "dailyLimitExceeded"))
            .mount(&h.server)
            .await;
        let pdf = |id| file(id, "a.pdf", "application/pdf", None);
        assert!(matches!(
            fetch(&api, &f, pdf("r1")).await,
            Err(Error::RateLimited { .. })
        ));
        assert!(matches!(
            fetch(&api, &f, pdf("r2")).await,
            Err(Error::Unauthorized(_))
        ));
        assert!(matches!(
            fetch(&api, &f, pdf("r3")).await,
            Err(Error::RateLimited {
                retry_after_ms: Some(3_600_000)
            })
        ));
    }

    #[tokio::test]
    async fn sheets_every_grid_sheet_as_csv() {
        let (h, api, f) = setup().await;
        Mock::given(method("GET"))
            .and(path("/sheets/v4/spreadsheets/s1"))
            .and(query_param("fields", SHEETS_FIELDS))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sheets": [
                {"properties": {"sheetId": 0, "title": "Q1 'plan'", "sheetType": "GRID",
                    "gridProperties": {"rowCount": 30000, "columnCount": 3}}},
                {"properties": {"sheetId": 1, "title": "Chart", "sheetType": "OBJECT"}},
                {"properties": {"sheetId": 2, "title": "Broken\nname"}}
            ]})))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/sheets/v4/spreadsheets/s1/values/'Q1%20''plan'''!1:20000",
            ))
            .and(query_param("majorDimension", "ROWS"))
            .and(query_param("valueRenderOption", "FORMATTED_VALUE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "range": "x", "majorDimension": "ROWS",
                "values": [["Item", "Cost"], ["Tea, green", "$3"]]
            })))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/sheets/v4/spreadsheets/s1/values/'Broken%0Aname'!1:20000",
            ))
            .respond_with(google_error(400, "badRequest"))
            .mount(&h.server)
            .await;

        assert_eq!(
            fetch(&api, &f, file("s1", "Budget", SHEET, None))
                .await
                .unwrap(),
            Fetched::Ready {
                mime: "text/markdown".into(),
                content: Content::Text(
                    "## Q1 'plan'\n\nItem,Cost\n\"Tea, green\",$3\n\
                     (truncated: only the first 20000 rows of this sheet are included)\n\n\
                     ## Broken name\n\n(this sheet could not be read)\n\n"
                        .into()
                ),
                via: "sheets:csv"
            }
        );
    }

    #[tokio::test]
    async fn sheets_api_disabled_falls_back_to_csv_export() {
        let (h, api, f) = setup().await;
        Mock::given(method("GET"))
            .and(path("/sheets/v4/spreadsheets/s2"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
                "code": 403, "message": "Google Sheets API has not been used",
                "errors": [{"reason": "accessNotConfigured"}],
                "details": [{"reason": "SERVICE_DISABLED"}]
            }})))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/s2/export"))
            .and(query_param("mimeType", "text/csv"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a,b\r\n1,2\r\n"))
            .mount(&h.server)
            .await;
        assert_eq!(
            fetch(&api, &f, file("s2", "Budget", SHEET, None))
                .await
                .unwrap(),
            Fetched::Ready {
                mime: "text/markdown".into(),
                content: Content::Text(format!("{FIRST_SHEET_ONLY}\n\na,b\n1,2\n")),
                via: "export:csv"
            }
        );
    }

    #[tokio::test]
    async fn slides_api_then_plain_export_fallback() {
        let (h, api, f) = setup().await;
        Mock::given(method("GET"))
            .and(path("/slides/v1/presentations/k1"))
            .and(query_param("fields", SLIDES_FIELDS))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"slides": [
                {"pageElements": [shape("Hello\n")]}
            ]})))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slides/v1/presentations/k2"))
            .respond_with(google_error(403, "accessNotConfigured"))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/k2/export"))
            .and(query_param("mimeType", "text/plain"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Hello deck"))
            .mount(&h.server)
            .await;
        assert_eq!(
            fetch(&api, &f, file("k1", "Deck", SLIDES, None))
                .await
                .unwrap(),
            Fetched::Ready {
                mime: "text/markdown".into(),
                content: Content::Text("## Slide 1\n\nHello\n\n".into()),
                via: "slides"
            }
        );
        assert_eq!(
            fetch(&api, &f, file("k2", "Deck", SLIDES, None))
                .await
                .unwrap(),
            Fetched::Ready {
                mime: "text/plain".into(),
                content: Content::Text("Hello deck".into()),
                via: "export:slides-plain"
            }
        );
    }
}
