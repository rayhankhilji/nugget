//! Runtime installation from a signed manifest (served locally) through `Runtime::ensure`.
mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{env, serve, sha256_hex};
use nugget_models::{
    ArchiveFormat, GpuKind, Hardware, Manifest, Phase, RUNTIME_EGRESS_FEATURE, RuntimeAccel,
    RuntimeEntry, SigningKey, current_arch, current_os, sign_manifest, verify_manifest,
};
use nugget_runtime::{Runtime, RuntimeRole, RuntimeState};

const TAG: &str = "b1";

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-b1/llama-server"
    }
}

/// A release archive containing `server` as the binary (mode 0644: the installer must add +x).
fn archive(server: &[u8]) -> (Vec<u8>, ArchiveFormat) {
    if cfg!(windows) {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("llama-server.exe", o).unwrap();
        w.write_all(server).unwrap();
        w.start_file("LICENSE", o).unwrap();
        w.write_all(b"MIT").unwrap();
        (w.finish().unwrap().into_inner(), ArchiveFormat::Zip)
    } else {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut b = tar::Builder::new(enc);
        let mut add = |name: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, data).unwrap();
        };
        add("llama-b1/llama-server", server);
        add("llama-b1/libggml.so.0.1", b"lib");
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        b.append_link(&mut h, "llama-b1/libggml.so", "libggml.so.0.1")
            .unwrap();
        let enc = b.into_inner().unwrap();
        (enc.finish().unwrap(), ArchiveFormat::TarGz)
    }
}

async fn entry(
    server: &wiremock::MockServer,
    id: &str,
    accel: RuntimeAccel,
    bytes: Vec<u8>,
    format: ArchiveFormat,
) -> RuntimeEntry {
    let ext = if format == ArchiveFormat::Zip {
        "zip"
    } else {
        "tar.gz"
    };
    let url = serve(
        server,
        &format!("/ggml-org/llama.cpp/releases/download/{TAG}/{id}.{ext}"),
        bytes.clone(),
    )
    .await;
    RuntimeEntry {
        id: id.into(),
        version: TAG.into(),
        os: current_os().into(),
        arch: current_arch().into(),
        accel,
        url,
        mirrors: vec![],
        size: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
        archive: format,
        binary: binary_name().into(),
        license: "MIT".into(),
    }
}

/// Round-trip through a signed manifest, as production code must.
fn signed(runtimes: Vec<RuntimeEntry>) -> Manifest {
    let key = SigningKey::from_bytes(&[3u8; 32]);
    let bytes = serde_json::to_vec(&Manifest {
        version: 1,
        models: vec![],
        runtimes,
    })
    .unwrap();
    let sig = sign_manifest(&bytes, &key);
    verify_manifest(&bytes, sig.as_bytes(), &[key.verifying_key()]).unwrap()
}

fn runtime(e: &common::Env, runtimes: Vec<RuntimeEntry>, gpu: GpuKind) -> Runtime {
    runtime_with_sink(e, runtimes, gpu, e.sink.clone())
}

fn runtime_with_sink(
    e: &common::Env,
    runtimes: Vec<RuntimeEntry>,
    gpu: GpuKind,
    sink: Arc<common::VecSink>,
) -> Runtime {
    Runtime::new(&e.paths, e.store.clone(), sink)
        .unwrap()
        .with_manifest(signed(runtimes))
        .with_hardware(Hardware {
            gpu,
            ..common::hw()
        })
        .with_options(common::opts())
}

/// The CPU build: an archive holding the fake server.
async fn cpu_entry(e: &common::Env) -> RuntimeEntry {
    let (bytes, format) = archive(&std::fs::read(common::fake_bin()).unwrap());
    entry(&e.server, "llama-cpu", RuntimeAccel::Cpu, bytes, format).await
}

