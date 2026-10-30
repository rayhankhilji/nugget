//! The llama-server argument builder, per role and hardware.

use std::ffi::OsString;
use std::path::PathBuf;

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

fn plan(role: RuntimeRole, accel: RuntimeAccel, hw: &Hardware, t: &Tuning) -> LaunchPlan {
    LaunchPlan::new(
        role,
        PathBuf::from("/m/blob"),
        "model-id",
        4242,
        accel,
        hw,
        t,
    )
    .unwrap()
}

fn strs(a: &[OsString]) -> Vec<String> {
    a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
}

/// Value following `flag`, if present.
fn val(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

#[test]
fn every_plan_binds_loopback_and_never_carries_a_key() {
    for role in [
        RuntimeRole::Chat,
        RuntimeRole::Embedding,
        RuntimeRole::Rerank,
        RuntimeRole::Draft,
    ] {
        for accel in [
            RuntimeAccel::Cpu,
            RuntimeAccel::Metal,
            RuntimeAccel::Vulkan,
            RuntimeAccel::Cuda,
        ] {
            let a = strs(&plan(role, accel, &hw(16, 8), &Tuning::default()).args());
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
}

#[test]
fn a_socket_plan_listens_on_the_socket_only() {
    let mut p = plan(
        RuntimeRole::Chat,
        RuntimeAccel::Cpu,
        &hw(16, 8),
        &Tuning::default(),
    );
    p.socket = Some(PathBuf::from("/run/nugget/chat.sock"));
    let a = strs(&p.args());
    assert_eq!(val(&a, "--host").as_deref(), Some("/run/nugget/chat.sock"));
    assert_eq!(a.iter().filter(|x| *x == "--host").count(), 1);
    assert!(!a.contains(&"--port".to_string()));
}

#[test]
fn chat_context_and_slots_scale_with_ram_and_cores() {
    let t = Tuning::default();
    let small = plan(RuntimeRole::Chat, RuntimeAccel::Cpu, &hw(8, 4), &t);
    assert_eq!((small.ctx_per_slot, small.parallel), (4096, 1));
    let mid = plan(RuntimeRole::Chat, RuntimeAccel::Cpu, &hw(16, 8), &t);
    assert_eq!((mid.ctx_per_slot, mid.parallel), (8192, 2));
    let big = plan(RuntimeRole::Chat, RuntimeAccel::Metal, &hw(32, 12), &t);
    assert_eq!((big.ctx_per_slot, big.parallel), (16384, 2));
    // --ctx-size is the total across slots.
    let a = strs(&big.args());
    assert_eq!(val(&a, "--ctx-size").as_deref(), Some("32768"));
    assert_eq!(val(&a, "--parallel").as_deref(), Some("2"));
    assert!(!a.contains(&"--embedding".to_string()));
    assert!(val(&a, "--pooling").is_none());
    assert!(val(&a, "--ubatch-size").is_none());
    // Few cores: one slot even with lots of RAM.
    assert_eq!(
        plan(RuntimeRole::Chat, RuntimeAccel::Cpu, &hw(64, 4), &t).parallel,
        1
    );
}

#[test]
fn gpu_layers_follow_the_build() {
    let t = Tuning::default();
    let ngl = |accel| {
        val(
            &strs(&plan(RuntimeRole::Chat, accel, &hw(16, 8), &t).args()),
            "--n-gpu-layers",
        )
        .unwrap()
    };
    assert_eq!(ngl(RuntimeAccel::Cpu), "0");
    assert_eq!(ngl(RuntimeAccel::Metal), "all");
    assert_eq!(ngl(RuntimeAccel::Vulkan), "auto");
    assert_eq!(ngl(RuntimeAccel::Cuda), "auto");
    let t = Tuning {
        gpu_layers: Some(GpuLayers::Count(20)),
        ..Tuning::default()
    };
    let a = strs(&plan(RuntimeRole::Chat, RuntimeAccel::Vulkan, &hw(16, 8), &t).args());
    assert_eq!(val(&a, "--n-gpu-layers").as_deref(), Some("20"));
}

#[test]
fn embedding_role_enables_embeddings_and_sizes_batches() {
    let p = plan(
        RuntimeRole::Embedding,
        RuntimeAccel::Cpu,
        &hw(16, 8),
        &Tuning::default(),
    );
    let a = strs(&p.args());
    assert!(a.contains(&"--embedding".to_string()));
    // Default: the GGUF's own pooling type.
    assert!(val(&a, "--pooling").is_none());
    // A whole input must fit one micro-batch for non-causal embedding models.
    assert_eq!(val(&a, "--batch-size").as_deref(), Some("2048"));
    assert_eq!(val(&a, "--ubatch-size").as_deref(), Some("2048"));
    assert_eq!(val(&a, "--parallel").as_deref(), Some("4"));
    assert_eq!(val(&a, "--ctx-size").as_deref(), Some("8192"));
    assert_eq!(
        plan(
            RuntimeRole::Embedding,
            RuntimeAccel::Cpu,
            &hw(4, 2),
            &Tuning::default()
        )
        .parallel,
        1
    );

    let t = Tuning {
        pooling: Some(Pooling::Mean),
        ctx_per_slot: Some(8192),
        parallel: Some(2),
        threads: Some(3),
        ..Tuning::default()
    };
    let a = strs(&plan(RuntimeRole::Embedding, RuntimeAccel::Cpu, &hw(16, 8), &t).args());
    assert_eq!(val(&a, "--pooling").as_deref(), Some("mean"));
    assert_eq!(val(&a, "--ubatch-size").as_deref(), Some("8192"));
    assert_eq!(val(&a, "--ctx-size").as_deref(), Some("16384"));
    assert_eq!(val(&a, "--threads").as_deref(), Some("3"));
    // Pooling is ignored for chat.
    let a = strs(&plan(RuntimeRole::Chat, RuntimeAccel::Cpu, &hw(16, 8), &t).args());
    assert!(val(&a, "--pooling").is_none());
}

#[test]
fn rerank_role_enables_reranking_with_rank_pooling_and_whole_ubatches() {
    let p = plan(
        RuntimeRole::Rerank,
        RuntimeAccel::Cpu,
        &hw(16, 8),
        &Tuning::default(),
    );
    assert_eq!(p.pooling, Pooling::Rank);
    let a = strs(&p.args());
    assert!(a.contains(&"--reranking".to_string()));
    // `--reranking` sets embedding mode and rank pooling itself.
    assert!(!a.contains(&"--embedding".to_string()));
    assert!(val(&a, "--pooling").is_none());
    // A query and passage must fit one micro-batch.
    assert_eq!(val(&a, "--batch-size").as_deref(), Some("2048"));
    assert_eq!(val(&a, "--ubatch-size").as_deref(), Some("2048"));
    assert_eq!(val(&a, "--parallel").as_deref(), Some("4"));

    // A pooling override cannot break rank pooling; other tuning applies.
    let t = Tuning {
        pooling: Some(Pooling::Mean),
        ctx_per_slot: Some(4096),
        parallel: Some(1),
        ..Tuning::default()
    };
    let p = plan(RuntimeRole::Rerank, RuntimeAccel::Cpu, &hw(16, 8), &t);
    assert_eq!(p.pooling, Pooling::Rank);
    let a = strs(&p.args());
    assert!(val(&a, "--pooling").is_none());
    assert_eq!(val(&a, "--ubatch-size").as_deref(), Some("4096"));
    assert_eq!(val(&a, "--ctx-size").as_deref(), Some("4096"));
    // Neither chat, embedding nor draft plans rerank.
    for role in [
        RuntimeRole::Chat,
        RuntimeRole::Embedding,
        RuntimeRole::Draft,
    ] {
        let a = strs(&plan(role, RuntimeAccel::Cpu, &hw(16, 8), &Tuning::default()).args());
        assert!(!a.contains(&"--reranking".to_string()));
    }
}

#[test]
fn draft_role_generates_from_one_slot_with_room_for_a_thread() {
    for ram in [8, 16, 64] {
        let p = plan(
            RuntimeRole::Draft,
            RuntimeAccel::Cpu,
            &hw(ram, 16),
            &Tuning::default(),
        );
        assert_eq!(p.pooling, Pooling::Model);
        let a = strs(&p.args());
        // A generating server: no embedding or rerank mode, no whole-input batches.
        for f in ["--embedding", "--reranking", "--pooling", "--batch-size"] {
            assert!(!a.contains(&f.to_string()), "{f}");
        }
        // One slot, so the prefix a read-ahead cached is the one the trigger reuses.
        assert_eq!(val(&a, "--parallel").as_deref(), Some("1"));
        assert_eq!(val(&a, "--ctx-size").as_deref(), Some("8192"));
    }
    // Its own tuning applies.
    let t = Tuning {
        ctx_per_slot: Some(4096),
        threads: Some(2),
        ..Tuning::default()
    };
    let a = strs(&plan(RuntimeRole::Draft, RuntimeAccel::Metal, &hw(16, 8), &t).args());
    assert_eq!(val(&a, "--ctx-size").as_deref(), Some("4096"));
    assert_eq!(val(&a, "--threads").as_deref(), Some("2"));
    assert_eq!(val(&a, "--n-gpu-layers").as_deref(), Some("all"));
}

#[test]
fn extra_args_pass_through_but_dangerous_flags_are_refused() {
    let t = Tuning {
        extra_args: vec!["--flash-attn".into(), "on".into()],
        ..Tuning::default()
    };
    let a = strs(&plan(RuntimeRole::Chat, RuntimeAccel::Cpu, &hw(16, 8), &t).args());
    assert_eq!(val(&a, "--flash-attn").as_deref(), Some("on"));
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
        let err = LaunchPlan::new(
            RuntimeRole::Chat,
            PathBuf::from("/m"),
            "id",
            1,
            RuntimeAccel::Cpu,
            &hw(16, 8),
            &t,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{bad}");
    }
}
