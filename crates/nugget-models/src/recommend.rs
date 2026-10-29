//! Explaining the hardware-aware recommendation: for every manifest entry, what it needs (RAM,
//! disk), whether this machine has it, whether it is recommended and why.
//!
//! The `recommended` answer is exactly the one the daemon has always given (the union of
//! [`recommend`] over [`Tier::ROUTED`]); this module only adds the reasons around it.

use nugget_core::capability::Tier;
use serde::Serialize;

use crate::hardware::{GpuKind, Hardware, recommend};
use crate::manifest::{Manifest, ModelEntry};
use crate::preflight::{GIB, required_bytes};
use crate::runtime::{RuntimeAccel, accel_preference};

/// Why a model is (or is not) recommended for a machine. Stable snake_case codes, meant to be
/// mapped to localized text by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FitReason {
    /// Fits in RAM and is the most capable model that fits for at least one of its tiers.
    RamFitsLargest,
    /// Fits in RAM, but a more capable model is preferred for all of its tiers.
    RamFits,
    /// Needs more RAM than this machine has.
    RamTooSmall,
    /// A reranker: opt-in, never recommended by default.
    RerankOptIn,
    /// Needs a newer Nugget than this build.
    NeedsNewerApp,
    /// A GPU backend is expected to accelerate it.
    GpuAccelerated,
    /// No usable GPU backend detected: runs on the CPU, more slowly.
    CpuOnlySlower,
}

/// How one manifest entry fits one machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelFit {
    pub id: String,
    /// Same answer as the daemon's `models.list` `recommended` flag.
    pub recommended: bool,
    pub tiers: Vec<Tier>,
    pub size_bytes: u64,
    /// Nominal RAM the model needs (GiB).
    pub min_ram_gb: u32,
    /// Free disk space a fresh install needs ([`required_bytes`]: size + 10% + 1 GiB).
    pub disk_needed_bytes: u64,
    /// Whether `min_ram_gb` fits this machine's RAM (10% slack, as in [`recommend`]).
    pub fits_ram: bool,
    /// This machine's RAM as a nominal figure in GiB (e.g. 16 for a "16 GB" machine).
    pub machine_ram_gb: u32,
    pub gpu: GpuKind,
    /// The compute backend expected to run it: `metal`, `cuda`, `vulkan` or `cpu`.
    pub accel: &'static str,
    pub reasons: Vec<FitReason>,
}

/// Memory sizes machines are commonly sold with (GiB).
const COMMON_RAM_GB: [u32; 18] = [
    1, 2, 3, 4, 6, 8, 12, 16, 18, 24, 32, 36, 48, 64, 96, 128, 192, 256,
];

/// Total RAM as the nominal figure a machine is sold with. The OS reports somewhat less (firmware,
/// GPU and kernel reservations), so a value up to 15% below a common size snaps up to it;
/// anything else rounds to the nearest GiB.
pub fn nominal_ram_gb(hw: &Hardware) -> u32 {
    let gib = hw.total_ram as f64 / GIB as f64;
    COMMON_RAM_GB
        .into_iter()
        .find(|&s| gib <= f64::from(s) && gib >= f64::from(s) * 0.85)
        .unwrap_or_else(|| gib.round().clamp(0.0, f64::from(u32::MAX)) as u32)
}

/// The compute backend expected on a machine with `gpu`: the first GPU backend of
/// [`accel_preference`] for that GPU (Apple Silicon means Metal), or CPU. Derived from the GPU
/// alone so the hint does not depend on the OS this daemon was built for.
pub fn accel_hint(gpu: GpuKind) -> RuntimeAccel {
    let (os, arch) = match gpu {
        GpuKind::AppleSilicon => ("macos", "aarch64"),
        _ => ("linux", "x86_64"),
    };
    accel_preference(os, arch, gpu)
        .into_iter()
        .next()
        .unwrap_or(RuntimeAccel::Cpu)
}

fn fits_ram(entry: &ModelEntry, hw: &Hardware) -> bool {
    u128::from(entry.min_ram_gb) * u128::from(GIB) <= u128::from(hw.total_ram) * 10 / 9
}

