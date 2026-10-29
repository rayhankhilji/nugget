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

### Sizes and sidecar layout

Installer sizes from the unsigned dry-run bundles (§3, *Dry run*). Signed builds differ by a few
KB (signatures, notarization ticket). Each `bundle-report-<target>` also lists where `nuggetd`
ended up inside the bundle; it should match the table above.

<!-- sizes:begin -->
| Platform | Installer | Size | Notes |
|---|---|---|---|
| macOS Apple silicon | `Nugget_<v>_aarch64.dmg` | 26.5 MiB | `.app` is 67.8 MiB on disk: `nugget-desktop` 15.3 MiB, `nuggetd` 52.4 MiB |
| macOS Intel | `Nugget_<v>_x64.dmg` | 28.9 MiB | `.app` is 76.4 MiB on disk: `nugget-desktop` 16.2 MiB, `nuggetd` 59.5 MiB |
| macOS (both) | `Nugget_<v>_<arch>.app.tar.gz` (updater payload) | 26.5 / 29.0 MiB | aarch64 / x64 |
| Windows x64 | `Nugget_<v>_x64-setup.exe` (NSIS) | 19.4 MiB | LZMA solid; unpacks to 76.7 MiB (`nugget-desktop.exe` 17.8, `nuggetd.exe` 58.8) |
| Windows x64 | `Nugget_<v>_x64_en-US.msi` | 28.1 MiB | installs to `Program Files\Nugget\{nugget-desktop,nuggetd}.exe` |
| Linux x64 | `Nugget_<v>_amd64.AppImage` | 105.4 MiB | carries its own GTK/WebKitGTK libraries; `usr/bin/{nugget-desktop,nuggetd,xdg-open}` |
| Linux x64 | `Nugget_<v>_amd64.deb` | 34.1 MiB | `/usr/bin/nugget-desktop` 23.4 MiB, `/usr/bin/nuggetd` 71.1 MiB |
| Linux x64 | `Nugget-<v>-1.x86_64.rpm` | 34.1 MiB | same layout as the `.deb` |
| Linux arm64 | AppImage / `.deb` / `.rpm` | 102.6 / 33.2 / 33.2 MiB | `nuggetd` 64.1 MiB |

