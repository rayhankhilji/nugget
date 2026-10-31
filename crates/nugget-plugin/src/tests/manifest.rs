use url::Url;

use super::manifest_toml;
use crate::{Capabilities, HostRule, Manifest, PluginKind};

fn parse(s: &str) -> nugget_core::Result<Manifest> {
    Manifest::parse(s.as_bytes())
}

#[test]
fn parses_a_valid_manifest() {
    let m = parse(&manifest_toml(
        "dev.nugget.test",
        "1.2.3",
        "[\"connector\", \"tool\"]",
        "hosts = [\"example.com\", \"http://127.0.0.1:8080\"]\nsecrets = [\"token\"]\nkv_max_bytes = 1024",
    ))
    .unwrap();
    assert_eq!(m.id, "dev.nugget.test");
    assert!(m.provides(PluginKind::Connector) && m.provides(PluginKind::Tool));
    assert_eq!(m.capabilities.secrets, ["token"]);
    assert_eq!(m.connector.unwrap().display_name, "Test");
}

#[test]
fn the_example_manifest_is_valid() {
    let text = include_str!("../../sdk/examples/feed-reader/nugget-plugin.toml");
    let m = parse(text).unwrap();
    assert!(m.connector.unwrap().settings_schema.unwrap().is_object());
}

#[test]
fn malicious_manifests_are_rejected() {
    let ok = |caps: &str| manifest_toml("dev.nugget.test", "1.0.0", "[\"tool\"]", caps);
    let cases = [
        // Unknown fields cannot sneak in capabilities the user never sees.
        ok("hosts = []\nfilesystem = \"/\""),
        manifest_toml("../../etc", "1.0.0", "[\"tool\"]", ""),
        manifest_toml("Dev.Nugget", "1.0.0", "[\"tool\"]", ""),
        manifest_toml("single", "1.0.0", "[\"tool\"]", ""),
        manifest_toml("dev.nugget.test", "latest", "[\"tool\"]", ""),
        manifest_toml("dev.nugget.test", "1.0.0", "[]", ""),
        manifest_toml("dev.nugget.test", "1.0.0", "[\"tool\", \"tool\"]", ""),
        manifest_toml("dev.nugget.test", "1.0.0", "[\"admin\"]", ""),
        ok("hosts = [\"*\"]"),
        ok("hosts = [\"*.com\"]"),
        ok("hosts = [\"evil.com/path\"]"),
        ok("hosts = [\"user:pw@evil.com\"]"),
        ok("hosts = [\"ftp://evil.com\"]"),
        ok("hosts = [\"a.*.com\"]"),
        ok("secrets = [\"../oauth:gmail\"]"),
        ok("secrets = [\"a\", \"a\"]"),
        ok("kv_max_bytes = 1073741824"),
    ];
    for case in cases {
        assert!(parse(&case).is_err(), "accepted:\n{case}");
    }
    // A connector needs its section; a tool must not have one.
    let no_section = manifest_toml("dev.nugget.test", "1.0.0", "[\"connector\"]", "")
        .replace("[connector]\ndisplay_name = \"Test\"\n", "");
    assert!(parse(&no_section).is_err());
    let stray = format!("{}\n[connector]\ndisplay_name = \"x\"\n", ok(""));
    assert!(parse(&stray).is_err());
    // Oversized and non-UTF-8 input.
    assert!(Manifest::parse(&vec![b'#'; 70 * 1024]).is_err());
    assert!(Manifest::parse(&[0xff, 0xfe]).is_err());
}

#[test]
fn host_rules_match_exactly() {
    let url = |s: &str| Url::parse(s).unwrap();
    let r = HostRule::parse("Feeds.Example.com").unwrap();
    assert!(r.matches(&url("https://feeds.example.com/a.xml")));
    assert!(r.matches(&url("https://FEEDS.example.com:443/")));
    assert!(!r.matches(&url("http://feeds.example.com/")), "scheme");
    assert!(!r.matches(&url("https://feeds.example.com:8443/")), "port");
    assert!(!r.matches(&url("https://feeds.example.com.evil.com/")));
    assert!(!r.matches(&url("https://evil.com/?feeds.example.com")));
    assert!(
        !r.matches(&url("https://user@feeds.example.com/")),
        "userinfo"
    );

    let w = HostRule::parse("*.example.com").unwrap();
    assert!(w.matches(&url("https://a.b.example.com/")));
    assert!(!w.matches(&url("https://example.com/")), "apex not covered");
    assert!(!w.matches(&url("https://badexample.com/")));

    let local = HostRule::parse("http://127.0.0.1:8080").unwrap();
    assert!(local.matches(&url("http://127.0.0.1:8080/x")));
    assert!(!local.matches(&url("http://127.0.0.1:8081/x")));
    assert!(!local.matches(&url("https://127.0.0.1:8080/x")));
    assert_eq!(local.to_string(), "http://127.0.0.1:8080");
}

#[test]
fn capability_subsets() {
    let caps = |hosts: &[&str], secrets: &[&str], kv: u64| Capabilities {
        hosts: hosts.iter().map(|s| s.to_string()).collect(),
        secrets: secrets.iter().map(|s| s.to_string()).collect(),
        kv_max_bytes: kv,
        file_roots: Vec::new(),
    };
    let approved = caps(&["example.com", "*.cdn.net"], &["token"], 100);
    assert!(caps(&["https://example.com"], &[], 50).is_subset_of(&approved));
    assert!(!caps(&["other.com"], &[], 0).is_subset_of(&approved));
    assert!(!caps(&[], &["password"], 0).is_subset_of(&approved));
    assert!(!caps(&[], &[], 101).is_subset_of(&approved));
}

