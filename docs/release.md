# Releases, installers and signed updates

The **checklist** creates every signing key and cuts a release, in order. The **reference**
(§1 to §8) explains packaging, the workflow, the keys and the in-app updater.

| Piece | Where |
|---|---|
| Bundle config | `apps/desktop/src-tauri/tauri.conf.json`; release overlay (sidecar) `tauri.bundle.conf.json` |
| Sidecar staging | `packaging/scripts/stage-sidecar.sh` |
| Icons | `packaging/icons/nugget.svg` → `packaging/scripts/icons.sh` → `apps/desktop/src-tauri/icons/` |
| Windows installer hooks / signing | `packaging/windows/installer-hooks.nsh`, `packaging/windows/sign.ps1` |
| Update manifest, channel promotion, checksums | `packaging/scripts/updater_manifest.py` |
| Release pipeline / rehearsal / Windows signing proof | `.github/workflows/release.yml`, `release-rehearsal.yml` (+ `packaging/scripts/rehearsal/`), `windows-signing.yml` |
| Installer smoke tests | `packaging/scripts/smoke-appimage.sh`, `smoke-macos.sh`, `smoke-windows.ps1` |
| Model manifest signing | `crates/nugget-models/examples/sign-manifest.rs`, [security/manifest-trust.md](security/manifest-trust.md) |
| In-app updater | `apps/desktop/src-tauri/src/updater.rs`, `settings.rs`, `login.rs`; UI `apps/desktop/src/components/UpdateSettings.tsx` |

Principles: the user picks the update channel, nothing installs without consent, and signing
happens only in CI behind a second maintainer's approval.

---

# Release checklist

Steps 1 to 5 happen once; 6 to 9 are each release. You need two maintainers with admin rights on
`rayhankhilji/nugget` (tagger and approver must differ), `gh` logged in, a Mac for Apple steps, an
Azure subscription (or a `.pfx`) for Windows, and a throwaway machine (fresh VM or live USB) for
the key ceremony. Commands assume `R=rayhankhilji/nugget`, run from a repository clone.

## Step 1. GitHub repository setup

- [ ] **Environments.** `release-build` (only `v*` tags, no reviewers; holds Apple secrets and
  Windows variables) and `release` (only `v*` tags, at least two required reviewers, *Prevent
  self-review*; holds the updater key):

  ```sh
  R=rayhankhilji/nugget
  gh api -X PUT repos/$R/environments/release-build --input - <<'EOF'
  {"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
  EOF
  gh api users/<maintainer-login> -q .id          # reviewer ids, once per reviewer
  gh api -X PUT repos/$R/environments/release --input - <<'EOF'
  {"prevent_self_review": true,
   "reviewers": [{"type": "User", "id": 1111111}, {"type": "User", "id": 2222222}],
   "deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
  EOF
  for e in release-build release; do
    gh api -X POST repos/$R/environments/$e/deployment-branch-policies -f name='v*' -f type=tag
  done
  ```

- [ ] **Tag ruleset** (Settings → Rules → Rulesets → *New tag ruleset*): tags `v*`, *Active*,
  *Restrict creations/updates/deletions*, maintainers in the bypass list.
- [ ] **Signed tags**: each tagger sets up GPG or SSH tag signing and adds the key to GitHub as a
  signing key. GitHub doesn't refuse unsigned tags; the approver checks with `git tag -v`.
- [ ] Never put a signing secret at repository level; environment secrets are readable only by
  jobs deploying to that environment.

## Step 2. Apple: Developer ID certificate and notarization key

Without these the macOS app is ad-hoc signed, not notarized, and Gatekeeper blocks it (the build
still succeeds, with a warning).

- [ ] Create a **Developer ID Application** certificate (Account Holder role; CSR from Keychain
  Access) and export it **with its private key** as `.p12`. Create an **App Store Connect API
  key** (Users and Access → Integrations → Team Keys, access *Developer*); the `.p8` downloads once.

  ```sh
  base64 -i DeveloperID.p12 | gh secret set APPLE_CERTIFICATE --env release-build -R $R
  gh secret set APPLE_CERTIFICATE_PASSWORD --env release-build -R $R      # the .p12 password
  security find-identity -v -p codesigning
  gh secret set APPLE_SIGNING_IDENTITY --env release-build -R $R --body 'Developer ID Application: <Name> (<TEAMID>)'
  gh secret set APPLE_API_ISSUER --env release-build -R $R --body '<issuer-uuid>'
  gh secret set APPLE_API_KEY_ID --env release-build -R $R --body '<key-id>'
  gh secret set APPLE_API_KEY_P8 --env release-build -R $R < AuthKey_<key-id>.p8   # plain PEM
  ```

