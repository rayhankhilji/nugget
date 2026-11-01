# Development setup

The canonical guide is
[`CONTRIBUTING.md`](https://github.com/rayhankhilji/nugget/blob/main/CONTRIBUTING.md).

**Tools:** Rust via rustup (`rust-toolchain.toml`; MSRV 1.88); a C toolchain, `make` and `perl` for
SQLCipher (on Windows: MSVC Build Tools and Strawberry Perl, with `OPENSSL_SRC_PERL` if Git's Perl comes
first); for the desktop app Node.js 22, pnpm and the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/); optionally `cargo-deny` and mdBook.

Build and test only what you change; `default-members` leave out the desktop crate.

```sh
cargo clippy -p nugget-store --all-targets -- -D warnings && cargo test -p nugget-store
cargo fmt --all --check
cargo clippy --workspace --all-targets --exclude nugget-desktop -- -D warnings
cargo test --workspace --exclude nugget-desktop
(cd apps/desktop && pnpm install --frozen-lockfile && pnpm build && pnpm test)
cargo test -p nugget-desktop
python3 docs/book/check-refs.py
cargo deny check licenses bans sources
```

Run a development daemon away from your real data:

```sh
cargo run -p nuggetd -- --home /tmp/nugget-dev
cargo run -p nuggetd -- --home /tmp/nugget-dev status
```

`NUGGET_KEYCHAIN=off` keeps it out of your keychain; `NUGGET_LOG=debug` shows what it does.

| Path | Contents |
|---|---|
| `crates/` | Libraries and `nuggetd` ([crate map](../architecture/crates.md)); daemon features in `crates/nuggetd/src/features/` |
| `crates/nugget-plugin/sdk` | Plugin guest SDK and examples (separate workspace, `wasm32-wasip2`) |
| `apps/desktop` | Tauri 2 shell: `src/` (React, TypeScript), `src-tauri/` (Rust) |
| `extensions/browser` | Browser extension |
| `bench`, `manifests`, `packaging` | Benchmarks, the signed model manifest, installer scripts |
| `docs` | The plan, clean-room research, release guide and this book |

CI (`ci.yml`) runs fmt, clippy, tests, a benchmark smoke run, the desktop build, `cargo deny` and the
DCO check on all three OSes; `docs.yml` runs `check-refs.py` and builds the book; `release.yml` builds
installers ([`docs/release.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/release.md)).