Measured from release dry run [40812638488](https://github.com/rayhankhilji/nugget/actions/runs/40812638488)
(version 0.0.1) on 2026-10-29. Every installer puts `nuggetd` next to `nugget-desktop`, as in
§2. `nuggetd` is three quarters of every bundle; the release profile keeps symbol tables
(no `strip`), so `strip = "symbols"` in `[profile.release]` is the first thing to try if size
matters. Each dry run writes the same tables to its job summary (`bundle-report-<target>`).
<!-- sizes:end -->

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

A `workflow_dispatch` run with `dry_run: true` (the default) builds unsigned bundles from any ref,
uploads them as workflow artifacts, and creates no release; see *Dry run* below.

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

Put every signing secret in its **environment**, never at repository level. A repository secret
is readable by any workflow on any branch; an environment secret only by a job that deploys to
that environment, which the `v*` tag rule limits to release tags.

### Secrets and variables checklist

| Name | Kind, where | Read by | What it is, how to make it | When absent |
|---|---|---|---|---|
| `APPLE_CERTIFICATE` | secret, `release-build` | `build` (macOS) | *Developer ID Application* certificate with its private key, exported from Keychain Access as `.p12`, then `base64 -i cert.p12` | Ad-hoc signed (`signingIdentity: "-"`), not notarized, warning in the log. Gatekeeper blocks it on users' machines. |
| `APPLE_CERTIFICATE_PASSWORD` | secret, `release-build` | `build` (macOS) | The password set on the `.p12` export | Needed whenever `APPLE_CERTIFICATE` is set; the import fails without it. |
| `APPLE_SIGNING_IDENTITY` | secret, `release-build` | `build` (macOS) | e.g. `Developer ID Application: Name (TEAMID)`; list with `security find-identity -v -p codesigning` | Optional in the workflow; set it if Tauri can't pick the identity from the imported certificate. |
| `APPLE_API_ISSUER` | secret, `release-build` | `build` (macOS) | App Store Connect → Users and Access → Integrations → Team Keys: the Issuer ID | Signed but not notarized, warning. All three API values are needed to notarize. |
| `APPLE_API_KEY_ID` | secret, `release-build` | `build` (macOS) | The Key ID of a team API key (role *Developer* is enough) | As above. |
| `APPLE_API_KEY_P8` | secret, `release-build` | `build` (macOS) | The text of `AuthKey_<id>.p8` (plain PEM, not base64). It can be downloaded only once. | As above. |
| `WINDOWS_CERTIFICATE` | secret, `release-build` | `build` (Windows) | Authenticode code-signing certificate with private key as `.pfx`, then `base64 -w0 cert.pfx` | Installers, app and sidecar unsigned, warning. SmartScreen warns users. |
| `WINDOWS_CERTIFICATE_PASSWORD` | secret, `release-build` | `build` (Windows) | The `.pfx` password | Needed whenever `WINDOWS_CERTIFICATE` is set; the import fails without it. |
| `TAURI_SIGNING_PRIVATE_KEY` | secret, `release` | `sign` | Updater minisign key, from the key ceremony (§4) | The draft is created without `.sig` files or `latest.json` (warning); `channels` then leaves the feeds unchanged, so installed apps never see the release. |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret, `release` | `sign` | The password chosen at `signer generate` | With the key set, `tauri signer sign` fails and the job stops. |
| `NUGGET_UPDATER_PUBKEY` | repository variable | `build` (tag pushes only), `sign` | The text of `nugget-updater.key.pub` | `build`: the app is compiled with no updater ("this build can't update itself"). `sign`: treated like a missing key. |
| `GITHUB_TOKEN` | automatic | `sign`, `channels` | Scoped by each job's `permissions` | n/a |

`NUGGET_UPDATER_FEED` is an optional build-time variable that overrides the feed base URL
(`updater.rs`); CI doesn't set it, so release builds use the `updater-feed` release.

**Windows certificate caveat.** Since mid-2023, CAs issue OV and EV code-signing keys only on
hardware (a USB token or an HSM), so an exportable `.pfx` is no longer the normal case. The
realistic options for CI are a cloud signing service (Azure Trusted Signing, DigiCert KeyLocker,
SSL.com eSigner and similar), which needs a custom `bundle.windows.signCommand` instead of the
`.pfx` import above. That path isn't implemented. An EV certificate no longer skips SmartScreen
reputation either; new publishers see warnings until downloads build reputation.

**Manifest signing keys.** Besides the updater key, the repository has two ed25519 signing
schemes. Neither uses a CI secret today:

- **Model and runtime manifest** (`manifests/default.json` + `default.json.sig`, verified by
  `nugget-models`). The manifest is compiled into `nuggetd` (`include_bytes!`) and checked at
  daemon start against the keys returned by `trusted_keys()`, which is only the **development**
  public key `DEV_PUBLIC_KEY_HEX` in `crates/nugget-models/src/manifest.rs`. A maintainer signs it
  locally whenever the manifest changes:

  ```sh
  cargo run -p nugget-models --example sign-manifest -- --generate-key ~/.config/nugget/manifest-dev.key  # once
  NUGGET_MANIFEST_SIGNING_KEY_FILE=~/.config/nugget/manifest-dev.key \
    cargo run -p nugget-models --example sign-manifest -- manifests/default.json
  cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json
  ```

  (`NUGGET_MANIFEST_SIGNING_KEY`, 64 hex chars, works instead of the file.) A stale or wrong
  signature fails the `nugget-models` tests in CI, and at runtime the daemon logs "bundled model
  manifest failed verification" and offers no model downloads. There is **no release manifest
  key** yet: releases ship trusting the development key (see §8).
- **Plugin packages** (`plugin.sig`, `nugget-plugin`). There is no project-wide plugin key. Users
  list the publishers they trust in `[plugins] trusted_keys`; authors sign with
  `cargo run -p nugget-plugin --example sign-plugin` and `NUGGET_PLUGIN_SIGNING_KEY` (or
  `_FILE`). Nothing in the release pipeline touches plugin keys.

The channel feeds (`beta.json`, `stable.json`) and `latest.json` are not signed as a whole; only
the payloads they point at are (§8).

### Dry run

Use it to check that installers build, before a tag or after changing packaging.

```sh
gh workflow run release.yml --ref <branch> -f dry_run=true
```

or Actions → **Release** → *Run workflow*, pick the branch, leave *dry_run* ticked.

With `dry_run: true` (the default) the workflow, from any branch:

- builds every target in the matrix (§1) with the `nuggetd` sidecar: macOS `.app`/`.dmg` (ad-hoc
  signed), Windows NSIS and MSI (unsigned), Linux AppImage, `.deb` and `.rpm`;
- uploads each target's installers as the workflow artifact `bundle-<target>` (kept 14 days),
  plus `bundle-report-<target>`, a size and sidecar-layout report that also appears in the job
  summary;
- runs the AppImage smoke test (below) on Linux x64;
- never enters the `release-build` or `release` environment, never reads a signing secret or
  `NUGGET_UPDATER_PUBKEY`, and never signs, notarizes, drafts or publishes anything. The apps it
  builds have no updater.

`dry_run: false` is accepted only on a `v*` tag ref. It re-runs a real release for that tag, with
the same environments, approvals and draft as a tag push. On any other ref the workflow fails at
the start.

Installing a dry-run build by hand:

- **macOS:** Gatekeeper says the app is from an *unidentified developer* (or "is damaged" for a
  downloaded quarantined copy). Right-click the app → *Open* → *Open*, or clear the quarantine flag:
  `xattr -dr com.apple.quarantine /Applications/Nugget.app`.
- **Windows:** SmartScreen shows *Windows protected your PC*: *More info* → *Run anyway*.
- **Linux:** no prompt; `chmod +x` the AppImage.

### Smoke test

`.github/workflows/package-smoke.yml` runs `packaging/scripts/smoke-appimage.sh` on pull requests
that touch `apps/desktop/`, `crates/` or `packaging/`, and the release dry run runs the same script
on Linux x64 against the AppImage that run uploads. The workflow builds the AppImage with the
sidecar staged as in §2; the script then:

1. starts it with throwaway `HOME`, XDG and `NUGGET_HOME` directories (`NUGGET_TRAY=off`);
2. runs it under Xvfb with the openbox window manager;
3. waits, with a time limit, for a window named `Nugget`, the app log line `connected to nuggetd`
   and the daemon's own log (`nuggetd ready`). The app must log `starting nuggetd` itself: it starts
   the bundled sidecar at launch when no daemon answers;
4. closes the window through the window manager (`wmctrl`, a `WM_DELETE_WINDOW` request, like a
   user clicking ×) and expects the app to exit with status 0;
5. sends SIGTERM to the daemon it started.

Any step that times out fails the job. macOS and Windows installers have no automated launch
test yet.

### Cutting a release

1. **Dry run first** on the commit you intend to tag (above). All required targets green, smoke
   test passed, sizes sane.
2. **Bump the version** in one PR: `version` in `apps/desktop/src-tauri/tauri.conf.json`,
   `[workspace.package] version` in the root `Cargo.toml` and the internal crate versions under
   `[workspace.dependencies]` next to it (`cargo-deny` needs them in step), and
   `apps/desktop/package.json`. Run `cargo update -w` so `Cargo.lock` follows. Merge.
3. **Tag** the merge commit with an annotated, signed tag. The message is the release notes:

   ```sh
   git tag -s v0.1.0 -m "Nugget 0.1.0

   - what changed, for users"
   git push origin v0.1.0
   ```

   Use `vX.Y.Z-beta.N` for a beta. A hyphen makes the release a prerelease, which only the beta
   channel receives.
4. **Watch `build` and `sbom`.** The macOS jobs log whether they signed and notarized, and Windows
   whether it signed; a warning there means a missing secret. Linux arm64 may fail without
   blocking.
5. **Approve `sign`.** A second maintainer (not the tagger) reviews the run and approves the
   `release` environment.
6. **Review the draft** on the Releases page: every installer is attached with its `.sig`,
   `latest.json` lists each platform, `SHA256SUMS` matches (`sha256sum -c SHA256SUMS` after
   downloading), the notes are right, and an installer from it installs and starts on at least
   one OS. Check one provenance attestation with `gh attestation verify`.
7. **Publish.** That triggers `channels`, which moves the feeds. Check the run log for
   `beta -> vX` (and `stable -> vX`).

### How the update feed moves

```
tag push ─► build ─► sign (approved) ─► draft release ─► Publish ─► channels ─► updater-feed/beta.json
                                                                              └► updater-feed/stable.json (not prereleases)
```

- Nothing reaches users before *Publish*. A draft never triggers `channels`.
- `channels` downloads the published release's `latest.json` and, for each channel it applies to,
  runs `updater_manifest.py promote latest.json current.json`. That prints the new version only if
  it is **strictly newer** (SemVer 2.0 precedence; build metadata ignored) than the version the
  feed already holds, or if the feed is empty. Otherwise the feed stays as it is and the job logs
  `keeping <current>: <new> is not newer`. So publishing an older patch, or re-publishing the same
  version, never moves a feed.
- A release without `latest.json` (no updater key) leaves both feeds unchanged.
- Installed apps also refuse to go backwards: the updater only offers a version newer than the one
  running.

**Pulling a bad release.** Neither the pipeline nor the app can downgrade anyone, so the fix for
people who already installed it is always a **newer** release. To stop it spreading meanwhile:

1. Point the feed back at the last good release by hand. `promote` would refuse this, so
   upload directly:

   ```sh
   gh release download vGOOD -p latest.json -O stable.json
   gh release upload updater-feed --clobber stable.json      # likewise beta.json if needed
   ```

   Apps still on the good version then see no update. Apps already on the bad version stay on it.
2. Mark the bad release as a prerelease or take it back to draft (`gh release edit vBAD --draft`)
   so nobody downloads it from the Releases page. Don't delete its assets while a feed still
   points at them, or updates fail with a download error instead.
3. Fix, bump to the next patch version and release as usual. `promote` accepts it because it is
   newer than both the bad and the restored version.

If the bad release was signed with a compromised key, follow *Rotation* in §4 instead.

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
- No release key for the model manifest: `trusted_keys()` returns only the development key, so a
  release build trusts whoever holds that key. A release key, held offline like the updater
  key, should replace it before 1.0. The `version` serial in the manifest is documented as
  rollback protection, but since the manifest is only ever bundled, nothing checks it yet.
- Windows signing only supports an exportable `.pfx`. Hardware-backed and cloud signing (the normal
  case for new certificates) needs a `signCommand` path that isn't written.
- No automated launch test for the macOS or Windows installers; only the Linux AppImage is
  smoke-tested.
