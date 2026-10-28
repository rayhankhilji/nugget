# Development setup

The canonical guide is
[`CONTRIBUTING.md`](https://github.com/rayhankhilji/nugget/blob/main/CONTRIBUTING.md); this page
summarises it.

## Tools

- **Rust** through rustup. `rust-toolchain.toml` selects stable with `rustfmt` and `clippy`. The MSRV
  is the workspace `rust-version` (1.88). CI builds with stable; to check the MSRV locally, install
  that toolchain and run `cargo +1.88 check -p <crate>`.
- **A C toolchain**, `make` and `perl` for SQLCipher and its vendored OpenSSL. On Windows use the MSVC
  Build Tools and [Strawberry Perl](https://strawberryperl.com/), and set `OPENSSL_SRC_PERL` if Git's
  Perl comes first on your `PATH`.
- **Desktop app**: Node.js 22, pnpm (`corepack enable`), and the
  [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).
- **Optional**: `cargo-deny` (`cargo install cargo-deny --locked`) for licence and advisory checks, and
  mdBook for these docs (see [Working on these docs](docs.md)).

## Scoped builds

The workspace's `default-members` leave out the desktop crate, so `cargo build` and `cargo test` work
without webview libraries. **Build and test only what you change**: feedback is faster, and several
people and agents often share one target directory (waiting on its lock is normal).

```sh
# One crate
cargo fmt -p nugget-store
cargo clippy -p nugget-store --all-targets -- -D warnings
cargo test -p nugget-store

# Everything CI checks, except the desktop app
cargo fmt --all --check
cargo clippy --workspace --all-targets --exclude nugget-desktop -- -D warnings
cargo test --workspace --exclude nugget-desktop

# Desktop app
cd apps/desktop && pnpm install --frozen-lockfile && pnpm build && cd ../..
cargo clippy -p nugget-desktop --all-targets -- -D warnings
cargo build -p nugget-desktop
cargo test -p nugget-desktop
cd apps/desktop && pnpm test && cd ../..        # frontend unit tests (vitest)

# Benchmarks (release builds for numbers you quote)
cargo run -p nugget-bench -- --list
cargo run -p nugget-bench -- all --scale smoke --no-write

# This book against the code
python3 docs/book/check-refs.py

# Licences, advisories, sources
cargo deny check licenses bans sources
```

`--workspace` includes `nugget-desktop`; pass `--exclude nugget-desktop` unless its system libraries
are installed and the frontend is built.

## Running a development daemon

Keep development data away from your real data with `--home`:

```sh
cargo run -p nuggetd -- --home /tmp/nugget-dev
cargo run -p nuggetd -- --home /tmp/nugget-dev status
```

With `NUGGET_KEYCHAIN=off`, the store key goes to a file and source secrets stay in memory, which
keeps a scratch daemon out of your keychain. `NUGGET_LOG=debug` shows what it is doing.

## Repository layout

| Path | What lives there |
|---|---|
| `crates/` | Every library crate and the daemon (`nuggetd`); see the [crate map](../architecture/crates.md) |
| `crates/nuggetd/src/features/` | One module per subsystem wired into the daemon after M2 (being filled in wave W) |
| `crates/nugget-plugin/sdk` | The plugin guest SDK and example plugins: a separate cargo workspace built for `wasm32-wasip2` |
| `apps/desktop` | The Tauri 2 shell: `src/` (React, TypeScript), `src-tauri/` (Rust) |
| `extensions/browser` | The browser extension for `nugget-browser` |
| `bench` | `nugget-bench`, its suites, fixtures and report schema |
| `manifests` | The signed model and runtime manifest |
| `packaging` | Installer, sidecar, icon and update-manifest scripts |
| `docs` | The plan, milestone specs, clean-room research, wave briefs, the release guide and this book (`docs/book`) |
| `.github/workflows` | CI (`ci.yml`), the docs build (`docs.yml`) and releases (`release.yml`) |

## CI

`ci.yml` runs on Linux, macOS and Windows: `cargo fmt`, clippy with `-D warnings`, the tests, a
benchmark smoke run of every suite (plus the real-stack `memory` smoke and a report schema check on
Linux), the desktop frontend build with clippy, build and tests for `nugget-desktop`, `cargo deny` and
the DCO check on pull requests. `docs.yml` runs `check-refs.py`, builds this book and checks its links.
`release.yml` builds installers and signed updates on version tags; see
[`docs/release.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/release.md).
