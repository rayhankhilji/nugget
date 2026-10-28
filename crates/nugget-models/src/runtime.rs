//! Signed inference-runtime entries (llama.cpp `llama-server` builds) and hardware-aware choice.
//!
//! A [`RuntimeEntry`] pins one upstream release archive for one OS, CPU architecture and
//! accelerator: exact release tag, URL under that tag, size, SHA-256, archive format and the path
//! of the server binary inside the archive. Entries live in the signed manifest next to the models
//! (`Manifest::runtimes`) and are validated by [`crate::verify_manifest`] like model entries.
//! [`crate::ModelStore::install_runtime`] downloads, verifies and safely extracts them.

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::hardware::{GpuKind, Hardware};
use crate::manifest::{Manifest, is_sha256_hex};

/// Largest runtime archive a manifest may declare (CUDA builds are a few hundred MB).
pub const MAX_RUNTIME_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Compute backend a runtime build uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAccel {
    Cpu,
    /// Apple GPUs (unified memory).
    Metal,
    /// NVIDIA GPUs.
    Cuda,
    /// Cross-vendor GPU backend (NVIDIA, AMD, Intel) through the system Vulkan driver.
    Vulkan,
}

impl RuntimeAccel {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeAccel::Cpu => "cpu",
            RuntimeAccel::Metal => "metal",
            RuntimeAccel::Cuda => "cuda",
            RuntimeAccel::Vulkan => "vulkan",
        }
    }

    /// Whether this backend offloads to a GPU.
    pub fn is_gpu(self) -> bool {
        !matches!(self, RuntimeAccel::Cpu)
    }
}

/// Container format of a runtime release asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Zip,
    #[serde(rename = "tar.gz")]
    TarGz,
}

/// One downloadable `llama-server` build, pinned to an exact release tag and content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEntry {
    /// Stable id, e.g. `llama-server-linux-x86_64-cpu`. `[a-z0-9._-]`, 1–64 chars.
    pub id: String,
    /// Upstream release tag, e.g. `b11940`. Never a moving ref such as `latest` or `master`.
    pub version: String,
    /// Target OS as in `std::env::consts::OS`: `macos`, `linux` or `windows`.
    pub os: String,
    /// Target CPU as in `std::env::consts::ARCH`: `x86_64` or `aarch64`.
    pub arch: String,
    pub accel: RuntimeAccel,
    /// Release asset URL. Must contain the tag as a whole path segment.
    pub url: String,
    /// Alternative URLs serving the identical bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    /// Exact archive size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the archive.
    pub sha256: String,
    pub archive: ArchiveFormat,
    /// Path of the server executable inside the archive, `/`-separated, relative.
    pub binary: String,
    /// SPDX license expression of the archive contents, e.g. `MIT`.
    pub license: String,
}

impl RuntimeEntry {
    /// All download sources in preference order: `url`, then `mirrors`.
    pub fn sources(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.url.as_str()).chain(self.mirrors.iter().map(String::as_str))
    }

    /// Whether this entry targets the given OS and architecture.
    pub fn matches_target(&self, os: &str, arch: &str) -> bool {
        self.os == os && self.arch == arch
    }

    /// Validate an entry in isolation. [`crate::verify_manifest`] calls this for every entry, and
    /// the store calls it again before installing.
    pub fn validate(&self) -> Result<()> {
        let id = &self.id;
        let bad = |what: String| Err(Error::Invalid(format!("runtime `{id}`: {what}")));
        if !is_valid_id(id) {
            return bad("id must be 1-64 chars of [a-z0-9._-] and not start with '.'".into());
        }
        if !is_pinned_tag(&self.version) {
            return bad(format!(
                "version `{}` is not a pinned release tag (moving refs like `latest` are not allowed)",
                self.version
            ));
        }
        if !matches!(self.os.as_str(), "macos" | "linux" | "windows") {
            return bad(format!("unsupported os `{}`", self.os));
        }
        if !matches!(self.arch.as_str(), "x86_64" | "aarch64") {
            return bad(format!("unsupported arch `{}`", self.arch));
        }
        if self.size == 0 || self.size > MAX_RUNTIME_ARCHIVE_BYTES {
            return bad(format!(
                "size {} is outside 1..={MAX_RUNTIME_ARCHIVE_BYTES}",
                self.size
            ));
        }
        if !is_sha256_hex(&self.sha256) {
            return bad("sha256 must be 64 lowercase hex chars".into());
        }
        if !is_safe_relative_path(&self.binary) {
            return bad(format!("unsafe binary path `{}`", self.binary));
        }
        if self.license.trim().is_empty() {
            return bad("license is empty".into());
        }
        for src in self.sources() {
            validate_runtime_url(src, &self.version).or_else(&bad)?;
        }
        Ok(())
    }
}

