//! App logos as `data:` URIs. The desktop's CSP is `img-src 'self' data:`, so `/v1/apps` cannot
//! hand it remote URLs. Logos are fetched here, size-capped, checked, cached, and served inline:
//!
//! - PNG, JPEG, WebP and GIF pass when their magic bytes match; anything else is refused.
//! - SVG is rebuilt from an allowlist: known shape and paint elements and attributes only.
//!   Scripts, `<style>`, `<foreignObject>`, `<image>`, event handlers, comments, DOCTYPEs and
//!   entities are dropped, and any reference that is not same-document (`href="#…"`,
//!   `url(#…)`) removes the attribute. An `<img>` would not run scripts anyway; this is defence
//!   in depth for any other way the markup gets used.
//! - Whatever cannot be fetched or fails a check becomes a generated monogram, so every app
//!   always has a logo.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;

/// Largest logo accepted, in bytes.
pub const MAX_LOGO_BYTES: usize = 32 * 1024;
const OK_TTL: Duration = Duration::from_secs(24 * 3600);
const FAIL_TTL: Duration = Duration::from_secs(3600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// When it was fetched, and the data URI (None: the fetch or a check failed).
type Cached = (Instant, Option<String>);
/// A tag's attributes, whether it closes itself, and how many bytes it took.
type Attrs = (Vec<(String, String)>, bool, usize);

#[derive(Clone, Default)]
pub struct LogoCache {
    /// None: never fetch (tests, or no network); every logo is a monogram.
    http: Option<reqwest::Client>,
    /// Plain `http://` is accepted only in tests (a local mock server).
    allow_http: bool,
    map: Arc<Mutex<HashMap<String, Cached>>>,
}

impl LogoCache {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http: Some(http),
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub fn insecure_for_tests(http: reqwest::Client) -> Self {
        Self {
            http: Some(http),
            allow_http: true,
            ..Default::default()
        }
    }

    /// The logo at `url` as a `data:` URI, or a monogram for `name`.
    pub async fn data_uri(&self, url: &str, name: &str) -> String {
        let cached = self
            .map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(url)
            .filter(|(at, v)| at.elapsed() < if v.is_some() { OK_TTL } else { FAIL_TTL })
            .map(|(_, v)| v.clone());
        let got = match cached {
            Some(v) => v,
            None => {
                let v = self.fetch(url).await;
                if self.http.is_some() {
                    self.map
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(url.to_owned(), (Instant::now(), v.clone()));
                }
                v
            }
        };
        got.unwrap_or_else(|| monogram(name))
    }

    async fn fetch(&self, url: &str) -> Option<String> {
        let http = self.http.as_ref()?;
        let ok_scheme =
            url.starts_with("https://") || (self.allow_http && url.starts_with("http://"));
        if !ok_scheme {
            return None;
        }
        let mut res = http.get(url).timeout(FETCH_TIMEOUT).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        if res
            .content_length()
            .is_some_and(|n| n > MAX_LOGO_BYTES as u64)
        {
            return None;
        }
        let ctype = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_LOGO_BYTES {
                return None;
            }
        }
        to_data_uri(&ctype, &body)
    }
}

/// Checks a fetched image and encodes it, or None.
pub fn to_data_uri(content_type: &str, body: &[u8]) -> Option<String> {
    if body.is_empty() || body.len() > MAX_LOGO_BYTES {
        return None;
    }
    let b64 = |mime: &str, bytes: &[u8]| {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    };
    let raster = [
        ("image/png", &b"\x89PNG\r\n\x1a\n"[..]),
        ("image/jpeg", &b"\xff\xd8\xff"[..]),
        ("image/gif", &b"GIF8"[..]),
    ];
    for (mime, magic) in raster {
        if body.starts_with(magic) {
            return Some(b64(mime, body));
        }
    }
    if body.len() > 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP" {
        return Some(b64("image/webp", body));
    }
    let looks_svg =
        content_type.contains("svg") || std::str::from_utf8(body).is_ok_and(|t| t.contains("<svg"));
    if looks_svg {
        let text = std::str::from_utf8(body).ok()?;
        let clean = sanitize_svg(text)?;
        return Some(b64("image/svg+xml", clean.as_bytes()));
    }
    None
}

const ELEMENTS: &[&str] = &[
    "svg",
    "g",
    "path",
    "circle",
    "ellipse",
    "rect",
    "line",
    "polyline",
    "polygon",
    "defs",
    "lineargradient",
    "radialgradient",
    "stop",
    "clippath",
    "mask",
    "title",
    "desc",
    "symbol",
    "use",
    "text",
    "tspan",
];

const ATTRIBUTES: &[&str] = &[
    "xmlns",
    "xmlns:xlink",
    "viewbox",
    "width",
    "height",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "d",
    "points",
    "fill",
    "fill-rule",
    "fill-opacity",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "stroke-dasharray",
    "opacity",
    "transform",
    "id",
    "class",
    "offset",
    "stop-color",
    "stop-opacity",
    "gradientunits",
    "gradienttransform",
    "spreadmethod",
    "fx",
    "fy",
    "clip-path",
    "clip-rule",
    "mask",
    "maskunits",
    "preserveaspectratio",
    "version",
    "href",
    "xlink:href",
    "font-size",
    "font-family",
    "font-weight",
    "text-anchor",
    "style",
    "patternunits",
    "display",
    "visibility",
];

