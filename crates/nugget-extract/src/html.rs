//! HTML → readable text with a streaming tokenizer (no DOM, so no recursion on deep nesting).
//! Drops script/style/template/svg; keeps the title, links and h1–h6 sections. For mail, quoted
//! replies and signatures can be diverted into separate buffers.

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use nugget_core::Result;
use serde_json::{Map, Value};

use crate::util::{self, LINK_CAP, SectionTracker, TextBuf};
use crate::{Ctx, Extracted, Section};

const MAX_TITLE_CHARS: usize = 1_000;
const MAX_HEADING_CHARS: usize = 500;
const MAX_SIDE_CHARS: usize = 200_000;

pub(crate) struct HtmlOut {
    pub text: String,
    pub truncated: bool,
    pub title: Option<String>,
    pub sections: Vec<Section>,
    pub links: Vec<String>,
    /// Quoted reply content (only with `divert_quotes`).
    pub quoted: String,
    /// Signature content (only with `divert_quotes`).
    pub signature: String,
    pub meta: Map<String, Value>,
}

pub(crate) fn extract(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    let html = decode(data, ctx.charset);
    let out = convert(&html, ctx.opts.max_text_chars, false);
    Ok(Extracted {
        title: out.title,
        text: out.text,
        sections: out.sections,
        links: out.links,
        tags: Vec::new(),
        metadata: Value::Object(out.meta),
        truncated: out.truncated,
    })
}

/// Decode HTML bytes, honouring a `<meta charset>` declaration when the bytes are not UTF-8.
pub(crate) fn decode(data: &[u8], charset: Option<&str>) -> String {
    if std::str::from_utf8(data).is_ok() || charset.is_some() {
        return util::decode_text(data, charset);
    }
    let head = &data[..data.len().min(4096)];
    let lower = head.to_ascii_lowercase();
    let label = lower.windows(8).position(|w| w == b"charset=").map(|i| {
        let rest = &head[i + 8..];
        let rest = rest
            .strip_prefix(b"\"")
            .or_else(|| rest.strip_prefix(b"'"))
            .unwrap_or(rest);
        let end = rest
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':')))
            .unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..end]).into_owned()
    });
    util::decode_text(data, label.as_deref())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Quote,
    Signature,
}

struct State {
    buf: TextBuf,
    quoted: TextBuf,
    signature: TextBuf,
    divert_quotes: bool,
    /// Element being skipped and its nesting depth.
    skip: Option<(String, usize)>,
    /// Element whose content is diverted, its depth and where it goes.
    divert: Option<(String, usize, Side)>,
    /// Divert everything from here on (Outlook reply header).
    divert_rest: bool,
    pre: usize,
    pending_space: bool,
    in_title: bool,
    title: String,
    heading: Option<(u32, usize, String)>,
    first_h1: Option<String>,
    tracker: SectionTracker,
    links: Vec<String>,
    meta: Map<String, Value>,
}

struct Sink(RefCell<State>);

fn attr<'a>(tag: &'a Tag, name: &str) -> Option<&'a str> {
    tag.attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| &*a.value)
}

fn has_class(tag: &Tag, class: &str) -> bool {
    attr(tag, "class").is_some_and(|c| c.split_whitespace().any(|x| x == class))
}

const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "aside",
    "main",
    "blockquote",
    "figure",
    "figcaption",
    "address",
    "details",
    "summary",
    "form",
    "fieldset",
    "table",
    "ul",
    "ol",
    "dl",
    "center",
    "body",
    "caption",
    "legend",
    "hgroup",
];
const LINES: &[&str] = &["li", "dt", "dd", "tr", "option"];
/// Elements whose content is not readable text. Raw-text ones are tokenized as raw data.
const SKIP_RAW: &[&str] = &[
    "script", "style", "xmp", "iframe", "noembed", "noframes", "noscript",
];
const SKIP_NESTED: &[&str] = &["template", "svg", "math", "object", "canvas", "select"];

fn heading_level(name: &str) -> Option<u32> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

impl State {
    fn side(&self) -> Option<Side> {
        if self.divert_rest {
            return Some(Side::Quote);
        }
        self.divert.as_ref().map(|d| d.2)
    }

    fn out(&mut self) -> &mut TextBuf {
        match self.side() {
            None => &mut self.buf,
            Some(Side::Quote) => &mut self.quoted,
            Some(Side::Signature) => &mut self.signature,
        }
    }

    fn paragraph(&mut self) {
        self.pending_space = false;
        self.out().paragraph();
    }

    fn newline(&mut self) {
        self.pending_space = false;
        self.out().newline();
    }

