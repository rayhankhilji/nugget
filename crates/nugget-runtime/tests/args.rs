//! The llama-server argument builder, per role and hardware.

use std::path::PathBuf;

use RuntimeAccel::{Cpu, Cuda, Metal, Vulkan};
use RuntimeRole::{Chat, Draft, Embedding, Rerank};
use nugget_core::Error;
use nugget_models::{GpuKind, Hardware, RuntimeAccel};
use nugget_runtime::{GpuLayers, LaunchPlan, Pooling, RuntimeRole, Tuning};

fn hw(ram_gib: u64, cores: usize) -> Hardware {
    Hardware {
        total_ram: ram_gib << 30,
        available_ram: (ram_gib << 30) / 2,
        cpu_cores: cores,
        gpu: GpuKind::Unknown,
    }
}

fn plan(role: RuntimeRole, accel: RuntimeAccel, ram: u64, cores: usize, t: &Tuning) -> LaunchPlan {
    LaunchPlan::new(
        role,
        PathBuf::from("/m/blob"),
        "model-id",
        4242,
        accel,
        &hw(ram, cores),
        t,
    )
    .unwrap()
}

fn args(p: &LaunchPlan) -> Vec<String> {
    p.args()
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect()
}

/// Value following `flag`, if present.
fn val(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

#[test]
fn every_plan_binds_loopback_and_never_carries_a_key() {
    for role in [Chat, Embedding, Rerank, Draft] {
        for accel in [Cpu, Metal, Vulkan, Cuda] {
            let a = args(&plan(role, accel, 16, 8, &Tuning::default()));
            assert_eq!(val(&a, "--host").as_deref(), Some("127.0.0.1"));
            assert_eq!(val(&a, "--port").as_deref(), Some("4242"));
            assert_eq!(val(&a, "--alias").as_deref(), Some("model-id"));
            assert!(!a.iter().any(|x| x.contains("api-key")));
            for f in ["--no-webui", "--offline", "--no-slots", "--metrics"] {
                assert!(a.contains(&f.to_string()), "{f}");
            }
            // The model path comes last, after any extra arguments.
            assert_eq!(&a[a.len() - 2..], ["--model", "/m/blob"]);
        }
    }
    // A socket plan listens on the socket only.
    let mut p = plan(Chat, Cpu, 16, 8, &Tuning::default());
    p.socket = Some(PathBuf::from("/run/nugget/chat.sock"));
    let a = args(&p);
    assert_eq!(val(&a, "--host").as_deref(), Some("/run/nugget/chat.sock"));
    assert_eq!(a.iter().filter(|x| *x == "--host").count(), 1);
    assert!(!a.contains(&"--port".to_string()));
}

#[test]
fn plans_follow_role_hardware_and_tuning() {
    let d = Tuning::default();
    let ngl20 = Tuning {
        gpu_layers: Some(GpuLayers::Count(20)),
        ..d.clone()
    };
    let mean = Tuning {
        pooling: Some(Pooling::Mean),
        ctx_per_slot: Some(8192),
        parallel: Some(2),
        threads: Some(3),
        ..d.clone()
    };
    let rerank = Tuning {
        pooling: Some(Pooling::Mean),
        ctx_per_slot: Some(4096),
        parallel: Some(1),
        ..d.clone()
    };
    let draft = Tuning {
        ctx_per_slot: Some(4096),
        threads: Some(2),
        ..d.clone()
    };
    // `flag=value`, `flag` (present) or `!flag` (absent). --ctx-size is the total across slots.
    for (role, accel, ram, cores, t, spec) in [
        (Chat, Cpu, 8, 4, &d, "--ctx-size=4096 --parallel=1"),
        (
            Chat,
            Cpu,
            16,
            8,
            &d,
            "--ctx-size=16384 --parallel=2 --n-gpu-layers=0 !--embedding !--reranking !--pooling !--ubatch-size",
        ),
        (
            Chat,
            Metal,
            32,
            12,
            &d,
            "--ctx-size=32768 --parallel=2 --n-gpu-layers=all",
        ),
        // Few cores: one slot even with lots of RAM.
        (Chat, Cpu, 64, 4, &d, "--parallel=1"),
        (Chat, Vulkan, 16, 8, &d, "--n-gpu-layers=auto"),
        (Chat, Cuda, 16, 8, &d, "--n-gpu-layers=auto"),
        (Chat, Vulkan, 16, 8, &ngl20, "--n-gpu-layers=20"),
        (Chat, Cpu, 16, 8, &mean, "!--pooling"),
        // A whole input must fit one micro-batch for non-causal embedding models; default
        // pooling is the GGUF's own.
        (
            Embedding,
            Cpu,
            16,
            8,
            &d,
            "--embedding !--reranking !--pooling --batch-size=2048 --ubatch-size=2048 --parallel=4 --ctx-size=8192",
        ),
        (Embedding, Cpu, 4, 2, &d, "--parallel=1"),
        (
            Embedding,
            Cpu,
            16,
            8,
            &mean,
            "--pooling=mean --ubatch-size=8192 --ctx-size=16384 --threads=3",
        ),
        // `--reranking` sets embedding mode and rank pooling itself; a query and passage must
        // fit one micro-batch; a pooling override cannot break rank pooling.
        (
            Rerank,
            Cpu,
            16,
            8,
            &d,
            "--reranking !--embedding !--pooling --batch-size=2048 --ubatch-size=2048 --parallel=4",
        ),
        (
            Rerank,
            Cpu,
            16,
            8,
            &rerank,
            "!--pooling --ubatch-size=4096 --ctx-size=4096",
        ),
        // A generating server with one slot, so the prefix a read-ahead cached is reused.
        (
            Draft,
            Cpu,
            8,
            16,
            &d,
            "!--embedding !--reranking !--pooling !--batch-size --parallel=1 --ctx-size=8192",
        ),
        (
            Draft,
            Cpu,
            64,
            16,
            &d,
            "!--embedding !--reranking --parallel=1 --ctx-size=8192",
        ),
        (
            Draft,
            Metal,
            16,
            8,
            &draft,
            "--ctx-size=4096 --threads=2 --n-gpu-layers=all",
        ),
    ] {
        let p = plan(role, accel, ram, cores, t);
        let a = args(&p);
        for want in spec.split(' ') {
            let ok = match (want.strip_prefix('!'), want.split_once('=')) {
                (Some(flag), _) => !a.iter().any(|x| x == flag),
                (None, Some((flag, v))) => val(&a, flag).as_deref() == Some(v),
                (None, None) => a.iter().any(|x| x == want),
            };
            assert!(ok, "{role:?} {accel:?} {ram}G/{cores}: {want} in {a:?}");
        }
        let pooling = match role {
            Rerank => Pooling::Rank,
            Draft | Chat => Pooling::Model,
            Embedding => p.pooling,
        };
        assert_eq!(p.pooling, pooling, "{role:?}");
    }
}

#[test]
fn extra_args_pass_through_but_dangerous_flags_are_refused() {
    let t = Tuning {
        extra_args: vec!["--flash-attn".into(), "on".into()],
        ..Tuning::default()
    };
    assert_eq!(
        val(&args(&plan(Chat, Cpu, 16, 8, &t)), "--flash-attn").as_deref(),
        Some("on")
    );
    for bad in [
        "--host",
        "--host=0.0.0.0",
        "--port",
        "--api-key",
        "--api-key-file",
        "--model-url",
        "-hf",
        "--hf-repo",
        "-m",
        "--webui-mcp-proxy",
        "--log-file",
    ] {
        let t = Tuning {
            extra_args: vec![bad.into(), "x".into()],
            ..Tuning::default()
        };
        let err =
            LaunchPlan::new(Chat, PathBuf::from("/m"), "id", 1, Cpu, &hw(16, 8), &t).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{bad}");
    }
}