impl Manifest {
    /// Look up a runtime entry by id.
    pub fn runtime(&self, id: &str) -> Option<&RuntimeEntry> {
        self.runtimes.iter().find(|r| r.id == id)
    }
}

/// `std::env::consts::OS` of this build.
pub fn current_os() -> &'static str {
    std::env::consts::OS
}

/// `std::env::consts::ARCH` of this build.
pub fn current_arch() -> &'static str {
    std::env::consts::ARCH
}

/// Accelerators worth trying on `gpu`, best first. CPU is always last, as the fallback.
///
/// Vulkan is preferred for discrete NVIDIA and AMD GPUs (CUDA first for NVIDIA when a CUDA build
/// is listed). Intel and unknown GPUs use the CPU build: integrated GPUs rarely beat the CPU build
/// and an unknown driver is a common source of start-up crashes.
pub fn accel_preference(os: &str, arch: &str, gpu: GpuKind) -> Vec<RuntimeAccel> {
    let mut out = Vec::new();
    if os == "macos" && arch == "aarch64" {
        out.push(RuntimeAccel::Metal);
    }
    match gpu {
        GpuKind::Nvidia => out.extend([RuntimeAccel::Cuda, RuntimeAccel::Vulkan]),
        GpuKind::Amd => out.push(RuntimeAccel::Vulkan),
        GpuKind::AppleSilicon | GpuKind::Intel | GpuKind::Unknown => {}
    }
    out.push(RuntimeAccel::Cpu);
    out.dedup();
    out
}

/// Runtime entries usable on `os`/`arch` with `gpu`, best first (see [`accel_preference`]).
pub fn runtime_candidates_for<'a>(
    manifest: &'a Manifest,
    os: &str,
    arch: &str,
    gpu: GpuKind,
) -> Vec<&'a RuntimeEntry> {
    accel_preference(os, arch, gpu)
        .into_iter()
        .flat_map(|accel| {
            manifest
                .runtimes
                .iter()
                .filter(move |r| r.matches_target(os, arch) && r.accel == accel)
        })
        .collect()
}

/// The best runtime entry for this machine, if the manifest has one for its OS and CPU.
pub fn select_runtime<'a>(manifest: &'a Manifest, hw: &Hardware) -> Option<&'a RuntimeEntry> {
    runtime_candidates_for(manifest, current_os(), current_arch(), hw.gpu)
        .into_iter()
        .next()
}

pub(crate) fn validate_runtimes(m: &Manifest) -> Result<()> {
    let mut ids = std::collections::HashSet::new();
    for r in &m.runtimes {
        r.validate()?;
        if !ids.insert(r.id.as_str()) {
            return Err(Error::Invalid(format!("duplicate runtime id `{}`", r.id)));
        }
    }
    Ok(())
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && !id.starts_with('.')
}

/// A release tag: 1–64 chars of `[A-Za-z0-9._-]`, containing a digit, and not a moving name.
fn is_pinned_tag(tag: &str) -> bool {
    const MOVING: &[&str] = &[
        "latest", "master", "main", "head", "nightly", "stable", "dev",
    ];
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && tag.bytes().any(|b| b.is_ascii_digit())
        && !tag.starts_with(['.', '-'])
        && !MOVING.iter().any(|m| tag.eq_ignore_ascii_case(m))
}

