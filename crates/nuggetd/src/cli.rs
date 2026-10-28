//! Command-line client pieces: `nuggetd status` and service-spec construction.

use std::fmt::Write as _;
use std::path::PathBuf;

use nugget_core::capability::Locality;
use nugget_core::ipc::StatusResponse;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};

use crate::config::tier_name;
use crate::service::ServiceSpec;

/// Resolve paths: `--home DIR` wins, then `NUGGET_HOME`, then the OS defaults.
pub fn resolve_paths(home: Option<PathBuf>) -> Result<Paths> {
    match home {
        Some(h) => Ok(Paths::rooted(std::path::absolute(&h).map_err(|e| {
            Error::Invalid(format!("--home {}: {e}", h.display()))
        })?)),
        None => Paths::discover(),
    }
}

/// The data root to bake into a service definition, if one is overridden.
pub fn home_override(home: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let h = home.or_else(|| std::env::var_os("NUGGET_HOME").map(PathBuf::from));
    h.map(|h| std::path::absolute(&h).map_err(|e| Error::Invalid(format!("{}: {e}", h.display()))))
        .transpose()
}

/// What `install-service` registers: this binary, the home override and the log directory.
pub fn service_spec(paths: &Paths, home: Option<PathBuf>) -> Result<ServiceSpec> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| Error::Other(format!("cannot locate the nuggetd binary: {e}")))?;
    Ok(ServiceSpec {
        exe,
        home: home_override(home)?,
        log_dir: crate::logging::log_dir(paths),
    })
}

fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

fn locality(l: Locality) -> &'static str {
    match l {
        Locality::OnDevice => "on-device",
        Locality::LocalNetwork => "local network",
        Locality::Cloud => "cloud",
    }
}

/// Human-readable `nuggetd status` output.
pub fn format_status(s: &StatusResponse, socket: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "nuggetd {} running (protocol {}), up {}",
        s.version,
        s.protocol,
        duration(s.uptime_secs)
    );
    let _ = writeln!(out, "socket:    {socket}");
    let _ = writeln!(out, "store:     {} items, {} chunks", s.items, s.chunks);
    if s.providers.is_empty() {
        let _ = writeln!(out, "providers: none configured");
        return out;
    }
    let _ = writeln!(out, "providers:");
    for p in &s.providers {
        let tiers: Vec<&str> = p.tiers.iter().map(|t| tier_name(*t)).collect();
        let caps = match &p.capabilities {
            None => "not probed".to_string(),
            Some(c) => {
                let names: Vec<&str> = [
                    ("streaming", c.streaming),
                    ("vision", c.vision),
                    ("tools", c.tools),
                    ("json_schema", c.json_schema),
                    ("embeddings", c.embeddings),
                ]
                .into_iter()
                .filter_map(|(n, ok)| ok.then_some(n))
                .collect();
                if names.is_empty() {
                    "probed: no capabilities".into()
                } else {
                    format!("probed: {}", names.join(", "))
                }
            }
        };
        let _ = writeln!(
            out,
            "  {}  {} {}  {}  tiers: {}  {}",
            p.id,
            p.kind,
            p.model,
            locality(p.locality),
            if tiers.is_empty() {
                "-".to_string()
            } else {
                tiers.join(", ")
            },
            caps
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::capability::{Capabilities, Tier};
    use nugget_core::ids::ProviderId;
    use nugget_core::ipc::ProviderInfo;

    #[test]
    fn formats_status() {
        let s = StatusResponse {
            protocol: 1,
            version: "0.0.1".into(),
            uptime_secs: 3725,
            items: 3,
            chunks: 9,
            providers: vec![
                ProviderInfo {
                    id: ProviderId::new("local"),
                    kind: "openai_compatible".into(),
                    model: "m".into(),
                    locality: Locality::OnDevice,
                    tiers: vec![Tier::Triage, Tier::Light],
                    capabilities: None,
                },
                ProviderInfo {
                    id: ProviderId::new("cloud"),
                    kind: "anthropic".into(),
                    model: "c".into(),
                    locality: Locality::Cloud,
                    tiers: vec![],
                    capabilities: Some(Capabilities {
                        streaming: true,
                        tools: true,
                        ..Default::default()
                    }),
                },
            ],
        };
        let out = format_status(&s, "/tmp/sock");
        assert!(out.starts_with("nuggetd 0.0.1 running (protocol 1), up 1h 2m\n"));
        assert!(out.contains("store:     3 items, 9 chunks"));
        assert!(
            out.contains(
                "  local  openai_compatible m  on-device  tiers: triage, light  not probed"
            )
        );
        assert!(out.contains("  cloud  anthropic c  cloud  tiers: -  probed: streaming, tools"));
    }

    #[test]
    fn durations() {
        assert_eq!(duration(5), "5s");
        assert_eq!(duration(65), "1m 5s");
        assert_eq!(duration(90_061), "1d 1h");
    }

    #[test]
    fn home_flag_is_made_absolute() {
        let p = resolve_paths(Some(PathBuf::from("rel-home"))).unwrap();
        assert!(p.data.is_absolute());
        assert!(p.data.ends_with("rel-home/data") || p.data.ends_with(r"rel-home\data"));
    }
}
