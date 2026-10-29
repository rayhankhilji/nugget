//! Runtime manifest entries, verified runtime install, and the hostile-archive extractor.
mod common;

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use common::{TestServer, VecSink, sha256_hex, store, test_key};
use nugget_core::Error;
use nugget_models::archive::{ExtractLimits, extract_archive};
use nugget_models::{
    ArchiveFormat, DEFAULT_MANIFEST_JSON, GpuKind, Hardware, Manifest, RUNTIME_EGRESS_FEATURE,
    RuntimeAccel, RuntimeEntry, default_manifest, runtime_candidates_for, select_runtime,
    sign_manifest, verify_manifest,
};

// ---------------------------------------------------------------------------------------------
// Archive builders (raw headers so hostile names are not "fixed" by the tar builder)
// ---------------------------------------------------------------------------------------------

enum T<'a> {
    File(&'a str, &'a [u8], u32),
    Dir(&'a str),
    Symlink(&'a str, &'a str),
    HardLink(&'a str, &'a str),
    Fifo(&'a str),
    /// Header claims `declared` bytes; the data is `data`.
    Lying(&'a str, u64, &'a [u8]),
}

fn raw_header(name: &str, kind: tar::EntryType, size: u64, mode: u32) -> tar::Header {
    let mut h = tar::Header::new_old();
    let old = h.as_old_mut();
    assert!(name.len() < 100);
    old.name[..name.len()].copy_from_slice(name.as_bytes());
    h.set_entry_type(kind);
    h.set_size(size);
    h.set_mode(mode);
    h.set_mtime(0);
    h
}

fn set_link(h: &mut tar::Header, target: &str) {
    let old = h.as_old_mut();
    old.linkname[..target.len()].copy_from_slice(target.as_bytes());
}

fn tar_gz(entries: &[T<'_>]) -> Vec<u8> {
    let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut b = tar::Builder::new(enc);
    for e in entries {
        let (mut h, data): (tar::Header, &[u8]) = match e {
            T::File(n, d, mode) => (
                raw_header(n, tar::EntryType::Regular, d.len() as u64, *mode),
                d,
            ),
            T::Dir(n) => (raw_header(n, tar::EntryType::Directory, 0, 0o755), &[]),
            T::Symlink(n, t) => {
                let mut h = raw_header(n, tar::EntryType::Symlink, 0, 0o777);
                set_link(&mut h, t);
                (h, &[])
            }
            T::HardLink(n, t) => {
                let mut h = raw_header(n, tar::EntryType::Link, 0, 0o644);
                set_link(&mut h, t);
                (h, &[])
            }
            T::Fifo(n) => (raw_header(n, tar::EntryType::Fifo, 0, 0o644), &[]),
            T::Lying(n, declared, d) => {
                (raw_header(n, tar::EntryType::Regular, *declared, 0o644), d)
            }
        };
        h.set_cksum();
        if let T::Lying(_, declared, d) = e {
            // Pad/truncate so the stream stays well-formed for the declared size.
            let mut body = d.to_vec();
            body.resize(*declared as usize, 0);
            b.append(&h, body.as_slice()).unwrap();
        } else {
            b.append(&h, data).unwrap();
        }
    }
    b.into_inner().unwrap().finish().unwrap()
}

enum Z<'a> {
    File(&'a str, &'a [u8], u32),
    Dir(&'a str),
    Symlink(&'a str, &'a str),
}

fn zip_bytes(entries: &[Z<'_>]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for e in entries {
        match e {
            Z::File(n, d, mode) => {
                let o = SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(*mode);
                w.start_file(*n, o).unwrap();
                w.write_all(d).unwrap();
            }
            Z::Dir(n) => w.add_directory(*n, SimpleFileOptions::default()).unwrap(),
            Z::Symlink(n, t) => w.add_symlink(*n, *t, SimpleFileOptions::default()).unwrap(),
        }
    }
    w.finish().unwrap().into_inner()
}

fn write_tmp(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn extract(
    bytes: &[u8],
    format: ArchiveFormat,
    limits: ExtractLimits,
) -> (
    tempfile::TempDir,
    nugget_core::Result<nugget_models::archive::ExtractReport>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let a = write_tmp(tmp.path(), "a.bin", bytes);
    let res = extract_archive(&a, format, &tmp.path().join("out"), &limits);
    (tmp, res)
}

/// A failed extraction leaves neither the target nor a staging directory behind.
fn assert_clean_failure(tmp: &Path) {
    assert!(!tmp.join("out").exists(), "target must not exist");
    let leftovers: Vec<_> = std::fs::read_dir(tmp)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "a.bin")
        .collect();
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
}

fn expect_hostile(bytes: &[u8], format: ArchiveFormat, limits: ExtractLimits, what: &str) {
    let (tmp, res) = extract(bytes, format, limits);
    let err = res.expect_err(what);
    assert!(matches!(err, Error::Integrity(_)), "{what}: {err}");
    assert_clean_failure(tmp.path());
    // Nothing escaped next to the temp dir either.
    assert!(
        !tmp.path().parent().unwrap().join("evil").exists(),
        "{what}"
    );
}

// ---------------------------------------------------------------------------------------------
// Extractor: good archives
// ---------------------------------------------------------------------------------------------

#[test]
fn extracts_tar_gz_with_dirs_exec_bits_and_internal_symlinks() {
    let bytes = tar_gz(&[
        T::Dir("./rt/"),
        T::File("rt/llama-server", b"#!/bin/sh\necho hi\n", 0o4777),
        T::File("rt/libx.so.1.0", b"lib", 0o644),
        T::Symlink("rt/libx.so.1", "libx.so.1.0"),
        T::Symlink("rt/libx.so", "libx.so.1"),
        T::File("rt/LICENSE", b"MIT", 0o600),
    ]);
    let (tmp, res) = extract(&bytes, ArchiveFormat::TarGz, ExtractLimits::default());
    let report = res.unwrap();
    let out = tmp.path().join("out");
    assert_eq!(report.files.len(), 3);
    assert_eq!(report.symlinks.len(), 2);
    assert_eq!(report.total_bytes, 18 + 3 + 3);
    let bin = report.file("rt/llama-server").unwrap();
    assert!(bin.executable);
    assert_eq!(bin.sha256, sha256_hex(b"#!/bin/sh\necho hi\n"));
    assert_eq!(std::fs::read(out.join("rt/libx.so")).unwrap(), b"lib");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| std::fs::metadata(out.join(p)).unwrap().permissions().mode() & 0o7777;
        // setuid and world-writable bits are dropped.
        assert_eq!(mode("rt/llama-server"), 0o755);
        assert_eq!(mode("rt/LICENSE"), 0o644);
        assert!(
            std::fs::symlink_metadata(out.join("rt/libx.so"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    // No staging dirs left.
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2);
}

#[test]
fn extracts_zip() {
    let bytes = zip_bytes(&[
        Z::Dir("bin/"),
        Z::File("bin/llama-server.exe", b"MZ fake", 0o755),
        Z::File("bin/ggml.dll", b"dll", 0o644),
        Z::Symlink("bin/ggml-link.dll", "ggml.dll"),
    ]);
    let (tmp, res) = extract(&bytes, ArchiveFormat::Zip, ExtractLimits::default());
    let report = res.unwrap();
    assert!(report.file("bin/llama-server.exe").unwrap().executable);
    assert_eq!(
        std::fs::read(tmp.path().join("out/bin/ggml-link.dll")).unwrap(),
        b"dll"
    );
}

#[test]
fn refuses_existing_target() {
    let bytes = tar_gz(&[T::File("a", b"x", 0o644)]);
    let tmp = tempfile::tempdir().unwrap();
    let a = write_tmp(tmp.path(), "a.bin", &bytes);
    std::fs::create_dir(tmp.path().join("out")).unwrap();
    let err = extract_archive(
        &a,
        ArchiveFormat::TarGz,
        &tmp.path().join("out"),
        &ExtractLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Extractor: hostile archives
// ---------------------------------------------------------------------------------------------

#[test]
fn tar_path_traversal_and_absolute_paths_are_rejected() {
    let d = ExtractLimits::default();
    for name in [
        "../evil",
        "ok/../../evil",
        "./../evil",
        "/tmp/evil",
        "C:/evil",
        "a\\..\\evil",
        "x/CON",
    ] {
        let bytes = tar_gz(&[
            T::File("fine", b"ok", 0o644),
            T::File(name, b"pwned", 0o644),
        ]);
        expect_hostile(&bytes, ArchiveFormat::TarGz, d, name);
    }
}

#[test]
fn tar_symlink_escapes_are_rejected() {
    let d = ExtractLimits::default();
    for target in ["../evil", "/etc/passwd", "a/../../evil", "..", "."] {
        let bytes = tar_gz(&[T::Symlink("link", target)]);
        expect_hostile(&bytes, ArchiveFormat::TarGz, d, target);
    }
    // Classic two-step: a symlinked dir, then a file written "through" it.
    let bytes = tar_gz(&[T::Symlink("dir", "../.."), T::File("dir/evil", b"x", 0o644)]);
    expect_hostile(&bytes, ArchiveFormat::TarGz, d, "write through symlink");
}

#[test]
fn file_written_through_an_in_archive_symlink_cannot_land_outside() {
    // Even a "valid" symlink is created only after every file, so `sub/x` becomes a real dir and
    // the later link creation collides instead of redirecting writes.
    let bytes = tar_gz(&[
        T::Symlink("sub", "real"),
        T::File("sub/x", b"x", 0o644),
        T::Dir("real/"),
    ]);
    let (tmp, res) = extract(&bytes, ArchiveFormat::TarGz, ExtractLimits::default());
    #[cfg(unix)]
    {
        assert!(res.is_err());
        assert_clean_failure(tmp.path());
    }
    // Where links are copied (Windows) there is no link to write through: a link to a directory
    // is skipped, and `sub/x` stays a real file inside the destination.
    #[cfg(not(unix))]
    {
        res.unwrap();
        assert!(tmp.path().join("out/sub/x").is_file());
        assert!(tmp.path().join("out/real").is_dir());
    }
}

#[test]
fn tar_hard_links_devices_and_duplicates_are_rejected() {
    let d = ExtractLimits::default();
    let bytes = tar_gz(&[T::File("a", b"x", 0o644), T::HardLink("b", "a")]);
    expect_hostile(&bytes, ArchiveFormat::TarGz, d, "hard link");
    let bytes = tar_gz(&[T::Fifo("pipe")]);
    expect_hostile(&bytes, ArchiveFormat::TarGz, d, "fifo");
    let bytes = tar_gz(&[T::File("a", b"x", 0o644), T::File("a", b"y", 0o644)]);
    expect_hostile(&bytes, ArchiveFormat::TarGz, d, "duplicate");
    let bytes = tar_gz(&[
        T::File("Readme", b"x", 0o644),
        T::File("README", b"y", 0o644),
    ]);
    expect_hostile(
        &bytes,
        ArchiveFormat::TarGz,
        d,
        "case-insensitive duplicate",
    );
}

#[test]
fn tar_bombs_hit_the_caps() {
    let zeros = vec![0u8; 4 * 1024 * 1024];
    let small = ExtractLimits {
        max_entries: 100,
        max_file_bytes: 1024 * 1024,
        max_total_bytes: 3 * 1024 * 1024,
    };
    // One file bigger than the per-file cap (compresses to a few KiB).
    let bytes = tar_gz(&[T::File("big", &zeros, 0o644)]);
    assert!(
        bytes.len() < 64 * 1024,
        "the bomb should be small: {}",
        bytes.len()
    );
    expect_hostile(&bytes, ArchiveFormat::TarGz, small, "file cap");
    // Many files under the per-file cap that together exceed the total cap.
    let chunk = vec![0u8; 900 * 1024];
    let names: Vec<String> = (0..5).map(|i| format!("f{i}")).collect();
    let entries: Vec<T<'_>> = names.iter().map(|n| T::File(n, &chunk, 0o644)).collect();
    expect_hostile(&tar_gz(&entries), ArchiveFormat::TarGz, small, "total cap");
    // Header lies about the size (declares more than the cap).
    let bytes = tar_gz(&[T::Lying("liar", 2 * 1024 * 1024, b"x")]);
    expect_hostile(&bytes, ArchiveFormat::TarGz, small, "declared size");
    // Too many entries.
    let names: Vec<String> = (0..101).map(|i| format!("e{i}")).collect();
    let entries: Vec<T<'_>> = names.iter().map(|n| T::File(n, b"", 0o644)).collect();
    expect_hostile(&tar_gz(&entries), ArchiveFormat::TarGz, small, "entry cap");
}

#[test]
fn garbage_is_rejected_cleanly() {
    let d = ExtractLimits::default();
    expect_hostile(
        b"definitely not gzip",
        ArchiveFormat::TarGz,
        d,
        "garbage tar.gz",
    );
    expect_hostile(b"PK\x03\x04 nope", ArchiveFormat::Zip, d, "garbage zip");
    // Truncated gzip of a valid archive.
    let bytes = tar_gz(&[T::File("a", &vec![7u8; 100_000], 0o644)]);
    let (tmp, res) = extract(&bytes[..bytes.len() / 2], ArchiveFormat::TarGz, d);
    assert!(res.is_err());
    assert_clean_failure(tmp.path());
}

#[test]
fn zip_traversal_absolute_symlink_and_bomb_are_rejected() {
    let d = ExtractLimits::default();
    for name in [
        "../evil",
        "a/../../evil",
        "/tmp/evil",
        "C:\\evil",
        "..\\evil",
    ] {
        let bytes = zip_bytes(&[Z::File(name, b"pwned", 0o644)]);
        expect_hostile(&bytes, ArchiveFormat::Zip, d, name);
    }
    for target in ["../evil", "/etc/passwd", "a/../../x"] {
        let bytes = zip_bytes(&[Z::Symlink("l", target)]);
        expect_hostile(&bytes, ArchiveFormat::Zip, d, target);
    }
    let small = ExtractLimits {
        max_entries: 10,
        max_file_bytes: 1024 * 1024,
        max_total_bytes: 2 * 1024 * 1024,
    };
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let bytes = zip_bytes(&[Z::File("bomb", &zeros, 0o644)]);
    assert!(bytes.len() < 64 * 1024);
    expect_hostile(&bytes, ArchiveFormat::Zip, small, "zip bomb");
    let names: Vec<String> = (0..11).map(|i| format!("e{i}")).collect();
    let entries: Vec<Z<'_>> = names.iter().map(|n| Z::File(n, b"", 0o644)).collect();
    expect_hostile(
        &zip_bytes(&entries),
        ArchiveFormat::Zip,
        small,
        "zip entry cap",
    );
}

// ---------------------------------------------------------------------------------------------
// Manifest runtime entries
// ---------------------------------------------------------------------------------------------

fn rt_entry(
    id: &str,
    tag: &str,
    url: &str,
    bytes: &[u8],
    format: ArchiveFormat,
    binary: &str,
) -> RuntimeEntry {
    RuntimeEntry {
        id: id.into(),
        version: tag.into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        accel: RuntimeAccel::Cpu,
        url: url.into(),
        mirrors: vec![],
        size: bytes.len() as u64,
        sha256: sha256_hex(bytes),
        archive: format,
        binary: binary.into(),
        license: "MIT".into(),
    }
}

fn sign(m: &Manifest) -> (Vec<u8>, String) {
    let bytes = serde_json::to_vec_pretty(m).unwrap();
    let sig = sign_manifest(&bytes, &test_key());
    (bytes, sig)
}

#[test]
fn runtime_entries_round_trip_through_a_signed_manifest() {
    let mut cpu = rt_entry(
        "rt-cpu",
        "b42",
        "https://github.com/o/r/releases/download/b42/a.tar.gz",
        b"x",
        ArchiveFormat::TarGz,
        "a/llama-server",
    );
    cpu.mirrors = vec!["https://mirror.example/b42/a.tar.gz".into()];
    let mut vk = cpu.clone();
    vk.id = "rt-vulkan".into();
    vk.accel = RuntimeAccel::Vulkan;
    vk.archive = ArchiveFormat::Zip;
    let m = Manifest {
        version: 9,
        models: vec![],
        runtimes: vec![cpu, vk],
    };
    let (bytes, sig) = sign(&m);
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("\"archive\": \"tar.gz\"") && text.contains("\"accel\": \"vulkan\""));
    let back = verify_manifest(&bytes, sig.as_bytes(), &[test_key().verifying_key()]).unwrap();
    assert_eq!(back, m);
    assert_eq!(
        back.runtime("rt-vulkan").unwrap().archive,
        ArchiveFormat::Zip
    );

    // Without runtimes, the field is omitted entirely (old manifests stay byte-identical).
    let empty = Manifest {
        version: 1,
        models: vec![],
        runtimes: vec![],
    };
    assert!(
        !String::from_utf8(serde_json::to_vec(&empty).unwrap())
            .unwrap()
            .contains("runtimes")
    );
    let parsed: Manifest = serde_json::from_str(r#"{"version":1,"models":[]}"#).unwrap();
    assert!(parsed.runtimes.is_empty());
}

type Mutation = Box<dyn Fn(&mut Manifest)>;

#[test]
fn signed_manifest_with_bad_runtime_is_rejected() {
    let good = rt_entry(
        "rt",
        "b42",
        "https://h.example/b42/a.tar.gz",
        b"x",
        ArchiveFormat::TarGz,
        "llama-server",
    );
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "moving url",
            Box::new(|m| m.runtimes[0].url = "https://h.example/latest/a.tar.gz".into()),
        ),
        (
            "short hash",
            Box::new(|m| m.runtimes[0].sha256 = "abc".into()),
        ),
        (
            "traversal binary",
            Box::new(|m| m.runtimes[0].binary = "../../bin/sh".into()),
        ),
        (
            "duplicate id",
            Box::new(|m| {
                let dup = m.runtimes[0].clone();
                m.runtimes.push(dup)
            }),
        ),
    ];
    for (what, f) in cases {
        let mut m = Manifest {
            version: 1,
            models: vec![],
            runtimes: vec![good.clone()],
        };
        f(&mut m);
        let (bytes, sig) = sign(&m);
        let err =
            verify_manifest(&bytes, sig.as_bytes(), &[test_key().verifying_key()]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{what}: {err}");
    }
}

#[test]
fn shipped_manifest_pins_one_llama_cpp_release_for_every_desktop_target() {
    let m = default_manifest().expect("shipped manifest verifies with the embedded key");
    assert!(m.version >= 2, "adding runtimes bumps the manifest version");
    assert!(!m.runtimes.is_empty());
    let tag = &m.runtimes[0].version;
    for r in &m.runtimes {
        assert_eq!(&r.version, tag, "{}: one release for all runtimes", r.id);
        assert!(
            r.url.starts_with(&format!(
                "https://github.com/ggml-org/llama.cpp/releases/download/{tag}/llama-{tag}-bin-"
            )),
            "{}",
            r.url
        );
        assert_eq!(r.sha256.len(), 64);
        assert!(
            r.sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert!(r.size > 1024 * 1024);
        assert_eq!(r.license.split_whitespace().next(), Some("MIT"));
        let is_zip = r.url.ends_with(".zip");
        assert_eq!(r.archive == ArchiveFormat::Zip, is_zip, "{}", r.id);
        if r.os == "windows" {
            assert!(r.binary.ends_with("llama-server.exe"));
        } else {
            assert!(r.binary.ends_with("/llama-server"));
        }
    }
    let has = |os: &str, arch: &str, accel: RuntimeAccel| {
        m.runtimes
            .iter()
            .any(|r| r.os == os && r.arch == arch && r.accel == accel)
    };
    assert!(has("macos", "aarch64", RuntimeAccel::Metal));
    assert!(has("linux", "x86_64", RuntimeAccel::Cpu));
    assert!(has("linux", "x86_64", RuntimeAccel::Vulkan));
    assert!(has("windows", "x86_64", RuntimeAccel::Cpu));
    assert!(has("windows", "x86_64", RuntimeAccel::Vulkan));
    // The shipped JSON names the field explicitly.
    assert!(
        std::str::from_utf8(DEFAULT_MANIFEST_JSON)
            .unwrap()
            .contains("\"runtimes\"")
    );
}

#[test]
fn best_runtime_follows_the_hardware() {
    let m = default_manifest().unwrap();
    let pick = |os, arch, gpu| {
        runtime_candidates_for(&m, os, arch, gpu)
            .into_iter()
            .map(|r| r.accel)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pick("macos", "aarch64", GpuKind::AppleSilicon)[0],
        RuntimeAccel::Metal
    );
    assert_eq!(
        pick("linux", "x86_64", GpuKind::Nvidia),
        vec![RuntimeAccel::Vulkan, RuntimeAccel::Cpu]
    );
    assert_eq!(
        pick("linux", "x86_64", GpuKind::Amd),
        vec![RuntimeAccel::Vulkan, RuntimeAccel::Cpu]
    );
    assert_eq!(
        pick("linux", "x86_64", GpuKind::Intel),
        vec![RuntimeAccel::Cpu]
    );
    assert_eq!(
        pick("windows", "x86_64", GpuKind::Unknown),
        vec![RuntimeAccel::Cpu]
    );
    assert!(pick("freebsd", "x86_64", GpuKind::Unknown).is_empty());
    // On this machine, whatever it is, the pick matches its OS and CPU.
    let hw = Hardware {
        total_ram: 16 << 30,
        available_ram: 8 << 30,
        cpu_cores: 8,
        gpu: GpuKind::Unknown,
    };
    if let Some(r) = select_runtime(&m, &hw) {
        assert_eq!(r.os, std::env::consts::OS);
        assert_eq!(r.arch, std::env::consts::ARCH);
        assert!(matches!(r.accel, RuntimeAccel::Cpu | RuntimeAccel::Metal));
    }
}

// ---------------------------------------------------------------------------------------------
// Verified install
// ---------------------------------------------------------------------------------------------

fn runtime_archive() -> Vec<u8> {
    tar_gz(&[
        T::Dir("llama-b7/"),
        T::File("llama-b7/llama-server", b"#!/bin/sh\nexit 0\n", 0o644),
        T::File("llama-b7/libllama.so.0.1", b"lib", 0o644),
        T::Symlink("llama-b7/libllama.so", "libllama.so.0.1"),
    ])
}

#[tokio::test]
async fn install_runtime_downloads_verifies_extracts_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(VecSink::default());
    let st = store(tmp.path(), sink.clone());
    let srv = TestServer::start().await;
    let bytes = runtime_archive();
    let url = srv.add("b7", bytes.clone());
    let e = rt_entry(
        "rt",
        "b7",
        &url,
        &bytes,
        ArchiveFormat::TarGz,
        "llama-b7/llama-server",
    );
    assert_eq!(st.runtime_binary(&e).unwrap(), None);

    let bin = st.install_runtime(&e, |_| {}).await.unwrap();
    assert!(bin.ends_with("llama-b7/llama-server"));
    assert!(bin.starts_with(st.runtimes_dir()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&bin).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    assert_eq!(st.runtime_binary(&e).unwrap(), Some(bin.clone()));
    st.verify_runtime(&e).await.unwrap();
    // The archive itself is not kept.
    assert!(!st.staging_data_path(&nugget_models_pseudo(&e)).exists());
    let rec = st.installed_runtime(&e).unwrap().unwrap();
    assert_eq!(rec.release, "b7");
    assert_eq!(rec.files.len(), 2);
    {
        let events = sink.0.lock().unwrap();
        assert!(!events.is_empty());
        assert!(events.iter().all(|ev| ev.feature == RUNTIME_EGRESS_FEATURE));
    }

    // Second install: no network.
    srv.clear_requests();
    assert_eq!(st.install_runtime(&e, |_| {}).await.unwrap(), bin);
    assert!(srv.requests().is_empty());

    // Tampering is caught by verify_runtime, and remove_runtime cleans up.
    std::fs::write(bin.parent().unwrap().join("libllama.so.0.1"), b"evil").unwrap();
    let err = st.verify_runtime(&e).await.unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    st.remove_runtime(&e).await.unwrap();
    assert_eq!(st.runtime_binary(&e).unwrap(), None);
}

/// The staging path helper is keyed by a model entry; build the same pseudo key.
fn nugget_models_pseudo(e: &RuntimeEntry) -> nugget_models::ModelEntry {
    nugget_models::ModelEntry {
        id: e.id.clone(),
        family: "runtime".into(),
        file_name: "x".into(),
        url: e.url.clone(),
        mirrors: vec![],
        revision: String::new(),
        size: e.size,
        sha256: e.sha256.clone(),
        min_ram_gb: 0,
        tiers: vec![],
        license: e.license.clone(),
        min_app_version: "0.0.0".into(),
    }
}

#[tokio::test]
async fn install_runtime_rejects_hash_mismatch_and_missing_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::new(VecSink::default()));
    let srv = TestServer::start().await;
    let bytes = runtime_archive();
    let url = srv.add("b8", bytes.clone());

    let mut bad = rt_entry(
        "rt",
        "b8",
        &url,
        &bytes,
        ArchiveFormat::TarGz,
        "llama-b7/llama-server",
    );
    bad.sha256 = sha256_hex(b"something else");
    let err = st.install_runtime(&bad, |_| {}).await.unwrap_err();
    assert!(
        matches!(err, Error::Integrity(ref m) if m.contains("SHA-256")),
        "{err}"
    );
    assert!(!st.runtime_dir(&bad).unwrap().exists());

    let missing = rt_entry(
        "rt",
        "b8",
        &url,
        &bytes,
        ArchiveFormat::TarGz,
        "llama-b7/not-there",
    );
    let err = st.install_runtime(&missing, |_| {}).await.unwrap_err();
    assert!(
        matches!(err, Error::Integrity(ref m) if m.contains("not-there")),
        "{err}"
    );
    assert!(!st.runtime_dir(&missing).unwrap().exists());

    // A hostile archive that matches its (attacker-signed) hash still cannot escape.
    let evil = tar_gz(&[T::File("../../evil", b"x", 0o644)]);
    let url = srv.add("b9", evil.clone());
    let e = rt_entry(
        "rt",
        "b9",
        &url,
        &evil,
        ArchiveFormat::TarGz,
        "llama-server",
    );
    let err = st.install_runtime(&e, |_| {}).await.unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(!st.runtime_dir(&e).unwrap().exists());
    assert!(!st.root().join("evil").exists() && !st.root().parent().unwrap().join("evil").exists());
}
