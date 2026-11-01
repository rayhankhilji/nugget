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