    fn text(&mut self, s: &str) {
        if self.in_title {
            if self.title.len() < MAX_TITLE_CHARS * 4 {
                self.title.push_str(s);
            }
            return;
        }
        if self.side().is_none()
            && let Some(h) = self.heading.as_mut()
            && h.2.len() < MAX_HEADING_CHARS * 4
        {
            h.2.push_str(s);
        }
        if self.pre > 0 {
            self.pending_space = false;
            self.out().push(s);
            return;
        }
        for (i, word) in s.split_whitespace().enumerate() {
            let leading_ws = i > 0 || s.starts_with(char::is_whitespace);
            let pending = self.pending_space || leading_ws;
            let out = self.out();
            if pending && !out.at_line_start() {
                out.push(" ");
            }
            out.push(word);
            self.pending_space = false;
        }
        if s.ends_with(char::is_whitespace) {
            self.pending_space = true;
        }
    }

    fn quote_kind(&self, name: &str, tag: &Tag) -> Option<Side> {
        if !self.divert_quotes {
            return None;
        }
        if name == "blockquote"
            && (attr(tag, "type").is_some_and(|t| t.eq_ignore_ascii_case("cite"))
                || has_class(tag, "gmail_quote"))
        {
            return Some(Side::Quote);
        }
        if name == "div"
            && (has_class(tag, "gmail_quote")
                || has_class(tag, "yahoo_quoted")
                || has_class(tag, "moz-cite-prefix"))
        {
            return Some(Side::Quote);
        }
        if has_class(tag, "gmail_signature")
            || has_class(tag, "moz-signature")
            || attr(tag, "id").is_some_and(|i| i == "Signature")
        {
            return Some(Side::Signature);
        }
        None
    }

