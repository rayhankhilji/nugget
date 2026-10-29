# Reproducible builds

Goal: anyone can rebuild a Nugget binary from the tagged source and get the same bytes we ship, so
a compromised build machine cannot slip in code that is not in the repository.

**Status in one line:** the `nuggetd` daemon on x86_64 Linux builds to the same bytes from
different directories, checked on every relevant change by CI (`.github/workflows/reproducible.yml`,
symbol names included since the [symbol-name gap](#symbol-names-a-fixed-gap) was fixed). Nothing else is
verified yet; see [What is not reproducible](#what-is-not-reproducible-yet).

## What is verified

| Artifact | Platform | Status |
|---|---|---|
| `nuggetd` release binary | x86_64 Linux (`ubuntu-22.04`) | Same bytes required. CI builds it twice and compares, symbol names included |
| `nuggetd` | aarch64 Linux, macOS, Windows | Same measures applied in `release.yml`; not verified |
| `nugget-desktop` binary | all | Same measures applied in `release.yml`; not verified (embeds the frontend bundle, see below) |
| Installers (AppImage, deb, rpm, dmg, app tarball, NSIS, MSI) | all | Not reproducible |

The CI job checks out the same commit into two directories with different absolute paths
(`$GITHUB_WORKSPACE/one` and `$RUNNER_TEMP/elsewhere/deeper/nugget-two`), builds
`cargo build --release -p nuggetd --locked` in each with the environment from
`packaging/scripts/repro-env.sh`, empties the target directory between the two builds so nothing
is reused, and fails if the two binaries differ. It also fails if any of the checkout paths,
`CARGO_HOME` or the toolchain sysroot appears in either binary. On a mismatch it prints the
differing sections and strings and uploads both binaries, ready for
[diffoscope](https://diffoscope.org/).

## The deterministic build environment

`packaging/scripts/repro-env.sh` sets two variables (source it, `eval` its output, or call it with
`--github-env` in a workflow):

- **`SOURCE_DATE_EPOCH`**: kept if already set, otherwise the commit time of `HEAD`
  (`git log -1 --format=%ct`). For a release this is the tagged commit's time.
- **`RUSTFLAGS`** (appended to any existing value): `--remap-path-prefix` for
  - the workspace root → `/build`,
  - `CARGO_HOME` (registry and git checkouts of dependencies) → `/cargo`,
  - the toolchain sysroot (`rustc --print sysroot`) → `/rustc-sysroot`,
  - `CARGO_TARGET_DIR`, when set and outside the workspace → `/target`.

  Paths are taken without symlinks and, on Windows (Git Bash), in native `C:\...` form, which is
  what rustc sees. rustc applies the last matching prefix, so the workspace and target dir come
  after the broader ones. The paths must not contain whitespace (RUSTFLAGS is split on spaces);
  the script refuses them.

Setting `RUSTFLAGS` changes cargo's fingerprints, so the first build after sourcing the script
rebuilds everything, and CI caches keyed without it are not reused. That is why only
`release.yml` and `reproducible.yml` use it; the ordinary CI jobs do not.

Other inputs are pinned as described in [release.md §7](../release.md#7-reproducibility):
`Cargo.lock` with `--locked`, the toolchain channel from `rust-toolchain.toml`, Actions by commit
SHA. Cargo (1.85 and later, so every toolchain at or above the 1.88 MSRV) does not mix `RUSTFLAGS`
into crate metadata hashes, so symbol names do not depend on the remapped paths.

## Reproducing `nuggetd` yourself

On x86_64 Linux, with the toolchain the build log recorded (`rustc -vV`):

```sh
git clone https://github.com/rayhankhilji/nugget && cd nugget
git checkout vX.Y.Z
export CARGO_TARGET_DIR=/tmp/nugget-repro-target   # see the OpenSSL caveat below
. packaging/scripts/repro-env.sh
cargo build --release -p nuggetd --locked
sha256sum "$CARGO_TARGET_DIR/release/nuggetd"
```

Two such builds on one machine, from any two checkout directories, give the same hash as long as
`CARGO_TARGET_DIR` is the same absolute path and empty at the start. That is what CI checks.

**The OpenSSL caveat.** `nugget-store` builds SQLCipher with a vendored OpenSSL
(`rusqlite/bundled-sqlcipher-vendored-openssl`). The `openssl-src` build script configures
OpenSSL with `--prefix=<OUT_DIR>/install`, and OpenSSL compiles `ENGINESDIR` and `MODULESDIR`
(derived from the prefix) into the library as C string constants. `--remap-path-prefix` only
affects what rustc emits, so this absolute path, which lies under the target directory, ends up in
`nuggetd`. Consequences:

- The target directory's absolute path is an input to the build. Use the same one to compare two
  builds. A build with the target dir inside the checkout (the cargo default) is only
  reproducible from a checkout at the same absolute path.
- The path is harmless at runtime (OpenSSL is built with `no-module`, and Nugget loads no
  engines), but it does reveal the build machine's target path.
- Release builds (`release.yml`) use the default target dir inside the checkout
  (`/home/runner/work/nugget/nugget/target` on Linux runners) and build with `--target <triple>`.
  To match a released `nuggetd` byte for byte, rebuild from a checkout at that same path (a
  container makes this easy) with the default target dir and
  `packaging/scripts/stage-sidecar.sh <triple>` after sourcing `repro-env.sh`; `strings nuggetd |
  grep ENGINESDIR` shows the path a binary was built with. Making release builds use a fixed
  target path outside the checkout, or patching the prefix, would remove this constraint.

C code compiled through the `cc` crate (SQLCipher, OpenSSL, `aws-lc-sys`, `ring`) could also
embed `__FILE__` paths. In the CI comparison no such path from the checkout or `CARGO_HOME`
appears in `nuggetd` (the job fails if one does).

## Bundles and SOURCE_DATE_EPOCH

`packaging/scripts/clamp_mtimes.py` clamps the modification time of every file, directory and
symlink under the given paths to `SOURCE_DATE_EPOCH` if it is newer (pure Python, so GNU and BSD
`touch` differences do not matter). `stage-sidecar.sh` runs it on the staged sidecar when
`SOURCE_DATE_EPOCH` is set, which `release.yml` does.

What the Tauri bundler (tauri-bundler 2.9.4, used by `@tauri-apps/cli` 2.11.5) does with
timestamps, from its source:

- **deb**: tar entries use `HeaderMode::Deterministic` (no uid/gid/user names) but keep each
  file's mtime, and most files are written by the bundler itself during the build, so they carry
  the build time. Files are added in directory-walk order (not sorted), and the `ar` members take
  the mtimes of `control.tar.gz`/`data.tar.gz`. The bundler does not read `SOURCE_DATE_EPOCH`.
  The gzip headers carry no timestamp.
- **rpm**: built with the `rpm` crate; the bundler does not set its `source_date`, so the build time
  and file mtimes are recorded.
- **AppImage**: `linuxdeploy` copies the app plus GTK/WebKit libraries from the build machine and
  `appimagetool` packs a squashfs. The contents depend on the runner's installed system packages,
  and timestamps in the squashfs depend on the tool versions linuxdeploy ships.
- **macOS / Windows**: code signatures, notarization tickets and Authenticode timestamps are
  different on every signing, by design; DMG and MSI containers carry their own metadata.

There is no hook between the bundler writing its staging tree and archiving it, so clamping mtimes
from our scripts cannot make these installers reproducible today.

## Symbol names (a fixed gap)

Two local builds from different directories used to give different SHA-256 for the whole file,
but identical bytes after `objcopy --strip-all --remove-section .note.gnu.build-id`. The cause:
wasmtime's `bindgen!` macro, given `path: "wit"`, emits
`include_str!("<absolute path of each WIT file>")` for change tracking. That path entered
`nugget-plugin`'s crate hash (its `.rmeta` and bitcode differed between checkouts), which changed
the ThinLTO `.llvm.<hash>` suffix of the crate's local symbols, and the build id hashes the symbol
table. `--remap-path-prefix` does not reach macro-expanded string literals.

`nugget-plugin` now passes the WIT to `bindgen!` inline (`src/host/bindings.rs`). `wit/plugin.wit`
stays the source of truth: a compile-time assertion fails the build unless the inline copy is
byte-for-byte equal to the file (which `nugget_plugin::WIT` includes by a relative path, so cargo
rebuilds the crate when it changes), and a unit test checks the same. With that, the CI job runs
with `STRICT: "true"` and fails on any difference, symbol names included. Any new proc macro that
embeds an absolute path (`include_str!`/`include_bytes!` of `CARGO_MANIFEST_DIR`-based paths
generated by a macro) would reintroduce this.

## What is not reproducible (yet)

- **Installers** (all of the above): verify the binaries inside them instead. Extract `nuggetd`
  from the AppImage (`--appimage-extract`), deb (`dpkg-deb -x`) or rpm (`rpm2cpio | cpio -id`) and
  compare it with your rebuild; on macOS and Windows the extracted binaries are additionally
  signed, so compare after stripping the signature (`codesign --remove-signature`, or
  `osslsigncode remove-signature`).
- **`nugget-desktop`**: built with the same `RUSTFLAGS` in releases but never compared. It embeds
  the Vite frontend bundle, whose determinism (pnpm lockfile, Node version, hashed chunk names) has
  not been checked.
- **Other targets**: aarch64 Linux, macOS and Windows get the same environment in `release.yml`
  but no CI comparison. Windows (MSVC) and macOS (ld64) linkers are believed to be deterministic
  for these flags, but this is unverified.
- **Cross-machine**: CI compares two builds on one runner. A different runner image (another
  glibc, linker or C compiler version) can change the C parts of the build (SQLCipher, OpenSSL,
  aws-lc, ring). Rebuild on the runner image recorded in the release job log (`ubuntu-22.04`).
- **Toolchain drift**: `rust-toolchain.toml` pins the `stable` channel, not a version. Use the
  exact `rustc -V` from the build log.

## Files

- `packaging/scripts/repro-env.sh`: the environment above.
- `packaging/scripts/clamp_mtimes.py`: mtime clamping.
- `packaging/scripts/stage-sidecar.sh`: clamps the staged sidecar.
- `.github/workflows/reproducible.yml`: the double build.
- `.github/workflows/release.yml`, "Deterministic build environment" step: applies the environment
  to release builds.
