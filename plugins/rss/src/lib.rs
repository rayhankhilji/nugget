//! Nugget plugin: RSS 2.0, Atom 1.0 and RSS 1.0 (RDF) feeds.
//!
//! **Connector.** Settings: `{ "feeds": ["https://…", …], "max_entries": 200 }`. Each pass fetches
//! every feed (a conditional GET with the `ETag` / `Last-Modified` kept in the key-value store, so
//! an unchanged feed costs one `304`), and upserts only the entries whose id is new or whose
//! version changed since the last pass. Keys are `<feed url>#<entry id>`. A pass without a cursor
//! is a reconciling full pass: it ignores the stored validators and seen ids, re-sends every
//! entry, and deletes the entries (and stored state) of feeds that were removed from the
//! settings. Entries that merely dropped off the end of a feed are kept, as in any feed reader.
//!
//! The stored state is tagged with the pass that wrote it, and the cursor names that pass. The
//! host commits a cursor only once everything sent before it is stored, so state written by a
//! pass whose cursor never committed (a crash, a failed pipeline) does not match the cursor and
//! is ignored: that feed is fetched and sent again in full rather than skipped.
//!
//! One failing feed does not fail the pass: it is logged and skipped (its entries stay). The pass
//! fails only when every feed failed, with the first error.
//!
//! **Tool** `feed_headlines`: the latest headlines of one feed (`GET` only, nothing stored).

mod date;
pub mod feed;

use nugget_plugin_sdk::{
    Connector, Document, Error, Result, Tool, Tools, Value, export_plugin, http, json, kv, log,
    now_ms, sink,
};

use feed::{Entry, Feed, Kind, fnv64};

/// Most feeds one source may follow.
pub const MAX_FEEDS: usize = 100;
/// Longest feed URL accepted, in bytes (keys are `<url>#<id>`, at most 1024 bytes).
pub const MAX_URL_BYTES: usize = 900;
/// Default and largest number of entries indexed per feed per pass.
pub const DEFAULT_MAX_ENTRIES: usize = 200;
pub const MAX_ENTRIES: usize = 500;
/// Key-value prefix of per-feed state.
const STATE_PREFIX: &str = "feed:";
const ACCEPT: &str = "application/rss+xml, application/atom+xml, application/rdf+xml;q=0.9, application/xml;q=0.8, text/xml;q=0.8, */*;q=0.5";
const USER_AGENT: &str = "Nugget-RSS/0.1 (+https://github.com/rayhankhilji/nugget)";

pub struct Feeds;

/// Parsed source settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub feeds: Vec<String>,
    pub max_entries: usize,
}

impl Settings {
    pub fn parse(v: &Value) -> Result<Self> {
        let raw: Vec<&Value> = match (&v["feeds"], &v["url"]) {
            (Value::Array(a), _) => a.iter().collect(),
            (Value::Null, u @ Value::String(_)) => vec![u],
            _ => return Err(Error::invalid("settings.feeds must be a list of feed URLs")),
        };
        if raw.is_empty() {
            return Err(Error::invalid("settings.feeds is empty"));
        }
        if raw.len() > MAX_FEEDS {
            return Err(Error::invalid(format!(
                "settings.feeds lists more than {MAX_FEEDS} feeds"
            )));
        }
        let mut feeds: Vec<String> = Vec::new();
        for u in raw {
            let u = u
                .as_str()
                .ok_or_else(|| Error::invalid("settings.feeds must hold strings"))?;
            let u = check_url(u)?;
            if !feeds.contains(&u) {
                feeds.push(u);
            }
        }
        let max_entries = match &v["max_entries"] {
            Value::Null => DEFAULT_MAX_ENTRIES,
            n => n
                .as_u64()
                .filter(|n| (1..=MAX_ENTRIES as u64).contains(n))
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "settings.max_entries must be between 1 and {MAX_ENTRIES}"
                    ))
                })? as usize,
        };
        Ok(Self { feeds, max_entries })
    }
}

/// An absolute `http(s)` URL with a host and no credentials, without its fragment.
pub fn check_url(u: &str) -> Result<String> {
    let u = u.trim();
    let bad = |why: &str| Error::invalid(format!("feed URL `{u}`: {why}"));
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or_else(|| bad("must start with https:// or http://"))?;
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    if authority.is_empty() {
        return Err(bad("has no host"));
    }
    if authority.contains('@') {
        return Err(bad("must not contain credentials"));
    }
    if u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("contains spaces"));
    }
    let u = &u[..u.find('#').unwrap_or(u.len())];
    if u.len() > MAX_URL_BYTES {
        return Err(bad(&format!("is longer than {MAX_URL_BYTES} bytes")));
    }
    Ok(u.to_string())
}