    fn start_tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        if let Some((skip, depth)) = self.skip.as_mut() {
            if skip == name && !tag.self_closing {
                *depth += 1;
            }
            return TokenSinkResult::Continue;
        }
        if let Some((div, depth, _)) = self.divert.as_mut()
            && div == name
            && !tag.self_closing
        {
            *depth += 1;
        }
        if SKIP_RAW.contains(&name) {
            self.skip = Some((name.to_owned(), 1));
            return TokenSinkResult::RawData(if name == "script" {
                RawKind::ScriptData
            } else {
                RawKind::Rawtext
            });
        }
        if SKIP_NESTED.contains(&name) {
            if !tag.self_closing {
                self.skip = Some((name.to_owned(), 1));
            }
            return TokenSinkResult::Continue;
        }
        if self.divert.is_none() && !self.divert_rest {
            if self.divert_quotes && attr(tag, "id").is_some_and(|i| i == "divRplyFwdMsg") {
                self.paragraph();
                self.divert_rest = true;
            } else if let Some(side) = self.quote_kind(name, tag)
                && !tag.self_closing
            {
                self.paragraph();
                self.divert = Some((name.to_owned(), 1, side));
            }
        }
        match name {
            "title" => {
                self.in_title = true;
                return TokenSinkResult::RawData(RawKind::Rcdata);
            }
            "textarea" => return TokenSinkResult::RawData(RawKind::Rcdata),
            "plaintext" => return TokenSinkResult::Plaintext,
            "br" => self.newline(),
            "hr" => self.paragraph(),
            "pre" => {
                self.paragraph();
                self.pre += 1;
            }
            "td" | "th" => {
                self.pending_space = false;
                let out = self.out();
                if !out.at_line_start() {
                    out.push(" | ");
                }
            }
            "li" => {
                self.newline();
                self.out().push("- ");
            }
            "a" => {
                if let Some(href) = attr(tag, "href") {
                    let href = href.trim();
                    let lower = href.get(..11).unwrap_or(href).to_ascii_lowercase();
                    if !href.is_empty()
                        && !href.starts_with('#')
                        && !lower.starts_with("javascript:")
                        && !lower.starts_with("data:")
                    {
                        util::push_unique(&mut self.links, href, LINK_CAP);
                    }
                }
            }
            "meta" => {
                let key = attr(tag, "name").or_else(|| attr(tag, "property"));
                if let (Some(key), Some(content)) = (key, attr(tag, "content")) {
                    let key = key.to_ascii_lowercase();
                    let field = match key.as_str() {
                        "description" | "og:description" => Some("description"),
                        "author" => Some("author"),
                        "keywords" => Some("keywords"),
                        "og:title" => Some("og_title"),
                        _ => None,
                    };
                    if let Some(f) = field
                        && !self.meta.contains_key(f)
                        && !content.trim().is_empty()
                    {
                        let v = util::truncate_chars(content.trim(), 2_000);
                        self.meta.insert(f.into(), Value::String(v));
                    }
                }
            }
            "html" => {
                if let Some(lang) = attr(tag, "lang") {
                    self.meta.insert("lang".into(), Value::from(lang.trim()));
                }
            }
            _ => {
                if let Some(level) = heading_level(name) {
                    self.paragraph();
                    if self.side().is_none() {
                        let at = self.buf.len();
                        self.heading = Some((level, at, String::new()));
                    }
                } else if BLOCKS.contains(&name) {
                    self.paragraph();
                } else if LINES.contains(&name) {
                    self.newline();
                }
            }
        }
        TokenSinkResult::Continue
    }

    fn end_tag(&mut self, tag: &Tag) {
        let name: &str = &tag.name;
        if let Some((skip, depth)) = self.skip.as_mut() {
            if skip == name {
                *depth -= 1;
                if *depth == 0 {
                    self.skip = None;
                }
            }
            return;
        }
        let mut leaving = false;
        if let Some((div, depth, _)) = self.divert.as_mut()
            && div == name
        {
            *depth -= 1;
            leaving = *depth == 0;
        }
        match name {
            "title" => self.in_title = false,
            "pre" => {
                self.pre = self.pre.saturating_sub(1);
                self.paragraph();
            }
            _ => {
                if heading_level(name).is_some() {
                    if let Some((level, start, raw)) = self.heading.take() {
                        let title = util::truncate_chars(&util::normalize_ws(&raw), 500);
                        if level == 1 && self.first_h1.is_none() && !title.is_empty() {
                            self.first_h1 = Some(title.clone());
                        }
                        self.tracker.heading(level, &title, start);
                    }
                    self.paragraph();
                } else if BLOCKS.contains(&name) {
                    self.paragraph();
                } else if LINES.contains(&name) {
                    self.newline();
                }
            }
        }
        if leaving {
            self.paragraph();
            self.divert = None;
        }
    }
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut st = self.0.borrow_mut();
        match token {
            Token::TagToken(tag) => match tag.kind {
                TagKind::StartTag => return st.start_tag(&tag),
                TagKind::EndTag => st.end_tag(&tag),
            },
            Token::CharacterTokens(t) if st.skip.is_none() => {
                st.text(&t);
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

/// Convert HTML to text. `divert_quotes` moves quoted replies and signatures (mail clients'
/// markup) out of the main text.
pub(crate) fn convert(html: &str, max_chars: usize, divert_quotes: bool) -> HtmlOut {
    let state = State {
        buf: TextBuf::new(max_chars),
        quoted: TextBuf::new(MAX_SIDE_CHARS),
        signature: TextBuf::new(MAX_SIDE_CHARS),
        divert_quotes,
        skip: None,
        divert: None,
        divert_rest: false,
        pre: 0,
        pending_space: false,
        in_title: false,
        title: String::new(),
        heading: None,
        first_h1: None,
        tracker: SectionTracker::default(),
        links: Vec::new(),
        meta: Map::new(),
    };
    let tok = Tokenizer::new(Sink(RefCell::new(state)), TokenizerOpts::default());
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(html));
    let _ = tok.feed(&queue);
    tok.end();
    let mut st = tok.sink.0.into_inner();
    if let Some((level, start, raw)) = st.heading.take() {
        st.tracker.heading(level, &util::normalize_ws(&raw), start);
    }
    let (text, truncated) = st.buf.finish();
    let sections = st.tracker.finish(text.len());
    let title = Some(util::truncate_chars(
        &util::normalize_ws(&st.title),
        MAX_TITLE_CHARS,
    ))
    .filter(|t| !t.is_empty())
    .or_else(|| match st.meta.get("og_title") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    })
    .or(st.first_h1);
    HtmlOut {
        text,
        truncated,
        title,
        sections,
        links: st.links,
        quoted: st.quoted.finish().0,
        signature: st.signature.finish().0,
        meta: st.meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_conversion() {
        let o = convert(
            "<html lang=en><head><title> T  1 </title><style>p{x:y}</style><script>var a='<p>';</script></head>\
             <body><h1>Guide</h1><p>Hello <b>big</b>\n world &amp; co.</p><h2>Install</h2><ul><li>one</li><li>two</li></ul>\
             <a href='https://x.example/'>x</a><a href='#top'>t</a><table><tr><td>a</td><td>b</td></tr></table></body></html>",
            10_000,
            false,
        );
        assert_eq!(o.title.as_deref(), Some("T 1"));
        assert_eq!(
            o.text,
            "Guide\n\nHello big world & co.\n\nInstall\n\n- one\n- two\n\nxt\n\na | b"
        );
        assert_eq!(o.links, ["https://x.example/"]);
        assert_eq!(o.sections.len(), 2);
        assert_eq!(o.sections[1].heading_path, ["Guide", "Install"]);
        assert_eq!(
            &o.text[o.sections[1].start..o.sections[1].start + 7],
            "Install"
        );
        assert_eq!(o.meta["lang"], "en");
    }

    #[test]
    fn diverts_quotes() {
        let o = convert(
            "<div>Thanks!</div><div class=gmail_signature>Bob</div><div class=gmail_quote>On Mon wrote:<blockquote>old <div>stuff</div></blockquote></div>",
            1000,
            true,
        );
        assert_eq!(o.text, "Thanks!");
        assert_eq!(o.signature, "Bob");
        assert!(o.quoted.contains("old") && o.quoted.contains("stuff"));
    }
}
