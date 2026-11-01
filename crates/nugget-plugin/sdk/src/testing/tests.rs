use super::*;
use crate::{json, kv, secrets, sink};

const MANIFEST: &str = r#"
id = "dev.example.test"
name = "Test"
version = "0.1.0"
description = "test"
kind = ["connector", "tool"]

[capabilities]
hosts = ["api.example.com", "*.cdn.example.org", "http://127.0.0.1:8080", "alt.example.com:8443"]
secrets = ["token"]
kv_max_bytes = 64
file_roots = ["folder"]

[connector]
display_name = "Test"

[connector.settings_schema]
type = "object"
properties.folder = { type = "string" }
"#;

fn host() -> TestHost {
    TestHost::from_manifest(MANIFEST)
}

/// A connector and tool that do whatever the settings or input say, for exercising the host.
struct Probe;

fn run(op: &Value) -> Result<Value> {
    match op["op"].as_str().unwrap_or_default() {
        "get" => {
            let r = http::get(op["url"].as_str().unwrap_or_default())?;
            Ok(json!({ "status": r.status, "body": r.text()? }))
        }
        "post" => {
            http::Request::new("POST", op["url"].as_str().unwrap_or_default())
                .body("x")
                .send()?;
            Ok(Value::Null)
        }
        "kv_set" => kv::set("k", b"v").map(|_| Value::Null),
        "kv_get" => Ok(json!(kv::get("k")?.map(|v| String::from_utf8(v).unwrap()))),
        "secret" => Ok(json!(secrets::get("token")?)),
        "secret_set" => secrets::set("token", "new").map(|_| Value::Null),
        "ls" => Ok(json!(
            fs::read_dir("folder", op["path"].as_str().unwrap_or_default())?
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
        )),
        "upsert" => sink::upsert(Document::text("k", "text/plain", "x")).map(|_| Value::Null),
        _ => Ok(Value::Null),
    }
}

impl Connector for Probe {
    fn validate(settings: &Value) -> Result<()> {
        run(settings).map(|_| ())
    }

    fn sync(settings: &Value, cursor: Option<Value>) -> Result<Value> {
        let out = run(settings)?;
        Ok(json!({ "out": out, "had_cursor": cursor.is_some() }))
    }
}

impl Tools for Probe {
    fn tools() -> Vec<Tool> {
        vec![Tool::new("probe", "probe", json!({ "type": "object" }))]
    }

    fn call(_: &str, input: Value) -> Result<Value> {
        run(&input)
    }
}

#[test]
fn host_rules_match_like_the_host() {
    let r = HostRule::parse("example.com").unwrap();
    let ok = |u: &str| r.matches(&Url::parse(u).unwrap());
    assert!(ok("https://example.com/x"));
    assert!(ok("https://EXAMPLE.com:443/x"));
    assert!(ok("https://example.com./x"));
    assert!(!ok("http://example.com/x"));
    assert!(!ok("https://example.com:8443/x"));
    assert!(!ok("https://www.example.com/x"));
    assert!(!ok("https://user@example.com/x"));

    let w = HostRule::parse("*.example.com").unwrap();
    assert!(w.matches(&Url::parse("https://a.b.example.com/").unwrap()));
    assert!(!w.matches(&Url::parse("https://example.com/").unwrap()));
    assert!(!w.matches(&Url::parse("https://badexample.com/").unwrap()));

    let v6 = HostRule::parse("http://[::1]:8080").unwrap();
    assert!(v6.matches(&Url::parse("http://[::1]:8080/x").unwrap()));

    assert!(HostRule::parse("*.com").is_err());
    assert!(HostRule::parse("example.com/path").is_err());
    assert!(HostRule::parse("a.*.com").is_err());
    assert!(HostRule::parse("ftp://example.com").is_err());
}

