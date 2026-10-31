//! Plugin capability checks: `HostRule::parse` / `HostRule::matches`, `Capabilities::is_subset_of`
//! and the manifest's `[capabilities]` validation.
//!
//! The input is read as (lossy) UTF-8 lines:
//!
//! ```text
//! line 0      a host rule
//! line 1      a URL
//! line 2..    capability entries: `$name` is a secret, `@name` a file root, anything else a host
//! ```
//!
//! Invariants:
//! - a rule that parses round-trips through `to_string`/`parse` to an equal rule, with a lowercase
//!   host and a scheme of `https` or `http`;
//! - `matches` needs the rule's scheme and port, and never matches a URL with userinfo;
//! - a non-wildcard rule matches only its own host; a wildcard rule matches only proper
//!   subdomains and never its apex;
//! - the rule spelled from a URL's own scheme, host and port matches that URL;
//! - `is_subset_of` is reflexive, holds for a superset and a subset, and treats canonical and
//!   non-canonical spellings of the same host as equal;
//! - a manifest carrying these capabilities parses exactly when every host parses and the counts,
//!   secret names and `kv_max_bytes` are within limits, and keeps them unchanged.
//!
//! The host's per-request check (`check_request` in `host.rs`) is private; it calls
//! `HostRule::matches` for every declared rule, which is what is fuzzed here.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nugget_plugin::{Capabilities, HostRule, MAX_FILE_ROOTS, Manifest};
use url::Url;

const MAX_HOSTS: usize = 64;
const MAX_SECRETS: usize = 16;
const MAX_KV_BYTES: u64 = 16 * 1024 * 1024;

