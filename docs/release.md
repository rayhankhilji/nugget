# Releases, installers and signed updates

This file has two parts. The **checklist** at the top is what a maintainer follows to create
every signing key and cut v1.0, in order, with the exact commands. The **reference** below it
(§1 to §8) explains how packaging, the release workflow, the keys and the in-app updater work.

| Piece | Where |
|---|---|
| Bundle config | `apps/desktop/src-tauri/tauri.conf.json` (base) and `tauri.bundle.conf.json` (release overlay: the sidecar) |
| Sidecar staging | `packaging/scripts/stage-sidecar.sh` |
| Icons | `packaging/icons/nugget.svg` → `packaging/scripts/icons.sh` → `apps/desktop/src-tauri/icons/` |
| Windows installer hooks | `packaging/windows/installer-hooks.nsh` |
| Windows code signing | `packaging/windows/sign.ps1` (Tauri `bundle.windows.signCommand`) |
| Update manifest, channel promotion, checksums | `packaging/scripts/updater_manifest.py` |
| Release pipeline | `.github/workflows/release.yml` |
| Release rehearsal (throwaway keys, end to end) | `.github/workflows/release-rehearsal.yml`, `packaging/scripts/rehearsal/` |
| Windows signing proof (throwaway certificate) | `.github/workflows/windows-signing.yml` |
| Installer smoke tests | `packaging/scripts/smoke-appimage.sh`, `smoke-macos.sh`, `smoke-windows.ps1` |
| Model manifest signing | `crates/nugget-models/examples/sign-manifest.rs`, `docs/security/manifest-trust.md` |
| In-app updater | `apps/desktop/src-tauri/src/updater.rs`, `settings.rs`, `login.rs`; UI in `apps/desktop/src/components/UpdateSettings.tsx` (on the Status page) |

Why it looks like this: Sentient's updates were mandatory and silent, its signing key sat with
one person, and it released by hand from a laptop (`docs/research/sentient-os-teardown.md` §2.9).
Nugget does the opposite: the user picks the channel, nothing installs without consent, and
signing happens only in CI, behind a second maintainer's approval.

---

# Release checklist: keys and v1.0

