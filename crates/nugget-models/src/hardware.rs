//! Best-effort hardware detection and manifest-driven model recommendation.

use nugget_core::capability::Tier;
use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, ModelEntry};
use crate::preflight::GIB;

/// Coarse GPU classification. Detection is best effort; `Unknown` means "could not tell", not
/// "no GPU".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuKind {
    /// Apple Silicon unified memory (Metal).
    AppleSilicon,
    Nvidia,
    Amd,
    Intel,
    Unknown,
}

/// What this machine has, as far as we can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hardware {
    /// Total RAM in bytes (capped by a cgroup memory limit when one applies).
    pub total_ram: u64,
    /// RAM currently available in bytes.
    pub available_ram: u64,
    /// Logical CPU cores usable by this process.
    pub cpu_cores: usize,
    pub gpu: GpuKind,
}

/// Probe the current machine. Never fails; unknown values degrade to conservative defaults.
pub fn detect_hardware() -> Hardware {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let mut total_ram = sys.total_memory();
    let mut available_ram = sys.available_memory();
    if let Some(cg) = sys.cgroup_limits()
        && cg.total_memory > 0
        && cg.total_memory < total_ram
    {
        total_ram = cg.total_memory;
        available_ram = available_ram.min(cg.free_memory);
    }
    let cpu_cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    Hardware {
        total_ram,
        available_ram,
        cpu_cores,
        gpu: detect_gpu(),
    }
}

/// Pick the most capable entry for `tier` that this machine can run.
///
/// An entry qualifies if it serves `tier`, is compatible with this app version, and its
/// `min_ram_gb` fits in total RAM. `min_ram_gb` is a *nominal* figure (a "16 GB" machine), while
/// the OS reports somewhat less after firmware, GPU and kernel reservations, so 10% slack is
/// allowed. Among qualifying entries the one with the highest RAM requirement (then the largest
/// file) wins, as a proxy for capability.
pub fn recommend<'a>(manifest: &'a Manifest, hw: &Hardware, tier: Tier) -> Option<&'a ModelEntry> {
    let app = semver::Version::parse(crate::APP_VERSION).ok()?;
    let nominal_ram = u128::from(hw.total_ram) * 10 / 9;
    manifest
        .models
        .iter()
        .filter(|m| m.tiers.contains(&tier))
        .filter(|m| m.compatible_with(&app))
        .filter(|m| u128::from(m.min_ram_gb) * u128::from(GIB) <= nominal_ram)
        .max_by_key(|m| (m.min_ram_gb, m.size))
}

fn detect_gpu() -> GpuKind {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        GpuKind::AppleSilicon
    }
    #[cfg(target_os = "linux")]
    {
        linux_gpu()
    }
    #[cfg(windows)]
    {
        windows_gpu()
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        target_os = "linux",
        windows
    )))]
    {
        GpuKind::Unknown
    }
}

/// Map a PCI vendor id to a GPU kind.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn gpu_from_pci_vendor(vendor: u32) -> Option<GpuKind> {
    match vendor {
        0x10de => Some(GpuKind::Nvidia),
        0x1002 | 0x1022 => Some(GpuKind::Amd),
        0x8086 => Some(GpuKind::Intel),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn linux_gpu() -> GpuKind {
    if std::path::Path::new("/proc/driver/nvidia/version").exists() {
        return GpuKind::Nvidia;
    }
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return GpuKind::Unknown;
    };
    // Prefer a discrete vendor over Intel integrated graphics when several cards exist.
    let mut best: Option<GpuKind> = None;
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let Ok(v) = std::fs::read_to_string(e.path().join("device/vendor")) else {
            continue;
        };
        let Ok(v) = u32::from_str_radix(v.trim().trim_start_matches("0x"), 16) else {
            continue;
        };
        if let Some(kind) = gpu_from_pci_vendor(v) {
            best = match (best, kind) {
                (Some(GpuKind::Nvidia), _) | (_, GpuKind::Nvidia) => Some(GpuKind::Nvidia),
                (Some(GpuKind::Amd), _) | (_, GpuKind::Amd) => Some(GpuKind::Amd),
                _ => Some(kind),
            };
        }
    }
    best.unwrap_or(GpuKind::Unknown)
}

#[cfg(windows)]
fn windows_gpu() -> GpuKind {
    // Vendor user-mode driver DLLs installed into System32 are a cheap, process-free signal.
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let sys32 = std::path::Path::new(&root).join("System32");
    let has = |f: &str| sys32.join(f).exists();
    if has("nvapi64.dll") || has("nvcuda.dll") {
        GpuKind::Nvidia
    } else if has("amdxc64.dll") || has("atiadlxx.dll") {
        GpuKind::Amd
    } else if has("igdumdim64.dll") || has("igc64.dll") {
        GpuKind::Intel
    } else {
        GpuKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_reports_plausible_values() {
        let hw = detect_hardware();
        assert!(hw.total_ram > 0);
        assert!(hw.available_ram <= hw.total_ram || hw.available_ram > 0);
        assert!(hw.cpu_cores >= 1);
    }

    #[test]
    fn pci_vendor_mapping() {
        assert_eq!(gpu_from_pci_vendor(0x10de), Some(GpuKind::Nvidia));
        assert_eq!(gpu_from_pci_vendor(0x1002), Some(GpuKind::Amd));
        assert_eq!(gpu_from_pci_vendor(0x8086), Some(GpuKind::Intel));
        assert_eq!(gpu_from_pci_vendor(0x1234), None);
    }
}
