//! `nugget-plugin.toml`: TOML parsing, validation and host-rule parsing.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nugget_plugin::{HostRule, Manifest, PluginKind};

fuzz_target!(|data: &[u8]| {
    let Ok(manifest) = Manifest::parse(data) else {
        return;
    };
    let _ = manifest.provides(PluginKind::Connector);
    let _ = manifest.provides(PluginKind::Tool);
    assert!(manifest.capabilities.is_subset_of(&manifest.capabilities));
    // An accepted manifest's hosts all parse, and their canonical form parses to the same rule.
    let rules = manifest
        .capabilities
        .host_rules()
        .expect("validated hosts parse");
    let probe = url::Url::parse("https://feeds.example.com:443/x").expect("static url");
    for rule in rules {
        let again = HostRule::parse(&rule.to_string()).expect("canonical host rule parses");
        assert_eq!(again.to_string(), rule.to_string());
        let _ = rule.matches(&probe);
    }
});