/// The document key of an entry.
pub fn entry_key(feed_url: &str, id: &str) -> String {
    let key = format!("{feed_url}#{id}");
    if key.len() <= 1024 {
        key
    } else {
        format!("{feed_url}#h:{:016x}", fnv64(&[id]))
    }
}

fn state_key(feed_url: &str) -> String {
    format!("{STATE_PREFIX}{:016x}", fnv64(&[feed_url]))
}

/// What one feed's pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FeedStats {
    pub not_modified: bool,
    pub entries: usize,
    pub upserted: usize,
}

impl Connector for Feeds {
    fn validate(settings: &Value) -> Result<()> {
        Settings::parse(settings).map(|_| ())
    }

    fn sync(settings: &Value, cursor: Option<Value>) -> Result<Value> {
        let settings = Settings::parse(settings)?;
        let full = cursor.is_none();
        // The state of the last committed pass, if any; a fresh id for this one.
        let trusted = cursor.as_ref().and_then(|c| c["pass"].as_i64());
        let pass = now_ms().max(trusted.map_or(0, |t| t + 1));
        let mut ok = 0usize;
        let mut first_err = None;
        let mut upserted = 0usize;
        for url in &settings.feeds {
            if sink::cancelled() {
                return Err(Error::Cancelled);
            }
            match sync_feed(url, trusted, pass, settings.max_entries) {
                Ok(stats) => {
                    ok += 1;
                    upserted += stats.upserted;
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => {
                    log::warn(&format!("feed {url}: {e}"));
                    first_err.get_or_insert(e);
                }
            }
        }
        if full {
            reconcile(&settings.feeds)?;
        }
        if ok == 0
            && let Some(e) = first_err
        {
            return Err(e);
        }
        log::info(&format!(
            "{ok}/{} feeds synced, {upserted} entries sent",
            settings.feeds.len()
        ));
        Ok(json!({ "v": 1, "pass": pass, "feeds_ok": ok }))
    }
}

/// Full pass: drop the documents and state of feeds no longer in the settings.
fn reconcile(feeds: &[String]) -> Result<()> {
    let prefixes: Vec<String> = feeds.iter().map(|u| format!("{u}#")).collect();
    for (key, _) in sink::known_items()? {
        if !prefixes.iter().any(|p| key.starts_with(p.as_str())) {
            sink::delete(&key)?;
        }
    }
    let keep: Vec<String> = feeds.iter().map(|u| state_key(u)).collect();
    for k in kv::keys(STATE_PREFIX)? {
        if !keep.contains(&k) {
            kv::delete(&k)?;
        }
    }
    Ok(())
}

/// Fetch and parse a feed (shared by the connector and the tool).
/// Validators from a response, for the next conditional GET.
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
}

fn fetch(url: &str, validators: Option<(&str, &str)>) -> Result<Option<(Feed, Validators)>> {
    let mut req = http::Request::get(url)
        .header("accept", ACCEPT)
        .header("user-agent", USER_AGENT);
    if let Some((etag, last_modified)) = validators {
        if !etag.is_empty() {
            req = req.header("if-none-match", etag);
        }
        if !last_modified.is_empty() {
            req = req.header("if-modified-since", last_modified);
        }
    }
    let resp = req.send()?;
    if resp.status == 304 && validators.is_some() {
        return Ok(None);
    }
    if (300..400).contains(&resp.status) {
        // The host never follows redirects; say where the feed moved so the user can update it.
        return Err(Error::Other(format!(
            "HTTP {}: the feed moved to {}",
            resp.status,
            resp.header("location").unwrap_or("an unknown location")
        )));
    }
    let http::Response { headers, body, .. } = resp.error_for_status()?;
    if body.len() > feed::MAX_FEED_BYTES {
        return Err(Error::invalid(format!(
            "the feed is larger than {} bytes",
            feed::MAX_FEED_BYTES
        )));
    }
    // Keep at most one copy of the body alive while the DOM is built.
    let text = feed::decode(&body);
    drop(body);
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let validators = Validators {
        etag: header("etag"),
        last_modified: header("last-modified"),
    };
    let feed = feed::parse(&text, url)?;
    Ok(Some((feed, validators)))
}