- [ ] Delete the `.p12` and `.p8` files. All three API values are needed to notarize.

## Step 3. Windows: Azure Trusted Signing

`sign.ps1` runs once per file via Tauri's `bundle.windows.signCommand` (§4). The service is
Azure Trusted Signing (renamed Artifact Signing). CI logs in with GitHub OIDC, so no Azure secret
is stored. Without this (or a `.pfx`) installers are unsigned and SmartScreen warns.

- [ ] **Account**: `az provider register --namespace Microsoft.CodeSigning`, then create a
  Trusted Signing account. Its *Account URI* is the endpoint (e.g.
  `https://eus.codesigning.azure.net`).
- [ ] **Identity validation**: give yourself *Trusted Signing Identity Verifier*, then
  *Identity validation* → *New* → *Public*. Can take days; eligibility is limited. The validated
  name is the publisher users see.
- [ ] **Certificate profile**: *Public Trust*, linked to the validated identity.
- [ ] **Entra app, federated credential, Signer role on the one profile only, and the five
  `release-build` variables** (all or none; a partial set fails the build). If the repository's
  OIDC subject template is customised, match it.

  ```sh
  az ad app create --display-name nugget-release-signing --query appId -o tsv   # = AZURE_CLIENT_ID
  az ad sp create --id <appId>
  az account show --query tenantId -o tsv                                        # = AZURE_TENANT_ID
  az ad app federated-credential create --id <appId> --parameters '{
    "name": "github-release-build",
    "issuer": "https://token.actions.githubusercontent.com",
    "subject": "repo:rayhankhilji/nugget:environment:release-build",
    "audiences": ["api://AzureADTokenExchange"]}'
  az role assignment create --assignee <appId> \
    --role "Trusted Signing Certificate Profile Signer" \
    --scope "/subscriptions/<sub-id>/resourceGroups/<rg>/providers/Microsoft.CodeSigning/codeSigningAccounts/<account>/certificateProfiles/<profile>"
  gh variable set AZURE_TRUSTED_SIGNING_ENDPOINT --env release-build -R $R --body 'https://eus.codesigning.azure.net'
  gh variable set AZURE_TRUSTED_SIGNING_ACCOUNT  --env release-build -R $R --body '<account>'
  gh variable set AZURE_TRUSTED_SIGNING_PROFILE  --env release-build -R $R --body '<profile>'
  gh variable set AZURE_CLIENT_ID                --env release-build -R $R --body '<appId>'
  gh variable set AZURE_TENANT_ID                --env release-build -R $R --body '<tenantId>'
  ```

- [ ] Optional manual test on Windows with your own `az login` (needs the Signer role too):
  `sign.ps1 -InstallDlib <dir>` prints the plugin path for `AZURE_TRUSTED_SIGNING_DLIB`; set the
  endpoint, account and profile, then `pwsh packaging/windows/sign.ps1 test.exe`.

**Alternative: exportable `.pfx`** (rare now; Trusted Signing wins if both are set):
`base64 -w0 cert.pfx | gh secret set WINDOWS_CERTIFICATE --env release-build -R $R` and
`gh secret set WINDOWS_CERTIFICATE_PASSWORD --env release-build -R $R`.

## Step 4. Key ceremony: updater key

The updater key signs every payload and the feed. Apps trust exactly the built-in public key:
losing the private key ends updates, leaking it lets anyone ship one. Two maintainers present; do
Step 5 in the same session.