#[tokio::test]
async fn ensure_installs_the_runtime_from_the_manifest_then_runs_it() {
    let e = env().await;
    let rt_entry = cpu_entry(&e).await;
    let egress = Arc::new(common::VecSink::default());
    let rt = runtime_with_sink(&e, vec![rt_entry.clone()], GpuKind::Unknown, egress.clone());
    assert_eq!(rt.runtime_candidates().len(), 1);

    let progress = Arc::new(Mutex::new(Vec::new()));
    let p = progress.clone();
    let cfg = rt
        .ensure_with_progress(RuntimeRole::Embedding, &e.embed, move |x| {
            p.lock().unwrap().push(x.phase)
        })
        .await
        .unwrap();
    assert_eq!(cfg.model, "embed-model");
    let phases = progress.lock().unwrap().clone();
    assert!(phases.contains(&Phase::Downloading), "{phases:?}");
    assert_eq!(phases.last(), Some(&Phase::Installed));

    let d = rt.details(RuntimeRole::Embedding).unwrap();
    assert_eq!(
        (d.info.state, d.accel),
        (RuntimeState::Ready, Some(RuntimeAccel::Cpu))
    );
    let bin = d.binary.unwrap();
    assert!(bin.starts_with(e.store.runtimes_dir()), "{}", bin.display());
    assert_eq!(
        e.store.runtime_binary(&rt_entry).unwrap(),
        Some(bin.clone())
    );
    e.store.verify_runtime(&rt_entry).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&bin).unwrap().permissions().mode() & 0o111,
            0o111
        );
        let link = bin.parent().unwrap().join("libggml.so");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    // Egress for the runtime download went to the runtime's sink, tagged as such.
    {
        let ev = egress.0.lock().unwrap();
        assert!(!ev.is_empty());
        assert!(ev.iter().all(|x| x.feature == RUNTIME_EGRESS_FEATURE));
    }

    // Second role: no second download.
    let before = e.server.received_requests().await.unwrap().len();
    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    assert_eq!(e.server.received_requests().await.unwrap().len(), before);
    rt.shutdown().await;
}

#[tokio::test]
async fn tampered_or_missing_runtime_builds_are_refused() {
    let e = env().await;
    let mut rt_entry = cpu_entry(&e).await;
    rt_entry.sha256 = sha256_hex(b"what the manifest promised");
    let rt = runtime(&e, vec![rt_entry.clone()], GpuKind::Unknown);
    let err = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap_err();
    assert!(matches!(err, nugget_core::Error::Integrity(_)), "{err}");
    assert!(!e.store.runtime_dir(&rt_entry).unwrap().exists());
    assert_eq!(rt.status()[0].state, RuntimeState::Failed);

    let rt = runtime(&e, vec![], GpuKind::Unknown);
    let err = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap_err();
    assert!(matches!(err, nugget_core::Error::Unsupported(_)), "{err}");
}

/// A GPU build that cannot start falls back to the CPU build.
#[cfg(unix)]
#[tokio::test]
async fn failing_gpu_build_falls_back_to_cpu() {
    let e = env().await;
    let (vk_bytes, f) = archive(b"#!/bin/sh\necho 'ggml_vulkan: no devices found' >&2\nexit 1\n");
    let vk = entry(&e.server, "llama-vulkan", RuntimeAccel::Vulkan, vk_bytes, f).await;
    let rt = runtime(&e, vec![cpu_entry(&e).await, vk], GpuKind::Amd);
    let accels: Vec<_> = rt.runtime_candidates().iter().map(|r| r.accel).collect();
    assert_eq!(accels, vec![RuntimeAccel::Vulkan, RuntimeAccel::Cpu]);

    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(
        (d.info.state, d.accel),
        (RuntimeState::Ready, Some(RuntimeAccel::Cpu))
    );
    let log = std::fs::read_to_string(rt.log_file(RuntimeRole::Chat)).unwrap();
    assert!(log.contains("no devices found"), "{log}");
    rt.shutdown().await;
}

#[tokio::test]
async fn both_roles_starting_at_once_share_one_install() {
    let e = env().await;
    let rt = runtime(&e, vec![cpu_entry(&e).await], GpuKind::Unknown);
    let (a, b) = tokio::join!(
        rt.ensure(RuntimeRole::Chat, &e.chat),
        rt.ensure(RuntimeRole::Embedding, &e.embed)
    );
    a.unwrap();
    b.unwrap();
    let requests = e.server.received_requests().await.unwrap();
    let gets = requests
        .iter()
        .filter(|r| r.url.path().contains("/releases/download/"))
        .count();
    assert!(gets <= 2, "one download (probe + body at most): {gets}");
    rt.shutdown().await;
}
