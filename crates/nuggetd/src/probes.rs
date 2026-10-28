//! Persisted probe results (`probes.json` in the config dir).
//!
//! `nugget-store` exposes no API for its `meta` table, so results live next to `config.toml`.
//! Each result is keyed by provider id and carries a fingerprint of the provider's kind, base URL
//! and model: editing any of those in config invalidates the stored capabilities.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nugget_core::capability::Capabilities;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::ProviderConfig;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const FILE_NAME: &str = "probes.json";
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeRecord {
    pub fingerprint: String,
    #[serde(with = "time::serde::rfc3339")]
    pub probed_at: OffsetDateTime,
    pub capabilities: Capabilities,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProbesFile {
    version: u32,
    providers: BTreeMap<String, ProbeRecord>,
}

/// What identifies "the same model behind the same API" for a provider config.
pub fn fingerprint(cfg: &ProviderConfig) -> String {
    let base = cfg
        .base_url()
        .map(|u| u.to_string())
        .unwrap_or_else(|_| cfg.base_url.clone().unwrap_or_default());
    format!("{}|{}|{}", cfg.kind, base, cfg.model)
}

#[derive(Debug)]
pub struct ProbeCache {
    path: PathBuf,
    records: BTreeMap<String, ProbeRecord>,
}

impl ProbeCache {
    pub fn path_for(paths: &Paths) -> PathBuf {
        paths.config.join(FILE_NAME)
    }

    /// Load the cache. A missing file is empty; an unreadable or corrupt one is logged and
    /// treated as empty (probes can always be re-run).
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let records = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ProbesFile>(&bytes) {
                Ok(f) if f.version == VERSION => f.providers,
                Ok(f) => {
                    tracing::warn!(path = %path.display(), version = f.version, "ignoring probe results of unknown version");
                    BTreeMap::new()
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable probe results");
                    BTreeMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read probe results");
                BTreeMap::new()
            }
        };
        Self { path, records }
    }

    /// Stored capabilities for `cfg`, if they were probed against the same kind/URL/model.
    pub fn get(&self, cfg: &ProviderConfig) -> Option<&Capabilities> {
        self.records
            .get(cfg.id.as_str())
            .filter(|r| r.fingerprint == fingerprint(cfg))
            .map(|r| &r.capabilities)
    }

    pub fn record(&self, id: &str) -> Option<&ProbeRecord> {
        self.records.get(id)
    }

    /// Remember a probe result and write the file atomically.
    pub fn put(&mut self, cfg: &ProviderConfig, caps: Capabilities) -> Result<()> {
        self.records.insert(
            cfg.id.as_str().to_string(),
            ProbeRecord {
                fingerprint: fingerprint(cfg),
                probed_at: OffsetDateTime::now_utc(),
                capabilities: caps,
            },
        );
        self.save()
    }

    fn save(&self) -> Result<()> {
        let file = ProbesFile {
            version: VERSION,
            providers: self.records.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        write_atomic(&self.path, &bytes)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Other(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(model: &str) -> ProviderConfig {
        ProviderConfig::from_toml_str(&format!(
            "id = \"p\"\nkind = \"openai_compatible\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"{model}\""
        ))
        .unwrap()
    }

    #[test]
    fn round_trips_and_invalidates_on_model_change() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cfg").join(FILE_NAME);
        let caps = Capabilities {
            streaming: true,
            tools: true,
            ..Default::default()
        };
        let mut cache = ProbeCache::load(&path);
        assert!(cache.get(&cfg("a")).is_none());
        cache.put(&cfg("a"), caps.clone()).unwrap();

        let cache = ProbeCache::load(&path);
        assert_eq!(cache.get(&cfg("a")), Some(&caps));
        assert!(cache.get(&cfg("b")).is_none(), "model changed");
        assert!(cache.record("p").is_some());
    }

    #[test]
    fn corrupt_file_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(FILE_NAME);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(ProbeCache::load(&path).get(&cfg("a")).is_none());
    }
}
