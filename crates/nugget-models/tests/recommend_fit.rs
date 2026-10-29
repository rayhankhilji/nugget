//! The explained recommendation against the shipped manifest, for typical machines.
//!
//! Concrete ids from `manifests/default.json` are asserted on purpose: a manifest change that
//! shifts what a common machine gets recommended must be noticed here.

use nugget_core::capability::Tier;
use nugget_models::{
    FitReason, GIB, GpuKind, Hardware, ModelFit, default_manifest, explain, recommend,
    required_bytes,
};

const CHAT_SMALL: &str = "qwen3-4b-q4km";
const CHAT_LARGE: &str = "qwen3-8b-q4km";
const EMBED: &str = "nomic-embed-text-v1.5-q8";
const RERANK: &str = "bge-reranker-v2-m3-q4km";

/// A machine sold as `gb` GB: the OS reports a little less (here 95%).
fn machine(gb: u64, gpu: GpuKind) -> Hardware {
    Hardware {
        total_ram: gb * GIB * 95 / 100,
        available_ram: gb * GIB / 2,
        cpu_cores: 8,
        gpu,
    }
}

fn recommended(fits: &[ModelFit]) -> Vec<&str> {
    fits.iter()
        .filter(|f| f.recommended)
        .map(|f| f.id.as_str())
        .collect()
}

fn get<'a>(fits: &'a [ModelFit], id: &str) -> &'a ModelFit {
    fits.iter().find(|f| f.id == id).expect(id)
}

/// The most capable chat model recommended (the one used for synthesis).
fn chat_pick(fits: &[ModelFit]) -> Option<&str> {
    fits.iter()
        .filter(|f| f.recommended && f.tiers.iter().any(|t| t.is_chat()))
        .max_by_key(|f| (f.min_ram_gb, f.size_bytes))
        .map(|f| f.id.as_str())
}

fn embed_pick(fits: &[ModelFit]) -> Option<&str> {
    fits.iter()
        .find(|f| f.recommended && f.tiers.contains(&Tier::Embedding))
        .map(|f| f.id.as_str())
}

#[test]
fn table_of_machines_against_the_shipped_manifest() {
    let m = default_manifest().unwrap();
    // (GB, gpu, chat pick, all recommended, accel)
    let gpus = [
        (GpuKind::Nvidia, true),
        (GpuKind::AppleSilicon, true),
        (GpuKind::Unknown, false),
    ];
    let table: [(u64, &str, &[&str]); 3] = [
        (8, CHAT_SMALL, &[CHAT_SMALL, EMBED]),
        (16, CHAT_LARGE, &[CHAT_SMALL, CHAT_LARGE, EMBED]),
        (32, CHAT_LARGE, &[CHAT_SMALL, CHAT_LARGE, EMBED]),
    ];
    for (gb, chat, all) in table {
        for (gpu, accelerated) in gpus {
            let hw = machine(gb, gpu);
            let fits = explain(&m, &hw);
            let ctx = format!("{gb} GB, {gpu:?}");
            assert_eq!(fits.len(), m.models.len(), "{ctx}");
            assert_eq!(chat_pick(&fits), Some(chat), "{ctx}");
            assert_eq!(embed_pick(&fits), Some(EMBED), "{ctx}");
            assert_eq!(recommended(&fits), all, "{ctx}");

            // Same answer as the daemon's union of `recommend` over routed tiers.
            for f in &fits {
                let union = Tier::ROUTED
                    .iter()
                    .any(|t| recommend(&m, &hw, *t).is_some_and(|e| e.id == f.id));
                assert_eq!(f.recommended, union, "{ctx}: {}", f.id);
            }

            for f in &fits {
                assert_eq!(f.machine_ram_gb as u64, gb, "{ctx}");
                assert_eq!(f.gpu, gpu);
                assert!(f.disk_needed_bytes >= f.size_bytes, "{ctx}: {}", f.id);
                assert_eq!(f.disk_needed_bytes, required_bytes(f.size_bytes, 0));
                if f.recommended {
                    // Fits RAM under the 10% slack rule.
                    assert!(f.fits_ram, "{ctx}: {}", f.id);
                    assert!(
                        u128::from(f.min_ram_gb) * u128::from(GIB)
                            <= u128::from(hw.total_ram) * 10 / 9,
                        "{ctx}: {}",
                        f.id
                    );
                    assert!(f.reasons.contains(&FitReason::RamFitsLargest), "{ctx}");
                    let accel_reason = if accelerated {
                        FitReason::GpuAccelerated
                    } else {
                        FitReason::CpuOnlySlower
                    };
                    assert!(f.reasons.contains(&accel_reason), "{ctx}: {:?}", f.reasons);
                }
            }
            let expected_accel = match gpu {
                GpuKind::AppleSilicon => "metal",
                GpuKind::Nvidia => "cuda",
                _ => "cpu",
            };
            assert_eq!(fits[0].accel, expected_accel, "{ctx}");

            // Rerankers are opt-in, whatever the machine.
            let rr = get(&fits, RERANK);
            assert!(!rr.recommended, "{ctx}");
            assert!(rr.reasons.contains(&FitReason::RerankOptIn), "{ctx}");
        }
    }
}

#[test]
fn eight_gb_cannot_fit_the_large_model() {
    let m = default_manifest().unwrap();
    let fits = explain(&m, &machine(8, GpuKind::Unknown));
    let large = get(&fits, CHAT_LARGE);
    assert!(!large.recommended);
    assert!(!large.fits_ram);
    assert_eq!(large.reasons, [FitReason::RamTooSmall]);
}

#[test]
fn two_gb_gets_nothing() {
    let m = default_manifest().unwrap();
    for gpu in [GpuKind::Nvidia, GpuKind::Unknown] {
        let fits = explain(&m, &machine(2, gpu));
        assert!(recommended(&fits).is_empty(), "{gpu:?}");
        assert!(fits.iter().all(|f| !f.fits_ram), "{gpu:?}");
        assert!(
            fits.iter()
                .all(|f| f.reasons.contains(&FitReason::RamTooSmall))
        );
    }
}

#[test]
fn serializes_the_wire_shape() {
    let m = default_manifest().unwrap();
    let fits = explain(&m, &machine(16, GpuKind::Nvidia));
    let v = serde_json::to_value(get(&fits, CHAT_LARGE)).unwrap();
    assert_eq!(v["id"], CHAT_LARGE);
    assert_eq!(v["recommended"], true);
    assert_eq!(v["min_ram_gb"], 16);
    assert_eq!(v["machine_ram_gb"], 16);
    assert_eq!(v["gpu"], "nvidia");
    assert_eq!(v["accel"], "cuda");
    assert_eq!(v["fits_ram"], true);
    assert_eq!(v["tiers"][0], "synthesis");
    assert_eq!(
        v["reasons"],
        serde_json::json!(["ram_fits_largest", "gpu_accelerated"])
    );
    assert!(v["disk_needed_bytes"].as_u64().unwrap() > v["size_bytes"].as_u64().unwrap());
}