#[test]
fn http_is_canned_recorded_and_limited_to_declared_hosts() {
    let h = host();
    h.respond(
        "GET",
        "https://api.example.com/a",
        http::Response::ok("hello"),
    );
    let out = h
        .call::<Probe>(
            "probe",
            json!({ "op": "get", "url": "https://api.example.com/a" }),
        )
        .unwrap();
    assert_eq!(out, json!({ "status": 200, "body": "hello" }));

    // Declared, but nothing canned: a network error, never a real request.
    let err = h
        .call::<Probe>(
            "probe",
            json!({ "op": "get", "url": "https://x.cdn.example.org/b" }),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");

    for url in [
        "https://evil.example.net/",
        "http://api.example.com/a",
        "https://cdn.example.org/",
        "https://alt.example.com/",
        "file:///etc/passwd",
    ] {
        let err = h
            .call::<Probe>("probe", json!({ "op": "get", "url": url }))
            .unwrap_err();
        assert!(
            matches!(err, Error::Denied(_) | Error::Invalid(_)),
            "{url}: {err:?}"
        );
    }
    assert_eq!(h.requests().len(), 7);
    assert_eq!(h.requests()[0].url, "https://api.example.com/a");
}

#[test]
fn forbidden_headers_and_request_caps() {
    let h = host();
    h.respond("GET", "https://api.example.com/", http::Response::ok(""));
    let _active = h.enter(Mode::Sync, None);
    let err = http::Request::get("https://api.example.com/")
        .header("Host", "evil")
        .send()
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)));
    let err = http::Request::get("https://api.example.com/")
        .header("Proxy-Foo", "1")
        .send()
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)));
    assert!(
        http::Request::get("https://api.example.com/")
            .header("If-None-Match", "x")
            .send()
            .is_ok()
    );
}

#[test]
fn validate_has_no_network_kv_secrets_or_sink() {
    let h = host();
    h.respond("GET", "https://api.example.com/a", http::Response::ok(""));
    for op in [
        json!({ "op": "get", "url": "https://api.example.com/a" }),
        json!({ "op": "kv_get" }),
        json!({ "op": "secret" }),
        json!({ "op": "upsert" }),
    ] {
        let err = h.validate::<Probe>(op.clone()).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{op}: {err:?}");
    }
    assert!(h.validate::<Probe>(json!({})).is_ok());
}

#[test]
fn tools_are_read_only() {
    let h = host();
    h.kv_set("k", "source-only");
    h.tools_kv_set("k", "v");
    h.set_secret("token", "s3cret");
    h.mount("folder", MemFs::new().file("a.txt", "a"));
    for op in ["post", "kv_set", "secret_set", "upsert", "ls"] {
        let err = h
            .call::<Probe>(
                "probe",
                json!({ "op": op, "url": "https://api.example.com/" }),
            )
            .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{op}: {err:?}");
    }
    assert_eq!(
        h.call::<Probe>("probe", json!({ "op": "kv_get" })).unwrap(),
        json!("v")
    );
    assert_eq!(
        h.call::<Probe>("probe", json!({ "op": "secret" })).unwrap(),
        json!("s3cret")
    );
}

#[test]
fn kv_respects_the_declared_size_and_secrets_the_declared_names() {
    let h = host();
    let _active = h.enter(Mode::Sync, None);
    kv::set("a", &[0; 30]).unwrap();
    kv::set("a", &[0; 60]).unwrap_or_else(|e| panic!("replacing within the cap: {e:?}"));
    assert!(matches!(kv::set("b", &[0; 10]), Err(Error::Invalid(_))));
    assert_eq!(kv::keys("").unwrap(), vec!["a".to_string()]);
    kv::delete("a").unwrap();
    assert_eq!(kv::get("a").unwrap(), None);
    // Like the host: an empty key reads as absent, but cannot be written.
    assert_eq!(kv::get("").unwrap(), None);
    assert!(matches!(kv::set("", b"x"), Err(Error::Invalid(_))));

    secrets::set("token", "t").unwrap();
    assert_eq!(h.secret("token").as_deref(), Some("t"));
    assert!(matches!(secrets::get("other"), Err(Error::Denied(_))));

    let bare = TestHost::new();
    let _active = bare.enter(Mode::Sync, None);
    assert!(matches!(kv::get("a"), Err(Error::Denied(_))));
}

