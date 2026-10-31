# Install

> **No release has been published yet.** The release pipeline is built (see
> [release process](https://github.com/rayhankhilji/nugget/blob/main/docs/release.md)), but no version has
> been released and the signing keys don't exist yet. Until then you can:
>
> - [build from source](#build-from-source), the same steps CI runs on every change; or
> - download the **unsigned installers from a release dry run** ([below](#dry-run-installers)), which
>   have no updater and show the [unsigned-build warnings](#unsigned-builds).
>
> The rest of this page describes what a release will contain, so it is ready when one exists.

## What a release contains

Every installer bundles the desktop app (`nugget-desktop`) and the background service (`nuggetd`)
next to each other. `<v>` is the version, for example `1.0.0`. Sizes are from the 0.0.1 dry run.

| Platform | File | Size | Installs to |
|---|---|---|---|
| macOS 11+, Apple Silicon | `Nugget_<v>_aarch64.dmg` | 26.5 MiB | `/Applications/Nugget.app` (you drag it there) |
| macOS 11+, Intel | `Nugget_<v>_x64.dmg` | 28.9 MiB | `/Applications/Nugget.app` |
| Windows 10 and 11, x64 | `Nugget_<v>_x64-setup.exe` (NSIS, recommended) | 19.4 MiB | `%LOCALAPPDATA%\Nugget`, for your user only; no administrator rights |
| Windows 10 and 11, x64 | `Nugget_<v>_x64_en-US.msi` | 28.1 MiB | `C:\Program Files\Nugget`, needs administrator rights |
| Linux x64 | `Nugget_<v>_amd64.AppImage` | 105.4 MiB | Wherever you keep the file; carries its own libraries |
| Linux x64 | `Nugget_<v>_amd64.deb` (Debian, Ubuntu) | 34.1 MiB | `/usr/bin/nugget-desktop`, `/usr/bin/nuggetd` |
| Linux x64 | `Nugget-<v>-1.x86_64.rpm` (Fedora, openSUSE) | 34.1 MiB | Same as the `.deb` |
| Linux arm64 | `Nugget_<v>_aarch64.AppImage`, `Nugget_<v>_arm64.deb`, `Nugget-<v>-1.aarch64.rpm` | 102.6 / 33.2 / 33.2 MiB | As for x64. Experimental: the arm64 build may fail without stopping a release, so a release can lack these files |

The Linux packages are built on Ubuntu 22.04 and need glibc 2.35 or later, WebKitGTK 4.1, GTK 3
and libayatana-appindicator (the `.deb` and `.rpm` pull these in). They work on X11 and Wayland;
on Wayland the [Double Tap](double-tap.md) global shortcut does not fire yet, so start it from the
tray or the app.

A release also attaches files you don't install:

| File | What it is |
|---|---|
| `Nugget_<v>_<arch>.app.tar.gz` | The macOS update payload, used by the in-app updater |
| `*.sig` | The updater's signature for each update payload |
| `latest.json`, `latest.json.sig` | The update feed and its signature |
| `SHA256SUMS` | SHA-256 checksums of every other file in the release |
| `nuggetd.cdx.json`, `nugget-desktop.cdx.json`, `nugget-desktop-frontend.cdx.json` | Software bills of materials (CycloneDX) |

Every file except `SHA256SUMS` also has a build provenance attestation on GitHub, which proves it
was built by the project's release workflow from the tagged commit.

## Verify a download

Download `SHA256SUMS` next to your installer, then check it. `--ignore-missing` skips the files you
didn't download.

On Linux:

```sh
sha256sum -c --ignore-missing SHA256SUMS
```

On macOS:

```sh
grep ' Nugget_<v>_aarch64.dmg$' SHA256SUMS | shasum -a 256 -c
```

On Windows (PowerShell), compare the output with the file's line in `SHA256SUMS`:

```powershell
(Get-FileHash .\Nugget_<v>_x64-setup.exe -Algorithm SHA256).Hash.ToLower()
```

The checksum file comes from the same place as the installer, so it only catches a damaged
download. To check where the file came from, verify its provenance with the
[GitHub CLI](https://cli.github.com/):

```sh
gh auth login    # once; gh needs a GitHub login to fetch attestations
gh attestation verify Nugget_<v>_aarch64.dmg -R rayhankhilji/nugget \
  --signer-workflow rayhankhilji/nugget/.github/workflows/release.yml
```

It succeeds only if the file was built by this repository's release workflow.

## Install

### macOS

Open the `.dmg` and drag **Nugget** to **Applications**, then open it from Applications. (Run from
the disk image, it can't start the background service at login.)

[Double Tap](double-tap.md) and [the agent](agent.md) need Nugget to be allowed under **System
Settings → Privacy & Security → Accessibility**. macOS asks the first time you use Double Tap, and
the setup's Permissions step explains why.

### Windows

Run `Nugget_<v>_x64-setup.exe`. It installs for your user only and needs no administrator rights.
Use the `.msi` instead if you want a machine-wide install in Program Files (for example, deployed
by an administrator). The app needs WebView2, which Windows 11 and most Windows 10 installs
already have; if it is missing, the installer downloads it.

### Linux

```sh
# AppImage: no install step
chmod +x Nugget_<v>_amd64.AppImage
./Nugget_<v>_amd64.AppImage

# Debian, Ubuntu
sudo apt install ./Nugget_<v>_amd64.deb

# Fedora, openSUSE
sudo dnf install ./Nugget-<v>-1.x86_64.rpm    # or: sudo zypper install ./Nugget-<v>-1.x86_64.rpm
```

There is no apt or rpm repository, and the packages aren't GPG-signed; use the checksums and the
attestation above. Updates come through the [in-app updater](#updates). Double Tap and the agent
need accessibility (AT-SPI) enabled in your desktop.

### Then

The first launch opens a short setup; see [First run](first-run.md).

## Unsigned builds

Until the project has its signing certificates, every installer is unsigned (the macOS app is
ad-hoc signed and not notarised). The operating system warns before opening it:

- **macOS:** *"Nugget" can't be opened because it is from an unidentified developer*: Control-click
  the app → **Open** → **Open**. On macOS 15 and later that route is gone: try to open it once, then
  go to **System Settings → Privacy & Security** and click **Open Anyway**. If macOS instead says the
  app *is damaged* (a downloaded copy), Control-click doesn't help; use **Open Anyway** on macOS 15
  and later, or clear the download flag:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Nugget.app
  ```

  Because an ad-hoc signature changes with every build, macOS may ask for Accessibility again after
  you install a different build.
- **Windows:** SmartScreen shows *Windows protected your PC*. Click **More info → Run anyway**.
- **Linux:** no warning.

A signed release removes these warnings on macOS (signed with a Developer ID and notarised by
Apple). On Windows, a signed installer names the publisher, but SmartScreen may still warn for a
new publisher until enough people have downloaded it.

### Dry-run installers

Each run of the release workflow's dry run uploads unsigned installers as workflow artifacts:
open [Actions → Release](https://github.com/rayhankhilji/nugget/actions/workflows/release.yml), pick
a recent successful run, and download `bundle-<target>` from its **Artifacts**, for example
`bundle-aarch64-apple-darwin` or `bundle-x86_64-pc-windows-msvc`. You need to be signed in to
GitHub, and artifacts are kept for 14 days. Each downloads as a `.zip`; extract it to get the
installers. With the GitHub CLI, which extracts it for you:

```sh
gh run download <run-id> -R rayhankhilji/nugget -n bundle-x86_64-unknown-linux-gnu
```

Dry-run builds have no `SHA256SUMS` or attestations, and no updater: to move to a newer build,
install it over the old one.

## Start at login

Two separate settings, both on the **Status** page:

- **Start the Nugget background service at login.** The app asks once (*Start the Nugget background
  service when you log in?*). **Yes** registers a per-user service: a launchd agent on macOS, a
  `systemd --user` unit on Linux, a Task Scheduler task on Windows. It keeps your sources indexed
  without the app open. No installer does this on its own, and nothing needs administrator rights.
  It isn't available for an AppImage or for an app run from the disk image, because their paths
  change on every run; install the `.deb` or `.rpm`, or move the app to Applications.
- **Open Nugget in the tray when I log in** starts the desktop app hidden in the tray or menu bar.

Installing the service on macOS and Windows is not yet exercised in CI.

From a terminal, `nuggetd install-service` and `nuggetd uninstall-service` do the same as the first
setting (on macOS, the binary is `/Applications/Nugget.app/Contents/MacOS/nuggetd`). See
[the command line](../reference/cli.md).

## Updates

The **Updates** card on the Status page (also **Updates…** in the tray menu):

| Setting | Default | What it does |
|---|---|---|
| Channel | **Stable** | **Stable** gets released versions. **Beta** also gets prereleases. **Off** never checks. |
| Download updates automatically and install them when I quit Nugget | **Off** | Off means notify only: the app shows the new version and its notes, and downloads nothing until you click **Install … and restart**. On downloads and verifies the update in the background and installs it when you quit. |

Checks run a minute after launch and then once a day, or on **Check now**. A check downloads only
the channel's feed and its signature, and sends no identifiers. The app checks the feed's signature
before reading it and each update's signature before installing, against a key built into the app,
and never offers a version older than the one you have. Each install updates itself the way it was
installed: an AppImage replaces its file, a `.deb` or `.rpm` asks for your password, and on Windows
the installer runs with a progress bar.

A build without the project's updater key (a dry run or your own build) says *This build has no
update signing key* and can't update itself.

## Uninstall

1. On the Status page, turn off **Start the Nugget background service at login** and **Open Nugget
   in the tray when I log in**, then quit Nugget from the tray or menu bar. (Or run
   `nuggetd uninstall-service`.)
2. Remove the app:
   - **macOS:** drag `/Applications/Nugget.app` to the Trash.
   - **Windows:** **Settings → Apps → Installed apps → Nugget → Uninstall**. Uninstalling the NSIS
     install also removes the login task.
   - **Linux:** `sudo apt remove nugget`, `sudo dnf remove nugget` or `sudo zypper remove nugget`;
     for an AppImage, delete the file.
3. Your data stays until you delete it:
   - the daemon's [data, config and cache directories](../reference/data-dir.md) and the vault
     (`~/Nugget Vault` by default);
   - the desktop app's own directory, `dev.nugget.desktop` (its settings, history and window
     state): `~/.config/dev.nugget.desktop` and `~/.local/share/dev.nugget.desktop` on Linux,
     `~/Library/Application Support/dev.nugget.desktop` on macOS, `%APPDATA%\dev.nugget.desktop` on
     Windows. The webview keeps its data in directories of the same name (for example
     `%LOCALAPPDATA%\dev.nugget.desktop` on Windows, `~/Library/WebKit/dev.nugget.desktop` and
     `~/Library/Caches/dev.nugget.desktop` on macOS);
   - your OS keychain: delete every entry under the service `dev.nugget.Nugget`. That holds the
     store's encryption key, your API keys and your account tokens.

## Build from source

### Prerequisites

All platforms:

- **Rust** through [rustup](https://rustup.rs/). The repository's `rust-toolchain.toml` picks the
  toolchain. The minimum supported Rust version is 1.88.
- **A C toolchain**, `make` and `perl`. The store builds SQLCipher and a vendored OpenSSL from source.
- For the desktop app: **Node.js 22** and **pnpm** (`corepack enable` picks the pinned version).

**macOS:** install the Xcode command-line tools:

```sh
xcode-select --install
```

**Windows:** install the **MSVC Build Tools** (Desktop development with C++) and
[Strawberry Perl](https://strawberryperl.com/). Git's bundled Perl does not work for the OpenSSL
build. If it comes first on your `PATH`, point the build at Strawberry Perl:

```powershell
$env:OPENSSL_SRC_PERL = "C:\Strawberry\perl\bin\perl.exe"
```

**Linux (Debian, Ubuntu):**

```sh
sudo apt-get install -y build-essential perl \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libsoup-3.0-dev libssl-dev file
```

The second and third lines are only needed for the desktop app. For other distributions, see the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).

### Build

```sh
git clone https://github.com/rayhankhilji/nugget
cd nugget

# The daemon
cargo build --release -p nuggetd

# The desktop app (optional)
cd apps/desktop && pnpm install --frozen-lockfile && pnpm build && cd ../..
cargo build --release -p nugget-desktop
```

Both binaries land in `target/release/`: `nuggetd` and `nugget-desktop` (`.exe` on Windows). The app
looks for `nuggetd` next to itself first, then on your `PATH`, so keeping them together is enough.

**Local models in your own release build.** Nugget downloads its local models only from a signed
model manifest. A release build trusts only the project's release key, which a build from source
doesn't have, so it offers no model downloads; other providers (Ollama, LM Studio, any
OpenAI-compatible server, or a cloud key) still work. To use the built-in downloads, build `nuggetd`
with the development key trusted, which signs the manifest in the repository:

```sh
NUGGET_TRUST_DEV_MANIFEST_KEY=1 cargo build --release -p nuggetd
```

The daemon logs a warning that it trusts the development key. Debug builds always trust it.

### Run

Start the desktop app, and it starts the daemon for you:

```sh
./target/release/nugget-desktop
```

Or run the daemon on its own, in the foreground, and check it from another terminal:

```sh
./target/release/nuggetd
./target/release/nuggetd status
```

`./target/release/nuggetd install-service` starts it at login, as [above](#start-at-login). See
[the command line](../reference/cli.md) for details, and [First run](first-run.md) for what happens
next.

### Building the installers yourself

From the repository root, after the prerequisites above (on Linux, add `rpm` for `.rpm` output):

```sh
packaging/scripts/stage-sidecar.sh
cd apps/desktop
pnpm install --frozen-lockfile
pnpm tauri build --config src-tauri/tauri.bundle.conf.json
```

The first script builds `nuggetd` in release mode and puts it where the bundle expects it (export
`NUGGET_TRUST_DEV_MANIFEST_KEY=1` first for model downloads, as above). The installers land under
`target/release/bundle/`. They are unsigned and have no updater key, so they can't update
themselves.
