//! A small incremental Server-Sent Events parser for upstream responses. It only reads the
//! stream (usage, translation); Anthropic bytes are forwarded to the client unchanged.

/// One complete event: its `event:` name (empty when absent) and its joined `data:` lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub data: String,
}

/// Upstream lines longer than this are dropped rather than buffered without bound.
const MAX_PENDING: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    name: String,
    data: Vec<String>,
}

impl Parser {
    /// Feed bytes; returns the events they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(pos) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + pos;
            let mut line = &self.buf[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let line = String::from_utf8_lossy(line).into_owned();
            start = end + 1;
            self.line(&line, &mut out);
        }
        self.buf.drain(..start);
        if self.buf.len() > MAX_PENDING {
            self.buf.clear();
        }
        out
    }

    /// End of stream: an event not followed by a blank line still counts.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            self.line(rest.trim_end_matches('\r'), &mut out);
        }
        self.line("", &mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<Event>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push(Event {
                    name: std::mem::take(&mut self.name),
                    data: std::mem::take(&mut self.data).join("\n"),
                });
            }
            self.name.clear();
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.name = value.to_owned(),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
    }
}

/// Serialise one event for the client.
pub fn encode(name: &str, data: &serde_json::Value) -> Vec<u8> {
    format!("event: {name}\ndata: {data}\n\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_across_chunks_and_crlf() {
        let mut p = Parser::default();
        assert!(p.feed(b"event: a\r\ndata: {\"x\"").is_empty());
        let ev = p.feed(b":1}\r\n\r\n: comment\n\ndata: 2\ndata: 3\n\n");
        assert_eq!(
            ev,
            vec![
                Event {
                    name: "a".into(),
                    data: "{\"x\":1}".into()
                },
                Event {
                    name: String::new(),
                    data: "2\n3".into()
                }
            ]
        );
        assert!(p.feed(b"data: [DONE]").is_empty());
        assert_eq!(p.finish()[0].data, "[DONE]");
        assert!(p.finish().is_empty());
    }
}
