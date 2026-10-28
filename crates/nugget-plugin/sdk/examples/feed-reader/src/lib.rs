//! Example Nugget plugin: an RSS 2.0 / Atom feed connector and a `feed_headlines` tool.
//!
//! Settings: `{ "url": "https://feeds.example.com/news.xml" }`. The feed's host must be listed in
//! `capabilities.hosts` of `nugget-plugin.toml`; the host refuses every other destination.
//!
//! - Document keys are `<feed url>#<entry id>`, so a reconciling pass (no cursor) can delete the
//!   items of a feed URL the source no longer points at.
//! - The cursor holds the response's `ETag`/`Last-Modified` for a conditional GET; a `304` ends
//!   the pass without changes.

use nugget_plugin_sdk::{
    Connector, Document, Error, Result, Tool, Tools, Value, export_plugin, http, json, kv, log,
    sink,
};

struct Feeds;

/// One feed entry, RSS `<item>` or Atom `<entry>`.
struct Entry {
    id: String,
    title: Option<String>,
    link: Option<String>,
    body: String,
    updated: Option<String>,
    modified_ms: Option<i64>,
}

struct Feed {
    title: Option<String>,
    entries: Vec<Entry>,
}

fn feed_url(settings: &Value) -> Result<&str> {
    let url = settings["url"]
        .as_str()
        .ok_or_else(|| Error::invalid("settings.url is required"))?;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Error::invalid("settings.url must be an http(s) URL"));
    }
    Ok(url)
}

impl Connector for Feeds {
    fn validate(settings: &Value) -> Result<()> {
        feed_url(settings).map(|_| ())
    }

    fn sync(settings: &Value, cursor: Option<Value>) -> Result<Value> {
        let url = feed_url(settings)?;
        let mut req = http::Request::get(url).header(
            "accept",
            "application/rss+xml, application/atom+xml, application/xml;q=0.9, */*;q=0.5",
        );
        if let Some(c) = &cursor {
            if let Some(etag) = c["etag"].as_str() {
                req = req.header("if-none-match", etag);
            }
            if let Some(lm) = c["last_modified"].as_str() {
                req = req.header("if-modified-since", lm);
            }
        }
        let resp = req.send()?;
        if resp.status == 304
            && let Some(c) = cursor
        {
            log::debug("feed not modified");
            return Ok(c);
        }
        let resp = resp.error_for_status()?;
        let next = json!({
            "etag": resp.header("etag"),
            "last_modified": resp.header("last-modified"),
        });
        let feed = parse_feed(&resp.text()?)?;

        let prefix = format!("{url}#");
        for e in &feed.entries {
            if sink::cancelled() {
                return Err(Error::Cancelled);
            }
            let mut doc = Document::text(format!("{prefix}{}", e.id), "text/html", e.body.clone())
                .with_metadata(json!({ "feed_url": url, "feed_title": feed.title }));
            if let Some(t) = &e.title {
                doc = doc.with_title(t.clone());
            }
            if let Some(l) = &e.link {
                doc = doc.with_url(l.clone());
            }
            if let Some(v) = &e.updated {
                doc = doc.with_version(v.clone());
            }
            if let Some(ms) = e.modified_ms {
                doc = doc.with_modified_at(ms);
            }
            sink::upsert(doc)?;
        }
        if cursor.is_none() {
            // Reconciling pass: drop what came from another feed URL (the settings changed).
            for (key, _) in sink::known_items()? {
                if !key.starts_with(&prefix) {
                    sink::delete(&key)?;
                }
            }
        }
        kv::set_json(
            "last_sync",
            &json!({ "at_ms": nugget_plugin_sdk::now_ms(), "entries": feed.entries.len() }),
        )?;
        log::info(&format!("synced {} entries", feed.entries.len()));
        Ok(next)
    }
}

impl Tools for Feeds {
    fn tools() -> Vec<Tool> {
        vec![Tool::new(
            "feed_headlines",
            "List the latest headlines of an RSS or Atom feed (on a host this plugin declares).",
            json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "format": "uri" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
                }
            }),
        )]
    }

    fn call(name: &str, input: Value) -> Result<Value> {
        if name != "feed_headlines" {
            return Err(Error::NotFound(format!("no tool `{name}`")));
        }
        let url = feed_url(&input)?;
        let limit = input["limit"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
        let feed = parse_feed(&http::get(url)?.error_for_status()?.text()?)?;
        let items: Vec<Value> = feed
            .entries
            .iter()
            .take(limit)
            .map(|e| json!({ "title": e.title, "link": e.link, "updated": e.updated }))
            .collect();
        Ok(json!({ "feed": feed.title, "items": items }))
    }
}

fn parse_feed(xml: &str) -> Result<Feed> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| Error::invalid(format!("feed is not XML: {e}")))?;
    let root = doc.root_element();
    match root.tag_name().name() {
        "rss" => {
            let channel = child(root, "channel")
                .ok_or_else(|| Error::invalid("RSS feed without <channel>"))?;
            let entries = channel
                .children()
                .filter(|n| n.has_tag_name("item"))
                .filter_map(|item| {
                    let link = text(item, "link");
                    let id = text(item, "guid").or_else(|| link.clone())?;
                    let updated = text(item, "pubDate");
                    Some(Entry {
                        id,
                        title: text(item, "title"),
                        body: text(item, "description").unwrap_or_default(),
                        modified_ms: updated.as_deref().and_then(parse_rfc2822),
                        updated,
                        link,
                    })
                })
                .collect();
            Ok(Feed {
                title: text(channel, "title"),
                entries,
            })
        }
        "feed" => {
            let entries = root
                .children()
                .filter(|n| n.has_tag_name("entry"))
                .filter_map(|entry| {
                    let link = entry
                        .children()
                        .filter(|n| n.has_tag_name("link"))
                        .find(|n| n.attribute("rel").is_none_or(|r| r == "alternate"))
                        .and_then(|n| n.attribute("href"))
                        .map(str::to_string);
                    let updated = text(entry, "updated").or_else(|| text(entry, "published"));
                    Some(Entry {
                        id: text(entry, "id").or_else(|| link.clone())?,
                        title: text(entry, "title"),
                        body: text(entry, "content")
                            .or_else(|| text(entry, "summary"))
                            .unwrap_or_default(),
                        modified_ms: updated.as_deref().and_then(parse_rfc3339),
                        updated,
                        link,
                    })
                })
                .collect();
            Ok(Feed {
                title: text(root, "title"),
                entries,
            })
        }
        other => Err(Error::invalid(format!("unknown feed format <{other}>"))),
    }
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|n| n.tag_name().name() == name)
}

fn text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    let t: String = child(node, name)?
        .descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn parse_rfc2822(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc2822).ok()?;
    Some((t.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some((t.unix_timestamp_nanos() / 1_000_000) as i64)
}

export_plugin!(connector: Feeds, tools: Feeds);
