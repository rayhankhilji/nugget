# Releases, installers and signed updates

This covers how Nugget is packaged, how a release is cut and signed, how installed apps update,
and how to reproduce a build. The pieces:

| Piece | Where |
|---|---|
| Bundle config | `apps/desktop/src-tauri/tauri.conf.json` (base) and `tauri.bundle.conf.json` (release overlay: the sidecar) |
| Sidecar staging | `packaging/scripts/stage-sidecar.sh` |
| Icons | `packaging/icons/nugget.svg` → `packaging/scripts/icons.sh` → `apps/desktop/src-tauri/icons/` |
| Windows installer hooks | `packaging/windows/installer-hooks.nsh` |
| Update manifest, channel promotion, checksums | `packaging/scripts/updater_manifest.py` |
| Release pipeline | `.github/workflows/release.yml` |
| In-app updater | `apps/desktop/src-tauri/src/updater.rs`, `settings.rs`, `login.rs`; UI in `apps/desktop/src/components/UpdateSettings.tsx` (on the Status page) |

Why it looks like this: Sentient's updates were mandatory and silent, its signing key sat with
one person, and it released by hand from a laptop (`docs/research/sentient-os-teardown.md` §2.9).
Nugget does the opposite: the user picks the channel, nothing installs without consent, and
signing happens only in CI, behind a second maintainer's approval.

## 1. Bundle matrix

| Platform | Runner | Target | Installers | Updater payload |
|---|---|---|---|---|
| macOS Apple silicon | `macos-14` | `aarch64-apple-darwin` | `.app`, `.dmg` | `Nugget_<v>_aarch64.app.tar.gz` |
| macOS Intel | `macos-15-intel` | `x86_64-apple-darwin` | `.app`, `.dmg` | `Nugget_<v>_x64.app.tar.gz` |
| Windows x64 | `windows-2022` | `x86_64-pc-windows-msvc` | NSIS `-setup.exe` (per-user), MSI | both installers |
| Linux x64 | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | AppImage, `.deb`, `.rpm` | all three |
| Linux arm64 | `ubuntu-22.04-arm` | `aarch64-unknown-linux-gnu` | AppImage, `.deb`, `.rpm` | all three (job may fail without blocking the release) |

Linux builds use Ubuntu 22.04 so the packages need only glibc 2.35. The `.deb` and `.rpm` declare
no extra dependencies beyond what Tauri computes (WebKitGTK 4.1, GTK 3, libayatana-appindicator).

The updater picks the payload that matches how it was installed (`tauri` patches the bundle type
into each binary): an AppImage updates its AppImage, a `.deb` install updates through the `.deb`
(with a `pkexec` prompt), the NSIS install through the NSIS installer, and so on.

## 2. The `nuggetd` sidecar

The daemon ships inside every bundle as a Tauri sidecar (`bundle.externalBin: ["binaries/nuggetd"]`).
Tauri wants `apps/desktop/src-tauri/binaries/nuggetd-<target-triple>[.exe]` at build time and installs it
**next to the app executable** without the suffix:

| Bundle | App | Daemon |
|---|---|---|
| macOS | `Nugget.app/Contents/MacOS/nugget-desktop` | `Nugget.app/Contents/MacOS/nuggetd` (signed with the app) |
| Windows | `%LOCALAPPDATA%\Nugget\nugget-desktop.exe` | `%LOCALAPPDATA%\Nugget\nuggetd.exe` |
| deb / rpm | `/usr/bin/nugget-desktop` | `/usr/bin/nuggetd` |
| AppImage | `$APPDIR/usr/bin/nugget-desktop` | `$APPDIR/usr/bin/nuggetd` |

`daemon::find_nuggetd()` already looks next to `current_exe()` first, so "Start Nugget" finds it
unchanged. In development, `target/<profile>/nuggetd` sits next to the dev binary in the same way.

