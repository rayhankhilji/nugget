# Install

> **No published release yet.** The release pipeline exists: it builds signed installers for every
> platform and an in-app updater (see
> [Releases](https://github.com/rayhankhilji/nugget/blob/main/docs/release.md)), but no version has been
> released. Until one is, build Nugget from source with the steps below (the same ones CI runs on every
> change), or build the installers yourself ([below](#building-the-installers-yourself)).

| Platform | Installers (from the release pipeline) | Build from source |
|---|---|---|
| macOS (Apple Silicon and Intel) | `.dmg` (signed and notarised when the release has Apple credentials) | Yes |
| Windows 10 and 11 (x64) | Per-user NSIS `-setup.exe` and `.msi` | Yes |
| Linux x64 and arm64 (X11 and Wayland) | AppImage, `.deb`, `.rpm` | Yes |

Each installer bundles the desktop app and `nuggetd`. CI builds and tests on all three platforms. The
[M0 follow-ups](https://github.com/rayhankhilji/nugget/blob/main/docs/m0-followups.md) still list
service install and the keychain backends on real macOS and Windows machines as not yet verified
outside CI.

## 1. Prerequisites

All platforms:

- **Rust** through [rustup](https://rustup.rs/). The repository's `rust-toolchain.toml` picks the
  toolchain. The minimum supported Rust version is 1.88.
- **A C toolchain**, `make` and `perl`. The store builds SQLCipher and a vendored OpenSSL from source.
- For the desktop app: **Node.js 22** and **pnpm** (`corepack enable` picks the pinned version).

### macOS

Install the Xcode command-line tools:

```sh
xcode-select --install
```

### Windows

Install the **MSVC Build Tools** (Desktop development with C++) and
[Strawberry Perl](https://strawberryperl.com/). Git's bundled Perl does not work for the OpenSSL build.
If it comes first on your `PATH`, point the build at Strawberry Perl:

```powershell
$env:OPENSSL_SRC_PERL = "C:\Strawberry\perl\bin\perl.exe"
```

The desktop app also needs WebView2, which Windows 10 and 11 already include.

### Linux (Debian, Ubuntu)

```sh
sudo apt-get install -y build-essential perl \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libsoup-3.0-dev libssl-dev file
```

The second and third lines are only needed for the desktop app. For other distributions, see the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).

## 2. Build

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

## 3. Run

Start the desktop app, and it starts the daemon for you:

```sh
./target/release/nugget-desktop
```

Or run the daemon on its own, in the foreground:

```sh
./target/release/nuggetd
```

To start the daemon automatically when you log in, install it as a per-user service (a launchd agent on
macOS, a `systemd --user` unit on Linux, a Task Scheduler logon task on Windows). Nothing needs root or
administrator rights:

```sh
./target/release/nuggetd install-service
```

See [the command line](../reference/cli.md) for details, and [First run](first-run.md) for what happens
next.

## Building the installers yourself

From the repository root, after the prerequisites above (on Linux, add `rpm` for `.rpm` output):

```sh
packaging/scripts/stage-sidecar.sh
cd apps/desktop
pnpm install --frozen-lockfile
pnpm tauri build --config src-tauri/tauri.bundle.conf.json
```

The first script builds `nuggetd` in release mode and puts it where the bundle expects it. The
installers land under `target/release/bundle/`. A local build has no updater key, so it cannot update
itself.

With an installed app, the app offers once, on the Status page, to start the background service when
you log in. It runs the bundled `nuggetd install-service` for you.

## Uninstall

```sh
nuggetd uninstall-service
```

Then delete the [data, config and cache directories](../reference/data-dir.md). Your data stays on disk
until you do. The store's key is in your OS keychain under the service `dev.nugget.Nugget`; delete
that entry too if you want nothing left behind.
