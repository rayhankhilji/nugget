//! Plugins reach only the public internet unless `HostConfig::allow_private_network` is set.
//! The address classifier and the destination check run without guests (and without network:
//! only IP literals and `localhost`, which resolves locally); the end-to-end test needs the
//! hostile test plugin.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use nugget_core::Error;
use reqwest::dns::Resolve;
use serde_json::json;
use url::Url;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::host::{NetGuard, NetRefusal, is_non_public};
use crate::{HostConfig, Limits, PluginHost};

fn v4(s: &str) -> IpAddr {
    IpAddr::V4(s.parse::<Ipv4Addr>().unwrap())
}

fn v6(s: &str) -> IpAddr {
    IpAddr::V6(s.parse::<Ipv6Addr>().unwrap())
}

#[test]
fn ipv4_classification() {
    for blocked in [
        "127.0.0.1",
        "127.255.255.254",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "100.127.255.255",
        "0.0.0.0",
        "0.1.2.3",
        "255.255.255.255",
        "224.0.0.1",
        "239.255.255.250",
        "192.0.2.10",
        "198.51.100.10",
        "203.0.113.10",
        "192.0.0.8",
        "198.18.0.1",
        "198.19.255.255",
        "240.0.0.1",
    ] {
        assert!(is_non_public(v4(blocked)), "{blocked} should be blocked");
    }
    for public in [
        "1.1.1.1",
        "8.8.8.8",
        "93.184.216.34",
        "172.15.255.255",
        "172.32.0.1",
        "100.63.255.255",
        "100.128.0.1",
        "192.169.0.1",
        "198.17.255.255",
        "198.20.0.1",
        "223.255.255.255",
    ] {
        assert!(!is_non_public(v4(public)), "{public} should be allowed");
    }
}

#[test]
fn ipv6_classification() {
    for blocked in [
        "::1",
        "::",
        "fc00::1",
        "fd12:3456:789a::1",
        "fe80::1",
        "febf::1",
        "fec0::1",
        "ff02::1",
        "2001:db8::1",
        "3fff::1",
        "64:ff9b:1::1",
        "100::1",
    ] {
        assert!(is_non_public(v6(blocked)), "{blocked} should be blocked");
    }
    for public in ["2606:4700:4700::1111", "2001:4860:4860::8888", "fe00::1"] {
        assert!(!is_non_public(v6(public)), "{public} should be allowed");
    }
}

#[test]
fn embedded_ipv4_classification() {
    for blocked in [
        // IPv4-mapped.
        "::ffff:127.0.0.1",
        "::ffff:10.1.2.3",
        "::ffff:169.254.169.254",
        "::ffff:0.0.0.0",
        // IPv4-compatible (deprecated).
        "::127.0.0.1",
        "::192.168.0.1",
        // NAT64 well-known prefix.
        "64:ff9b::7f00:1",
        "64:ff9b::a00:1",
    ] {
        assert!(is_non_public(v6(blocked)), "{blocked} should be blocked");
    }
    for public in ["::ffff:1.1.1.1", "::8.8.8.8", "64:ff9b::808:808"] {
        assert!(!is_non_public(v6(public)), "{public} should be allowed");
    }
}

#[tokio::test]
async fn destination_check_refuses_private_literals_and_names() {
    let guard = NetGuard::default();
    for url in [
        "http://127.0.0.1:8080/",
        "https://10.0.0.1/",
        "http://[::1]/",
        "http://[::ffff:192.168.0.1]/",
        "http://169.254.169.254/latest/meta-data/",
        "http://0x7f.1/",
        "http://localhost:9/",
        "http://LOCALHOST./",
    ] {
        match guard.check(&Url::parse(url).unwrap()).await {
            Err(NetRefusal::Blocked(_)) => {}
            other => panic!("{url}: expected a refusal, got {other:?}"),
        }
    }
    // Public literals pass without any lookup.
    for url in ["https://1.1.1.1/", "https://[2606:4700:4700::1111]/"] {
        guard.check(&Url::parse(url).unwrap()).await.unwrap();
    }
}

#[tokio::test]
async fn the_client_resolver_refuses_private_names_and_serves_pins() {
    let guard = NetGuard::default();
    let err = guard
        .resolve("localhost".parse().unwrap())
        .await
        .err()
        .expect("localhost must not resolve through the guard");
    assert!(err.to_string().contains("private or local"), "{err}");
    assert!(guard.pinned("localhost").is_none());
}

#[tokio::test]
async fn plugins_cannot_reach_private_addresses_by_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .mount(&server)
        .await;
    let Some(wasm) = guest_wasm("hostile_plugin") else {
        return;
    };
    let port = server.address().port();
    let id = "dev.nugget.test.private-net";
    let caps = format!(
        "hosts = [\"{}\", \"http://localhost:{port}\", \"http://10.0.0.1\", \"http://[::1]:{port}\"]",
        server.uri()
    );
    let (_dir, store) = temp_store();
    let k = key(21);
    store
        .install(
            &package(&manifest_toml(id, "1.0.0", "[\"tool\"]", &caps), &wasm, &k),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    let egress = Arc::new(VecEgress::default());
    let host = PluginHost::new(HostConfig {
        limits: Limits {
            memory_bytes: 32 * 1024 * 1024,
            call_timeout: Duration::from_secs(20),
            ..Limits::default()
        },
        state_dir: store.state_root(),
        egress: egress.clone(),
        secrets: None,
        http_client: None,
        allow_private_network: false,
    })
    .unwrap();
    let plugin = host.load(&store.load(id).unwrap()).unwrap();
    for url in [
        server.uri(),
        format!("http://localhost:{port}/"),
        "http://10.0.0.1/".to_string(),
        format!("http://[::1]:{port}/"),
    ] {
        match plugin.call_tool("fetch", &json!({ "url": url })).await {
            Err(Error::Other(m)) if m.contains("capability denied") && m.contains("private") => {}
            other => panic!("{url}: expected a capability denial, got {other:?}"),
        }
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    // Refused before sending, like an undeclared host: nothing in the ledger.
    assert!(egress.0.lock().unwrap().is_empty());
}
