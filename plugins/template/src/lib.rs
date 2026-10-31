//! A Nugget plugin with one connector and one tool. Replace both with your own.
//!
//! **Connector** (`MyConnector`): pulls items from a JSON API,
//! `GET <endpoint>?since=<ms>` answering `{ "items": [{ "id", "title", "body", "updated_at" }] }`,
//! and sends each item to Nugget as a document. The cursor remembers the newest `updated_at`, so
//! later passes only fetch what changed. A pass without a cursor fetches everything and deletes
//! the documents of items that no longer exist.
//!
//! **Tool** (`word_count`): counts the words of a text. Tools are read-only and run when the
//! agent calls them.

use std::collections::BTreeSet;

use nugget_plugin_sdk::{
    Connector, Document, Error, Result, Tool, Tools, Value, export_plugin, http, json, log,
    secrets, sink,
};

pub struct MyConnector;

/// The `endpoint` setting, checked.
fn endpoint(settings: &Value) -> Result<&str> {
    let url = settings["endpoint"]
        .as_str()
        .ok_or_else(|| Error::invalid("settings.endpoint is required"))?;
    if !url.starts_with("https://") {
        return Err(Error::invalid("settings.endpoint must be an https:// URL"));
    }
    Ok(url)
}

impl Connector for MyConnector {
    /// Runs when the user saves the source. No network here: check the settings only.
    fn validate(settings: &Value) -> Result<()> {
        endpoint(settings).map(|_| ())
    }

    fn sync(settings: &Value, cursor: Option<Value>) -> Result<Value> {
        let url = endpoint(settings)?;
        let since = cursor
            .as_ref()
            .and_then(|c| c["since"].as_i64())
            .unwrap_or(0);

        let mut req = http::Request::get(format!("{url}?since={since}"));
        // Optional: an API token the user stored for this plugin (declared in the manifest).
        if let Some(token) = secrets::get("api_token")? {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let body = req.send()?.error_for_status()?.json()?;
        let items = body["items"]
            .as_array()
            .ok_or_else(|| Error::invalid("the API answered without `items`"))?;

        let mut newest = since;
        let mut seen = BTreeSet::new();
        for item in items {
            if sink::cancelled() {
                return Err(Error::Cancelled);
            }
            let Some(id) = item["id"].as_str() else {
                log::warn("skipping an item without an id");
                continue;
            };
            let updated = item["updated_at"].as_i64().unwrap_or(0);
            newest = newest.max(updated);
            seen.insert(id.to_string());
            let doc = Document::text(id, "text/plain", item["body"].as_str().unwrap_or(""))
                .with_title(item["title"].as_str().unwrap_or(id))
                .with_version(updated.to_string())
                .with_modified_at(updated)
                .with_metadata(json!({ "source": url }));
            sink::upsert(doc)?;
        }

        // A full pass (no cursor) saw every item, so anything else the store holds is gone.
        if cursor.is_none() {
            for (key, _version) in sink::known_items()? {
                if !seen.contains(&key) {
                    sink::delete(&key)?;
                }
            }
        }
        log::info(&format!("synced {} items", seen.len()));
        Ok(json!({ "since": newest }))
    }
}

pub struct MyTools;

impl Tools for MyTools {
    fn tools() -> Vec<Tool> {
        vec![Tool::new(
            "word_count",
            "Count the words in a text.",
            json!({
                "type": "object",
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            }),
        )]
    }

    fn call(name: &str, input: Value) -> Result<Value> {
        match name {
            "word_count" => {
                let text = input["text"]
                    .as_str()
                    .ok_or_else(|| Error::invalid("input.text is required"))?;
                Ok(json!({ "words": text.split_whitespace().count() }))
            }
            _ => Err(Error::NotFound(format!("no tool `{name}`"))),
        }
    }
}

export_plugin!(connector: MyConnector, tools: MyTools);

#[cfg(test)]
mod tests;