/// An attribute value that only refers within the document (or to nothing).
fn value_ok(name: &str, v: &str) -> bool {
    let low: String = v
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if name == "href" || name == "xlink:href" {
        return low.starts_with('#');
    }
    if low.contains("javascript:") || low.contains("expression(") || low.contains("@import") {
        return false;
    }
    // Every url(...) must be a fragment reference.
    let mut rest = low.as_str();
    while let Some(i) = rest.find("url(") {
        rest = &rest[i + 4..];
        let inner = rest.trim_start_matches(['"', '\'']);
        if !inner.starts_with('#') {
            return false;
        }
    }
    true
}

fn escape_attr(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_text(v: &str) -> String {
    // Entities (`&name;`) could pull in DTD definitions; numeric ones are kept by re-escaping.
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Parses `name="v" name2='v'` up to the end of a tag. Returns (attributes, self_closing, len).
fn parse_attrs(s: &str) -> Option<Attrs> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        if b[i] == b'>' {
            return Some((out, false, i + 1));
        }
        if b[i] == b'/' && b.get(i + 1) == Some(&b'>') {
            return Some((out, true, i + 2));
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !b"=>/".contains(&b[i]) {
            i += 1;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if b.get(i) == Some(&b'=') {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            match b.get(i) {
                Some(&q) if q == b'"' || q == b'\'' => {
                    let end = s[i + 1..].find(q as char)? + i + 1;
                    value = s[i + 1..end].to_owned();
                    i = end + 1;
                }
                Some(_) => {
                    let start = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    value = s[start..i].to_owned();
                }
                None => return None,
            }
        } else if name.is_empty() {
            // Stray character: skip it.
            i += 1;
            continue;
        }
        out.push((name, value));
    }
}

/// Rebuilds an SVG from the allowlist. None when there is no `<svg>` root or it is malformed.
pub fn sanitize_svg(input: &str) -> Option<String> {
    let mut out = String::new();
    let mut stack: Vec<(String, bool)> = Vec::new(); // (name, kept)
    let mut dropping = 0usize; // depth inside a dropped subtree
    let mut i = 0;
    let mut saw_root = false;
    while i < input.len() {
        let rest = &input[i..];
        if let Some(r) = rest.strip_prefix("<!--") {
            i += 4 + r.find("-->")? + 3;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>")?;
            i += end + 3;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            // DOCTYPE (entities), processing instructions: dropped. A DOCTYPE with an internal
            // subset (`[...]`) is refused outright.
            let end = rest.find('>')?;
            if rest[..end].contains('[') {
                return None;
            }
            i += end + 1;
            continue;
        }
        if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>')?;
            let name = r[..end].trim().to_ascii_lowercase();
            i += 2 + end + 1;
            let (open, kept) = stack.pop()?;
            if open != name {
                return None;
            }
            if dropping > 0 {
                dropping -= 1;
            } else if kept {
                out.push_str(&format!("</{name}>"));
            }
            continue;
        }
        if let Some(r) = rest.strip_prefix('<') {
            let name_len = r
                .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .unwrap_or(r.len());
            let name = r[..name_len].to_ascii_lowercase();
            if name.is_empty() {
                return None;
            }
            let (attrs, self_closing, len) = parse_attrs(&r[name_len..])?;
            i += 1 + name_len + len;
            if !saw_root {
                if name != "svg" {
                    return None;
                }
                saw_root = true;
            }
            let keep = dropping == 0 && ELEMENTS.contains(&name.as_str());
            let drop_subtree = dropping > 0 || !keep;
            if keep {
                out.push('<');
                out.push_str(&name);
                for (a, v) in attrs {
                    if a.starts_with("on") || !ATTRIBUTES.contains(&a.as_str()) || !value_ok(&a, &v)
                    {
                        continue;
                    }
                    if a.starts_with("xmlns") && !v.starts_with("http://www.w3.org/") {
                        continue;
                    }
                    out.push_str(&format!(" {a}=\"{}\"", escape_attr(&v)));
                }
                out.push_str(if self_closing { "/>" } else { ">" });
            }
            // Anything not allowlisted (script, style, foreignObject, image, a, ...) is
            // dropped together with its content.
            if !self_closing {
                stack.push((name, keep));
                if drop_subtree {
                    dropping += 1;
                }
            }
            continue;
        }
        let next = rest.find('<').unwrap_or(rest.len());
        if dropping == 0 && !stack.is_empty() {
            out.push_str(&escape_text(&rest[..next].replace("&amp;", "&")));
        }
        i += next;
    }
    (saw_root && stack.is_empty()).then_some(out)
}

/// A letter on a coloured rounded square, for apps without a usable logo.
pub fn monogram(name: &str) -> String {
    let letter = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    let hue = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)))
        % 360;
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\"><rect width=\"64\" height=\"64\" rx=\"14\" fill=\"hsl({hue},55%,45%)\"/><text x=\"32\" y=\"42\" font-size=\"30\" font-family=\"sans-serif\" font-weight=\"600\" text-anchor=\"middle\" fill=\"#fff\">{}</text></svg>",
        escape_text(&letter)
    );
    format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(uri: &str) -> String {
        let b = uri.split_once(";base64,").unwrap().1;
        String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b).unwrap()).unwrap()
    }

    #[test]
    fn svg_is_rebuilt_from_the_allowlist() {
        let evil = r##"<?xml version="1.0"?><!-- hi --><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10" onload="alert(1)">
<script>alert(2)</script><style>@import url(https://x/y.css)</style>
<foreignObject><div xmlns="http://www.w3.org/1999/xhtml">x</div></foreignObject>
<image href="https://tracker.example/p.png"/>
<a href="https://evil.example"><path d="M0 0h1"/></a>
<path d="M0 0h10v10z" fill="url(#g)" onclick="x()" style="fill:url(https://x/y)"/>
<use href="#p"/><use xlink:href="https://x/s.svg#a"/>
<linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></svg>"##;
        let clean = sanitize_svg(evil).unwrap();
        for bad in [
            "script",
            "alert",
            "onload",
            "onclick",
            "style",
            "foreignObject",
            "image",
            "https://",
            "<a",
            "@import",
        ] {
            assert!(!clean.contains(bad), "{bad} survived: {clean}");
        }
        assert!(
            clean.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewbox=\"0 0 10 10\">"),
            "{clean}"
        );
        assert!(clean.contains("fill=\"url(#g)\""));
        assert!(clean.contains("<use href=\"#p\"/>"));
        assert!(clean.contains("<stop offset=\"0\" stop-color=\"#f00\"/>"));
        assert!(clean.ends_with("</svg>"));
    }

    #[test]
    fn malformed_or_hostile_svg_is_refused() {
        assert_eq!(
            sanitize_svg("<html><svg/></html>"),
            None,
            "svg must be the root"
        );
        assert_eq!(sanitize_svg("<svg><g></svg>"), None, "unbalanced");
        assert_eq!(
            sanitize_svg(
                r#"<!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/passwd">]><svg>&x;</svg>"#
            ),
            None,
            "internal DTD subsets"
        );
        assert_eq!(sanitize_svg("<svg"), None);
        assert_eq!(sanitize_svg("plain text"), None);
    }

    #[test]
    fn rasters_by_magic_and_size_cap() {
        let png = b"\x89PNG\r\n\x1a\n rest";
        assert!(
            to_data_uri("image/png", png)
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(to_data_uri("image/png", b"<html>not a png").is_none());
        let big = [b"\x89PNG\r\n\x1a\n".to_vec(), vec![0; MAX_LOGO_BYTES]].concat();
        assert!(to_data_uri("image/png", &big).is_none());
        let svg = to_data_uri(
            "image/svg+xml",
            b"<svg viewBox=\"0 0 1 1\"><path d=\"M0 0\"/></svg>",
        )
        .unwrap();
        assert!(svg.starts_with("data:image/svg+xml;base64,"));
        assert_eq!(
            decode(&svg),
            "<svg viewbox=\"0 0 1 1\"><path d=\"M0 0\"/></svg>"
        );
    }

    #[test]
    fn monogram_is_a_safe_svg() {
        let m = monogram("<Slack>");
        assert!(m.starts_with("data:image/svg+xml;base64,"));
        let svg = decode(&m);
        assert!(svg.contains(">S</text>"));
        assert!(sanitize_svg(&svg).is_some());
    }

    #[tokio::test]
    async fn fetches_caches_and_falls_back() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slack.svg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/svg+xml")
                    .set_body_string("<svg><script>x</script><circle r=\"1\"/></svg>"),
            )
            .expect(1) // cached after the first fetch
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/huge.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x89; MAX_LOGO_BYTES + 1]))
            .mount(&server)
            .await;
        let c = LogoCache::insecure_for_tests(reqwest::Client::new());
        let url = format!("{}/slack.svg", server.uri());
        let a = c.data_uri(&url, "Slack").await;
        assert_eq!(decode(&a), "<svg><circle r=\"1\"/></svg>");
        assert_eq!(c.data_uri(&url, "Slack").await, a);
        let fallback = c
            .data_uri(&format!("{}/huge.png", server.uri()), "Notion")
            .await;
        assert!(
            decode(&fallback).contains(">N</text>"),
            "over the cap: monogram"
        );
        // https only outside tests; no fetcher: monogram.
        let prod = LogoCache::new(reqwest::Client::new());
        assert!(decode(&prod.data_uri(&url, "Slack").await).contains(">S</text>"));
        assert!(
            decode(&LogoCache::default().data_uri("https://x/y", "Gmail").await)
                .contains(">G</text>")
        );
    }
}
