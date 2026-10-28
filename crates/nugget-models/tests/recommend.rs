mod common;

use common::{REV, entry, signed};
use nugget_core::capability::Tier;
use nugget_models::{GIB, GpuKind, Hardware, default_manifest, recommend};

fn hw(gib_tenths: u64) -> Hardware {
    Hardware {
        total_ram: gib_tenths * GIB / 10,
        available_ram: gib_tenths * GIB / 20,
        cpu_cores: 8,
        gpu: GpuKind::Unknown,
    }
}

fn pick(m: &nugget_models::Manifest, ram_tenths: u64, tier: Tier) -> Option<String> {
    recommend(m, &hw(ram_tenths), tier).map(|e| e.id.clone())
}

#[test]
fn recommends_by_ram_and_tier_from_shipped_manifest() {
    let m = default_manifest().unwrap();
    let s = |r| pick(&m, r, Tier::Synthesis);

    assert_eq!(s(40), None, "4 GiB: no local chat model fits");
    assert_eq!(s(80).as_deref(), Some("qwen3-4b-q4km"));
    assert_eq!(s(120).as_deref(), Some("qwen3-4b-q4km"));
    // A "16 GB" machine reports less after firmware/GPU reservations.
    assert_eq!(s(155).as_deref(), Some("qwen3-8b-q4km"));
    assert_eq!(s(160).as_deref(), Some("qwen3-8b-q4km"));
    assert_eq!(s(640).as_deref(), Some("qwen3-8b-q4km"));

    // Triage stays on the small model however much RAM there is.
    assert_eq!(
        pick(&m, 640, Tier::Triage).as_deref(),
        Some("qwen3-4b-q4km")
    );
    assert_eq!(pick(&m, 80, Tier::Agentic), None);
    assert_eq!(
        pick(&m, 160, Tier::Agentic).as_deref(),
        Some("qwen3-8b-q4km")
    );
    assert_eq!(
        pick(&m, 40, Tier::Embedding).as_deref(),
        Some("nomic-embed-text-v1.5-q8")
    );
    assert_eq!(pick(&m, 20, Tier::Embedding), None);
}

#[test]
fn skips_entries_needing_a_newer_app_and_breaks_ties_by_size() {
    let url = |n: &str| format!("https://example.org/{n}/resolve/{REV}/{n}.gguf");
    let mut future = entry("future", &url("future"), b"0123456789");
    future.min_ram_gb = 2;
    future.min_app_version = "99.0.0".into();
    let mut small = entry("small", &url("small"), b"abc");
    small.min_ram_gb = 2;
    let mut big = entry("big", &url("big"), b"abcdef");
    big.min_ram_gb = 2;
    let m = signed(vec![future, small, big]);
    assert_eq!(pick(&m, 80, Tier::Triage).as_deref(), Some("big"));
    assert_eq!(pick(&m, 80, Tier::Synthesis), None);
}