fn connector_manifest(caps: &str, schema: &str) -> String {
    format!(
        "{}settings_schema = {schema}\n",
        manifest_toml("dev.nugget.test", "1.0.0", "[\"connector\"]", caps)
    )
}

#[test]
fn file_roots_are_validated() {
    let schema = r#"{ type = "object", properties = { vault = { type = "string" }, export_file = { type = "string" } } }"#;
    let m = parse(&connector_manifest(
        "file_roots = [\"vault\", \"export_file\"]",
        schema,
    ))
    .unwrap();
    assert_eq!(m.capabilities.file_roots, ["vault", "export_file"]);
    // Without `properties` in the schema (or without a schema), any valid name is accepted.
    parse(&connector_manifest(
        "file_roots = [\"folder\"]",
        "{ type = \"object\" }",
    ))
    .unwrap();
    parse(&manifest_toml(
        "dev.nugget.test",
        "1.0.0",
        "[\"connector\", \"tool\"]",
        "file_roots = [\"folder\"]",
    ))
    .unwrap();

    let nine: Vec<String> = (0..9).map(|i| format!("\"r{i}\"")).collect();
    let nine = format!("file_roots = [{}]", nine.join(", "));
    let long = format!("file_roots = [\"{}\"]", "a".repeat(65));
    for bad in [
        "file_roots = [\"\"]",
        "file_roots = [\"Vault\"]",
        "file_roots = [\"my-vault\"]",
        "file_roots = [\"../etc\"]",
        "file_roots = [\"/home\"]",
        "file_roots = [\"vault\", \"vault\"]",
        "file_roots = \"vault\"",
        long.as_str(),
        nine.as_str(),
    ] {
        assert!(
            parse(&connector_manifest(bad, "{ type = \"object\" }")).is_err(),
            "accepted: {bad}"
        );
    }
    // Every root must be a settings property when the schema lists properties.
    assert!(parse(&connector_manifest("file_roots = [\"other\"]", schema)).is_err());
    // A tool-only plugin cannot declare file roots.
    assert!(
        parse(&manifest_toml(
            "dev.nugget.test",
            "1.0.0",
            "[\"tool\"]",
            "file_roots = [\"vault\"]"
        ))
        .is_err()
    );
}

#[test]
fn file_roots_take_part_in_subsets() {
    let caps = |roots: &[&str]| Capabilities {
        file_roots: roots.iter().map(|s| s.to_string()).collect(),
        ..Capabilities::default()
    };
    assert!(caps(&[]).is_subset_of(&caps(&["vault"])));
    assert!(caps(&["vault"]).is_subset_of(&caps(&["vault", "inbox"])));
    assert!(!caps(&["vault"]).is_subset_of(&caps(&[])));
    assert!(!caps(&["vault", "other"]).is_subset_of(&caps(&["vault"])));
}

#[test]
fn empty_file_roots_keep_the_serialized_form() {
    // Approval seals hash this JSON: a plugin without file roots must serialize exactly as
    // before the field existed, and records without the field must still load.
    let c = Capabilities {
        hosts: vec!["example.com".into()],
        ..Capabilities::default()
    };
    let json = serde_json::to_string(&c).unwrap();
    assert_eq!(
        json,
        r#"{"hosts":["example.com"],"secrets":[],"kv_max_bytes":0}"#
    );
    let back: Capabilities = serde_json::from_str(&json).unwrap();
    assert_eq!(back, c);
    let with = Capabilities {
        file_roots: vec!["vault".into()],
        ..c
    };
    let json = serde_json::to_string(&with).unwrap();
    assert!(json.ends_with(r#""file_roots":["vault"]}"#), "{json}");
    assert_eq!(serde_json::from_str::<Capabilities>(&json).unwrap(), with);
}

#[test]
fn a_trailing_dot_in_a_host_rule_still_matches() {
    let r = HostRule::parse("example.com.").unwrap();
    assert_eq!(r, HostRule::parse("example.com").unwrap());
    assert!(r.matches(&url::Url::parse("https://example.com/feed").unwrap()));
    assert!(r.matches(&url::Url::parse("https://example.com./feed").unwrap()));
    let w = HostRule::parse("*.example.com.").unwrap();
    assert!(w.matches(&url::Url::parse("https://a.example.com/").unwrap()));
    assert!(HostRule::parse(".").is_err());
    // Found by the plugin_caps fuzz target: stripping dots must not turn a name into an address.
    for bad in [
        "2..",
        "a..b",
        "bücher.ex%2ample",
        "a%2eb.com",
        "example.com..",
        "*.example..com",
    ] {
        assert!(HostRule::parse(bad).is_err(), "{bad} parsed");
    }
    for ok in [
        "example.com.",
        "*.example.com.",
        "localhost.",
        "bücher.example",
    ] {
        let r = HostRule::parse(ok).unwrap();
        assert_eq!(
            HostRule::parse(&r.to_string()).unwrap(),
            r,
            "{ok} does not round-trip"
        );
    }
}
