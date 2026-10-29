//! `models.fit`: why each manifest model is (or is not) recommended for this machine, and what it
//! needs (RAM, disk). The `recommended` answers are the same as `models.list`'s.
//!
//! Params: none (`null` or `{}`). Result:
//! `{hardware: {total_ram, available_ram, cpu_cores, gpu}, ram_gb, free_disk_bytes, models}`,
//! where each model is a [`nugget_models::ModelFit`] plus `fits_disk` (`null` when free space at
//! the models directory cannot be read).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::{Error, Result};
use nugget_ipc::{Notifier, RpcError, to_result};
use nugget_models::{DiskSpace, Hardware, Manifest, ModelFit, SystemDiskSpace};
use serde::Serialize;
use serde_json::Value;

use super::{Feature, FeatureContext};
use crate::daemon::{State, blocking};

const MODELS_FIT: &str = "models.fit";

/// One model, as `models.fit` reports it.
#[derive(Debug, Serialize)]
pub(crate) struct ModelFitView {
    #[serde(flatten)]
    pub fit: ModelFit,
    /// Whether the models directory has room for a fresh install (`null`: unknown).
    pub fits_disk: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FitReport {
    pub hardware: Hardware,
    /// Nominal RAM in GiB (e.g. 16 for a "16 GB" machine).
    pub ram_gb: u32,
    /// Free space at the models directory, when readable.
    pub free_disk_bytes: Option<u64>,
    pub models: Vec<ModelFitView>,
}

/// The pure part of `models.fit`.
pub(crate) fn report(manifest: &Manifest, hw: &Hardware, free_disk: Option<u64>) -> FitReport {
    let models = nugget_models::explain(manifest, hw)
        .into_iter()
        .map(|fit| ModelFitView {
            fits_disk: free_disk.map(|free| free >= fit.disk_needed_bytes),
            fit,
        })
        .collect();
    FitReport {
        hardware: *hw,
        ram_gb: nugget_models::nominal_ram_gb(hw),
        free_disk_bytes: free_disk,
        models,
    }
}

/// Free space on the filesystem that holds (or will hold) `dir`: the nearest existing ancestor.
fn free_space(disk: &dyn DiskSpace, dir: &Path) -> Option<u64> {
    dir.ancestors()
        .find(|p| p.exists())
        .and_then(|p| disk.available_bytes(p).ok())
}

pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    Ok(Arc::new(ModelFitFeature {
        state: cx.state.clone(),
    }))
}

struct ModelFitFeature {
    state: Arc<State>,
}

#[async_trait]
impl Feature for ModelFitFeature {
    fn name(&self) -> &'static str {
        "model_fit"
    }

    fn methods(&self) -> &'static [&'static str] {
        &[MODELS_FIT]
    }

    async fn call(
        &self,
        method: &str,
        _params: Value,
        _notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        if method != MODELS_FIT {
            return Err(RpcError::method_not_found(method));
        }
        let manifest = self.state.manifest.as_ref().map_err(|e| {
            RpcError::from(Error::Integrity(format!("model manifest unavailable: {e}")))
        })?;
        let dir = self.state.models.root().to_path_buf();
        let free = blocking(move || Ok(free_space(&SystemDiskSpace, &dir))).await?;
        to_result(report(manifest, &self.state.hardware, free))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_models::{GIB, GpuKind, default_manifest};

    fn hw(gb: u64, gpu: GpuKind) -> Hardware {
        Hardware {
            total_ram: gb * GIB * 95 / 100,
            available_ram: gb * GIB / 2,
            cpu_cores: 8,
            gpu,
        }
    }

    #[test]
    fn report_explains_every_model_and_checks_disk() {
        let m = default_manifest().unwrap();
        let r = report(&m, &hw(16, GpuKind::Nvidia), Some(4 * GIB));
        assert_eq!(r.ram_gb, 16);
        assert_eq!(r.models.len(), m.models.len());
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["hardware"]["gpu"], "nvidia");
        assert_eq!(v["free_disk_bytes"], 4 * GIB);
        let large = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "qwen3-8b-q4km")
            .unwrap();
        assert_eq!(large["recommended"], true);
        assert_eq!(large["fits_ram"], true);
        // ~4.7 GiB + 10% + 1 GiB does not fit in 4 GiB.
        assert_eq!(large["fits_disk"], false);
        assert_eq!(large["accel"], "cuda");
        let embed = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "nomic-embed-text-v1.5-q8")
            .unwrap();
        assert_eq!(embed["fits_disk"], true);

        let unknown = report(&m, &hw(8, GpuKind::Unknown), None);
        let v = serde_json::to_value(&unknown).unwrap();
        assert_eq!(v["free_disk_bytes"], Value::Null);
        assert!(
            v["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["fits_disk"].is_null())
        );
    }

    #[test]
    fn free_space_uses_the_nearest_existing_ancestor() {
        struct Fixed;
        impl DiskSpace for Fixed {
            fn available_bytes(&self, _: &Path) -> std::io::Result<u64> {
                Ok(42)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            free_space(&Fixed, &tmp.path().join("a").join("b")),
            Some(42)
        );
    }
}