/// The host part of `url` as `HostRule::matches` compares it.
fn match_host(url: &Url) -> Option<String> {
    let host = match url.host()? {
        url::Host::Domain(d) => d.to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    Some(host.trim_end_matches('.').to_string())
}

fn check_rule(rule: &HostRule, url: Option<&Url>) {
    assert!(rule.scheme == "https" || rule.scheme == "http");
    assert!(!rule.host.is_empty());
    assert!(
        !rule.host.bytes().any(|b| b.is_ascii_uppercase()),
        "rule host is not lowercase: {rule:?}"
    );
    let canonical = rule.to_string();
    let again = HostRule::parse(&canonical)
        .unwrap_or_else(|e| panic!("canonical rule `{canonical}` does not parse: {e}"));
    assert_eq!(
        &again, rule,
        "rule does not round-trip through `{canonical}`"
    );
    assert_eq!(again.to_string(), canonical);

    // The apex of a wildcard rule never matches; the apex of a plain rule always does (unless
    // the rule host ends in a dot, which `matches` strips from the URL side).
    let apex_spelled = if rule.host.contains(':') {
        format!("[{}]", rule.host)
    } else {
        rule.host.clone()
    };
    let port = rule.port.map(|p| format!(":{p}")).unwrap_or_default();
    if let Ok(apex) = Url::parse(&format!("{}://{apex_spelled}{port}/x", rule.scheme)) {
        if rule.wildcard {
            assert!(
                !rule.matches(&apex),
                "wildcard {rule} matches its apex {apex}"
            );
            if let Ok(sub) = Url::parse(&format!("{}://sub.{apex_spelled}{port}/", rule.scheme))
                && match_host(&sub).is_some_and(|h| h.ends_with(&format!(".{}", rule.host)))
            {
                assert!(rule.matches(&sub), "wildcard {rule} does not match {sub}");
            }
        } else if !rule.host.ends_with('.') {
            assert!(
                rule.matches(&apex),
                "{rule} does not match its own host {apex}"
            );
        }
        // Userinfo on the same destination is always refused.
        let mut with_user = apex.clone();
        if with_user.set_username("u").is_ok() {
            assert!(!rule.matches(&with_user), "{rule} matches {with_user}");
        }
    }

    let Some(url) = url else { return };
    if !rule.matches(url) {
        return;
    }
    assert_eq!(url.scheme(), rule.scheme, "{rule} matches {url}");
    assert!(
        url.username().is_empty() && url.password().is_none(),
        "{rule} matches {url}"
    );
    assert_eq!(
        url.port(),
        rule.port,
        "{rule} matches {url} on another port"
    );
    let host = match_host(url).expect("a matched URL has a host");
    if rule.wildcard {
        assert!(
            host != rule.host && host.ends_with(&format!(".{}", rule.host)),
            "wildcard {rule} matches {url}"
        );
    } else {
        assert_eq!(host, rule.host, "{rule} matches a different host: {url}");
    }
}

fn secret_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A TOML basic string (JSON escaping, plus DEL, which TOML requires escaped).
fn toml_str(s: &str) -> String {
    serde_json::to_string(s)
        .expect("strings serialize")
        .replace('\u{7f}', "\\u007F")
}

fn toml_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| toml_str(s)).collect();
    format!("[{}]", parts.join(", "))
}

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut lines = text.split('\n');
    let rule_text = lines.next().unwrap_or("");
    let url = lines.next().and_then(|l| Url::parse(l).ok());

    // Rules and matching.
    if let Ok(rule) = HostRule::parse(rule_text) {
        check_rule(&rule, url.as_ref());
    }
    if let Some(url) = &url {
        if !url.username().is_empty() || url.password().is_some() {
            if let Ok(rule) = HostRule::parse(rule_text) {
                assert!(!rule.matches(url), "{rule} matches {url} with userinfo");
            }
        } else if let (Some(host), "https" | "http") = (url.host_str(), url.scheme()) {
            // The rule spelled from the URL itself matches it.
            let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
            let spelled = format!("{}://{host}{port}", url.scheme());
            if let Ok(rule) = HostRule::parse(&spelled) {
                check_rule(&rule, Some(url));
                if !rule.wildcard && !rule.host.ends_with('.') {
                    assert!(
                        rule.matches(url),
                        "{rule} (from `{spelled}`) does not match {url}"
                    );
                }
            }
        }
    }

    // Capabilities.
    let mut hosts = Vec::new();
    let mut secrets = Vec::new();
    let mut file_roots = Vec::new();
    for line in lines {
        if let Some(s) = line.strip_prefix('$') {
            secrets.push(s.to_string());
        } else if let Some(r) = line.strip_prefix('@') {
            file_roots.push(r.to_string());
        } else {
            hosts.push(line.to_string());
        }
    }
    let kv_max_bytes = (data.len() as u64).wrapping_mul(0x9e37_79b9) % (2 * MAX_KV_BYTES);
    let caps = Capabilities {
        hosts: hosts.clone(),
        secrets: secrets.clone(),
        kv_max_bytes,
        file_roots: file_roots.clone(),
    };
    for h in &caps.hosts {
        if let Ok(rule) = HostRule::parse(h) {
            check_rule(&rule, url.as_ref());
        }
    }
    assert!(caps.is_subset_of(&caps), "is_subset_of is not reflexive");
    let mut sup = caps.clone();
    sup.hosts.push("extra.example.com".into());
    sup.secrets.push("extra".into());
    sup.kv_max_bytes = sup.kv_max_bytes.saturating_add(1);
    sup.file_roots.push("extra".into());
    assert!(caps.is_subset_of(&sup), "not a subset of a superset");
    let mut sub = caps.clone();
    sub.hosts.pop();
    sub.secrets.pop();
    sub.kv_max_bytes /= 2;
    sub.file_roots.pop();
    assert!(sub.is_subset_of(&caps), "a subset is not a subset");
    // Canonical spellings are the same capability.
    let mut canon = caps.clone();
    for h in &mut canon.hosts {
        if let Ok(rule) = HostRule::parse(h) {
            *h = rule.to_string();
        }
    }
    assert!(canon.is_subset_of(&caps) && caps.is_subset_of(&canon));
    // `host_rules` fails exactly when some entry does not parse.
    let all_parse = caps.hosts.iter().all(|h| HostRule::parse(h).is_ok());
    assert_eq!(caps.host_rules().is_ok(), all_parse);

    // The manifest's validation of these capabilities. A connector, so file roots are allowed;
    // each file root is declared as a settings property, as validation requires.
    let mut props = file_roots.clone();
    props.sort();
    props.dedup();
    let props: String = props
        .iter()
        .map(|p| format!("{} = {{ type = \"string\" }}\n", toml_str(p)))
        .collect();
    let manifest = format!(
        "id = \"dev.example.fuzz.caps\"\nname = \"Caps\"\nversion = \"1.0.0\"\n\
         description = \"\"\nkind = [\"connector\"]\n\n[capabilities]\nhosts = {}\nsecrets = {}\n\
         kv_max_bytes = {kv_max_bytes}\nfile_roots = {}\n\n[connector]\ndisplay_name = \"Caps\"\n\n\
         [connector.settings_schema]\ntype = \"object\"\n\n[connector.settings_schema.properties]\n{props}",
        toml_list(&hosts),
        toml_list(&secrets),
        toml_list(&file_roots),
    );
    let unique = |v: &[String]| {
        let mut u = v.to_vec();
        u.sort();
        u.dedup();
        u.len() == v.len()
    };
    let root_ok = |r: &str| {
        !r.is_empty()
            && r.len() <= 64
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    let expect_ok = hosts.len() <= MAX_HOSTS
        && all_parse
        && secrets.len() <= MAX_SECRETS
        && secrets.iter().all(|s| secret_ok(s))
        && unique(&secrets)
        && kv_max_bytes <= MAX_KV_BYTES
        && file_roots.len() <= MAX_FILE_ROOTS
        && file_roots.iter().all(|r| root_ok(r))
        && unique(&file_roots);
    match Manifest::parse(manifest.as_bytes()) {
        Ok(m) => {
            assert!(
                expect_ok,
                "manifest accepted invalid capabilities: {caps:?}"
            );
            assert_eq!(m.capabilities, caps, "parsing changed the capabilities");
        }
        Err(e) => {
            // Over-long manifests are refused before validation.
            if manifest.len() <= 64 * 1024 {
                assert!(
                    !expect_ok,
                    "manifest refused valid capabilities {caps:?}: {e}"
                );
            }
        }
    }
});
