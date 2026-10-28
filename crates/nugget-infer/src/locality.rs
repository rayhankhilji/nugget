//! Where does a base URL send data?
//!
//! Without DNS ([`classify_url`], [`classify_host`]), only literals are trusted:
//! - `localhost` (exactly; no subdomains), `127.0.0.0/8`, `::1` and unspecified → `OnDevice`.
//! - Private/LAN IP literals (`10/8`, `172.16/12`, `192.168/16`, link-local, CGNAT `100.64/10`,
//!   IPv6 ULA and link-local) → `LocalNetwork`.
//! - Every other IP literal, and every other host name → `Cloud`.
//!
//! A name such as `gpu-box`, `studio.local` or `foo.localhost` says nothing about where DNS will
//! send the traffic, so it is never trusted by its spelling. When a provider is built, such a
//! name is resolved once: it is `OnDevice`/`LocalNetwork` only if EVERY resolved address is
//! loopback/private, and then the HTTP client is pinned to exactly that address set, so a later
//! DNS answer (rebinding) cannot move the traffic off-device without a ledger record. A name that
//! fails to resolve is `Cloud`: egress is recorded and plaintext http is refused. `localhost`
//! itself is pinned to the loopback addresses.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use nugget_core::capability::Locality;
use nugget_core::{Error, Result};
use url::{Host, Url};

/// Classify a URL by its host, without DNS (see the module docs).
pub fn classify_url(base_url: &str) -> Result<Locality> {
    let url =
        Url::parse(base_url).map_err(|e| Error::Invalid(format!("base_url {base_url:?}: {e}")))?;
    classify_parsed(&url)
}

pub(crate) fn classify_parsed(url: &Url) -> Result<Locality> {
    Ok(literal_locality(url)?.unwrap_or(Locality::Cloud))
}

/// The locality of a host that needs no DNS: an IP literal or `localhost`. `None` for any other
/// name, whose locality is only known after resolution.
pub(crate) fn literal_locality(url: &Url) -> Result<Option<Locality>> {
    match url.host() {
        Some(Host::Ipv4(ip)) => Ok(Some(classify_ip(IpAddr::V4(ip)))),
        Some(Host::Ipv6(ip)) => Ok(Some(classify_ip(IpAddr::V6(ip)))),
        Some(Host::Domain(d)) if is_localhost(d) => Ok(Some(Locality::OnDevice)),
        Some(Host::Domain(_)) => Ok(None),
        None => Err(Error::Invalid(format!("base_url {url} has no host"))),
    }
}

/// Classify a bare host name or IP literal (IPv6 may be bracketed), without DNS: names other
/// than `localhost` are `Cloud` (see the module docs).
pub fn classify_host(host: &str) -> Locality {
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return classify_ip(ip);
    }
    if is_localhost(host) {
        Locality::OnDevice
    } else {
        Locality::Cloud
    }
}

/// Exactly `localhost` (any case, optional trailing dot).
fn is_localhost(host: &str) -> bool {
    host.strip_suffix('.')
        .unwrap_or(host)
        .eq_ignore_ascii_case("localhost")
}

/// Resolves a host name to addresses. The system resolver in production; a fake in tests.
pub(crate) trait HostResolver: Send + Sync {
    fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>>;
}

/// The OS resolver (`getaddrinfo`). Blocking.
pub(crate) struct SystemResolver;

impl HostResolver for SystemResolver {
    fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok((host, port).to_socket_addrs()?.map(|a| a.ip()).collect())
    }
}

/// Where a provider's traffic goes, decided once when it is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// Classification of the addresses the client may connect to (before any config override).
    pub locality: Locality,
    /// `(host, addrs)`: the client must connect to exactly these addresses for `host`.
    pub pin: Option<(String, Vec<SocketAddr>)>,
    /// Resolution failed; `locality` is the conservative `Cloud`.
    pub unresolved: bool,
}

