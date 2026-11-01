# Reproducible builds

Goal: anyone can rebuild a Nugget binary from the tagged source and get the bytes we ship.

| Artifact | Platform | Status |
|---|---|---|
| `nuggetd` release binary | x86_64 Linux (`ubuntu-22.04`) | **Verified**: CI builds it twice and requires identical bytes, symbol names included |
| `nuggetd` | aarch64 Linux, macOS, Windows | Same measures in `release.yml`; not verified |
| `nugget-desktop` | all | Same measures; not verified (embeds the Vite bundle, whose determinism is unchecked) |
| Installers (AppImage, deb, rpm, dmg, app tarball, NSIS, MSI) | all | Not reproducible |

`.github/workflows/reproducible.yml` checks out one commit into two differently-located
directories, runs `cargo build --release -p nuggetd --locked` in each with
`packaging/scripts/repro-env.sh` and an emptied target dir, and fails if the binaries differ or
contain a checkout path, `CARGO_HOME` or the sysroot. On mismatch it uploads both for
[diffoscope](https://diffoscope.org/).

## The build environment

`packaging/scripts/repro-env.sh` (source it, `eval` its output, or `--github-env` in a workflow) sets:

- **`SOURCE_DATE_EPOCH`**: kept if set, else `git log -1 --format=%ct`.
- **`RUSTFLAGS`** (appended): `--remap-path-prefix` for the workspace → `/build`, `CARGO_HOME` →
  `/cargo`, the sysroot → `/rustc-sysroot`, and an external `CARGO_TARGET_DIR` → `/target`
  (symlink-free, native form on Windows; paths with whitespace are refused).

Changing `RUSTFLAGS` invalidates cargo's cache, so only `release.yml` and `reproducible.yml` use it.
Other pinned inputs: [release.md §7](../release.md#7-reproducibility).
`packaging/scripts/clamp_mtimes.py` clamps mtimes to `SOURCE_DATE_EPOCH`; `stage-sidecar.sh` runs it
on the staged sidecar.

## Reproducing `nuggetd`

On x86_64 Linux with the `rustc -vV` from the build log:

```sh
git clone https://github.com/rayhankhilji/nugget && cd nugget
git checkout vX.Y.Z
export CARGO_TARGET_DIR=/tmp/nugget-repro-target   # see the OpenSSL caveat
. packaging/scripts/repro-env.sh
cargo build --release -p nuggetd --locked
sha256sum "$CARGO_TARGET_DIR/release/nuggetd"
```

Two builds from any checkout directories match if `CARGO_TARGET_DIR` is the same absolute path
and starts empty.

**OpenSSL caveat.** The vendored OpenSSL (for SQLCipher) compiles its install prefix, under the
target dir, into `nuggetd` (`ENGINESDIR`/`MODULESDIR`), and `--remap-path-prefix` can't reach it.
So the target dir's absolute path is a build input (harmless at runtime: `no-module`, no engines).
Release builds use the default in-checkout target dir (`/home/runner/work/nugget/nugget/target`)
with `--target <triple>`: to match a released binary, rebuild from a checkout at that path (a
container helps) with `packaging/scripts/stage-sidecar.sh <triple>` after sourcing
`repro-env.sh`. `strings nuggetd | grep ENGINESDIR` shows the path. A fixed target path or a
patched prefix would remove this constraint.

**Symbol names.** wasmtime's `bindgen!` used to embed the WIT files' absolute path, changing
symbol hashes per checkout. `nugget-plugin` now passes the WIT inline (`src/host/bindings.rs`),
with a compile-time assertion and a test that it equals `wit/plugin.wit`. Any new macro embedding
an absolute path would reintroduce the problem.

## Not reproducible (yet)

- **Installers**: Tauri's deb and rpm writers keep build-time mtimes and ignore
  `SOURCE_DATE_EPOCH`; the AppImage bundles the runner's system libraries; signatures,
  notarization and DMG/MSI metadata vary. Instead, extract `nuggetd` (`--appimage-extract`,
  `dpkg-deb -x`, `rpm2cpio | cpio -id`) and compare it with your rebuild, after
  `codesign --remove-signature` or `osslsigncode remove-signature` on macOS and Windows.
- **Other targets and `nugget-desktop`**: same environment, never compared.
- **Cross-machine**: CI compares two builds on one runner; another image can change the C parts
  (SQLCipher, OpenSSL, aws-lc, ring). Rebuild on the image in the release log.
- **Toolchain**: `rust-toolchain.toml` pins the `stable` channel, not a version.