#[test]
fn mem_fs_lists_sorted_and_refuses_escapes() {
    let h = host();
    h.mount(
        "folder",
        MemFs::new()
            .file("b.md", "bee")
            .file_at("a/c.md", "sea", 5)
            .dir("empty"),
    );
    let _active = h.enter(Mode::Sync, None);
    let names: Vec<String> = fs::read_dir("folder", "")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a", "b.md", "empty"]);
    let c = fs::stat("folder", "a/c.md").unwrap();
    assert_eq!(
        (c.name.as_str(), c.size, c.modified_at),
        ("c.md", 3, Some(5))
    );
    assert_eq!(fs::read_to_string("folder", "a/c.md").unwrap(), "sea");
    assert!(fs::stat("folder", "").unwrap().is_dir);
    for bad in [
        "../x",
        "/etc",
        "a/../b.md",
        "a//c.md",
        "a\\c.md",
        "./b.md",
        "a/",
        "c:x",
        "nul.txt",
        "a.",
    ] {
        assert!(
            matches!(fs::read_file("folder", bad), Err(Error::Invalid(_))),
            "{bad}"
        );
    }
    assert!(matches!(
        fs::read_file("folder", "nope"),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        fs::read_file("folder", "a"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(fs::read_dir("other", ""), Err(Error::Denied(_))));
}

#[test]
fn disk_roots_come_from_settings_and_hide_escaping_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/n.md"), "note").unwrap();
    std::fs::write(dir.path().join("secret.txt"), "outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path().join("secret.txt"), root.join("leak")).unwrap();

    let h = host();
    let settings = json!({ "op": "ls", "folder": root.to_str().unwrap(), "path": "" });
    let pass = h.sync::<Probe>(settings, None).unwrap();
    assert_eq!(pass.cursor["out"], json!(["sub"]));

    let _active = h.enter(
        Mode::Sync,
        Some(json!({ "folder": root.to_str().unwrap() })),
    );
    assert_eq!(fs::read_to_string("folder", "sub/n.md").unwrap(), "note");
    #[cfg(unix)]
    assert!(matches!(
        fs::read_file("folder", "leak"),
        Err(Error::Denied(_))
    ));
    assert!(fs::stat("folder", "sub").unwrap().is_dir);
}

#[test]
fn sync_reports_events_and_keeps_a_store() {
    struct Docs;
    impl Connector for Docs {
        fn sync(settings: &Value, _: Option<Value>) -> Result<Value> {
            let known = sink::known_items()?;
            for k in settings["keys"].as_array().unwrap() {
                if sink::cancelled() {
                    return Err(Error::Cancelled);
                }
                sink::upsert(
                    Document::text(k.as_str().unwrap(), "text/plain", "x").with_version("1"),
                )?;
            }
            for (k, _) in known {
                if !settings["keys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|x| x == k.as_str())
                {
                    sink::delete(&k)?;
                }
            }
            sink::checkpoint(&json!(1))?;
            Ok(json!({ "n": known_len() }))
        }
    }
    fn known_len() -> usize {
        sink::known_items().unwrap().len()
    }
    let h = host();
    h.seed_item("old", Some("0"));
    let pass = h.sync::<Docs>(json!({ "keys": ["a", "b"] }), None).unwrap();
    assert_eq!(pass.upserted_keys(), ["a", "b"]);
    assert_eq!(pass.deletes, ["old"]);
    assert_eq!(pass.checkpoints, [json!(1)]);
    assert_eq!(pass.cursor, json!({ "n": 2 }));
    assert_eq!(
        h.known_items(),
        vec![
            ("a".to_string(), Some("1".to_string())),
            ("b".to_string(), Some("1".to_string()))
        ]
    );

    h.cancel_after_upserts(1);
    let err = h
        .sync::<Docs>(json!({ "keys": ["c", "d", "e"] }), Some(json!(1)))
        .unwrap_err();
    assert_eq!(err, Error::Cancelled);
    assert!(h.document("c").is_some() && h.document("d").is_none());
}

#[test]
fn parity_cancel_private_addresses_listing_and_dates() {
    let h = host();
    h.allow_host("http://127.0.0.1:8080");
    h.respond("GET", "http://127.0.0.1:8080/", http::Response::ok(""));
    {
        let _active = h.enter(Mode::Sync, None);
        assert!(matches!(
            http::get("http://127.0.0.1:8080/"),
            Err(Error::Denied(_))
        ));
        h.allow_private_network(true);
        assert!(http::get("http://127.0.0.1:8080/").is_ok());

        // Dates outside what the host stores are refused.
        let far = Document::text("k", "text/plain", "").with_modified_at(MAX_MODIFIED_MS + 1);
        assert!(matches!(sink::upsert(far), Err(Error::Invalid(_))));

        // After cancellation, http, fs and the sink fail with `cancelled`.
        h.mount("folder", MemFs::new().file("a", "x").fail("a"));
        assert!(matches!(fs::stat("folder", "a"), Err(Error::Other(_))));
        h.set_cancelled(true);
        assert_eq!(
            http::get("http://127.0.0.1:8080/").unwrap_err(),
            Error::Cancelled
        );
        assert_eq!(fs::stat("folder", "").unwrap_err(), Error::Cancelled);
        assert_eq!(sink::delete("k").unwrap_err(), Error::Cancelled);
    }
    // Unlisted tools are not callable.
    assert!(matches!(
        h.call::<Probe>("unlisted", json!({})),
        Err(Error::NotFound(_))
    ));
}
