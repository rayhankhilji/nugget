//! Incremental Server-Sent Events parser (WHATWG `text/event-stream` rules).
//!
//! Bytes are fed in arbitrary chunks; lines may be split anywhere, including inside a UTF-8
//! sequence or between `\r` and `\n`. Supports `\n`, `\r\n` and bare `\r` line endings, comment
//! lines, multi-line `data:`, an optional leading BOM, and a final event without the trailing
//! blank line at EOF.

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` field, if any (the spec's default type is `message`).
    pub event: Option<String>,
    /// `data:` lines joined with `\n`.
    pub data: String,
    pub id: Option<String>,
}

/// Maximum buffered bytes for a single line before the parser errors (guards against a peer
/// that never sends a newline).
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    /// A `\r` ended the previous chunk; a leading `\n` in the next chunk belongs to it.
    pending_cr: bool,
    started: bool,
    event: Option<String>,
    data: String,
    has_data: bool,
    id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTooLong;

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; returns every event completed by it.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, LineTooLong> {
        let mut out = Vec::new();
        let mut chunk = chunk;
        if self.pending_cr {
            self.pending_cr = false;
            if let Some(rest) = chunk.strip_prefix(b"\n") {
                chunk = rest;
            }
        }
        let mut start = 0;
        let mut i = 0;
        while i < chunk.len() {
            let b = chunk[i];
            if b == b'\n' || b == b'\r' {
                self.buf.extend_from_slice(&chunk[start..i]);
                let line = std::mem::take(&mut self.buf);
                self.line(&line, &mut out);
                if b == b'\r' {
                    if i + 1 < chunk.len() {
                        if chunk[i + 1] == b'\n' {
                            i += 1;
                        }
                    } else {
                        self.pending_cr = true;
                    }
                }
                start = i + 1;
            }
            i += 1;
        }
        self.buf.extend_from_slice(&chunk[start..]);
        if self.buf.len() > MAX_LINE_BYTES {
            return Err(LineTooLong);
        }
        Ok(out)
    }

    /// Signal EOF: flushes a final unterminated line and a pending event.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.line(&line, &mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn line(&mut self, raw: &[u8], out: &mut Vec<SseEvent>) {
        let mut raw = raw;
        if !self.started {
            self.started = true;
            if let Some(rest) = raw.strip_prefix(b"\xEF\xBB\xBF") {
                raw = rest;
            }
        }
        if raw.is_empty() {
            self.dispatch(out);
            return;
        }
        if raw[0] == b':' {
            return; // comment / keep-alive
        }
        let line = String::from_utf8_lossy(raw);
        let (field, value) = match line.find(':') {
            Some(pos) => {
                let v = &line[pos + 1..];
                (&line[..pos], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line.as_ref(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.id = Some(value.to_string()),
            _ => {} // `retry` and unknown fields are ignored
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        let event = self.event.take();
        if !self.has_data {
            // Per spec an event with no data is not dispatched.
            return;
        }
        self.has_data = false;
        out.push(SseEvent {
            event,
            data: std::mem::take(&mut self.data),
            id: self.id.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_chunks(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c).unwrap());
        }
        out.extend(p.finish());
        out
    }

    #[test]
    fn basic_events() {
        let ev = parse_chunks(&[b"event: a\ndata: {\"x\":1}\n\nevent: b\ndata: two\n\n"]);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].event.as_deref(), Some("a"));
        assert_eq!(ev[0].data, "{\"x\":1}");
        assert_eq!(ev[1].event.as_deref(), Some("b"));
        assert_eq!(ev[1].data, "two");
    }

    #[test]
    fn every_split_point_gives_same_result() {
        let input = "\u{feff}: comment\r\nevent: message_start\r\ndata: {\"t\":\"h\u{e9}llo \u{1f600}\"}\r\n\r\ndata:a\rdata: b\r\rdata: tail";
        let bytes = input.as_bytes();
        let whole = parse_chunks(&[bytes]);
        assert_eq!(whole.len(), 3, "{whole:?}");
        assert_eq!(whole[0].event.as_deref(), Some("message_start"));
        assert_eq!(whole[0].data, "{\"t\":\"h\u{e9}llo \u{1f600}\"}");
        assert_eq!(whole[1].data, "a\nb");
        assert_eq!(whole[1].event, None);
        assert_eq!(whole[2].data, "tail");
        for split in 0..bytes.len() {
            let (a, b) = bytes.split_at(split);
            assert_eq!(parse_chunks(&[a, b]), whole, "split at {split}");
        }
        // Byte-at-a-time.
        let singles: Vec<&[u8]> = bytes.chunks(1).collect();
        assert_eq!(parse_chunks(&singles), whole);
    }

    #[test]
    fn crlf_split_across_chunks_is_one_line_ending() {
        let ev = parse_chunks(&[b"data: x\r", b"\n\r", b"\ndata: y\n\n"]);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].data, "x");
        assert_eq!(ev[1].data, "y");
    }

    #[test]
    fn event_without_data_is_not_dispatched_and_type_resets() {
        let ev = parse_chunks(&[b"event: ping\n\ndata: z\n\n"]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].event, None);
        assert_eq!(ev[0].data, "z");
    }

    #[test]
    fn empty_data_line_and_field_without_colon() {
        let ev = parse_chunks(&[b"data\ndata:\n\n"]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].data, "\n");
    }

    #[test]
    fn id_and_unknown_fields() {
        let ev = parse_chunks(&[b"id: 7\nretry: 100\nfoo: bar\ndata: d\n\n"]);
        assert_eq!(ev[0].id.as_deref(), Some("7"));
        assert_eq!(ev[0].data, "d");
    }

    #[test]
    fn overlong_line_errors() {
        let mut p = SseParser::new();
        let big = vec![b'a'; MAX_LINE_BYTES + 1];
        assert_eq!(p.push(&big), Err(LineTooLong));
    }
}
