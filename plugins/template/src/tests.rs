//! Native tests: `cargo test-native`. The SDK's `TestHost` stands in for Nugget, enforcing the
//! manifest's capabilities, with canned HTTP responses instead of the network.

use nugget_plugin_sdk::testing::TestHost;
use nugget_plugin_sdk::{Error, http, json};

use super::*;

const ENDPOINT: &str = "https://api.example.com/items";

fn host() -> TestHost {
    TestHost::from_manifest(include_str!("../nugget-plugin.toml"))
}

#[test]
fn validate_checks_the_settings() {
    let host = host();
    assert!(
        host.validate::<MyConnector>(json!({ "endpoint": ENDPOINT }))
            .is_ok()
    );
    assert!(host.validate::<MyConnector>(json!({})).is_err());
    assert!(
        host.validate::<MyConnector>(json!({ "endpoint": "http://x" }))
            .is_err()
    );
}

#[test]
fn sync_sends_items_and_advances_the_cursor() {
    let host = host();
    host.set_secret("api_token", "t0ken");
    host.respond(
        "GET",
        "https://api.example.com/items?since=0",
        http::Response::ok(
            json!({ "items": [
                { "id": "a", "title": "First", "body": "hello", "updated_at": 10 },
                { "id": "b", "title": "Second", "body": "world", "updated_at": 20 },
            ]})
            .to_string(),
        ),
    );
    host.seed_item("gone", None);

    let pass = host
        .sync::<MyConnector>(json!({ "endpoint": ENDPOINT }), None)
        .unwrap();
    assert_eq!(pass.upserted_keys(), ["a", "b"]);
    assert_eq!(pass.upserted("a").unwrap().title.as_deref(), Some("First"));
    assert_eq!(pass.deletes, ["gone"]);
    assert_eq!(pass.cursor, json!({ "since": 20 }));
    assert_eq!(
        host.requests()[0].header_value("authorization"),
        Some("Bearer t0ken")
    );

    // The next pass asks only for what changed since.
    host.respond(
        "GET",
        "https://api.example.com/items?since=20",
        http::Response::ok(r#"{ "items": [] }"#),
    );
    let next = host
        .sync::<MyConnector>(json!({ "endpoint": ENDPOINT }), Some(pass.cursor))
        .unwrap();
    assert!(next.upserts.is_empty() && next.deletes.is_empty());
}

#[test]
fn hosts_outside_the_manifest_are_denied() {
    let host = host();
    let err = host
        .sync::<MyConnector>(
            json!({ "endpoint": "https://evil.example.net/items" }),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "{err:?}");
}

#[test]
fn word_count_tool() {
    let host = host();
    assert_eq!(host.tools::<MyTools>()[0].name, "word_count");
    assert_eq!(
        host.call::<MyTools>("word_count", json!({ "text": "one two  three" }))
            .unwrap(),
        json!({ "words": 3 })
    );
    assert!(host.call::<MyTools>("nope", json!({})).is_err());
}