/// Resolve the URL's host once and classify it (see the module docs).
pub(crate) fn resolve_url(url: &Url, resolver: &dyn HostResolver) -> Result<Resolved> {
    let port = url.port_or_known_default().unwrap_or(0);
    let name = match url.host() {
        Some(Host::Domain(d)) => d,
        _ => {
            // IP literals connect where they say; nothing to resolve or pin.
            return Ok(Resolved {
                locality: classify_parsed(url)?,
                pin: None,
                unresolved: false,
            });
        }
    };
    if is_localhost(name) {
        let addrs = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        ];
        return Ok(Resolved {
            locality: Locality::OnDevice,
            pin: Some((name.to_string(), addrs)),
            unresolved: false,
        });
    }
    let ips = match resolver.lookup(name, port) {
        Ok(ips) if !ips.is_empty() => ips,
        Ok(_) | Err(_) => {
            tracing::warn!(
                host = name,
                "could not resolve provider host; treating it as cloud"
            );
            return Ok(Resolved {
                locality: Locality::Cloud,
                pin: None,
                unresolved: true,
            });
        }
    };
    let locality = ips
        .iter()
        .map(|ip| classify_ip(*ip))
        .max_by_key(|l| rank(*l))
        .unwrap_or(Locality::Cloud);
    if locality == Locality::Cloud {
        // Already the most conservative class; a later DNS change cannot hide egress.
        return Ok(Resolved {
            locality,
            pin: None,
            unresolved: false,
        });
    }
    let mut addrs: Vec<SocketAddr> = Vec::with_capacity(ips.len());
    for ip in ips {
        let a = SocketAddr::new(ip, port);
        if !addrs.contains(&a) {
            addrs.push(a);
        }
    }
    Ok(Resolved {
        locality,
        pin: Some((name.to_string(), addrs)),
        unresolved: false,
    })
}

pub fn classify_ip(ip: IpAddr) -> Locality {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return classify_v4(v4);
            }
            classify_v6(v6)
        }
    }
}

fn classify_v4(ip: Ipv4Addr) -> Locality {
    let o = ip.octets();
    if ip.is_loopback() || ip.is_unspecified() {
        Locality::OnDevice
    } else if ip.is_private() || ip.is_link_local() || (o[0] == 100 && (o[1] & 0xC0) == 64) {
        Locality::LocalNetwork
    } else {
        Locality::Cloud
    }
}

fn classify_v6(ip: Ipv6Addr) -> Locality {
    let seg0 = ip.segments()[0];
    if ip.is_loopback() || ip.is_unspecified() {
        Locality::OnDevice
    } else if (seg0 & 0xFE00) == 0xFC00 || (seg0 & 0xFFC0) == 0xFE80 {
        Locality::LocalNetwork
    } else {
        Locality::Cloud
    }
}