The sidecar lives in an overlay config, not the base `tauri.conf.json`, because `tauri-build`
refuses to compile when an `externalBin` file is missing. That would break `cargo build`, clippy
and the CI desktop job, none of which build the daemon in release mode first.

### Local build (Linux shown; same on macOS and Windows under bash)

```sh
# Ubuntu: the packages from the CI desktop job, plus rpm for .rpm output.
packaging/scripts/stage-sidecar.sh            # host triple; or pass one, e.g. aarch64-apple-darwin
cd apps/desktop
pnpm install --frozen-lockfile
pnpm tauri build --bundles appimage,deb,rpm --config src-tauri/tauri.bundle.conf.json
ls ../../target/release/bundle/*/
```

A local build has no updater key (see §4), so it shows "this build can't update itself". To test
the update path, generate a throwaway key (`pnpm tauri signer generate -w /tmp/test.key`) and export
`NUGGET_UPDATER_PUBKEY="$(cat /tmp/test.key.pub)"` before building.

## 3. Release workflow

`.github/workflows/release.yml`, top-level `permissions: {}`. Each job asks for only what it needs,
and every action is pinned to a commit SHA.

1. **Tag.** A maintainer bumps `version` in `tauri.conf.json` and the workspace `Cargo.toml`,
   merges it, and pushes an **annotated, signed** tag `vX.Y.Z` (or `vX.Y.Z-beta.N`). The tag
   message becomes the release notes. The workflow fails if the tag and the app version differ.
2. **`build`** (matrix, `contents: read`, environment `release-build`). It stages the sidecar
   and runs `tauri build`, with OS signing when the secrets exist:
   - macOS: `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`,
     `APPLE_SIGNING_IDENTITY` sign with the hardened runtime. With `APPLE_API_ISSUER`,
     `APPLE_API_KEY_ID` and `APPLE_API_KEY_P8` it also notarizes and staples. With no
     certificate the app is ad-hoc signed and a warning is logged.
   - Windows: `WINDOWS_CERTIFICATE` (base64 .pfx), `WINDOWS_CERTIFICATE_PASSWORD`. The certificate
     is imported into the runner's user store; Tauri signs the app, the sidecar and both
     installers with SHA-256 and an RFC 3161 timestamp. With no certificate the installers are
     unsigned and a warning is logged.
   - The **updater key is never in this job**: build jobs compile hundreds of third-party crates
     whose build scripts run code. They get only the *public* key (`vars.NUGGET_UPDATER_PUBKEY`),
     which is compiled into the app.
3. **`sbom`.** CycloneDX SBOMs for `nuggetd` and `nugget-desktop` (`cargo-cyclonedx`) and for the
   frontend (`cdxgen`, from the pnpm lockfile).
4. **`sign`** (environment **`release`**, `contents: write`, `id-token: write`,
   `attestations: write`). It waits for a reviewer, then:
   - signs every update payload with `tauri signer sign` (minisign). The key comes from
     `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD`, secrets that exist only in this environment. Only
     the locked Tauri CLI is installed, with `--ignore-scripts`;
   - writes `latest.json` (the Tauri updater manifest: version, the tag's notes, and per-platform
     URL and signature);
   - writes `SHA256SUMS` over every asset, including the signatures, `latest.json` and the SBOMs;
   - records SLSA build provenance for every asset (`actions/attest-build-provenance`; check one with
     `gh attestation verify <file> -R rayhankhilji/nugget`);
   - creates a **draft** GitHub release with everything attached.
5. **Publish (human).** A maintainer checks the draft (it installs, the checksums match, the notes
   are right) and clicks *Publish*.
6. **`channels`** (on `release: published`, `contents: write`, no secrets). It copies the release's
   `latest.json` to the rolling `updater-feed` release as `beta.json`, and as `stable.json` too
   if it is not a prerelease. It only moves a feed forward (semver), so publishing an old patch
   release never pulls anyone back.

A `workflow_dispatch` run is a dry run: unsigned bundles from any ref, uploaded as workflow
artifacts, with no release created.

### Repository setup (once)