/// A `/`-separated relative path with only normal components (no `..`, `.`, root, drive, `\`).
pub(crate) fn is_safe_relative_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && !p.starts_with('/')
        && !p.contains(['\\', ':', '\0'])
        && !p.chars().any(char::is_control)
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// `https` (plain `http` only to loopback, for tests), no credentials, and the pinned tag as a
/// whole path segment so the URL can never name a moving release.
fn validate_runtime_url(raw: &str, tag: &str) -> std::result::Result<(), String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid url `{raw}`: {e}"))?;
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(format!("url `{raw}` has no host")),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        other => {
            return Err(format!(
                "url `{raw}`: scheme `{other}` not allowed (https only)"
            ));
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("url `{raw}` must not embed credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("url `{raw}` must not have a query or fragment"));
    }
    let pinned = url
        .path_segments()
        .is_some_and(|mut segs| segs.any(|s| s == tag));
    if !pinned {
        return Err(format!(
            "url `{raw}` does not reference pinned release tag `{tag}`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> RuntimeEntry {
        RuntimeEntry {
            id: "llama-server-linux-x86_64-cpu".into(),
            version: "b100".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            accel: RuntimeAccel::Cpu,
            url: "https://github.com/o/r/releases/download/b100/llama-b100-bin-ubuntu-x64.tar.gz"
                .into(),
            mirrors: vec![],
            size: 10,
            sha256: "a".repeat(64),
            archive: ArchiveFormat::TarGz,
            binary: "llama-b100/llama-server".into(),
            license: "MIT".into(),
        }
    }

    #[test]
    fn valid_entry_passes() {
        entry().validate().unwrap();
    }

    type Mutation = Box<dyn Fn(&mut RuntimeEntry)>;

    #[test]
    fn rejects_bad_fields() {
        let cases: Vec<Mutation> = vec![
            Box::new(|e| e.version = "latest".into()),
            Box::new(|e| e.version = "master".into()),
            Box::new(|e| e.version = "b1/2".into()),
            Box::new(|e| e.url = e.url.replace("b100/", "latest/")),
            Box::new(|e| e.url = e.url.replace("https", "http")),
            Box::new(|e| e.url = format!("{}?x=1", e.url)),
            Box::new(|e| e.url = "https://u:p@github.com/b100/x.tar.gz".into()),
            Box::new(|e| e.sha256 = "A".repeat(64)),
            Box::new(|e| e.sha256 = "a".repeat(63)),
            Box::new(|e| e.size = 0),
            Box::new(|e| e.size = MAX_RUNTIME_ARCHIVE_BYTES + 1),
            Box::new(|e| e.binary = "../llama-server".into()),
            Box::new(|e| e.binary = "/usr/bin/llama-server".into()),
            Box::new(|e| e.binary = "a\\b".into()),
            Box::new(|e| e.binary = "C:/x".into()),
            Box::new(|e| e.binary = "a//b".into()),
            Box::new(|e| e.os = "plan9".into()),
            Box::new(|e| e.arch = "mips".into()),
            Box::new(|e| e.id = "Upper".into()),
            Box::new(|e| e.license = " ".into()),
            Box::new(|e| e.mirrors = vec!["https://mirror.example/other/x.tar.gz".into()]),
        ];
        for (i, f) in cases.iter().enumerate() {
            let mut e = entry();
            f(&mut e);
            assert!(e.validate().is_err(), "case {i} should fail: {e:?}");
        }
    }

    #[test]
    fn loopback_http_allowed_for_tests() {
        let mut e = entry();
        e.url = "http://127.0.0.1:1234/b100/rt.tar.gz".into();
        e.validate().unwrap();
    }

    #[test]
    fn accel_order() {
        use RuntimeAccel::*;
        assert_eq!(
            accel_preference("macos", "aarch64", GpuKind::AppleSilicon),
            vec![Metal, Cpu]
        );
        assert_eq!(
            accel_preference("linux", "x86_64", GpuKind::Nvidia),
            vec![Cuda, Vulkan, Cpu]
        );
        assert_eq!(
            accel_preference("windows", "x86_64", GpuKind::Amd),
            vec![Vulkan, Cpu]
        );
        assert_eq!(
            accel_preference("linux", "x86_64", GpuKind::Intel),
            vec![Cpu]
        );
        assert_eq!(
            accel_preference("linux", "x86_64", GpuKind::Unknown),
            vec![Cpu]
        );
    }
}