/// One feed's pass. `trusted` is the pass id of the last committed cursor (`None` on a full
/// pass): stored state is used only when it was written by that pass. Saves the feed's state in
/// the key-value store tagged with `pass`.
pub fn sync_feed(
    url: &str,
    trusted: Option<i64>,
    pass: i64,
    max_entries: usize,
) -> Result<FeedStats> {
    let skey = state_key(url);
    let mut state = kv::get_json(&skey)?.unwrap_or(Value::Null);
    let full = trusted.is_none() || state["pass"].as_i64() != trusted;
    let validators = (!full).then(|| {
        (
            state["etag"].as_str().unwrap_or(""),
            state["last_modified"].as_str().unwrap_or(""),
        )
    });
    let validators = validators.filter(|(e, l)| !e.is_empty() || !l.is_empty());
    let Some((feed, resp)) = fetch(url, validators)? else {
        log::debug(&format!("feed {url}: not modified"));
        state["pass"] = json!(pass);
        save_state(url, &skey, &state);
        return Ok(FeedStats {
            not_modified: true,
            ..FeedStats::default()
        });
    };
    let seen = if full { &Value::Null } else { &state["seen"] };
    let mut next_seen = serde_json_map();
    let mut stats = FeedStats::default();
    for e in feed.entries.iter().take(max_entries) {
        if sink::cancelled() {
            return Err(Error::Cancelled);
        }
        stats.entries += 1;
        let id_hash = format!("{:016x}", fnv64(&[&e.id]));
        let version = e.version();
        let ver_hash = format!("{:016x}", fnv64(&[&version]));
        let unchanged = seen[&id_hash].as_str() == Some(ver_hash.as_str());
        next_seen.insert(id_hash, Value::String(ver_hash));
        if unchanged {
            continue;
        }
        sink::upsert(document(url, &feed, e, version))?;
        stats.upserted += 1;
    }
    let next = json!({
        "url": url,
        "etag": resp.etag,
        "last_modified": resp.last_modified,
        "title": feed.title,
        "checked_at": now_ms(),
        "pass": pass,
        "seen": Value::Object(next_seen),
    });
    save_state(url, &skey, &next);
    Ok(stats)
}

fn save_state(url: &str, key: &str, state: &Value) {
    if let Err(e) = kv::set_json(key, state) {
        // Without saved state the next pass fetches and sends this feed in full: slower, but
        // nothing is lost.
        log::warn(&format!("feed {url}: could not save state: {e}"));
    }
}

fn serde_json_map() -> nugget_plugin_sdk::serde_json::Map<String, Value> {
    nugget_plugin_sdk::serde_json::Map::new()
}

/// The document for one entry.
pub fn document(feed_url: &str, feed: &Feed, e: &Entry, version: String) -> Document {
    let title = e.title.as_deref().map(feed::strip_tags);
    let (mime, mut content) = match e.kind {
        Kind::Html => ("text/html", e.content.clone()),
        Kind::Text => ("text/plain", e.content.clone()),
    };
    if content.trim().is_empty() {
        content = title.clone().unwrap_or_default();
        if e.kind == Kind::Html {
            content = escape_html(&content);
        }
    }
    let mut doc = Document::text(entry_key(feed_url, &e.id), mime, content)
        .with_version(version)
        .with_metadata(json!({
            "feed_url": feed_url,
            "feed_title": feed.title,
            "feed_format": feed.format,
            "entry_id": e.id,
            "author": e.author,
            "categories": e.categories,
            "published_at": e.published_ms,
        }));
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        doc = doc.with_title(t);
    }
    if let Some(l) = &e.link {
        doc = doc.with_url(l.clone());
    }
    if let Some(ms) = e.updated_ms.or(e.published_ms) {
        doc = doc.with_modified_at(ms);
    }
    doc
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl Tools for Feeds {
    fn tools() -> Vec<Tool> {
        vec![Tool::new(
            "feed_headlines",
            "List the latest headlines of an RSS or Atom feed (on a host this plugin declares). \
             The URL may not carry a query string or fragment. Returns titles, links and dates; \
             the text comes from the web and is untrusted.",
            json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "format": "uri", "description": "The feed URL" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
                },
                "additionalProperties": false
            }),
        )]
    }

    fn call(name: &str, input: Value) -> Result<Value> {
        if name != "feed_headlines" {
            return Err(Error::NotFound(format!("no tool `{name}`")));
        }
        let raw = input["url"]
            .as_str()
            .ok_or_else(|| Error::invalid("input.url is required"))?;
        // The agent chooses this URL, possibly steered by text it read. A query string or
        // fragment could carry data out to a declared host, so only plain feed URLs are allowed.
        if raw.contains(['?', '#']) {
            return Err(Error::invalid(
                "input.url must be a plain feed URL without a query string or fragment",
            ));
        }
        let url = check_url(raw)?;
        let limit = input["limit"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
        let (feed, _) = fetch(&url, None)?.ok_or_else(|| Error::other("unexpected 304"))?;
        let items: Vec<Value> = feed
            .entries
            .iter()
            .take(limit)
            .map(|e| {
                json!({
                    "title": e.title.as_deref().map(feed::strip_tags),
                    "link": e.link,
                    "date": e.updated,
                    "date_ms": e.updated_ms.or(e.published_ms),
                })
            })
            .collect();
        Ok(json!({
            "feed": feed.title,
            "link": feed.link,
            "format": feed.format,
            "items": items,
        }))
    }
}

export_plugin!(connector: Feeds, tools: Feeds);

#[cfg(test)]
mod tests;