- **Environment `release-build`.** Deployment rule: tags matching `v*`. Secrets: the Apple and
  Windows ones above (all optional).
- **Environment `release`.** Deployment rule: tags matching `v*`. **Required reviewers:** the
  maintainers, with *Prevent self-review* on, so the person who pushed the tag can't approve their
  own release. Secrets: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
- **Repository variable** `NUGGET_UPDATER_PUBKEY`: the public key (the text of `*.key.pub`). Also
  publish it in this file (below) so anyone can check a payload by hand.
- **Tag protection** (repository rules): only maintainers may create `v*` tags, and tags require
  signatures.

## 4. Updater key: the two-person process

minisign (the scheme Tauri's updater uses) has no native threshold signatures, so the threshold
is enforced by process. No single person can ship an update:

1. **Two people to release.** Maintainer A pushes the tag. Maintainer B must approve the `release`
   environment before the key is used, and self-review is blocked. A draft still needs someone to
   publish it, and only published releases move the feeds.
2. **The key lives only in CI.** It is generated in an ephemeral session and goes straight into
   the environment secret. It is never written to a laptop, a password manager or a repository.
   GitHub secrets can't be read back, only used by the gated job.
3. **Recovery shares, not copies.** For disaster recovery, the encrypted key is split with Shamir's
   scheme into 3 shares with threshold 2 (for example `ssss-split -t 2 -n 3`), one share per
   maintainer, stored offline. The password is split the same way, separately. No one person
   can rebuild the key.

**Key ceremony** (two maintainers present, for example on a call with screen share):

```sh
# Ephemeral environment: a fresh VM or live USB, offline after the CLI is installed.
pnpm dlx @tauri-apps/cli@2.11.5 signer generate -w ./nugget-updater.key   # set a strong password
gh secret set TAURI_SIGNING_PRIVATE_KEY --env release < ./nugget-updater.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --env release            # paste the password
gh variable set NUGGET_UPDATER_PUBKEY < ./nugget-updater.key.pub
ssss-split -t 2 -n 3 < ./nugget-updater.key    # hand out the shares, then destroy the VM
```

**Rotation.** The app trusts exactly one public key, built in. To rotate, ship release N built
with the **new** public key but signed with the **old** key. Once N is installed, later releases
are signed with the new key. If the key is compromised, revoke the environment secret at once,
publish a security advisory, and ship a rotation release as fast as possible.

**Public key:** *not generated yet; paste `nugget-updater.key.pub` here after the ceremony.*

## 5. Updater behaviour in the app

Settings live in `settings.json` in the app config directory (for example
`~/.config/dev.nugget.desktop/` on Linux). The UI is the *Updates* card on the Status page; the tray
menu has an *Updates…* item that changes to *Update available: vX…*.

| Setting | Default | Effect |
|---|---|---|
| Channel | **Stable** | `stable.json` from the `updater-feed` release. **Beta** follows `beta.json` (prereleases as well as stable). **Off** never contacts the server. |
| Auto-install | **off** | Off means **notify only**: the app shows the version and its release notes, and nothing is downloaded until the user clicks *Install vX and restart*. On means the update is downloaded and verified in the background, its notes are shown, and it installs **when the user quits Nugget**. |

- Checks run 60 s after launch and then every 24 h, plus *Check now*. A check is one HTTPS GET of
  a static JSON file, with no identifiers beyond what any HTTPS request carries.
- Every payload is verified against the built-in minisign key before installation. A build
  without a key has no updater at all.
- *Install* sends the exact version whose notes were displayed. If the feed changed in the
  meantime, the install is refused and the new notes are shown.
- Only newer versions are offered (no downgrades). Switching from beta to stable leaves a beta
  build in place until a newer stable version exists.
- The webview holds no `updater:` permission. All updater calls go through the five app commands
  (`updates_status`, `updates_set_settings`, `updates_check`, `updates_install`, `login_set`).
- Windows: the installer runs in passive mode (progress bar only). The NSIS hooks stop `nuggetd.exe`
  so its file can be replaced, then restart the login task if the user has one.

## 6. Start at login (daemon service)

`nuggetd install-service` registers a per-user service (launchd LaunchAgent, `systemd --user` unit,
or the Task Scheduler task "Nugget Daemon"). Per-user is the point, so no installer does it on its
own:

- **deb/rpm** maintainer scripts run as root for the whole machine, not for the user who will log in.
- **macOS** `.app`/`.dmg` has no install step.
- **Windows NSIS** could run it, but there it would happen without the in-app explanation, and again
  on every silent update.

Instead the app asks once, on the Status page: *"Start the Nugget background service when you log
in?"* — **Yes** runs the bundled `nuggetd install-service`, and **Not now** records the answer. The
checkbox can later turn it off (`uninstall-service`). It is refused, with an explanation, where the
daemon's path is temporary: inside an AppImage (a new mount point on every run) and on macOS
when the app runs from the DMG or under App Translocation.

The installer hooks (`packaging/windows/installer-hooks.nsh`) keep Windows consistent: stop the
daemon before files are replaced, restart the task after install, and remove the task on a real
uninstall (`$UpdateMode` skips that during updates). deb/rpm have no hooks: on Linux a running
daemon keeps its old inode, and `systemctl --user restart nuggetd` or the next login picks up the
new binary.

To do it by hand: `nuggetd install-service` / `nuggetd uninstall-service` (on macOS, the binary is
`/Applications/Nugget.app/Contents/MacOS/nuggetd`).

## 7. Reproducibility

Pinned inputs:
- **Rust:** `Cargo.lock`, always used with `--locked`. The toolchain comes from
  `rust-toolchain.toml` (`stable`); for a bit-for-bit rebuild, use the `rustc -V` recorded in the
  build log. MSRV 1.88.
- **JS:** `apps/desktop/pnpm-lock.yaml` with `--frozen-lockfile`; pnpm from `packageManager`;
  Node 22.
- **Tauri:** crates on the 2.11 line, `tauri-plugin-updater =2.12.0` (the last release that accepts
  tauri 2.11 and Rust 1.88), and CLI `@tauri-apps/cli ~2.11.5` from the lockfile.
- **CI actions:** commit SHAs. **OS images:** the runner labels above (record the image version
  from the job log).

Determinism measures:
- `SOURCE_DATE_EPOCH` = the tagged commit's timestamp, for every packaging tool that honours it.
- `RUSTFLAGS=--remap-path-prefix=$GITHUB_WORKSPACE=/build --remap-path-prefix=$HOME/.cargo=/cargo`,
  so no runner paths end up in binaries.
- No build cache in release jobs; the macOS tarball is written with fixed owner ids.

What is and isn't reproducible today: the Rust binaries (`nuggetd`, `nugget-desktop`) built with the
same toolchain, target and flags should match. The installers don't, bit for bit: code
signatures and timestamps (Authenticode, Apple, notarization tickets), DMG/MSI metadata, and AppImage
squashfs ordering all vary. To check a release, rebuild the binaries and compare them with the
ones extracted from the installer, ignoring signature sections.

## 8. Known gaps

- `actionlint` hasn't been run on `release.yml` yet; the first tag push is its real test (use a
  `v0.0.1-rc.1` tag on a fork first).
- The macOS and Windows signing paths are written to Tauri's documented environment variables but
  untested without real certificates. Notarization of the `nuggetd` sidecar relies on Tauri
  signing `externalBin` with the app's identity.
- Linux packages aren't GPG-signed, and there's no apt/rpm repository yet; users get updates
  through the in-app updater or by downloading again.
- Update checks aren't recorded in the daemon's egress ledger (the desktop app makes them, not
  `nuggetd`).
- The `updater-feed` manifests are unsigned JSON, as Tauri designs it. A forged feed can only point
  at payloads, and those still need a valid signature. A forged feed could withhold updates
  (freeze attack); manifest signing would close that.
