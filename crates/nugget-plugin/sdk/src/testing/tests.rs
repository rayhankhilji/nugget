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
fn tool_calls_are_capped_at_32_requests() {
    struct Many;
    impl Tools for Many {
        fn tools() -> Vec<Tool> {
            vec![Tool::new("x", "", json!({}))]
        }
        fn call(_: &str, _: Value) -> Result<Value> {
            for _ in 0..40 {
                http::get("https://api.example.com/")?;
            }
            Ok(Value::Null)
        }
    }
    let h = host();
    h.respond("GET", "https://api.example.com/", http::Response::ok(""));
    assert!(matches!(
        h.call::<Many>("x", Value::Null),
        Err(Error::Denied(_))
    ));
    assert_eq!(h.requests().len(), 33);
}

#[test]
fn respond_with_sees_the_request() {
    let h = host();
    h.respond_with("GET", "https://api.example.com/c", |req| {
        Ok(match req.header_value("if-none-match") {
            Some("v1") => http::Response::new(304),
            _ => http::Response::ok("fresh").with_header("ETag", "v1"),
        })
    });
    let _active = h.enter(Mode::Sync, None);
    let r = http::get("https://api.example.com/c").unwrap();
    assert_eq!(r.header("etag"), Some("v1"));
    let r = http::Request::get("https://api.example.com/c")
        .header("If-None-Match", "v1")
        .send()
        .unwrap();
    assert_eq!(r.status, 304);
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
fn clock_and_logs() {
    let h = host();
    assert_eq!(crate::now_ms(), DEFAULT_NOW_MS);
    h.advance(1000);
    assert_eq!(crate::now_ms(), DEFAULT_NOW_MS + 1000);
    crate::log::warn("disk is on fire");
    assert!(h.logged(Level::Warn, "on fire"));
    assert!(!h.logged(Level::Error, "on fire"));
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
fn single_file_roots() {
    let h = host();
    h.mount("folder", MemFs::single_file("data", 7));
    let _active = h.enter(Mode::Sync, None);
    assert_eq!(fs::read_file("folder", "").unwrap(), b"data");
    let st = fs::stat("folder", "").unwrap();
    assert_eq!((st.name.as_str(), st.is_dir, st.size), ("", false, 4));
    assert!(fs::read_dir("folder", "").is_err());
}

#[test]
fn file_size_cap() {
    let h = host();
    h.set_limits(Limits {
        max_fs_file_bytes: 3,
        ..Limits::default()
    });
    h.mount("folder", MemFs::new().file("big", "four"));
    let _active = h.enter(Mode::Sync, None);
    assert!(matches!(
        fs::read_file("folder", "big"),
        Err(Error::Invalid(_))
    ));
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
fn sink_validates_documents() {
    let h = host();
    let _active = h.enter(Mode::Sync, None);
    assert!(sink::upsert(Document::text("", "text/plain", "")).is_err());
    assert!(sink::upsert(Document::text("k".repeat(1025), "text/plain", "")).is_err());
    assert!(sink::upsert(Document::text("k", "", "")).is_err());
    assert!(sink::upsert(Document::text("k", "text/plain", "").with_metadata(json!([]))).is_err());
    assert!(sink::upsert(Document::text("k", "text/plain", "")).is_ok());
}

#[test]
fn kinds_and_manifest_checks() {
    let tool_only = TestHost::from_manifest(
        r#"
        id = "dev.example.t"
        name = "T"
        version = "0.1.0"
        description = ""
        kind = ["tool"]
        "#,
    );
    assert!(matches!(
        tool_only.sync::<Probe>(json!({}), None),
        Err(Error::Denied(_))
    ));
    assert_eq!(tool_only.tools::<Probe>().len(), 1);

    for (bad, why) in [
        ("id = \"nodots\"", "reverse-DNS"),
        ("[capabilities]\nhosts = [\"*.com\"]", "wildcard"),
        ("[capabilities]\nfile_roots = [\"Bad\"]", "file root"),
        ("[capabilities]\nnope = 1", "unknown capability"),
    ] {
        let text = format!(
            "{}\nname = \"T\"\nversion = \"0.1.0\"\ndescription = \"\"\nkind = [\"tool\"]\n{}",
            if bad.starts_with("id") {
                bad
            } else {
                "id = \"dev.example.t\""
            },
            if bad.starts_with("id") { "" } else { bad }
        );
        let err = parse_manifest(&text).unwrap_err();
        assert!(err.contains(why), "{err}");
    }

    let not_in_schema = MANIFEST.replace("properties.folder", "properties.other");
    assert!(
        parse_manifest(&not_in_schema)
            .unwrap_err()
            .contains("not a property")
    );
}

#[test]
#[should_panic(expected = "must be 1-64 chars")]
fn bad_tool_names_panic() {
    struct Bad;
    impl Tools for Bad {
        fn tools() -> Vec<Tool> {
            vec![Tool::new("Bad-Name", "", json!({}))]
        }
        fn call(_: &str, _: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }
    TestHost::new().tools::<Bad>();
}

#[test]
fn tool_output_goes_through_json() {
    let h = host();
    assert_eq!(
        h.call::<Probe>("probe", json!({ "op": "noop" })).unwrap(),
        Value::Null
    );
}

#[test]
fn fs_budgets_and_settings() {
    struct V;
    impl Connector for V {
        fn validate(_: &Value) -> Result<()> {
            assert_eq!(fs::read_file("folder", "a")?, b"abc");
            assert!(matches!(
                fs::read_file("folder", "a"),
                Err(Error::Denied(_))
            ));
            assert!(matches!(fs::stat("folder", "a"), Err(Error::Denied(_))));
            Ok(())
        }
        fn sync(_: &Value, _: Option<Value>) -> Result<Value> {
            fs::stat("folder", "")?;
            Ok(Value::Null)
        }
    }
    let h = host();
    h.set_limits(Limits {
        max_fs_ops_per_call: 2,
        max_fs_read_bytes_per_call: 5,
        ..Limits::default()
    });
    h.mount("folder", MemFs::new().file("a", "abc"));
    // In validate (per-call budgets): two operations, five bytes.
    h.validate::<V>(json!({ "folder": "/x" })).unwrap();
    // The settings must name the root, even when a test mounts it.
    assert!(matches!(
        h.sync::<V>(json!({}), None),
        Err(Error::Invalid(_))
    ));
    assert!(h.sync::<V>(json!({ "folder": "/x" }), None).is_ok());
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
