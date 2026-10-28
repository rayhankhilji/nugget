//! Where does a base URL send data? Decided from the host alone, without DNS.
//!
//! - Loopback (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`, unspecified) → `OnDevice`.
//! - Private/LAN (`10/8`, `172.16/12`, `192.168/16`, link-local, CGNAT `100.64/10`, IPv6 ULA and
//!   link-local, `*.local`, `*.lan`, `*.home.arpa`, `*.internal`, single-label names) →
//!   `LocalNetwork`.
//! - Everything else → `Cloud`.
//!
//! Hostnames that merely *resolve* to private addresses classify as `Cloud`, which errs towards
//! recording egress.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use nugget_core::capability::Locality;
use nugget_core::{Error, Result};
use url::{Host, Url};

/// Classify a URL by its host.
pub fn classify_url(base_url: &str) -> Result<Locality> {
    let url =
        Url::parse(base_url).map_err(|e| Error::Invalid(format!("base_url {base_url:?}: {e}")))?;
    classify_parsed(&url)
}

pub(crate) fn classify_parsed(url: &Url) -> Result<Locality> {
    match url.host() {
        Some(Host::Ipv4(ip)) => Ok(classify_ip(IpAddr::V4(ip))),
        Some(Host::Ipv6(ip)) => Ok(classify_ip(IpAddr::V6(ip))),
        Some(Host::Domain(d)) => Ok(classify_host(d)),
        None => Err(Error::Invalid(format!("base_url {url} has no host"))),
    }
}

/// Classify a bare host name or IP literal (IPv6 may be bracketed).
pub fn classify_host(host: &str) -> Locality {
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return classify_ip(ip);
    }
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return Locality::Cloud;
    }
    if h == "localhost" || h.ends_with(".localhost") {
        return Locality::OnDevice;
    }
    const LAN_SUFFIXES: [&str; 4] = [".local", ".lan", ".home.arpa", ".internal"];
    if LAN_SUFFIXES.iter().any(|s| h.ends_with(s)) || !h.contains('.') {
        return Locality::LocalNetwork;
    }
    Locality::Cloud
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
mod tests {
    use super::*;
    use Locality::*;

    #[test]
    fn urls() {
        let cases = [
            ("http://localhost:11434/v1", OnDevice),
            ("http://LOCALHOST./v1", OnDevice),
            ("http://foo.localhost:8080", OnDevice),
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
            ("http://gpu-box:8080", LocalNetwork),
            ("http://studio.local:1234/v1", LocalNetwork),
            ("http://nas.lan", LocalNetwork),
            ("http://box.home.arpa", LocalNetwork),
            ("http://host.docker.internal:11434", LocalNetwork),
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
        assert_eq!(classify_host(""), Cloud);
    }
}