Do the steps in order. Steps 1 to 5 happen once; steps 6 to 9 are the release itself. Every
secret and variable named here is listed again in [Secrets and variables](#secrets-and-variables).

You need: two maintainers with admin rights on `rayhankhilji/nugget` (the tagger and the
approver must be different people), the GitHub CLI (`gh auth login`), a Mac for the Apple steps,
an Azure subscription for Windows signing (or a `.pfx`, see Step 3), and for the key ceremony a
throwaway machine (a fresh VM or a live USB). The commands below assume
`R=rayhankhilji/nugget` and are run from a clone of the repository unless they say otherwise.

## Step 1. GitHub repository setup

- [ ] **Environment `release-build`.** Only `v*` tags may deploy to it; no reviewers (the build
  must not wait). It holds the Apple secrets and the Windows signing variables.

  ```sh
  R=rayhankhilji/nugget
  gh api -X PUT repos/$R/environments/release-build --input - <<'EOF'
  {"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
  EOF
  gh api -X POST repos/$R/environments/release-build/deployment-branch-policies -f name='v*' -f type=tag
  ```

- [ ] **Environment `release`.** Only `v*` tags; **required reviewers** are the maintainers
  (at least two, so someone other than the tagger can approve), with **Prevent self-review** on.
  It holds the updater private key.

  ```sh
  gh api users/<maintainer-login> -q .id          # once per reviewer
  gh api -X PUT repos/$R/environments/release --input - <<'EOF'
  {"prevent_self_review": true,
   "reviewers": [{"type": "User", "id": 1111111}, {"type": "User", "id": 2222222}],
   "deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
  EOF
  gh api -X POST repos/$R/environments/release/deployment-branch-policies -f name='v*' -f type=tag
  ```

  In the web UI the same is Settings → Environments → *New environment*, then *Required
  reviewers*, *Prevent self-review*, and *Deployment branches and tags* → *Selected branches and
  tags* → *Add deployment branch or tag rule* → Ref type *Tag*, pattern `v*`.

- [ ] **Tag rules.** Settings → Rules → Rulesets → *New ruleset* → *New tag ruleset*: target
  tags matching `v*`, enforcement *Active*, rules *Restrict creations*, *Restrict updates*,
  *Restrict deletions*, and the maintainers in the bypass list (so only they can create release
  tags). Rulesets replace GitHub's older "tag protection rules".
- [ ] **Signed tags.** Every maintainer who tags sets up tag signing (GPG, or SSH with
  `git config --global gpg.format ssh` and `user.signingkey`) and adds the key to their GitHub
  account as a *signing key*, so the tag shows as *Verified*. GitHub does not refuse an
  unsigned tag; the approver checks it in Step 7 with `git tag -v`.
- [ ] Never put a signing secret at **repository** level. A repository secret is readable by any
  workflow on any branch; an environment secret only by a job that deploys to that environment,
  which the `v*` rule limits to release tags.

## Step 2. Apple: Developer ID certificate and notarization key

Without these the macOS app is ad-hoc signed and not notarized, and Gatekeeper blocks it on
users' machines (the release still builds, with a warning).

- [ ] **Developer ID Application certificate** (needs the Apple Developer Program, and the
  *Account Holder* role to create this certificate type). On a Mac: Keychain Access →
  Certificate Assistant → *Request a Certificate From a Certificate Authority* → save to disk.
  On developer.apple.com → Certificates → **+** → *Developer ID Application*, upload the request,
  download the certificate and double-click it. In Keychain Access → *My Certificates*, select
  the certificate **with its private key**, *Export* as `.p12` and set a password.

  ```sh
  base64 -i DeveloperID.p12 | gh secret set APPLE_CERTIFICATE --env release-build -R $R
  gh secret set APPLE_CERTIFICATE_PASSWORD --env release-build -R $R      # paste the .p12 password
  security find-identity -v -p codesigning                               # copy the identity string
  gh secret set APPLE_SIGNING_IDENTITY --env release-build -R $R \
    --body 'Developer ID Application: <Name> (<TEAMID>)'
  ```

- [ ] **App Store Connect API key** for notarization. App Store Connect → Users and Access →
  Integrations → App Store Connect API → *Team Keys* → **+**, access *Developer*. Note the
  **Issuer ID** and the **Key ID**, and download `AuthKey_<KeyID>.p8` (it can be downloaded only
  once).

  ```sh
  gh secret set APPLE_API_ISSUER --env release-build -R $R --body '<issuer-uuid>'
  gh secret set APPLE_API_KEY_ID --env release-build -R $R --body '<key-id>'
  gh secret set APPLE_API_KEY_P8 --env release-build -R $R < AuthKey_<key-id>.p8   # plain PEM, not base64
  ```

- [ ] Delete the `.p12` and `.p8` files from the Mac once they are stored (keep the certificate
  in the login keychain only if you want to sign by hand). All three API values are needed to
  notarize; with the certificate but no API key the app is signed but not notarized (warning).

## Step 3. Windows: Azure Trusted Signing

Windows signing goes through Tauri's `bundle.windows.signCommand`, which runs
`packaging/windows/sign.ps1` once per file (§4, *Windows code signing*). The supported service
is **Azure Trusted Signing**, which Microsoft has renamed **Artifact Signing** (same service, same
signtool plugin). CI logs in with a GitHub OIDC token, so **no Azure secret is stored**. Without
this step (and without a `.pfx`) the Windows installers are unsigned and SmartScreen warns users.

- [ ] **Account.** In the Azure portal, register the resource provider `Microsoft.CodeSigning` on
  the subscription (`az provider register --namespace Microsoft.CodeSigning`), then create an
  Artifact Signing (Trusted Signing) **account** in a resource group. Pick its region; the
  account's *Account URI* on its Overview page is the endpoint, for example
  `https://eus.codesigning.azure.net`.
- [ ] **Identity validation.** Give yourself the role *Trusted Signing Identity Verifier* on the
  account, then account → *Identity validation* → *New* → *Public*. Microsoft checks the
  organization (or individual) behind it, which can take days, and limits eligibility (by
  country and business history); check its current rules. The validated name is what users
  see as the publisher.
- [ ] **Certificate profile.** Account → *Certificate profiles* → *Create* → *Public Trust*, linked
  to the validated identity. Its name is the profile name below.
- [ ] **Entra app and federated credential** (the identity CI signs as):

  ```sh
  az ad app create --display-name nugget-release-signing --query appId -o tsv   # = AZURE_CLIENT_ID
  az ad sp create --id <appId>
  az account show --query tenantId -o tsv                                        # = AZURE_TENANT_ID
  az ad app federated-credential create --id <appId> --parameters '{
    "name": "github-release-build",
    "issuer": "https://token.actions.githubusercontent.com",
    "subject": "repo:rayhankhilji/nugget:environment:release-build",
    "audiences": ["api://AzureADTokenExchange"]}'
  ```

  The subject accepts only jobs that deploy to the `release-build` environment, which only `v*`
  tags reach (Step 1). If the repository's OIDC subject template has been customised, the
  subject must match that template instead.
- [ ] **Role, scoped to the one profile.** Give the app *Trusted Signing Certificate Profile
  Signer* (the portal may show it as *Artifact Signing Certificate Profile Signer*) on the
  certificate profile only, not the account or the subscription:

  ```sh
  az role assignment create --assignee <appId> \
    --role "Trusted Signing Certificate Profile Signer" \
    --scope "/subscriptions/<sub-id>/resourceGroups/<rg>/providers/Microsoft.CodeSigning/codeSigningAccounts/<account>/certificateProfiles/<profile>"
  ```

- [ ] **Variables in `release-build`** (identifiers, not secrets):

  ```sh
  gh variable set AZURE_TRUSTED_SIGNING_ENDPOINT --env release-build -R $R --body 'https://eus.codesigning.azure.net'
  gh variable set AZURE_TRUSTED_SIGNING_ACCOUNT  --env release-build -R $R --body '<account>'
  gh variable set AZURE_TRUSTED_SIGNING_PROFILE  --env release-build -R $R --body '<profile>'
  gh variable set AZURE_CLIENT_ID                --env release-build -R $R --body '<appId>'
  gh variable set AZURE_TENANT_ID                --env release-build -R $R --body '<tenantId>'
  ```

  All five or none: a partial set fails the Windows build on purpose.
- [ ] **Optional check by hand** (tests the account, profile and endpoint with your own login,
  not the app's): on Windows with the Windows SDK, `az login`, then

  ```powershell
  $env:AZURE_TRUSTED_SIGNING_DLIB = (pwsh packaging/windows/sign.ps1 -InstallDlib $env:TEMP\as | Select-Object -Last 1)
  $env:AZURE_TRUSTED_SIGNING_ENDPOINT = 'https://eus.codesigning.azure.net'
  $env:AZURE_TRUSTED_SIGNING_ACCOUNT = '<account>'; $env:AZURE_TRUSTED_SIGNING_PROFILE = '<profile>'
  Copy-Item C:\Windows\System32\whoami.exe .\test.exe
  pwsh packaging/windows/sign.ps1 .\test.exe     # auth: Azure CLI login; your user needs the Signer role too
  Get-AuthenticodeSignature .\test.exe
  ```

  `sign.ps1` also accepts `AZURE_CLIENT_ID` + `AZURE_TENANT_ID` + `AZURE_CLIENT_SECRET` (an app
  secret) for signing by hand; CI does not use a client secret.

**Alternative: an exportable `.pfx`.** If you have an Authenticode certificate with an exportable
key (rare since mid-2023, when CAs moved OV and EV keys onto hardware):

```sh
base64 -w0 cert.pfx | gh secret set WINDOWS_CERTIFICATE --env release-build -R $R
gh secret set WINDOWS_CERTIFICATE_PASSWORD --env release-build -R $R
```

If both are configured, Trusted Signing wins and the job warns that the `.pfx` is ignored.
Other services (DigiCert KeyLocker, SSL.com eSigner, a USB token) need a small change to
`sign.ps1`; see §4.

## Step 4. Key ceremony: updater key

The updater key signs every update payload and the update feed. Installed apps trust exactly the
public key built into them, so losing the private key means no more updates, and leaking it
means anyone can ship one. It is generated once, goes straight into the `release` environment,
and otherwise exists only as recovery shares (§4, *Updater key: the two-person process*).

Two maintainers present (a call with screen share is enough). Do Step 5 in the same session.

- [ ] **Prepare the throwaway machine while online**: Node 22 with pnpm, `gh` (logged in as a
  maintainer), `gfshare` (Debian/Ubuntu package `libgfshare-bin`, for `gfsplit`/`gfcombine`), and
  for Step 5 a Rust toolchain and a clone of the repository with the signing tool built:

  ```sh
  pnpm dlx @tauri-apps/cli@2.11.5 --version          # caches the CLI version the lockfile pins
  cargo build --locked -p nugget-models --example sign-manifest
  ```

- [ ] **Disconnect from the network**, then generate the key (choose a strong password):

  ```sh
  pnpm dlx @tauri-apps/cli@2.11.5 signer generate -w ./nugget-updater.key
  ```

- [ ] **Recovery shares**, 2 of 3, one per maintainer, stored offline (paper or a USB stick each),
  the key and the password split separately:

  ```sh
  gfsplit -n 2 -m 3 nugget-updater.key              # writes nugget-updater.key.NNN, three files
  printf '%s' '<password>' > updater-password.txt && gfsplit -n 2 -m 3 updater-password.txt
  ```

  (`ssss-split` is not suitable here: it takes at most 128 characters on one line, and the key
  file is longer.) Check that two shares rebuild the key: `gfcombine -o check.key
  nugget-updater.key.<a> nugget-updater.key.<b> && cmp check.key nugget-updater.key`.
- [ ] **Reconnect** and store the key in the `release` environment and the public key as a
  repository variable:

  ```sh
  gh secret set TAURI_SIGNING_PRIVATE_KEY --env release -R $R < ./nugget-updater.key
  gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --env release -R $R     # paste the password
  gh variable set NUGGET_UPDATER_PUBKEY -R $R < ./nugget-updater.key.pub
  ```

- [ ] **Publish the public key**: paste the text of `nugget-updater.key.pub` into
  [Public keys](#public-keys) in this file in a PR, so anyone can check a payload by hand.
- [ ] After Step 5, wipe the machine (destroy the VM). The private key is now only in the
  environment secret and in the shares.

## Step 5. Manifest release key

The model manifest (`manifests/default.json`, which pins every model and runtime download) is
signed with its own ed25519 key (§4, *Manifest release key*). **Release builds trust only this
key**, not the development key, so until the manifest carries a release signature a released
`nuggetd` offers no model downloads.

- [ ] On the same offline machine, in the repository clone:

  ```sh
  SM=target/debug/examples/sign-manifest
  $SM --generate-key ./manifest-release.key          # prints "public key: <64 hex>"
  $SM --append --key-file ./manifest-release.key manifests/default.json
  $SM --verify manifests/default.json --pubkey <64-hex-public-key>
  grep -c . manifests/default.json.sig               # 2: the development and the release signature
  gfsplit -n 2 -m 3 manifest-release.key             # shares, as for the updater key
  ```

  `--append` keeps the development signature and adds the release one. `--verify` without
  `--pubkey` checks against the keys of that (debug) build, which is the development key, so
  always pass `--pubkey` here.
- [ ] Copy `manifests/default.json.sig` off the machine (it is public), then, back online:

  ```sh
  gh variable set NUGGET_MANIFEST_RELEASE_PUBKEY -R $R --body '<64-hex-public-key>'
  ```

- [ ] Commit the two-line `manifests/default.json.sig` in a PR, and add the public key to
  [Public keys](#public-keys) in the same PR. On a normal machine, check it:
  `cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json --pubkey "$(gh variable get NUGGET_MANIFEST_RELEASE_PUBKEY -R $R)"`.
- [ ] Whenever `default.json` changes later, every signature must be redone: the development
  signature first (it replaces the file), then the release signature appended again, which
  needs two maintainers to rebuild the key from shares.

## Step 6. Prove the commit you will tag

Run these on the exact commit you are going to tag (the version-bump merge commit of Step 7 or
Step 9), and tag only when all are green.

- [ ] **Release dry run**: builds every installer unsigned and runs the AppImage, dmg and NSIS
  smoke tests.

  ```sh
  gh workflow run release.yml --ref main -f dry_run=true
  ```

- [ ] **Release rehearsal**: a whole release with throwaway keys; the old build updates itself
  to the new one on Linux and Windows, and wrong keys are refused (§3, *Release rehearsal*).

  ```sh
  gh workflow run release-rehearsal.yml --ref main
  ```

- [ ] If `packaging/windows/` changed since the last green run, also
  `gh workflow run windows-signing.yml --ref main` (it runs on pull requests that touch it).
- [ ] Check that each run built the commit you will tag and passed:

  ```sh
  git rev-parse origin/main
  gh run list -R $R -w release.yml -L 1 --json headSha,conclusion,url
  gh run list -R $R -w release-rehearsal.yml -L 1 --json headSha,conclusion,url
  ```

  In the dry run all required targets must be green (Linux arm64 may fail), all three smoke
  tests passed, and the sizes in the job summaries are close to §2's table.

## Step 7. Release candidate `v1.0.0-rc.1`

The first tag is the first real test of every signing path: Apple signing and notarization,
Azure Trusted Signing, the updater key and the manifest release key have only been exercised
with throwaway keys. Use a prerelease tag, which only the beta channel receives.

- [ ] **Bump the version** in one PR to `1.0.0-rc.1`:
  - `version` in `apps/desktop/src-tauri/tauri.conf.json`;
  - `[workspace.package] version` in the root `Cargo.toml`, and every internal crate's `version`
    under `[workspace.dependencies]` next to it (`cargo-deny` needs them in step);
  - `version` in `apps/desktop/package.json`;
  - then `cargo update -w` so `Cargo.lock` follows.
  - **MSI**: Tauri's MSI bundler accepts a prerelease only if it is numeric (`1.0.0-1`), and fails
    on `1.0.0-rc.1`. For the RC also set `"wix": {"version": "1.0.0.1"}` under
    `bundle.windows` in `tauri.conf.json` (the MSI's ProductVersion), and remove it again in the
    v1.0.0 bump. The dry run in Step 6 catches this if it is forgotten.
- [ ] Merge, then **Step 6** on the merge commit.
- [ ] **Tag** the merge commit with an annotated, signed tag. The message is the release notes.

  ```sh
  git switch main && git pull --ff-only
  git tag -s v1.0.0-rc.1 -m "Nugget 1.0.0-rc.1

  Release candidate. - what changed, for users"
  git push origin v1.0.0-rc.1
  ```

- [ ] **Watch `build` and `sbom`** (`gh run watch`). Each macOS job logs whether it signed and
  notarized, the Windows job which signing mode it used (`Windows signing: Azure Trusted
  Signing, account …`) and `sign.ps1: signed …` for every file (about 13 per build: the
  `nuggetd` sidecar, `nugget-desktop.exe` twice, the NSIS plugin DLLs and uninstaller, the WiX
  extension DLLs, the setup exe and the MSI). A real release then checks the signatures of the
  setup exe, the MSI and the binaries inside them with `signtool verify /pa /all` and fails if
  any is unsigned. A `::warning::` about a missing certificate or key means a secret or variable
  is missing: stop, fix it, delete the tag and the draft, and tag again.
- [ ] **Approve `sign`**: a second maintainer (not the tagger) runs `git tag -v v1.0.0-rc.1`,
  reviews the run, and approves the `release` environment.
- [ ] **Check the draft and the signatures**, Step 8.
- [ ] **Publish** the draft (it is marked prerelease). The `channels` run must log
  `beta -> v1.0.0-rc.1`; the stable feed doesn't move.
- [ ] Install rc.1 on each OS and set the channel to **Beta**. There is no earlier signed
  release to update from, so the first real in-app update is rc.1 → v1.0.0 (Step 9).

## Step 8. Check the draft and the signatures on each OS

Download the draft's assets (`gh release download v1.0.0-rc.1 -R $R -D rc1`; drafts need a
maintainer login) and check:

- [ ] **Contents**: every installer of §1 (Linux arm64 may be missing), a `.sig` per updater
  payload, `latest.json` and `latest.json.sig`, `SHA256SUMS`, the three SBOMs. No bundle
  reports or smoke logs (the `sign` job attaches only the `bundle-<target>` installers).
- [ ] **Checksums and provenance**: `sha256sum -c SHA256SUMS` and
  `gh attestation verify Nugget_1.0.0-rc.1_amd64.AppImage -R $R`.
- [ ] **Updater signatures** against the published public key (the `.sig` files and the
  `.key.pub` are base64-wrapped minisign files):

  ```sh
  gh variable get NUGGET_UPDATER_PUBKEY -R $R | base64 -d > updater.pub
  base64 -d Nugget_1.0.0-rc.1_amd64.AppImage.sig > appimage.minisig
  minisign -V -p updater.pub -m Nugget_1.0.0-rc.1_amd64.AppImage -x appimage.minisig
  base64 -d latest.json.sig > latest.minisig && minisign -V -p updater.pub -m latest.json -x latest.minisig
  ```

  The `sign` job already runs the same check (`packaging/scripts/check_release_keys.py`) on
  `latest.json` and every payload before it drafts; this repeats it on the files you downloaded.
- [ ] **macOS** (both architectures), after installing from the dmg:

  ```sh
  codesign --verify --deep --strict --verbose=2 /Applications/Nugget.app
  codesign -dv --verbose=4 /Applications/Nugget.app/Contents/MacOS/nuggetd   # Authority=Developer ID Application: …
  spctl -a -vv /Applications/Nugget.app                                      # accepted, source=Notarized Developer ID
  xcrun stapler validate /Applications/Nugget.app
  ```

- [ ] **Windows**, for the setup exe, the MSI and the installed binaries:

  ```powershell
  signtool verify /pa /all /v Nugget_1.0.0-rc.1_x64-setup.exe
  signtool verify /pa /all /v Nugget_1.0.0-rc.1_x64_en-US.msi
  Get-AuthenticodeSignature "$env:LOCALAPPDATA\Nugget\nugget-desktop.exe", "$env:LOCALAPPDATA\Nugget\nuggetd.exe", "$env:LOCALAPPDATA\Nugget\uninstall.exe" |
    Format-Table Status, @{n='Signer';e={$_.SignerCertificate.Subject}}, @{n='Timestamp';e={$_.TimeStamperCertificate.Subject}}
  ```

  Each must be *Valid*, signed by the validated identity and timestamped (Trusted Signing
  certificates last three days, so an untimestamped signature soon stops verifying).
- [ ] **Model manifest**: start the installed app on one OS and open the model list; the daemon
  log must not say "bundled model manifest failed verification".
- [ ] **Each OS installs and starts** from the draft's installers (at least one per OS), and the
  notes are right.

## Step 9. Cut v1.0.0

- [ ] **Bump the version** to `1.0.0` exactly as in Step 7 (`tauri.conf.json`, the root
  `Cargo.toml` workspace and internal dependency versions, `apps/desktop/package.json`,
  `cargo update -w`), and remove the RC's `bundle.windows.wix.version`. Merge.
- [ ] **Step 6** on the merge commit.
- [ ] **Tag**: `git tag -s v1.0.0 -m "Nugget 1.0.0 …"` and `git push origin v1.0.0`.
- [ ] **Watch** the build, as in Step 7. **Approve** `sign` (second maintainer, `git tag -v`).
- [ ] **Review the draft**, Step 8, then **Publish**.
- [ ] **Check the `channels` log** for `beta -> v1.0.0` and `stable -> v1.0.0`, and the feeds:
  `gh release download updater-feed -R $R -p 'stable.json*' -D feed` then verify
  `stable.json.sig` with `minisign` as above.
- [ ] **Update for real**: an installed rc.1 on the beta channel offers v1.0.0 on *Check now*,
  installs it, and the new version starts.
- [ ] Update the README and `docs/book/src/guide/install.md` (the "no release has been published
  yet" note), and `docs/PLAN.md`.

**Later releases** repeat Steps 6, 7 (without the MSI note for numeric versions) and 9: bump, prove
the commit, tag (`vX.Y.Z`, or `vX.Y.Z-beta.N` with a `wix.version` override for a prerelease),
approve, review, publish. If `manifests/default.json` changed, redo its signatures first
(Step 5, last item).

---

# Secrets and variables

Every name the release workflow reads (`grep -n 'secrets\.\|vars\.' .github/workflows/release.yml`).
None is needed for a dry run, the rehearsal or the Windows signing proof; those never read them.

| Name | Kind, where | Read by | What it is, how to make it | When absent |
|---|---|---|---|---|
| `APPLE_CERTIFICATE` | secret, env `release-build` | `build` (macOS) | Developer ID Application certificate with its private key, `.p12`, base64 (Step 2) | Ad-hoc signed (`signingIdentity: "-"`), not notarized, warning. Gatekeeper blocks it. |
| `APPLE_CERTIFICATE_PASSWORD` | secret, env `release-build` | `build` (macOS) | The `.p12` export password | Needed whenever `APPLE_CERTIFICATE` is set; the import fails without it. |
| `APPLE_SIGNING_IDENTITY` | secret, env `release-build` | `build` (macOS) | `Developer ID Application: Name (TEAMID)`, from `security find-identity -v -p codesigning` | Optional; Tauri picks the identity from the imported certificate. |
| `APPLE_API_ISSUER` | secret, env `release-build` | `build` (macOS) | App Store Connect API Issuer ID (Step 2) | Signed but not notarized, warning. All three API values are needed. |
| `APPLE_API_KEY_ID` | secret, env `release-build` | `build` (macOS) | Key ID of a team API key, access *Developer* | As above. |
| `APPLE_API_KEY_P8` | secret, env `release-build` | `build` (macOS) | Text of `AuthKey_<id>.p8` (plain PEM) | As above. |
| `AZURE_TRUSTED_SIGNING_ENDPOINT` | variable, env `release-build` | `build` (Windows) | The signing account's region endpoint, `https://<region>.codesigning.azure.net` (Step 3) | With none of the five Azure variables: falls back to the `.pfx`, else unsigned with a warning. With some but not all five: the build fails. |
| `AZURE_TRUSTED_SIGNING_ACCOUNT` | variable, env `release-build` | `build` (Windows) | Signing account name | As above. |
| `AZURE_TRUSTED_SIGNING_PROFILE` | variable, env `release-build` | `build` (Windows) | Certificate profile name | As above. |
| `AZURE_CLIENT_ID` | variable, env `release-build` | `build` (Windows) | Application (client) ID of the Entra app that has the profile's Signer role and the federated credential | As above. |
| `AZURE_TENANT_ID` | variable, env `release-build` | `build` (Windows) | Entra tenant ID | As above. |
| `WINDOWS_CERTIFICATE` | secret, env `release-build` | `build` (Windows) | Alternative to Trusted Signing: exportable Authenticode `.pfx`, base64 | Unsigned, warning (if Trusted Signing isn't configured). Ignored, with a warning, if it is. |
| `WINDOWS_CERTIFICATE_PASSWORD` | secret, env `release-build` | `build` (Windows) | The `.pfx` password | Needed whenever `WINDOWS_CERTIFICATE` is used. |
| `TAURI_SIGNING_PRIVATE_KEY` | secret, env `release` | `sign` | Updater minisign private key (Step 4) | The draft has no `.sig` files or `latest.json` (warning); `channels` leaves the feeds unchanged, so installed apps never see the release. |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret, env `release` | `sign` | The password chosen at `signer generate` | With the key set, `tauri signer sign` fails and the job stops. |
| `NUGGET_UPDATER_PUBKEY` | repository variable | `build` (real releases only), `sign` | Text of `nugget-updater.key.pub` (Step 4) | `build`: app compiled without an updater ("this build can't update itself"). `sign`: treated like a missing key. |
| `NUGGET_MANIFEST_RELEASE_PUBKEY` | repository variable | `build` (sidecar step) | 64-hex ed25519 public key of the manifest release key (Step 5) | Real release: the `mode` job fails before anything is built, as it does when no line of `manifests/default.json.sig` verifies with this key (`check_release_keys.py manifest`). A malformed value fails the build. |
| `GITHUB_TOKEN` | automatic | `sign`, `channels` | Scoped by each job's `permissions` | n/a |

Set by the workflow, not by you: `NUGGET_TRUST_DEV_MANIFEST_KEY=1` (dry runs only),
`AZURE_TRUSTED_SIGNING_DLIB` (path of the pinned signtool plugin). `NUGGET_UPDATER_FEED` is an
optional build-time override of the feed URL (`updater.rs`); release builds don't set it, the
rehearsal does.

---

# Reference

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

The MSI's version must be numeric (`major.minor.patch[.build]`); Tauri derives it from the app
version and refuses a non-numeric prerelease such as `-rc.1` unless `bundle.windows.wix.version`
is set (Step 7).

## 2. The `nuggetd` sidecar

The daemon ships inside every bundle as a Tauri sidecar (`bundle.externalBin: ["binaries/nuggetd"]`).
Tauri wants `apps/desktop/src-tauri/binaries/nuggetd-<target-triple>[.exe]` at build time and installs it
**next to the app executable** without the suffix:

| Bundle | App | Daemon |
|---|---|---|
| macOS | `Nugget.app/Contents/MacOS/nugget-desktop` | `Nugget.app/Contents/MacOS/nuggetd` (signed with the app) |
| Windows (NSIS) | `%LOCALAPPDATA%\Nugget\nugget-desktop.exe` | `%LOCALAPPDATA%\Nugget\nuggetd.exe` (signed before bundling) |
| Windows (MSI) | `C:\Program Files\Nugget\nugget-desktop.exe` | `C:\Program Files\Nugget\nuggetd.exe` |
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

A local build has no updater key, so it shows "this build can't update itself", and trusts no
manifest key unless `NUGGET_TRUST_DEV_MANIFEST_KEY=1` was exported for `stage-sidecar.sh`. To
test the update path, generate a throwaway key (`pnpm tauri signer generate -w /tmp/test.key`)
and export `NUGGET_UPDATER_PUBKEY="$(cat /tmp/test.key.pub)"` before building; the release
rehearsal (§3) automates the whole loop.

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
(version 0.0.1) on 2026-10-29. That run failed its AppImage smoke test, but the bundles and sizes
are valid; the later dry run [40848720974](https://github.com/rayhankhilji/nugget/actions/runs/40848720974)
built every target and passed all three smoke tests (AppImage, dmg, NSIS). Every installer puts
`nuggetd` next to `nugget-desktop`, as above. `nuggetd` is three quarters of every bundle; the
release profile keeps symbol tables (no `strip`), so `strip = "symbols"` in `[profile.release]`
is the first thing to try if size matters. Each dry run writes the same tables to its job
summary (`bundle-report-<target>`).
<!-- sizes:end -->

## 3. Release workflow

`.github/workflows/release.yml`, top-level `permissions: {}`. Each job asks for only what it needs,
and every action is pinned to a commit SHA.

1. **Tag.** A maintainer bumps the version (Step 7), merges it, and pushes an **annotated,
   signed** tag `vX.Y.Z` (or a prerelease `vX.Y.Z-…`). The tag message becomes the release notes.
   The workflow fails if the tag and the `tauri.conf.json` version differ.
2. **`build`** (matrix, `contents: read`, `id-token: write`, environment `release-build` on real
   releases only). It stages the sidecar and runs `tauri build`, with OS signing when the
   secrets or variables exist:
   - macOS: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` sign with
     the hardened runtime. With `APPLE_API_ISSUER`, `APPLE_API_KEY_ID` and `APPLE_API_KEY_P8` it
     also notarizes and staples. With no certificate the app is ad-hoc signed and a warning is
     logged.
   - Windows: with the five Azure variables, the *Windows signing setup* step downloads the
     pinned signtool plugin (`sign.ps1 -InstallDlib`) and writes a `signCommand` config fragment;
     Tauri then calls `sign.ps1` for every file it signs, and `sign.ps1` authenticates with a
     fresh GitHub OIDC token per file (§4, *Windows code signing*). Otherwise, with
     `WINDOWS_CERTIFICATE`, the `.pfx` is imported into the runner's user store and Tauri signs
     by thumbprint (SHA-256, RFC 3161 timestamp from `timestampUrl`). With neither, the
     installers are unsigned and a warning is logged. On a real release a step after the build
     runs `signtool verify /pa /all` on the setup exe, the MSI and the binaries inside them and
     fails the release if any is unsigned.
   - The **updater key is never in this job**: build jobs compile hundreds of third-party crates
     whose build scripts run code. They get only the *public* key (`vars.NUGGET_UPDATER_PUBKEY`),
     which is compiled into the app, and `vars.NUGGET_MANIFEST_RELEASE_PUBKEY` for the sidecar.
   - Every leg uploads `bundle-<target>` (the installers), `bundle-report-<target>` (sizes and
     sidecar layout) and, for the three smoke-tested targets, `smoke-<target>` (logs,
     screenshots), and runs the smoke tests below.
3. **`sbom`.** CycloneDX SBOMs for `nuggetd` and `nugget-desktop` (`cargo-cyclonedx`) and for the
   frontend (`cdxgen`, from the pnpm lockfile).
4. **`sign`** (environment **`release`**, `contents: write`, `id-token: write`,
   `attestations: write`). It waits for a reviewer, then:
   - downloads only the `bundle-<target>` artifacts (bundle reports and smoke logs are never
     attached, checksummed or attested), plus the SBOMs;
   - signs every update payload with `tauri signer sign` (minisign). The key comes from
     `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD`, secrets that exist only in this environment. Only
     the locked Tauri CLI is installed, with `--ignore-scripts`;
   - writes `latest.json` (the Tauri updater manifest: version, the tag's notes, and per-platform
     URL and signature) and signs it as a whole with the same key and command, giving
     `latest.json.sig` (a detached minisign signature over the exact bytes, base64-wrapped like
     the payload signatures);
   - writes `SHA256SUMS` over every asset, including the signatures, `latest.json`,
     `latest.json.sig` and the SBOMs;
   - records SLSA build provenance for every asset in `SHA256SUMS`
     (`actions/attest-build-provenance`; check one with `gh attestation verify <file> -R rayhankhilji/nugget`);
   - creates a **draft** GitHub release with everything attached (prerelease if the tag has a
     hyphen).
5. **Publish (human).** A maintainer checks the draft (Step 8) and clicks *Publish*.
6. **`channels`** (on `release: published`, `contents: write`, no secrets). It copies the release's
   `latest.json` and `latest.json.sig` to the rolling `updater-feed` release as `beta.json` /
   `beta.json.sig`, and as `stable.json` / `stable.json.sig` too if it is not a prerelease. It
   only moves a feed forward (semver), so publishing an old patch release never pulls anyone
   back. It needs no key: the copies are byte for byte, so the signature stays valid.

### Dry run

Use it to check that installers build, before a tag or after changing packaging.

```sh
gh workflow run release.yml --ref <branch> -f dry_run=true
```

or Actions → **Release** → *Run workflow*, pick the branch, leave *dry_run* ticked.

With `dry_run: true` (the default) the workflow, from any branch:

- builds every target in the matrix (§1) with the `nuggetd` sidecar: macOS `.app`/`.dmg` (ad-hoc
  signed), Windows NSIS and MSI (unsigned), Linux AppImage, `.deb` and `.rpm`;
- uploads `bundle-<target>`, `bundle-report-<target>` and `smoke-<target>` (kept 14 days);
- runs the AppImage, dmg and NSIS smoke tests (below);
- builds `nuggetd` with `NUGGET_TRUST_DEV_MANIFEST_KEY=1`, so the dry-run apps can download models
  from the development-signed manifest;
- never enters the `release-build` or `release` environment, never reads a signing secret or
  `NUGGET_UPDATER_PUBKEY`, and never signs, notarizes, drafts or publishes anything. The apps it
  builds have no updater.

`dry_run: false` is accepted only on a `v*` tag ref. It re-runs a real release for that tag, with
the same environments, approvals and draft as a tag push. On any other ref the workflow fails at
the start.

Installing a dry-run build by hand:

- **macOS:** Gatekeeper says the app is from an *unidentified developer* (or "is damaged" for a
  downloaded quarantined copy). Right-click the app → *Open* → *Open* (on macOS 15 and later:
  System Settings → Privacy & Security → *Open Anyway*), or clear the quarantine flag:
  `xattr -dr com.apple.quarantine /Applications/Nugget.app`.
- **Windows:** SmartScreen shows *Windows protected your PC*: *More info* → *Run anyway*.
- **Linux:** no prompt; `chmod +x` the AppImage.

### Smoke tests

Every release run, dry or real, launches the installers it just built:

| Target | Script | What it does |
|---|---|---|
| Linux x64 AppImage | `packaging/scripts/smoke-appimage.sh` | Runs it under Xvfb with openbox and throwaway `HOME`, XDG and `NUGGET_HOME` (`NUGGET_TRAY=off`) |
| macOS aarch64 dmg | `packaging/scripts/smoke-macos.sh` | Mounts the dmg, copies `Nugget.app` out, launches it with throwaway `HOME`/`NUGGET_HOME` |
| Windows NSIS | `packaging/scripts/smoke-windows.ps1` | Installs silently into a throwaway directory, launches it, uninstalls silently at the end |

Each waits, with a time limit, for a window named `Nugget`, for the app to start the bundled
`nuggetd` itself (`starting nuggetd`) and connect to it (`connected to nuggetd`), then closes the
window the way a user would (`WM_DELETE_WINDOW` through the window manager, the close button,
`WM_CLOSE`), expects a clean exit, and stops the daemon. Any step that times out fails the job.
`.github/workflows/package-smoke.yml` runs the AppImage test on pull requests that touch
`apps/desktop/`, `crates/` or `packaging/`, and `real-os.yml` runs the dmg and NSIS tests on
pull requests that touch the desktop app or packaging. The macOS Intel build, the MSI and the
Linux `.deb`/`.rpm` are built but not launched.

### Release rehearsal

`.github/workflows/release-rehearsal.yml` is a complete release with **throwaway keys**, proving
everything short of the maintainers' real keys. It runs only when started by hand:

```sh
gh workflow run release-rehearsal.yml --ref <branch>
```

On Linux x64 (AppImage) and Windows x64 (NSIS), each job (scripts in `packaging/scripts/rehearsal/`):

1. generates a throwaway updater key and a second, wrong one, and a throwaway manifest release
   key and a wrong one, in the job's temp directory (never stored or uploaded, deleted at the
   end; the report step fails if the evidence contains a private key);
2. appends a release signature to `manifests/default.json.sig` in the job's workspace and builds
   `nuggetd` with `NUGGET_MANIFEST_RELEASE_PUBKEY` set and no development-key trust, like a real
   release; on Linux also a `nuggetd` built with the wrong key, which must reject the manifest;
3. builds an old (`0.9.0-rehearsal.1`) and a new (`0.9.1-rehearsal.1`) app, both with the
   throwaway updater key and `NUGGET_UPDATER_FEED` pointing at `https://localhost`;
4. signs the new build as the `sign` job does (payloads, `latest.json` and `.sig`,
   `SHA256SUMS`), then publishes it to the **beta** channel as `channels` does for a prerelease
   (`updater_manifest.py promote`, then `channel latest.json beta`, giving `beta.json` and
   `beta.json.sig`);
5. serves the feed over HTTPS from 127.0.0.1 with a throwaway CA trusted only for the job, and
   lets the installed old app, set to the beta channel, find, verify, download and install the
   new one: on Linux the
   AppImage replaces itself and must be byte-identical to the new build; on Windows the NSIS
   installer runs and the installed `nugget-desktop.exe` must carry the new ProductVersion;
6. checks the negative cases: a feed signed with the wrong key, a payload signed with the wrong
   key, and (Linux) a manifest signed with the wrong key are all refused and install nothing.

The evidence (a PASS/FAIL report, public keys, feeds, signatures, `SHA256SUMS`, app, daemon and
server logs; no installers, no private keys) is uploaded as `rehearsal-evidence-<os>` and shown
in the job summary. Last green run:
[41027356941](https://github.com/rayhankhilji/nugget/actions/runs/41027356941) (made before the
switch to the beta channel, when it published `stable.json`). Its limits are listed in §8.

### Windows signing proof

`.github/workflows/windows-signing.yml` (manual, and on pull requests that touch
`packaging/windows/`) proves the `signCommand` plumbing without a real key. A throwaway,
non-exportable self-signed certificate is made in the job and deleted at the end.

- `sign-script` checks `sign.ps1` itself: thumbprint mode signs and timestamps, failure paths exit
  non-zero, the pinned plugin downloads and its checksum matches, the Azure mode builds the right
  signtool command line and metadata for each authentication method, a partial Azure config or a
  foreign endpoint fails, and a real signtool call with fake Azure credentials fails without
  printing the secret.
- `bundles` builds the NSIS and MSI bundles with the sidecar exactly as `release.yml` does, signing
  through `sign.ps1` in thumbprint mode, and runs `signtool verify /pa /all` on the setup exe, the
  MSI, the NSIS-installed `nugget-desktop.exe`, `nuggetd.exe` and `uninstall.exe`, and the
  binaries extracted from the MSI: each must fail while the throwaway root is untrusted, then
  verify, timestamped, once it is trusted (test only).

Last green run: [41029408486](https://github.com/rayhankhilji/nugget/actions/runs/41029408486);
its summary lists the 13 files Tauri passed to `sign.ps1`. The real Azure path (OIDC login,
Microsoft's plugin, the real profile) has not signed anything yet; the first tag is its first
real test, hence the RC in Step 7.

### How the update feed moves

```
tag push ─► build ─► sign (approved) ─► draft release ─► Publish ─► channels ─► updater-feed/beta.json
                                                                              └► updater-feed/stable.json (not prereleases)
```

- Nothing reaches users before *Publish*. A draft never triggers `channels`.
- `channels` downloads the published release's `latest.json` and `latest.json.sig` and, for each
  channel it applies to, runs `updater_manifest.py promote latest.json current.json`. That prints
  the new version only if it is **strictly newer** (SemVer 2.0 precedence; build metadata ignored)
  than the version the feed already holds, or if the feed is empty. Otherwise the feed stays as it
  is and the job logs `keeping <current>: <new> is not newer`. So publishing an older patch, or
  re-publishing the same version, never moves a feed. To move it, `updater_manifest.py channel
  latest.json <ch>` copies both files to `<ch>.json` and `<ch>.json.sig` (refusing if the
  signature is missing) and both are uploaded in one `gh release upload`.
- The first `channels` run creates the `updater-feed` release (a prerelease) if it doesn't exist.
- A release without `latest.json` (no updater key) or without `latest.json.sig` leaves both feeds
  unchanged: apps refuse an unsigned feed.
- Each feed file always travels with its `.sig`; a feed and a signature from different releases
  don't verify. Apps that check during the few seconds between the two uploads report a failed
  check and get the new feed on the next one.
- Installed apps also refuse to go backwards: the updater only offers a version newer than the one
  running.

**Pulling a bad release.** Neither the pipeline nor the app can downgrade anyone, so the fix for
people who already installed it is always a **newer** release. To stop it spreading meanwhile:

1. Point the feed back at the last good release by hand. `promote` would refuse this, so
   upload directly:

   ```sh
   gh release download vGOOD -p latest.json -p latest.json.sig
   python3 packaging/scripts/updater_manifest.py channel latest.json stable   # likewise beta
   gh release upload updater-feed --clobber stable.json stable.json.sig
   ```

   Apps still on the good version then see no update. Apps already on the bad version stay on it.
2. Mark the bad release as a prerelease or take it back to draft (`gh release edit vBAD --draft`)
   so nobody downloads it from the Releases page. Don't delete its assets while a feed still
   points at them, or updates fail with a download error instead.
3. Fix, bump to the next patch version and release as usual. `promote` accepts it because it is
   newer than both the bad and the restored version.

If the bad release was signed with a compromised key, follow *Rotation* in §4 instead.

## 4. Signing keys

Four kinds of signature end up in a release: the OS code signatures (Apple, Authenticode), the
updater signatures (payloads and feed, one minisign key), and the model manifest signature (an
ed25519 key). Plugin packages have their own scheme with no project-wide key: users list the
publishers they trust in `[plugins] trusted_keys`, authors sign with
`cargo run -p nugget-plugin --example sign-plugin`, and nothing in the release pipeline touches
plugin keys.

### Updater key: the two-person process

minisign (the scheme Tauri's updater uses) has no native threshold signatures, so the threshold
is enforced by process. No single person can ship an update:

1. **Two people to release.** Maintainer A pushes the tag. Maintainer B must approve the `release`
   environment before the key is used, and self-review is blocked. A draft still needs someone to
   publish it, and only published releases move the feeds.
2. **The key lives only in CI.** It is generated on a throwaway machine and goes straight into the
   environment secret (Step 4). It is never kept on a laptop, in a password manager or in a
   repository. GitHub secrets can't be read back, only used by the gated job.
3. **Recovery shares, not copies.** For disaster recovery, the encrypted key is split with Shamir's
   scheme into 3 shares with threshold 2 (`gfsplit -n 2 -m 3`), one share per maintainer, stored
   offline. The password is split the same way, separately. No one person can rebuild the key.

**Rotation.** The app trusts exactly one public key, built in. To rotate, ship release N built
with the **new** public key (`NUGGET_UPDATER_PUBKEY`) but signed with the **old** key
(`TAURI_SIGNING_PRIVATE_KEY`). Once N is installed, later releases are signed with the new key.
If the key is compromised, revoke the environment secret at once, publish a security advisory,
and ship a rotation release as fast as possible.

### Manifest release key

The model manifest (`manifests/default.json` + `default.json.sig`, verified by `nugget-models`)
has its own ed25519 key, separate from the updater key (`docs/security/manifest-trust.md`). The
manifest is compiled into `nuggetd` (`include_bytes!`) and checked at daemon start against the
keys returned by `trusted_keys()`. `nugget-models` reads two **build-time** variables when the
sidecar is compiled:

- `NUGGET_MANIFEST_RELEASE_PUBKEY`: the release public key, 64 hex chars. Public, so it is a
  **repository variable**, not a secret. The `build` job passes it to *Build and stage the
  nuggetd sidecar*, which runs `packaging/scripts/stage-sidecar.sh` (`cargo build --release -p
  nuggetd`); `option_env!` bakes it into the binary. A malformed value fails the build.
- `NUGGET_TRUST_DEV_MANIFEST_KEY=1`: also trust the development key (`DEV_PUBLIC_KEY_HEX` in
  `crates/nugget-models/src/manifest.rs`) in a release build. The workflow sets it **only for
  dry runs**; real releases trust only the release key. Debug builds always trust the
  development key.

A release build with neither trusts no manifest key: the daemon starts but logs "bundled model
manifest failed verification" and offers no model downloads. The same happens if the variable is
set but `default.json.sig` has no release signature.

`default.json.sig` holds one hex signature per line; verification succeeds if any trusted key
verifies any of them, so the committed file carries the development signature (for debug builds
and tests) and the release signature (for releases) together. Signing:

```sh
# development signature (replaces the file)
cargo run -p nugget-models --example sign-manifest -- --generate-key ~/.config/nugget/manifest-dev.key  # once
NUGGET_MANIFEST_SIGNING_KEY_FILE=~/.config/nugget/manifest-dev.key \
  cargo run -p nugget-models --example sign-manifest -- manifests/default.json
# release signature, appended, on the offline machine that holds the rebuilt release key (Step 5)
cargo run -p nugget-models --example sign-manifest -- --append --key-file manifest-release.key manifests/default.json
# check
cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json --pubkey <hex>
```

(`NUGGET_MANIFEST_SIGNING_KEY`, 64 hex chars, works instead of the key file.) A stale or wrong
development signature fails the `nugget-models` tests in CI. Nothing in CI checks the release
signature; Step 8's model-list check and the rehearsal cover it.

### Windows code signing

`packaging/windows/sign.ps1` is Tauri's `bundle.windows.signCommand`: Tauri runs it once per file
(`%1` is the path) and stops the build if it exits non-zero. Per Windows build that is about 13
calls: the staged sidecar `nuggetd-x86_64-pc-windows-msvc.exe`, `nugget-desktop.exe` (twice, once
per bundle type), the NSIS plugin DLLs and uninstaller, the WiX extension DLLs, the setup exe and
the MSI. Every signature is SHA-256 with an RFC 3161 timestamp. Modes, first match wins:

- **Azure Trusted Signing / Artifact Signing**: `AZURE_TRUSTED_SIGNING_ENDPOINT`, `_ACCOUNT`,
  `_PROFILE` and `AZURE_TRUSTED_SIGNING_DLIB`. The plugin is Microsoft's `Azure.CodeSigning.Dlib.dll`
  from the NuGet package `Microsoft.ArtifactSigning.Client` 1.0.146, downloaded by
  `sign.ps1 -InstallDlib <dir>` and checked against a pinned SHA-256 (it needs the .NET 8 runtime,
  which GitHub's Windows runners have). Timestamps come from `http://timestamp.acs.microsoft.com`.
  The endpoint must be `https://<region>.codesigning.azure.net`. Authentication, first match:
  `AZURE_CLIENT_ID` + `AZURE_TENANT_ID` + `AZURE_CLIENT_SECRET` (an app secret; for signing by
  hand only); `AZURE_CLIENT_ID` + `AZURE_TENANT_ID` in a GitHub job with `id-token: write` (what
  CI uses: a fresh GitHub OIDC token, audience `api://AzureADTokenExchange`, is requested for
  every file and written to a temporary file that is deleted afterwards, so a long build never
  outlives the short-lived token and no secret is stored); otherwise the `az login` session.
  All other Azure credential types are excluded, so nothing on the runner is picked up by
  accident.
- **Certificate in the store**: `NUGGET_SIGN_CERT_THUMBPRINT` (SHA-1 thumbprint of a code-signing
  certificate in `CurrentUser\My`), timestamp from `NUGGET_SIGN_TIMESTAMP_URL` (default
  DigiCert). The signing proof uses this mode. (The `.pfx` path in `release.yml` doesn't go
  through `sign.ps1`; it uses Tauri's own `certificateThumbprint` signing.)

Optional: `NUGGET_SIGN_DESCRIPTION`, `NUGGET_SIGNTOOL` (or `TAURI_WINDOWS_SIGNTOOL_PATH`),
`NUGGET_SIGN_DRY_RUN=1` (print the signtool command and metadata, sign nothing),
`NUGGET_SIGN_LOG` (append each signed path to a file).

In the workflow: with all five Azure variables, Trusted Signing is used (and a `.pfx`, if also
set, is ignored with a warning); with only some of them the build fails; with none, the `.pfx`
is used if present; with neither, the installers are unsigned with a warning.

**Other signing services.** Each ships a signtool plugin, a key storage provider or its own CLI;
add a mode to `sign.ps1` that turns the file path into one command and keeps the `exit` on
failure:

- **DigiCert KeyLocker**: log in `smctl` (`SM_HOST`, `SM_API_KEY`, `SM_CLIENT_CERT_FILE` /
  `_PASSWORD`), run `smctl windows certsync` once in the setup step, and the certificate appears
  in the store: use the thumbprint mode as is, or add a mode that calls
  `smctl sign --keypair-alias <alias> --input %1`.
- **SSL.com eSigner**: a mode that calls `CodeSignTool sign -username=... -password=...
  -totp_secret=... -credential_id=... -input_file_path=%1 -override=true`, with the credentials
  from `release-build` secrets.
- **USB token** (SafeNet, YubiKey): needs a self-hosted Windows runner with the token's
  minidriver; the certificate then appears in `CurrentUser\My` and the thumbprint mode works (the
  PIN prompt must be pre-authorised, e.g. SafeNet "single logon").

An EV certificate no longer skips SmartScreen reputation; a new publisher sees warnings until
downloads build reputation, whichever service signs.

### Public keys

- **Updater (minisign, `NUGGET_UPDATER_PUBKEY`):** *not generated yet; paste the text of
  `nugget-updater.key.pub` here after the ceremony (Step 4).*
- **Model manifest release key (ed25519, `NUGGET_MANIFEST_RELEASE_PUBKEY`):** *not generated
  yet; paste the 64-hex public key here (Step 5).*
- **Model manifest development key:** `DEV_PUBLIC_KEY_HEX` in `crates/nugget-models/src/manifest.rs`.

## 5. Updater behaviour in the app

Settings live in `settings.json` in the app config directory (for example
`~/.config/dev.nugget.desktop/` on Linux). The UI is the *Updates* card on the Status page; the tray
menu has an *Updates…* item that changes to *Update available: vX…*.

| Setting | Default | Effect |
|---|---|---|
| Channel | **Stable** | `stable.json` from the `updater-feed` release. **Beta** follows `beta.json` (prereleases as well as stable). **Off** never contacts the server. |
| Auto-install | **off** | Off means **notify only**: the app shows the version and its release notes, and nothing is downloaded until the user clicks *Install vX and restart*. On means the update is downloaded and verified in the background, its notes are shown, and it installs **when the user quits Nugget**. |

- Checks run 60 s after launch and then every 24 h, plus *Check now*. A check fetches static
  files only (the channel's JSON feed and its `.sig`, then the feed again through the updater
  plugin), with no identifiers beyond what any HTTPS request carries.
- **The feed is verified before it is used.** The app downloads `<channel>.json` and
  `<channel>.json.sig` and checks the minisign signature over the exact bytes against the
  built-in updater key (`apps/desktop/src-tauri/src/updater/feed.rs`). Nothing in the feed is
  parsed before that; a missing, malformed or wrong-key signature, or any changed byte, fails the
  check ("the update feed failed signature verification") and nothing is offered. Because
  `tauri-plugin-updater` fetches the feed itself, the app then accepts the plugin's result only
  if the plugin read the same feed: the same version and, when an update is offered, the
  identical JSON document. Otherwise the check fails ("the update feed changed while it was being
  checked") and the next check tries again.
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

The release rehearsal (§3) exercises this end to end for the AppImage and NSIS installs.

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
- **Windows signing plugin:** `Microsoft.ArtifactSigning.Client` 1.0.146, checksum-pinned in
  `sign.ps1`.
- **CI actions:** commit SHAs. **OS images:** the runner labels above (record the image version
  from the job log).

Determinism measures (`packaging/scripts/repro-env.sh`, applied by the "Deterministic build
environment" step of every release build):
- `SOURCE_DATE_EPOCH` = the tagged commit's timestamp, for every packaging tool that honours it;
  `stage-sidecar.sh` clamps the staged sidecar's mtime to it (`packaging/scripts/clamp_mtimes.py`).
- `RUSTFLAGS` with `--remap-path-prefix` for the workspace (`/build`), `CARGO_HOME` (`/cargo`) and
  the toolchain sysroot (`/rustc-sysroot`), in native form on Windows, so no runner paths from Rust
  code end up in binaries.
- No build cache in release jobs; the macOS tarball is written with fixed owner ids.

What is reproducible today: **`nuggetd` on x86_64 Linux**, checked by
`.github/workflows/reproducible.yml`, which builds it twice from checkouts in different
directories and compares them. Code and data are identical; the whole files still differ in
symbol names, because wasmtime's `bindgen!` in `nugget-plugin` embeds the WIT files' absolute path
(the job warns on that and fails on anything else). One input remains path-dependent: the vendored OpenSSL
(SQLCipher) compiles its install prefix, which lies under the cargo target dir, into the binary,
so the target dir's absolute path must match. Not verified: `nuggetd` on other targets and
`nugget-desktop` (which embeds the frontend bundle). Not reproducible: the installers. Tauri's
deb and rpm writers keep build-time mtimes and do not read `SOURCE_DATE_EPOCH`, the AppImage
bundles the runner's system libraries, and code signatures, notarization tickets and DMG/MSI
metadata vary. To check a release, rebuild the binaries and compare them with the ones extracted
from the installer, ignoring signature sections. Details, the exact commands and the full list of
gaps: [security/reproducible-builds.md](security/reproducible-builds.md).

## 8. Known gaps

- **No real key has signed anything yet.** Apple signing and notarization, Azure Trusted
  Signing, the updater key and the manifest release key have been exercised only with throwaway
  keys (rehearsal, signing proof) or not at all (Apple). The RC in Step 7 is their first real
  test. Notarization of the `nuggetd` sidecar relies on Tauri signing `externalBin` with the
  app's identity.
- **The build job's OIDC token.** `build` has `id-token: write` on every leg (job permissions
  can't depend on the matrix). On a real release, every step of every matrix leg, including the
  build scripts of third-party crates, can mint a token with the
  `repo:rayhankhilji/nugget:environment:release-build` subject and sign arbitrary files with the
  Azure identity for the job's lifetime. On the Windows leg that equals the exposure of an
  imported `.pfx`; the Apple secrets are likewise exported through `GITHUB_ENV` to every later
  step of the macOS legs. Scope the Entra app's role to the single certificate profile (Step 3)
  so a stolen token can do nothing else. The proper fix, not done yet, is to split compiling
  from `tauri bundle` and signing, so third-party code never runs in a job that can sign.
- **Prerelease versions and the MSI.** A non-numeric prerelease (`-rc.1`, `-beta.1`) fails the MSI
  build unless `bundle.windows.wix.version` is set by hand (Step 7); nothing derives it
  automatically yet.
- `actionlint` (1.7.7) passes on `windows-signing.yml` and `release-rehearsal.yml`; on
  `release.yml` its only complaint is the `macos-15-intel` runner label, which it doesn't know
  yet. It was run without `shellcheck`, so the `run:` scripts weren't linted.
- Smoke tests launch the AppImage, the aarch64 dmg and the NSIS install; the x64 dmg, the MSI and
  the `.deb`/`.rpm` are built but not launched.
- **Release rehearsal limits.** Only the AppImage and NSIS update paths are exercised, not the
  MSI, `.deb`, `.rpm` or dmg; macOS is not covered at all. The Windows leg signs and writes the
  feed on Windows, so its `latest.json` and `SHA256SUMS` can have CRLF line endings, while
  `release.yml`'s `sign` job runs on Linux; the Windows leg therefore doesn't test the exact
  bytes a release produces (the Linux leg does). It uses a localhost feed, not the real
  `updater-feed` URL.
- **Payload signatures aren't bound to a version** (hardening candidate). Neither `release.yml`
  nor the rehearsal passes `--app-version` to `tauri signer sign`, so a payload signature doesn't
  name the version it belongs to, and the updater's `requireSignedVersion` check can't be turned
  on. The feed signature and the "newer only" rule limit what a replayed payload can do, but
  binding the version would close it at the payload level. (Both options are in Tauri's current
  sources; check that the pinned CLI 2.11.5 and `tauri-plugin-updater` 2.12.0 have them before
  relying on this, or upgrade.)
- Linux packages aren't GPG-signed, and there's no apt/rpm repository yet; users get updates
  through the in-app updater or by downloading again.
- Update checks aren't recorded in the daemon's egress ledger (the desktop app makes them, not
  `nuggetd`).
- The `updater-feed` manifests are signed (§5), but the signature covers only the bytes, not the
  channel name or a date: someone who controls the feed URL can still replay an older signed feed
  or serve the beta feed to stable users (a signed prerelease), and can always withhold the feed.
  None of that installs anything the updater key didn't sign or anything older than the running
  version.
- The model manifest is still only bundled; there is no download path for newer manifests yet.
  Rollback protection (a per-key highest-version record in the model store,
  `docs/security/manifest-trust.md`) is in place.
