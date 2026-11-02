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
| macOS 11+, Apple Silicon / Intel | `Nugget_<v>_aarch64.dmg` / `Nugget_<v>_x64.dmg` | `/Applications/Nugget.app` (you drag it there) |
| Windows 10 and 11, x64 | `Nugget_<v>_x64-setup.exe` (recommended) | `%LOCALAPPDATA%\Nugget`, for your user only; no admin rights |
| Windows 10 and 11, x64 | `Nugget_<v>_x64_en-US.msi` | `C:\Program Files\Nugget`, needs admin rights |
| Linux x64 | `Nugget_<v>_amd64.AppImage`, `.deb`, `.rpm` | AppImage: anywhere; packages: `/usr/bin/` |
| Linux arm64 (experimental, may be missing) | `Nugget_<v>_aarch64.AppImage`, `_arm64.deb`, `.aarch64.rpm` | As for x64 |

Linux packages need glibc 2.35+, WebKitGTK 4.1, GTK 3 and libayatana-appindicator (the `.deb` and
`.rpm` pull these in). On Wayland the [Double Tap](double-tap.md) global shortcut does not fire yet;
start it from the tray or the app.

## Verify a download

A release also carries `SHA256SUMS`, SBOMs, update payloads and a GitHub build provenance
attestation for every file. `sha256sum -c --ignore-missing SHA256SUMS` (or `shasum -a 256` on macOS)
catches a damaged download. To check a file was built by this repository's release workflow, use the
[GitHub CLI](https://cli.github.com/) (after `gh auth login`):

```sh
gh attestation verify Nugget_<v>_aarch64.dmg -R rayhankhilji/nugget \
  --signer-workflow rayhankhilji/nugget/.github/workflows/release.yml
```

## Install

- **macOS:** open the `.dmg`, drag **Nugget** to **Applications**, and open it from there (run from
  the disk image, it can't start the background service at login). [Double Tap](double-tap.md) and
  [the agent](agent.md) need **System Settings → Privacy & Security → Accessibility**; macOS asks the
  first time.
- **Windows:** run the `setup.exe`, or the `.msi` for a machine-wide install. WebView2 is downloaded
  if missing.
- **Linux:** `chmod +x` and run the AppImage, or `sudo apt install ./Nugget_<v>_amd64.deb` (or
  `dnf`/`zypper install` the `.rpm`). There is no package repository and packages aren't GPG-signed;
  updates come through the [in-app updater](#updates). Double Tap and the agent need AT-SPI enabled.

The first launch opens a short setup; see [First run](first-run.md).

## Unsigned builds

Installers are unsigned until the project has signing certificates, so the OS warns:

- **macOS:** Control-click the app → **Open** → **Open**. On macOS 15+, try to open it once, then
  click **Open Anyway** in **System Settings → Privacy & Security**. If macOS says the app *is
  damaged*, use **Open Anyway** or run `xattr -dr com.apple.quarantine /Applications/Nugget.app`.
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

- **Start the Nugget background service at login** registers a per-user service (launchd agent,
  `systemd --user` unit, or Task Scheduler task) so sources stay indexed without the app open. Not
  available for an AppImage or an app run from the disk image. From a terminal:
  `nuggetd install-service` / `nuggetd uninstall-service` (on macOS the binary is
  `/Applications/Nugget.app/Contents/MacOS/nuggetd`; see [the command line](../reference/cli.md)).
- **Open Nugget in the tray when I log in** starts the app hidden in the tray or menu bar.

## Updates

The **Updates** card on the Status page (also **Updates…** in the tray menu):

| Setting | Default | What it does |
|---|---|---|
| Channel | Not chosen (nothing is checked) | **Stable**: releases. **Beta**: also prereleases. **Off**: never check. |
| Download updates automatically and install them when I quit Nugget | Off | Off: notify only; nothing downloads until you click **Install … and restart**. |

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
- For the desktop app: **Node.js 22** and **pnpm** (`corepack enable`).
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

### Build and run

```sh
git clone https://github.com/rayhankhilji/nugget && cd nugget
export NUGGET_TRUST_DEV_MANIFEST_KEY=1   # optional, see below
cargo build --release -p nuggetd
# Desktop app (optional)
cd apps/desktop && pnpm install --frozen-lockfile && pnpm build && cd ../..
cargo build --release -p nugget-desktop --features tauri/custom-protocol
```

`--features tauri/custom-protocol` embeds the interface; without it the window stays blank. Both
binaries land in `target/release/`; the app finds `nuggetd` next to itself or on `PATH`. Run
`./target/release/nugget-desktop` (it starts the daemon), or `./target/release/nuggetd` in the
foreground and check it with `./target/release/nuggetd status`.

Managed model downloads need a manifest signed with a key the build trusts. Release builds trust only
the release key, so without `NUGGET_TRUST_DEV_MANIFEST_KEY=1` (which trusts the repository's
development key; debug builds always do) your build offers no model downloads. Ollama, LM Studio and
cloud keys work either way.

To build the installers (unsigned, no updater key; on Linux also install `rpm`), run
`packaging/scripts/stage-sidecar.sh`, then in `apps/desktop`:
`pnpm install --frozen-lockfile && pnpm tauri build --config src-tauri/tauri.bundle.conf.json`.
Output goes to `target/release/bundle/`.
