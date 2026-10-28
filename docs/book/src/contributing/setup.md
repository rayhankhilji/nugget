# Development setup

The canonical guide is
[`CONTRIBUTING.md`](https://github.com/rayhankhilji/nugget/blob/main/CONTRIBUTING.md); this page
summarises it.

## Tools

- **Rust** through rustup. `rust-toolchain.toml` selects stable with `rustfmt` and `clippy`. The MSRV
  is the workspace `rust-version` (1.88), checked with `cargo +1.88 check`.
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
| `apps/desktop` | The Tauri 2 shell: `src/` (React, TypeScript), `src-tauri/` (Rust) |
| `bench` | `nugget-bench` and its report schema |
| `manifests` | The signed model and runtime manifest |
| `docs` | The plan, milestone specs, clean-room research, and this book (`docs/book`) |
| `.github/workflows` | CI (`ci.yml`) and the docs build (`docs.yml`) |

## CI

`ci.yml` runs on Linux, macOS and Windows: `cargo fmt`, clippy with `-D warnings`, the tests, a
benchmark smoke run (plus the real-stack `memory` smoke on Linux), the desktop build, `cargo deny` and
the DCO check. `docs.yml` builds this book, checks its links, and checks it against the code.