- [ ] Prepare the throwaway machine **online** (Node 22 + pnpm, `gh`, `gfshare` from
  `libgfshare-bin`, Rust, a clone), generate **offline** with a strong password, split key and
  password 2-of-3 (one share per maintainer, offline; `ssss-split` can't take the key file), then
  store it **online**:

  ```sh
  pnpm dlx @tauri-apps/cli@2.11.5 --version          # online: caches the pinned CLI
  cargo build --locked -p nugget-models --example sign-manifest
  pnpm dlx @tauri-apps/cli@2.11.5 signer generate -w ./nugget-updater.key   # offline
  gfsplit -n 2 -m 3 nugget-updater.key              # nugget-updater.key.NNN ×3
  printf '%s' '<password>' > updater-password.txt && gfsplit -n 2 -m 3 updater-password.txt
  gfcombine -o check.key nugget-updater.key.<a> nugget-updater.key.<b> && cmp check.key nugget-updater.key
  gh secret set TAURI_SIGNING_PRIVATE_KEY --env release -R $R < ./nugget-updater.key   # online
  gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --env release -R $R
  gh variable set NUGGET_UPDATER_PUBKEY -R $R < ./nugget-updater.key.pub
  ```

- [ ] Paste `nugget-updater.key.pub` into [Public keys](#public-keys) in a PR.
- [ ] After Step 5, destroy the machine.

## Step 5. Manifest release key

`manifests/default.json` (pins every model and runtime download) is signed with its own ed25519
key (§4). **Release builds trust only this key**, so until the manifest carries a release
signature a released `nuggetd` offers no model downloads.

- [ ] On the same offline machine, then (with the public `.sig` copied off) online:

  ```sh
  SM=target/debug/examples/sign-manifest
  $SM --generate-key ./manifest-release.key          # prints "public key: <64 hex>"
  $SM --append --key-file ./manifest-release.key manifests/default.json
  $SM --verify manifests/default.json --pubkey <64-hex-public-key>   # always pass --pubkey here
  grep -c . manifests/default.json.sig               # 2: dev + release signature
  gfsplit -n 2 -m 3 manifest-release.key
  gh variable set NUGGET_MANIFEST_RELEASE_PUBKEY -R $R --body '<64-hex-public-key>'   # online
  ```

- [ ] Commit the two-line `.sig` and add the key to [Public keys](#public-keys) in one PR. Check:
  `cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json --pubkey "$(gh variable get NUGGET_MANIFEST_RELEASE_PUBKEY -R $R)"`.
- [ ] Whenever `default.json` changes, redo both signatures: dev first (it replaces the file),
  then append the release one (two maintainers rebuild the key from shares).

## Step 6. Prove the commit you will tag

On the exact version-bump merge commit; tag only when all are green.

```sh
gh workflow run release.yml --ref main -f dry_run=true     # unsigned installers + smoke tests
gh workflow run release-rehearsal.yml --ref main            # full release with throwaway keys (§3)
gh workflow run windows-signing.yml --ref main              # only if packaging/windows/ changed
git rev-parse origin/main
gh run list -R $R -w release.yml -L 1 --json headSha,conclusion,url
gh run list -R $R -w release-rehearsal.yml -L 1 --json headSha,conclusion,url
```

All required targets green (Linux arm64 may fail), all three smoke tests passed, sizes close to §2.

## Step 7. Release candidate `v1.0.0-rc.1`

The first tag is the first real test of every signing path (§8), so use a prerelease, which only
the beta channel receives.

- [ ] **Bump the version** in one PR: `version` in `apps/desktop/src-tauri/tauri.conf.json`;
  `[workspace.package] version` and every internal crate's version under
  `[workspace.dependencies]` in the root `Cargo.toml`; `version` in `apps/desktop/package.json`;
  then `cargo update -w`. The MSI version is derived automatically (§1).
- [ ] Merge, then Step 6 on the merge commit.
- [ ] **Tag** (signed, annotated; the message is the release notes):

  ```sh
  git switch main && git pull --ff-only
  git tag -s v1.0.0-rc.1 -m "Nugget 1.0.0-rc.1

  Release candidate. - what changed, for users"
  git push origin v1.0.0-rc.1
  ```

- [ ] **Watch `build` and `sbom`** (`gh run watch`). macOS logs whether it signed and notarized;
  Windows logs its signing mode and `sign.ps1: signed …` per file (about 13). A `::warning::`
  about a missing certificate or key means a secret is missing: fix it, delete tag and draft,
  re-tag.
- [ ] **Approve `sign`**: a second maintainer runs `git tag -v v1.0.0-rc.1`, reviews the run, and
  approves the `release` environment.
- [ ] Step 8, then **Publish** the draft (prerelease). `channels` must log `beta -> v1.0.0-rc.1`.
- [ ] Install rc.1 on each OS on the **Beta** channel; the first real in-app update is rc.1 → 1.0.0.

## Step 8. Check the draft and signatures

`gh release download v1.0.0-rc.1 -R $R -D rc1`, then:

- [ ] **Contents**: every installer of §1 (arm64 may be missing), a `.sig` per updater payload,
  `latest.json` + `.sig`, `SHA256SUMS`, three SBOMs; no bundle reports or smoke logs.
- [ ] `sha256sum -c SHA256SUMS` and `gh attestation verify Nugget_1.0.0-rc.1_amd64.AppImage -R $R`.
- [ ] **Updater signatures** (the `sign` job already checks them with
  `packaging/scripts/check_release_keys.py`) and **macOS** (both architectures):

  ```sh
  gh variable get NUGGET_UPDATER_PUBKEY -R $R | base64 -d > updater.pub
  base64 -d Nugget_1.0.0-rc.1_amd64.AppImage.sig > appimage.minisig
  minisign -V -p updater.pub -m Nugget_1.0.0-rc.1_amd64.AppImage -x appimage.minisig
  base64 -d latest.json.sig > latest.minisig && minisign -V -p updater.pub -m latest.json -x latest.minisig
  codesign --verify --deep --strict --verbose=2 /Applications/Nugget.app
  codesign -dv --verbose=4 /Applications/Nugget.app/Contents/MacOS/nuggetd   # Authority=Developer ID Application: …
  spctl -a -vv /Applications/Nugget.app                                      # source=Notarized Developer ID
  xcrun stapler validate /Applications/Nugget.app
  ```

- [ ] **Windows** (each *Valid*, the validated signer, timestamped):

  ```powershell
  signtool verify /pa /all /v Nugget_1.0.0-rc.1_x64-setup.exe
  signtool verify /pa /all /v Nugget_1.0.0-rc.1_x64_en-US.msi
  Get-AuthenticodeSignature "$env:LOCALAPPDATA\Nugget\nugget-desktop.exe", "$env:LOCALAPPDATA\Nugget\nuggetd.exe", "$env:LOCALAPPDATA\Nugget\uninstall.exe" |
    Format-Table Status, @{n='Signer';e={$_.SignerCertificate.Subject}}, @{n='Timestamp';e={$_.TimeStamperCertificate.Subject}}
  ```

- [ ] **Model manifest**: the daemon log must not say "bundled model manifest failed verification".
- [ ] Each OS installs and starts; the notes are right.

## Step 9. Cut v1.0.0

- [ ] Bump to `1.0.0` as in Step 7, merge, Step 6.
- [ ] `git tag -s v1.0.0 -m "Nugget 1.0.0 …"` and `git push origin v1.0.0`.
- [ ] Watch, approve `sign` (second maintainer, `git tag -v`), Step 8, Publish.
- [ ] `channels` logs `beta -> v1.0.0` and `stable -> v1.0.0`; verify the feed:
  `gh release download updater-feed -R $R -p 'stable.json*' -D feed`, then `minisign` as above.
- [ ] An installed rc.1 on beta offers v1.0.0 on *Check now*, installs it and restarts.
- [ ] Update the README, `docs/book/src/guide/install.md` ("no release yet" note) and `docs/PLAN.md`.

**Later releases** repeat Steps 6, 7 and 9 (`vX.Y.Z`, or `vX.Y.Z-beta.N`). If
`manifests/default.json` changed, redo its signatures first (Step 5).

---

# Secrets and variables

Everything `release.yml` reads. Dry runs, the rehearsal and the signing proof read none.

| Name | Kind, where | What | When absent |
|---|---|---|---|
| `APPLE_CERTIFICATE`, `_PASSWORD` | secret, `release-build` | Developer ID `.p12` (base64) and its password | Ad-hoc signed, not notarized, warning |
| `APPLE_SIGNING_IDENTITY` | secret, `release-build` | `Developer ID Application: Name (TEAMID)` | Optional; taken from the certificate |
| `APPLE_API_ISSUER`, `APPLE_API_KEY_ID`, `APPLE_API_KEY_P8` | secret, `release-build` | Notarization API key (`.p8` as plain PEM) | Signed but not notarized, warning |
| `AZURE_TRUSTED_SIGNING_ENDPOINT`, `_ACCOUNT`, `_PROFILE`, `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` | variable, `release-build` | Trusted Signing config (Step 3) | None: `.pfx` or unsigned with warning. Some: build fails |
| `WINDOWS_CERTIFICATE`, `_PASSWORD` | secret, `release-build` | Exportable `.pfx` (base64) | Unsigned with warning; ignored if Azure is set |
| `TAURI_SIGNING_PRIVATE_KEY` | secret, `release` | Updater minisign key (Step 4) | No `.sig`/`latest.json`; feeds don't move |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret, `release` | Its password | `sign` fails |
| `NUGGET_UPDATER_PUBKEY` | repo variable | `nugget-updater.key.pub` text | App built without an updater; `sign` treats as no key |
| `NUGGET_MANIFEST_RELEASE_PUBKEY` | repo variable | 64-hex manifest release key (Step 5) | Real release fails in `mode` before building, also if no `.sig` line verifies with it; malformed fails the build |
| `GITHUB_TOKEN` | automatic | Scoped per job | n/a |

Set by the workflow: `NUGGET_TRUST_DEV_MANIFEST_KEY=1` (dry runs only), `AZURE_TRUSTED_SIGNING_DLIB`.
`NUGGET_UPDATER_FEED` overrides the feed URL at build time (the rehearsal uses it; releases don't).

---

# Reference

## 1. Bundle matrix

| Platform | Runner | Target | Installers | Updater payload |
|---|---|---|---|---|
| macOS Apple silicon | `macos-15` | `aarch64-apple-darwin` | `.app`, `.dmg` | `Nugget_<v>_aarch64.app.tar.gz` |
| macOS Intel | `macos-15-intel` | `x86_64-apple-darwin` | `.app`, `.dmg` | `Nugget_<v>_x64.app.tar.gz` |
| Windows x64 | `windows-2022` | `x86_64-pc-windows-msvc` | NSIS `-setup.exe` (per-user), MSI | both |
| Linux x64 | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | AppImage, `.deb`, `.rpm` | all three |
| Linux arm64 | `ubuntu-22.04-arm` | `aarch64-unknown-linux-gnu` | AppImage, `.deb`, `.rpm` | all three (may fail without blocking) |

- macOS minimum is 11.0 (`bundle.macOS.minimumSystemVersion`). Linux needs glibc 2.35; packages
  depend on WebKitGTK 4.1, GTK 3, libayatana-appindicator.
- A job that never gets a runner (no log, `runner_id` 0) may mean Actions minutes or the
  spending limit ran out: check Settings → Billing.
- The updater uses the payload matching how the app was installed (AppImage, `.deb` with a
  `pkexec` prompt, NSIS, …).
- MSI versions must be numeric: `packaging/windows/wix-version.ps1` maps `X.Y.Z-rc.N` to
  `X.Y.Z.N`. Windows Installer ignores the fourth field, so the final release still replaces an RC.

## 2. The `nuggetd` sidecar

The daemon ships as a Tauri sidecar (`bundle.externalBin: ["binaries/nuggetd"]`): staged as
`apps/desktop/src-tauri/binaries/nuggetd-<target-triple>[.exe]`, installed next to the app:

| Bundle | Daemon |
|---|---|
| macOS | `Nugget.app/Contents/MacOS/nuggetd` (signed with the app) |
| Windows NSIS / MSI | `%LOCALAPPDATA%\Nugget\nuggetd.exe` / `C:\Program Files\Nugget\nuggetd.exe` |
| deb / rpm | `/usr/bin/nuggetd` |
| AppImage | `$APPDIR/usr/bin/nuggetd` |

`daemon::find_nuggetd()` looks next to `current_exe()` first. The sidecar is in an overlay config
because `tauri-build` refuses to compile when an `externalBin` file is missing.

Local build:

```sh
packaging/scripts/stage-sidecar.sh            # host triple, or pass one
cd apps/desktop
pnpm install --frozen-lockfile
pnpm tauri build --bundles appimage,deb,rpm --config src-tauri/tauri.bundle.conf.json
ls ../../target/release/bundle/*/
```

A local build has no updater ("this build can't update itself") and trusts no manifest key
unless `NUGGET_TRUST_DEV_MANIFEST_KEY=1` was exported for `stage-sidecar.sh`. To test updates,
`pnpm tauri signer generate -w /tmp/test.key` and export
`NUGGET_UPDATER_PUBKEY="$(cat /tmp/test.key.pub)"` before building, or use the rehearsal.

Sizes from unsigned dry-run bundles (version 0.0.1); each dry run writes the same tables to its
job summary (`bundle-report-<target>`):

<!-- sizes:begin -->
| Platform | Installers (size) | Notes |
|---|---|---|
| macOS aarch64 / x64 | dmg 26.5 / 28.9 MiB; `.app.tar.gz` 26.5 / 29.0 MiB | `.app` 67.8 / 76.4 MiB on disk; `nuggetd` 52.4 / 59.5 MiB |
| Windows x64 | NSIS 19.4 MiB; MSI 28.1 MiB | unpacks to 76.7 MiB (`nugget-desktop.exe` 17.8, `nuggetd.exe` 58.8) |
| Linux x64 | AppImage 105.4 MiB (own GTK/WebKitGTK); deb, rpm 34.1 MiB | `nugget-desktop` 23.4 MiB, `nuggetd` 71.1 MiB |
| Linux arm64 | AppImage 102.6 MiB; deb, rpm 33.2 MiB | `nuggetd` 64.1 MiB |

`nuggetd` is three quarters of every bundle; the release profile keeps symbols, so
`strip = "symbols"` in `[profile.release]` is the first thing to try if size matters.
<!-- sizes:end -->

## 3. Release workflow

`release.yml` has top-level `permissions: {}`; each job asks only for what it needs; actions are
pinned to commit SHAs.

1. **Tag** (annotated, signed; must match the `tauri.conf.json` version).
2. **`build`** (matrix; `release-build` env on real releases): stages the sidecar, runs
   `tauri build`, signs macOS (hardened runtime, notarized and stapled with the API key) and
   Windows (`sign.ps1` or the `.pfx`); a real release fails if `signtool verify /pa /all` finds an
   unsigned file. **The updater private key is never here** (third-party build scripts run), only
   public keys. Uploads `bundle-`, `bundle-report-` and `smoke-<target>`.
3. **`sbom`**: CycloneDX via `cargo-cyclonedx` and `cdxgen`.
4. **`sign`** (`release` env, waits for approval): signs each payload with `tauri signer sign`
   (locked CLI, `--ignore-scripts`), writes and signs `latest.json`, writes `SHA256SUMS`, records
   SLSA provenance, and creates a **draft** release (prerelease if the tag has a hyphen).
5. **Publish** by a maintainer after Step 8.
6. **`channels`** (on publish, no secrets): copies `latest.json` + `.sig` to the `updater-feed`
   release as `beta.json` and, unless a prerelease, `stable.json` (§5).

### Dry run

```sh
gh workflow run release.yml --ref <branch> -f dry_run=true
```

From any branch: builds every target (macOS ad-hoc signed, Windows unsigned), uploads bundles,
reports and smoke logs (kept 14 days), runs the smoke tests, and builds `nuggetd` with
`NUGGET_TRUST_DEV_MANIFEST_KEY=1`. It never enters a signing environment or reads a secret, and
the apps have no updater. `dry_run: false` is accepted only on a `v*` tag ref (re-runs a real
release). Installing a dry-run build: see the
[install guide](book/src/guide/install.md#dry-run-installers).

### Smoke tests

`smoke-appimage.sh` (Linux x64, under Xvfb), `smoke-macos.sh` (aarch64 dmg) and
`smoke-windows.ps1` (NSIS, silent install and uninstall) launch the app with throwaway
`HOME`/`NUGGET_HOME`, wait for a `Nugget` window, `starting nuggetd` and `connected to nuggetd`,
close it as a user would, expect a clean exit, stop the daemon (SIGTERM; `CTRL_BREAK` on Windows)
and require `stopped`. `package-smoke.yml` and `real-os.yml` run them on relevant pull requests.
The x64 dmg, MSI, `.deb` and `.rpm` are built but not launched.

### Release rehearsal

`gh workflow run release-rehearsal.yml --ref <branch>` (manual only). On Linux x64 (AppImage) and
Windows x64 (NSIS) it: generates throwaway updater and manifest keys plus wrong ones (never
stored; the report fails if evidence holds a private key); builds `nuggetd` trusting only the
throwaway manifest key; builds an old and a new app pointed at a localhost HTTPS feed; signs and
promotes the new one to beta as `sign`/`channels` would; lets the old app find, verify, download
and install it (AppImage byte-identical; NSIS ProductVersion updated); and checks that a feed,
payload or (Linux) manifest signed with a wrong key is refused. Evidence is uploaded as
`rehearsal-evidence-<os>`. Limits in §8.

### Windows signing proof

`windows-signing.yml` (manual, and on PRs touching `packaging/windows/`) uses a throwaway
self-signed certificate. `sign-script` tests `sign.ps1` modes and failure paths (partial Azure
config, foreign endpoint, no secret printed); `bundles` builds NSIS and MSI as `release.yml` does
and checks every signed file fails before the throwaway root is trusted and verifies after. The
`azure-oidc` job (real OIDC token, fake Azure identity) runs only manually and **has not run yet**.

### Pulling a bad release

Nothing can downgrade anyone; the fix for affected users is always a newer release. Meanwhile:

1. Point the feed back at the last good release (`promote` would refuse, so upload directly):

   ```sh
   gh release download vGOOD -p latest.json -p latest.json.sig
   python3 packaging/scripts/updater_manifest.py channel latest.json stable   # likewise beta
   gh release upload updater-feed --clobber stable.json stable.json.sig
   ```

2. `gh release edit vBAD --draft` (or mark prerelease). Don't delete its assets while a feed
   still points at them.
3. Fix and release the next patch version.

If the key was compromised, follow *Rotation* in §4.

## 4. Signing keys

Release signatures: OS code signatures (Apple, Authenticode), updater signatures (minisign, payloads
and feed), and the model manifest signature (ed25519). Plugin packages use publisher keys that
users trust in `[plugins] trusted_keys`; the release pipeline doesn't touch them
([plugins.md](plugins.md#packaging-and-signing)).

### Updater key: the two-person process

minisign has no threshold signatures, so process enforces it: maintainer A tags, maintainer B
approves `release` (self-review blocked), and only published releases move feeds. The key exists
only as the CI secret and as 2-of-3 Shamir shares (key and password split separately); never on a
laptop or in a password manager.

**Rotation.** Ship release N built with the **new** `NUGGET_UPDATER_PUBKEY` but signed with the
**old** key; sign later releases with the new key. On compromise: revoke the secret, publish an
advisory, ship a rotation release at once.

### Manifest release key

`manifests/default.json` + `.sig` is compiled into `nuggetd` and verified at start. Build-time
variables: `NUGGET_MANIFEST_RELEASE_PUBKEY` (repo variable, passed to `stage-sidecar.sh`) and
`NUGGET_TRUST_DEV_MANIFEST_KEY=1` (dry runs only). Debug builds always trust the dev key. Key
slots, multi-signature files, rollback protection and the signing commands:
[security/manifest-trust.md](security/manifest-trust.md). A stale dev signature fails the
`nugget-models` tests; nothing in CI checks the release signature (Step 8 and the rehearsal do).

### Windows code signing

`sign.ps1` is called once per file (`%1`) and exits non-zero on failure; SHA-256 with RFC 3161
timestamps. Modes, first match wins:

- **Azure Trusted Signing**: `AZURE_TRUSTED_SIGNING_ENDPOINT` (`https://<region>.codesigning.azure.net`),
  `_ACCOUNT`, `_PROFILE`, `AZURE_TRUSTED_SIGNING_DLIB` (Microsoft's `Azure.CodeSigning.Dlib.dll`
  from `Microsoft.ArtifactSigning.Client` 1.0.146, fetched by `-InstallDlib`, checksum-pinned,
  needs .NET 8). Auth, first match: `AZURE_CLIENT_ID` + `AZURE_TENANT_ID` + `AZURE_CLIENT_SECRET`
  (manual only); `AZURE_CLIENT_ID` + `AZURE_TENANT_ID` with GitHub OIDC, a fresh token per file
  (CI); else `az login`.
- **Store certificate**: `NUGGET_SIGN_CERT_THUMBPRINT` (`CurrentUser\My`), timestamp
  `NUGGET_SIGN_TIMESTAMP_URL` (default DigiCert). The `.pfx` path in `release.yml` uses Tauri's
  own `certificateThumbprint` instead.

Optional: `NUGGET_SIGN_DESCRIPTION`, `NUGGET_SIGNTOOL` (or `TAURI_WINDOWS_SIGNTOOL_PATH`),
`NUGGET_SIGN_DRY_RUN=1`, `NUGGET_SIGN_LOG`. Other services (DigiCert KeyLocker, SSL.com eSigner,
a USB token on a self-hosted runner) need a new mode. EV certificates no longer skip SmartScreen
reputation.

### Public keys

- **Updater (minisign, `NUGGET_UPDATER_PUBKEY`):** *not generated yet (Step 4).*
- **Model manifest release key (ed25519, `NUGGET_MANIFEST_RELEASE_PUBKEY`):** *not generated yet (Step 5).*
- **Model manifest development key:** `DEV_PUBLIC_KEY_HEX` in `crates/nugget-models/src/manifest.rs`.

## 5. Signed update feed and updater behaviour

Settings live in `settings.json` in the app config dir (e.g. `~/.config/dev.nugget.desktop/`). UI:
the *Updates* card on the Status page; the tray has *Updates…* (*Update available: vX…*).

| Setting | Default | Effect |
|---|---|---|
| Channel | **not chosen** | Nothing contacts the server until the user picks. **Stable** follows `stable.json`, **Beta** `beta.json` (prereleases too), **Off** never checks. |
| Auto-install | **off** | Off: notify only; nothing downloads until *Install vX and restart*. On: download and verify in the background, install when the user quits. |

- **Opt-in.** While `channel_chosen` is false no check runs. Setup's last step and the *Updates*
  card ask once (Stable recommended / Beta / Off). Builds without an updater key never ask. Old
  settings files count as chosen only for Beta or Off.
- Checks run right after choosing, 60 s after launch, every 24 h, and on *Check now*. They fetch
  only static files, with nothing identifying beyond any HTTPS request.
- **Egress ledger.** Every request is recorded via `egress.record` (`update_check` for feed, `.sig`
  and the plugin's fetch, one row per host; `update_download` for packages). Rows queue while the
  daemon is down; rows still queued at quit are lost.
- **Feed verification.** The app fetches `<channel>.json` and `.sig` and checks the minisign
  signature over the exact bytes against the built-in key (`src/updater/feed.rs`) before parsing
  anything. It then accepts `tauri-plugin-updater`'s result only if the plugin saw the same feed
  ("the update feed changed while it was being checked" otherwise).
- Every payload is verified against the built-in key. *Install* uses the exact version whose
  notes were shown. Only newer versions are offered; beta → stable keeps the beta build until a
  newer stable exists.
- The webview has no `updater:` permission; everything goes through `updates_status`,
  `updates_set_settings`, `updates_check`, `updates_install`, `login_set`.
- Windows installs run in passive mode; NSIS hooks stop `nuggetd.exe` and restart the login task.

**How feeds move.** Nothing reaches users before *Publish* (drafts don't trigger `channels`).
`channels` runs `updater_manifest.py promote latest.json current.json`, which moves a feed only
to a **strictly newer** SemVer (or an empty feed), logging `keeping <current>: <new> is not newer`
otherwise; then `updater_manifest.py channel latest.json <ch>` writes `<ch>.json` + `.sig`
(refusing without a signature), uploaded together. The first run creates `updater-feed`. A
release without `latest.json` or its `.sig` leaves feeds unchanged. A check landing between the
two uploads fails and succeeds next time.

## 6. Start at login (daemon service)

`nuggetd install-service` / `uninstall-service` registers a per-user launchd agent,
`systemd --user` unit or Task Scheduler task "Nugget Daemon". Installers don't do it; the app
asks once on the Status page, and refuses where the daemon path is temporary (AppImage, macOS DMG
or App Translocation). Windows hooks stop the daemon before replacing files, restart the task
after install and remove it on uninstall (not during updates). On Linux the running daemon keeps
its old binary until `systemctl --user restart nuggetd` or the next login.

## 7. Reproducibility

Pinned: `Cargo.lock` (`--locked`), toolchain from `rust-toolchain.toml` (`stable`; use the `rustc -V`
in the build log; MSRV 1.88), `apps/desktop/pnpm-lock.yaml` (`--frozen-lockfile`), pnpm from
`packageManager`, Node 22, Tauri 2.11 crates, `tauri-plugin-updater =2.12.0`, CLI
`@tauri-apps/cli ~2.11.5`, `Microsoft.ArtifactSigning.Client` 1.0.146, actions by SHA, runner labels
above. Release builds apply `packaging/scripts/repro-env.sh` (`SOURCE_DATE_EPOCH`, path remapping),
use no build cache, and write the macOS tarball with fixed owner ids.

Only `nuggetd` on x86_64 Linux is verified reproducible (`reproducible.yml`); installers are not.
Details, commands and gaps: [security/reproducible-builds.md](security/reproducible-builds.md).

## 8. Known gaps

- **Signing paths are untested without real certificates.** Apple signing and notarization, Azure
  Trusted Signing, the updater key and the manifest release key have run only with throwaway keys
  (rehearsal, signing proof) or not at all (Apple, real Azure). The RC in Step 7 is their first
  real test. Notarizing the sidecar relies on Tauri signing `externalBin` with the app identity.
- **Build job OIDC exposure.** `build` has `id-token: write` on every leg, so on a real release any
  step, including third-party build scripts, can mint a `release-build` token and sign files with
  the Azure identity during the job; Apple secrets likewise reach later macOS steps via
  `GITHUB_ENV`. Mitigation: the profile-scoped role (Step 3). Fix not done: split compiling from
  bundling and signing.
- MSI: `-rc.N` and `-beta.N` with the same N get the same ProductVersion.
- `actionlint` was run without `shellcheck`; it flags only the `macos-15-intel` label on `release.yml`.
- Smoke tests don't launch the x64 dmg, MSI, `.deb` or `.rpm`.
- **Rehearsal limits**: only AppImage and NSIS update paths; no macOS; the Windows leg's feed may
  have CRLF endings, unlike the Linux `sign` job; localhost feed, not the real `updater-feed` URL.
- **Payload signatures aren't bound to a version**: no `--app-version` is passed to `tauri signer
  sign`, so `requireSignedVersion` can't be enabled (check the pinned CLI and plugin support it first).
- Linux packages aren't GPG-signed; no apt/rpm repository.
- Update ledger rows are written after each request; a crash mid-request leaves none.
- Feed signatures cover only bytes, not channel or date: whoever controls the feed URL can replay
  an older signed feed, serve beta to stable users, or withhold the feed (never install unsigned
  or older versions).
- The model manifest is only bundled; no download path for newer manifests yet.
