//! Minimal, hostile-input-safe WebDAV XML: a small element tree with namespace resolution.
//!
//! - DOCTYPE (and so any DTD or custom entity) is rejected outright;
//! - only the five predefined entities and character references are resolved;
//! - nesting depth, element count and text size are bounded.

use std::borrow::Cow;

use nugget_core::{Error, Result};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};

pub const DAV: &str = "DAV:";
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const CS: &str = "http://calendarserver.org/ns/";

const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 500_000;
/// Attributes kept across a whole document (WebDAV responses hardly use any).
const MAX_ATTRIBUTES: usize = 100_000;
/// Longest namespace URI kept verbatim. Every element owns a copy of its namespace, so a long
/// URI declared once and used by many elements would otherwise multiply into gigabytes.
const MAX_NS_LEN: usize = 256;
/// What a namespace URI longer than [`MAX_NS_LEN`] is recorded as (it never matches ours).
const LONG_NS: &str = "urn:nugget:overlong-namespace";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    pub ns: String,
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    pub fn child(&self, ns: &str, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.is(ns, name))
    }

    pub fn children<'a>(&'a self, ns: &'a str, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.is(ns, name))
    }

    /// Trimmed text of a direct child.
    pub fn child_text(&self, ns: &str, name: &str) -> Option<String> {
        self.child(ns, name).map(|c| c.text.trim().to_string())
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn bad(msg: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("malformed WebDAV XML: {msg}"))
}

fn qualified(ns: ResolveResult<'_>, local: &str) -> Result<(String, String)> {
    let ns = match ns {
        ResolveResult::Bound(n) => {
            let n = n.as_ref();
            if n.len() > MAX_NS_LEN {
                LONG_NS.to_string()
            } else {
                n.to_string()
            }
        }
        ResolveResult::Unbound => String::new(),
        ResolveResult::Unknown(p) => return Err(bad(format!("unknown prefix `{p}`"))),
    };
    Ok((ns, local.to_string()))
}

/// Parse a document into its root element.
pub fn parse(body: &[u8]) -> Result<Element> {
    let text = String::from_utf8_lossy(body);
    let mut reader = NsReader::from_str(&text);
    let cfg = reader.config_mut();
    cfg.check_end_names = true;
    cfg.expand_empty_elements = false;

    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut count = 0usize;
    let mut attr_count = 0usize;

    loop {
        let (ns, event) = reader.read_resolved_event().map_err(bad)?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if root.is_some() {
                    return Err(bad("content after the root element"));
                }
                count += 1;
                if count > MAX_ELEMENTS {
                    return Err(bad("too many elements"));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(bad("nesting too deep"));
                }
                let (ns, name) = qualified(ns, e.local_name().as_ref())?;
                let mut attrs = Vec::new();
                for a in e.attributes() {
                    let a = a.map_err(bad)?;
                    attr_count += 1;
                    if attr_count > MAX_ATTRIBUTES {
                        return Err(bad("too many attributes"));
                    }
                    let v = a.normalized_value(XmlVersion::Implicit1_0).map_err(bad)?;
                    attrs.push((a.key.local_name().as_ref().to_string(), v.into_owned()));
                }
                let el = Element {
                    ns,
                    name,
                    attrs,
                    ..Default::default()
                };
                if matches!(event, Event::Empty(_)) {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => root = Some(el),
                    }
                } else {
                    stack.push(el);
                }
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| bad("unbalanced end tag"))?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => root = Some(el),
                }
            }
            Event::Text(t) => push_text(&mut stack, t.xml10_content())?,
            Event::CData(t) => push_text(&mut stack, t.xml10_content())?,
            Event::GeneralRef(r) => {
                let resolved: String = match r.resolve_char_ref().map_err(bad)? {
                    Some(c) => c.to_string(),
                    None => {
                        let name = r.xml10_content();
                        quick_xml::escape::resolve_predefined_entity(&name)
                            .ok_or_else(|| bad(format!("undefined entity `&{name};`")))?
                            .to_string()
                    }
                };
                push_text(&mut stack, Cow::Owned(resolved))?;
            }
            Event::DocType(_) => return Err(bad("DOCTYPE is not allowed")),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(bad("unexpected end of document"));
    }
    root.ok_or_else(|| bad("no root element"))
}

fn push_text(stack: &mut [Element], t: Cow<'_, str>) -> Result<()> {
    match stack.last_mut() {
        Some(el) => {
            el.text.push_str(&t);
            Ok(())
        }
        None if t.trim().is_empty() => Ok(()),
        None => Err(bad("text outside the root element")),
    }
}

/// Escape text for an XML body.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Multistatus
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct MultiStatus {
    pub responses: Vec<DavResponse>,
    pub sync_token: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DavResponse {
    pub href: String,
    /// `DAV:status` directly on the response (e.g. 404 for a deleted member, 507 for a
    /// truncated sync result).
    pub status: Option<u16>,
    pub propstats: Vec<(u16, Element)>,
}

impl DavResponse {
    /// Properties reported with a 2xx status.
    pub fn ok_props(&self) -> impl Iterator<Item = &Element> {
        self.propstats
            .iter()
            .filter(|(s, _)| (200..300).contains(s))
            .map(|(_, p)| p)
    }

    pub fn prop(&self, ns: &str, name: &str) -> Option<&Element> {
        self.ok_props().find_map(|p| p.child(ns, name))
    }

    pub fn prop_text(&self, ns: &str, name: &str) -> Option<String> {
        self.prop(ns, name)
            .map(|e| e.text.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// `<prop><X><href>..</href></X></prop>`
    pub fn prop_href(&self, ns: &str, name: &str) -> Option<String> {
        self.prop(ns, name)?
            .child_text(DAV, "href")
            .filter(|s| !s.is_empty())
    }

    pub fn resource_types(&self) -> Vec<(String, String)> {
        self.prop(DAV, "resourcetype")
            .map(|rt| {
                rt.children
                    .iter()
                    .map(|c| (c.ns.clone(), c.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_calendar(&self) -> bool {
        self.resource_types()
            .iter()
            .any(|(ns, n)| ns == CALDAV && n == "calendar")
    }

    pub fn is_collection(&self) -> bool {
        self.resource_types()
            .iter()
            .any(|(ns, n)| ns == DAV && n == "collection")
    }
}

/// `HTTP/1.1 404 Not Found` → 404.
pub fn status_code(s: &str) -> Option<u16> {
    s.split_whitespace().nth(1)?.parse().ok()
}

pub fn multistatus(body: &[u8]) -> Result<MultiStatus> {
    let root = parse(body)?;
    if !root.is(DAV, "multistatus") {
        return Err(bad(format!("expected DAV:multistatus, got {}", root.name)));
    }
    let mut ms = MultiStatus {
        sync_token: root.child_text(DAV, "sync-token").filter(|s| !s.is_empty()),
        ..Default::default()
    };
    for r in root.children(DAV, "response") {
        let Some(href) = r.child_text(DAV, "href") else {
            continue;
        };
        let status = r.child_text(DAV, "status").and_then(|s| status_code(&s));
        let mut propstats = Vec::new();
        for ps in r.children(DAV, "propstat") {
            let code = ps
                .child_text(DAV, "status")
                .and_then(|s| status_code(&s))
                .unwrap_or(0);
            if let Some(prop) = ps.child(DAV, "prop") {
                propstats.push((code, prop.clone()));
            }
        }
        ms.responses.push(DavResponse {
            href,
            status,
            propstats,
        });
    }
    Ok(ms)
}