/// How `entry` (an entry of `manifest`) fits `hw`.
pub fn model_fit(manifest: &Manifest, entry: &ModelEntry, hw: &Hardware) -> ModelFit {
    let picked_for: Vec<Tier> = Tier::ROUTED
        .into_iter()
        .filter(|t| recommend(manifest, hw, *t).is_some_and(|e| e.id == entry.id))
        .collect();
    let recommended = !picked_for.is_empty();
    let fits = fits_ram(entry, hw);
    let compatible = semver::Version::parse(crate::APP_VERSION)
        .map(|app| entry.compatible_with(&app))
        .unwrap_or(false);
    let accel = accel_hint(hw.gpu);

    let mut reasons = Vec::new();
    if !compatible {
        reasons.push(FitReason::NeedsNewerApp);
    }
    if entry.tiers.contains(&Tier::Rerank) {
        reasons.push(FitReason::RerankOptIn);
    }
    reasons.push(match (fits, recommended) {
        (false, _) => FitReason::RamTooSmall,
        (true, true) => FitReason::RamFitsLargest,
        (true, false) => FitReason::RamFits,
    });
    if fits {
        reasons.push(if accel.is_gpu() {
            FitReason::GpuAccelerated
        } else {
            FitReason::CpuOnlySlower
        });
    }

    ModelFit {
        id: entry.id.clone(),
        recommended,
        tiers: entry.tiers.clone(),
        size_bytes: entry.size,
        min_ram_gb: entry.min_ram_gb,
        disk_needed_bytes: required_bytes(entry.size, 0),
        fits_ram: fits,
        machine_ram_gb: nominal_ram_gb(hw),
        gpu: hw.gpu,
        accel: accel.as_str(),
        reasons,
    }
}

/// [`model_fit`] for every manifest entry, in manifest order.
pub fn explain(manifest: &Manifest, hw: &Hardware) -> Vec<ModelFit> {
    manifest
        .models
        .iter()
        .map(|e| model_fit(manifest, e, hw))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(total_ram: u64, gpu: GpuKind) -> Hardware {
        Hardware {
            total_ram,
            available_ram: total_ram / 2,
            cpu_cores: 8,
            gpu,
        }
    }

    #[test]
    fn nominal_ram_rounds_reported_ram_up_to_the_sold_figure() {
        assert_eq!(nominal_ram_gb(&hw(16 * GIB, GpuKind::Unknown)), 16);
        assert_eq!(nominal_ram_gb(&hw(155 * GIB / 10, GpuKind::Unknown)), 16);
        assert_eq!(nominal_ram_gb(&hw(75 * GIB / 10, GpuKind::Unknown)), 8);
        assert_eq!(nominal_ram_gb(&hw(304 * GIB / 10, GpuKind::Unknown)), 32);
        assert_eq!(nominal_ram_gb(&hw(13 * GIB, GpuKind::Unknown)), 13);
        assert_eq!(nominal_ram_gb(&hw(0, GpuKind::Unknown)), 0);
        assert_eq!(nominal_ram_gb(&hw(GIB / 4, GpuKind::Unknown)), 0);
    }

    #[test]
    fn accel_hint_follows_the_gpu() {
        assert_eq!(accel_hint(GpuKind::AppleSilicon), RuntimeAccel::Metal);
        assert_eq!(accel_hint(GpuKind::Nvidia), RuntimeAccel::Cuda);
        assert_eq!(accel_hint(GpuKind::Amd), RuntimeAccel::Vulkan);
        assert_eq!(accel_hint(GpuKind::Unknown), RuntimeAccel::Cpu);
        assert_eq!(accel_hint(GpuKind::Intel), RuntimeAccel::Cpu);
    }

    #[test]
    fn reasons_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_value(FitReason::RamFitsLargest).unwrap(),
            "ram_fits_largest"
        );
        assert_eq!(
            serde_json::to_value(FitReason::CpuOnlySlower).unwrap(),
            "cpu_only_slower"
        );
    }
}