/// How strongly a locality implies data leaves the device (higher = further away).
pub(crate) fn rank(l: Locality) -> u8 {
    match l {
        Locality::OnDevice => 0,
        Locality::LocalNetwork => 1,
        Locality::Cloud => 2,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use Locality::*;

    #[test]
    fn urls() {
        let cases = [
            ("http://localhost:11434/v1", OnDevice),
            ("http://LOCALHOST./v1", OnDevice),
            ("http://foo.localhost:8080", Cloud),
            ("http://127.0.0.1:1234/v1", OnDevice),
            ("http://127.5.6.7", OnDevice),
            ("http://[::1]:8080/v1", OnDevice),
            ("http://0.0.0.0:8080", OnDevice),
            ("http://[::ffff:127.0.0.1]:80", OnDevice),
            ("http://192.168.1.20:11434/v1", LocalNetwork),
            ("http://10.0.0.5", LocalNetwork),
            ("http://172.16.0.1", LocalNetwork),
            ("http://172.31.255.255", LocalNetwork),
            ("http://169.254.1.1", LocalNetwork),
            ("http://100.101.102.103", LocalNetwork),
            ("http://[fd12:3456::1]", LocalNetwork),
            ("http://[fe80::1]", LocalNetwork),
            ("http://[::ffff:192.168.0.2]", LocalNetwork),
            // Names are never trusted by their spelling without resolution.
            ("http://gpu-box:8080", Cloud),
            ("http://studio.local:1234/v1", Cloud),
            ("http://nas.lan", Cloud),
            ("http://box.home.arpa", Cloud),
            ("http://host.docker.internal:11434", Cloud),
            ("https://api.anthropic.com", Cloud),
            ("https://api.openai.com/v1", Cloud),
            ("https://openrouter.ai/api/v1", Cloud),
            ("http://172.32.0.1", Cloud),
            ("http://100.128.0.1", Cloud),
            ("http://8.8.8.8", Cloud),
            ("http://[2001:4860::8888]", Cloud),
            ("https://localhost.example.com", Cloud),
            ("https://my.local.example.com", Cloud),
        ];
        for (url, want) in cases {
            assert_eq!(classify_url(url).unwrap(), want, "{url}");
        }
    }

    #[test]
    fn invalid_urls() {
        assert!(classify_url("not a url").is_err());
        assert!(classify_url("unix:/tmp/sock").is_err());
    }

    #[test]
    fn bare_hosts() {
        assert_eq!(classify_host("[::1]"), OnDevice);
        assert_eq!(classify_host("192.168.0.1"), LocalNetwork);
        assert_eq!(classify_host("example.com"), Cloud);
        assert_eq!(classify_host("localhost"), OnDevice);
        assert_eq!(classify_host("a.localhost"), Cloud);
        assert_eq!(classify_host("nas"), Cloud);
        assert_eq!(classify_host(""), Cloud);
    }

    /// Answers from a fixed table and counts lookups; never touches real DNS.
    pub(crate) struct FakeResolver {
        pub answers: Vec<(&'static str, std::io::Result<Vec<IpAddr>>)>,
        pub calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeResolver {
        pub(crate) fn new(answers: Vec<(&'static str, Vec<&str>)>) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|(h, ips)| (h, Ok(ips.iter().map(|i| i.parse().unwrap()).collect())))
                    .collect(),
                calls: Default::default(),
            }
        }
    }

    impl HostResolver for FakeResolver {
        fn lookup(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.answers.iter().find(|(h, _)| *h == host) {
                Some((_, Ok(ips))) => Ok(ips.clone()),
                Some((_, Err(e))) => Err(std::io::Error::new(e.kind(), e.to_string())),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such host",
                )),
            }
        }
    }

    #[test]
    fn names_are_classified_by_every_resolved_address() {
        let r = FakeResolver::new(vec![
            ("gpu-box", vec!["192.168.1.7"]),
            ("dev.localhost", vec!["127.0.0.1", "::1"]),
            ("mixed.lan", vec!["192.168.1.7", "8.8.8.8"]),
            ("loop-and-lan.internal", vec!["127.0.0.1", "10.0.0.2"]),
            ("api.example.com", vec!["93.184.216.34"]),
            ("empty.lan", vec![]),
        ]);
        let res = |u: &str| resolve_url(&Url::parse(u).unwrap(), &r).unwrap();

        let lan = res("http://gpu-box:8080/v1");
        assert_eq!(lan.locality, LocalNetwork);
        assert_eq!(
            lan.pin,
            Some(("gpu-box".into(), vec!["192.168.1.7:8080".parse().unwrap()]))
        );
        assert_eq!(res("http://dev.localhost:9/").locality, OnDevice);
        // One public address is enough to make the whole name Cloud, and Cloud is not pinned.
        let mixed = res("http://mixed.lan/");
        assert_eq!((mixed.locality, mixed.pin), (Cloud, None));
        assert_eq!(res("http://loop-and-lan.internal/").locality, LocalNetwork);
        assert_eq!(res("https://api.example.com/").locality, Cloud);
        // Resolution failures (and empty answers) are conservatively Cloud.
        let nx = res("http://nas.lan/");
        assert_eq!((nx.locality, nx.unresolved), (Cloud, true));
        assert_eq!(res("http://empty.lan/").locality, Cloud);
        let calls = r.calls.load(std::sync::atomic::Ordering::SeqCst);

        // `localhost` and IP literals are never looked up; localhost is pinned to loopback.
        let lh = res("http://LOCALHOST:11434/v1");
        assert_eq!(lh.locality, OnDevice);
        let (host, addrs) = lh.pin.unwrap();
        assert_eq!(host, "localhost");
        assert!(
            addrs
                .iter()
                .all(|a| a.ip().is_loopback() && a.port() == 11434)
        );
        assert_eq!(res("http://192.168.0.3/").pin, None);
        assert_eq!(res("http://[::1]:80/").locality, OnDevice);
        assert_eq!(r.calls.load(std::sync::atomic::Ordering::SeqCst), calls);
    }
}
