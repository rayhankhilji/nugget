# Install

> **No release has been published yet**, and the signing keys don't exist yet. Until then, either
> [build from source](#build-from-source) or download the unsigned
> [dry-run installers](#dry-run-installers), which have no updater and show the
> [unsigned-build warnings](#unsigned-builds).

## What a release contains

Every installer bundles the desktop app (`nugget-desktop`) and the background service (`nuggetd`).
`<v>` is the version.

| Platform | File | Installs to |
|---|---|---|
| macOS 11+, Apple Silicon / Intel | `Heed_<v>_aarch64.dmg` / `Heed_<v>_x64.dmg` | `/Applications/Heed.app` (you drag it there) |
| Windows 10 and 11, x64 | `Heed_<v>_x64-setup.exe` (recommended) | `%LOCALAPPDATA%\Heed`, for your user only; no admin rights |
| Windows 10 and 11, x64 | `Heed_<v>_x64_en-US.msi` | `C:\Program Files\Heed`, needs admin rights |
| Linux x64 | `Heed_<v>_amd64.AppImage`, `.deb`, `.rpm` | AppImage: anywhere; packages: `/usr/bin/` |
| Linux arm64 (experimental, may be missing) | `Heed_<v>_aarch64.AppImage`, `_arm64.deb`, `.aarch64.rpm` | As for x64 |

Linux packages need glibc 2.35+, WebKitGTK 4.1, GTK 3 and libayatana-appindicator (the `.deb` and
`.rpm` pull these in). On Wayland the [Double Tap](double-tap.md) global shortcut does not fire yet;
start it from the tray or the app.

## Verify a download

A release also carries `SHA256SUMS`, SBOMs, update payloads and a GitHub build provenance
attestation for every file. `sha256sum -c --ignore-missing SHA256SUMS` (or `shasum -a 256` on macOS)
catches a damaged download. To check a file was built by this repository's release workflow, use the
[GitHub CLI](https://cli.github.com/) (after `gh auth login`):

```sh
gh attestation verify Heed_<v>_aarch64.dmg -R rayhankhilji/nugget \
  --signer-workflow rayhankhilji/nugget/.github/workflows/release.yml
```

## Install

- **macOS:** open the `.dmg`, drag **Heed** to **Applications**, and open it from there (run from
  the disk image, it can't start the background service at login). [Double Tap](double-tap.md) and
  [the agent](agent.md) need **System Settings → Privacy & Security → Accessibility**; macOS asks the
  first time.
- **Windows:** run the `setup.exe`, or the `.msi` for a machine-wide install. WebView2 is downloaded
  if missing.
- **Linux:** `chmod +x` and run the AppImage, or `sudo apt install ./Heed_<v>_amd64.deb` (or
  `dnf`/`zypper install` the `.rpm`). There is no package repository and packages aren't GPG-signed;
  updates come through the [in-app updater](#updates). Double Tap and the agent need AT-SPI enabled.

The first launch opens a short setup; see [First run](first-run.md).

## Unsigned builds

Installers are unsigned until the project has signing certificates, so the OS warns:

- **macOS:** Control-click the app → **Open** → **Open**. On macOS 15+, try to open it once, then
  click **Open Anyway** in **System Settings → Privacy & Security**. If macOS says the app *is
  damaged*, use **Open Anyway** or run `xattr -dr com.apple.quarantine /Applications/Heed.app`.
  macOS may ask for Accessibility again after you install a different build.
- **Windows:** SmartScreen: **More info → Run anyway**. Linux shows no warning.

### Dry-run installers

From a recent successful run of
[Actions → Release](https://github.com/rayhankhilji/nugget/actions/workflows/release.yml), download
`bundle-<target>` (for example `bundle-aarch64-apple-darwin`) from **Artifacts** while signed in to
GitHub (kept 14 days), or run `gh run download <run-id> -R rayhankhilji/nugget -n bundle-<target>`.
They have no checksums, attestations or updater; install a newer one over the old one.

## Start at login

Both settings are on the **Status** page:

- **Start the Heed background service at login** registers a per-user service (launchd agent,
  `systemd --user` unit, or Task Scheduler task) so sources stay indexed without the app open. Not
  available for an AppImage or an app run from the disk image. From a terminal:
  `nuggetd install-service` / `nuggetd uninstall-service` (on macOS the binary is
  `/Applications/Heed.app/Contents/MacOS/nuggetd`; see [the command line](../reference/cli.md)).
- **Open Heed in the tray when I log in** starts the app hidden in the tray or menu bar.

## Updates

The **Updates** card on the Status page (also **Updates…** in the tray menu):

| Setting | Default | What it does |
|---|---|---|
| Channel | Not chosen (nothing is checked) | **Stable**: releases. **Beta**: also prereleases. **Off**: never check. |
| Download updates automatically and install them when I quit Heed | Off | Off: notify only; nothing downloads until you click **Install … and restart**. |

Setup asks for a channel. Then the app checks a minute after launch, daily, and on **Check now**,
fetching only the signed feed from GitHub with no identifiers (logged in the
[egress ledger](privacy.md#the-egress-ledger) as `update_check` and `update_download`). Signatures are
verified against a key built into the app. Dry runs and your own builds can't update themselves.

## Uninstall

1. Turn off both login settings on the Status page (or run `nuggetd uninstall-service`), then quit.
2. Remove the app: Trash on macOS; **Settings → Apps → Installed apps** on Windows;
   `sudo apt remove nugget` (or `dnf`, `zypper`), or delete the AppImage, on Linux.

Your data (models, encrypted store, settings, logs, keychain entries, `~/Nugget Vault`) stays. The
Windows `setup.exe` uninstaller offers to remove it; elsewhere, see [Uninstall](uninstall.md).

## Build from source

### Prerequisites

- **Rust** via [rustup](https://rustup.rs/) (`rust-toolchain.toml` picks the toolchain; MSRV 1.88).
- A C toolchain, `make` and `perl` (SQLCipher and OpenSSL are built from source).
- For the desktop app: **Node.js 22** or newer and **pnpm** 10.28.0 (`corepack enable`; Node 25
  and newer no longer ship corepack, so there run `npm install -g pnpm@10.28.0`).
- **macOS:** `xcode-select --install`.
- **Windows:** MSVC Build Tools (Desktop development with C++) and
  [Strawberry Perl](https://strawberryperl.com/); Git's Perl doesn't work. If it comes first on
  `PATH`, set `$env:OPENSSL_SRC_PERL = "C:\Strawberry\perl\bin\perl.exe"`.
- **Debian, Ubuntu** (lines 2–3 only for the desktop app; other distros: see the
  [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)):

  ```sh
  sudo apt-get install -y build-essential perl \
    libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
    librsvg2-dev libxdo-dev libsoup-3.0-dev libssl-dev file
  ```

### macOS: build Heed.app

On an Intel or Apple Silicon Mac, one script builds the app, signs it ad hoc (no Apple Developer ID
needed) and tells you where it is. Paste each block into Terminal as it is.

1. Install the tools: the Xcode command line tools, Rust, Node.js (22 or newer, for example from
   [nodejs.org](https://nodejs.org/)) and pnpm.

   ```sh
   xcode-select --install
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

   Open a new Terminal window so `cargo` is on your `PATH`, then:

   ```sh
   npm install -g pnpm@10.28.0
   ```

2. Get the code and build the app. The first build takes a while.

   ```sh
   git clone https://github.com/rayhankhilji/nugget
   cd nugget
   scripts/build-macos-app.sh
   ```

   The script builds `nuggetd` and the app (with the frontend embedded), trusts the repository's
   development model-manifest key so managed model downloads work, and signs `Heed.app` ad hoc.
   It ends by printing where `Heed.app` is: `target/x86_64-apple-darwin/release/bundle/macos/`
   on Intel, `target/aarch64-apple-darwin/release/bundle/macos/` on Apple Silicon.

3. Move it to Applications and open it from there. Either drag `Heed.app` into Applications in
   Finder, or (on Intel; on Apple Silicon use `aarch64-apple-darwin`):

   ```sh
   rm -rf /Applications/Heed.app
   ditto target/x86_64-apple-darwin/release/bundle/macos/Heed.app /Applications/Heed.app
   open /Applications/Heed.app
   ```

   Don't start it with `./target/.../nugget-desktop` from Terminal: macOS then files the
   permissions below under Terminal instead of Heed.

4. Turn on the permissions. Setup and the Double Tap page show what is missing, with a button that
   opens the right page of System Settings → Privacy & Security, and check again when you come
   back:
   - **Accessibility**: read the text field you're in and insert the draft.
   - **Input Monitoring**: notice your Double Tap key.
   - **Full Disk Access**: only if you add a Messages source (it reads `~/Library/Messages`).

   If Heed isn't in a list, add it with **+** from Applications. Each build is signed afresh, so
   after a rebuild macOS can show Heed as on but still refuse it. Reset its entries, reopen
   Heed and turn them on again:

   ```sh
   tccutil reset Accessibility app.heed.desktop
   tccutil reset ListenEvent app.heed.desktop
   tccutil reset SystemPolicyAllFiles app.heed.desktop
   ```

### Build and run

```sh
git clone https://github.com/rayhankhilji/nugget && cd nugget
export HEED_TRUST_DEV_MANIFEST_KEY=1
cargo build --release -p nuggetd
cd apps/desktop && pnpm install --frozen-lockfile && pnpm build && cd ../..
cargo build --release -p nugget-desktop --features tauri/custom-protocol
```

`HEED_TRUST_DEV_MANIFEST_KEY=1` is optional (see below); the last two lines build the desktop app
and can be skipped. `--features tauri/custom-protocol` embeds the interface; without it the window
would stay blank, so a release build without it stops with an error instead.

Both binaries land in `target/release/`; the app finds `nuggetd` next to itself or on `PATH`. Run
`./target/release/nugget-desktop` (it starts the daemon), or `./target/release/nuggetd` in the
foreground and check it with `./target/release/nuggetd status`.

Managed model downloads need a manifest signed with a key the build trusts. Release builds trust only
the release key, so without `HEED_TRUST_DEV_MANIFEST_KEY=1` (which trusts the repository's
development key; debug builds always do) your build offers no model downloads. Ollama, LM Studio and
cloud keys work either way.

To build the installers (unsigned, no updater key; on Linux also install `rpm`), run
`packaging/scripts/stage-sidecar.sh`, then in `apps/desktop`:
`pnpm install --frozen-lockfile && pnpm tauri build --config src-tauri/tauri.bundle.conf.json`.
Output goes to `target/release/bundle/`.
