//! CSV / TSV, and the row rendering shared with XLSX (`a | b | c`, one row per line).

use nugget_core::{Error, Result};
use serde_json::{Value, json};

use crate::util::{self, TextBuf};
use crate::{Ctx, Extracted};

const MAX_ROWS: usize = 100_000;
const MAX_COLS: usize = 512;
const MAX_CELL_CHARS: usize = 2_000;

/// Render a row as `a | b | c`: cells whitespace-normalized and capped, trailing empty cells
/// dropped. `None` when every cell is empty.
pub(crate) fn render_row<S: AsRef<str>>(cells: &[S]) -> Option<String> {
    let cells: Vec<String> = cells
        .iter()
        .take(MAX_COLS)
        .map(|c| util::truncate_chars(&util::normalize_ws(c.as_ref()), MAX_CELL_CHARS))
        .collect();
    let used = cells.iter().rposition(|c| !c.is_empty())? + 1;
    Some(cells[..used].join(" | "))
}

fn sniff_delimiter(text: &str) -> u8 {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut best = (0usize, b',');
    for d in *b",;\t|" {
        let n = first.bytes().filter(|&b| b == d).count();
        if n > best.0 {
            best = (n, d);
        }
    }
    best.1
}

pub(crate) fn extract_csv(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    if util::looks_binary(data) {
        return Err(Error::Invalid("csv: binary content".into()));
    }
    let text = util::decode_text(data, ctx.charset);
    let is_tsv = ctx.mime.contains("tab-separated")
        || ctx.mime == "text/tsv"
        || ctx
            .path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("tsv"));
    let delimiter = if is_tsv {
        b'\t'
    } else {
        sniff_delimiter(&text)
    };
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(text.as_bytes());

    let mut buf = TextBuf::new(ctx.opts.max_text_chars);
    let mut rows = 0usize;
    let mut cols = 0usize;
    let mut header: Option<Vec<String>> = None;
    let mut truncated = false;
    let mut record = csv::StringRecord::new();
    loop {
        match reader.read_record(&mut record) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                if rows == 0 {
                    return Err(Error::Invalid(format!("csv: {e}")));
                }
                truncated = true;
                break;
            }
        }
        if rows >= MAX_ROWS {
            truncated = true;
            break;
        }
        let cells: Vec<&str> = record.iter().take(MAX_COLS).collect();
        if record.len() > MAX_COLS {
            truncated = true;
        }
        let Some(line) = render_row(&cells) else {
            continue;
        };
        if header.is_none() {
            header = Some(
                cells
                    .iter()
                    .take(64)
                    .map(|c| util::truncate_chars(c.trim(), 200))
                    .collect(),
            );
        }
        cols = cols.max(cells.len());
        buf.newline();
        buf.push(&line);
        rows += 1;
        if buf.is_full() {
            break;
        }
    }
    let full = buf.is_full();
    let (text, cut) = buf.finish();
    let mut meta = util::obj();
    meta.insert("rows".into(), Value::from(rows));
    meta.insert("columns".into(), Value::from(cols));
    meta.insert(
        "delimiter".into(),
        Value::from(char::from(delimiter).to_string()),
    );
    if let Some(h) = header {
        meta.insert("header".into(), json!(h));
    }
    if truncated {
        meta.insert("rows_truncated".into(), Value::Bool(true));
    }
    let mut links = Vec::new();
    util::find_urls(&text, |_, u| {
        util::push_unique(&mut links, u, util::LINK_CAP)
    });
    Ok(Extracted {
        title: None,
        text,
        sections: Vec::new(),
        links,
        tags: Vec::new(),
        metadata: Value::Object(meta),
        truncated: full || cut || truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows() {
        assert_eq!(
            render_row(&["a", " b\nc ", "", ""]).as_deref(),
            Some("a | b c")
        );
        assert_eq!(render_row(&["", ""]), None);
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3"), b';');
        assert_eq!(sniff_delimiter("a\tb\n"), b'\t');
    }
}
