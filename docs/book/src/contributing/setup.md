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

Run a development daemon with `cargo run -p nuggetd -- --home /tmp/nugget-dev` (add `status` to
query it); `HEED_KEYCHAIN=off` keeps it out of your keychain and `HEED_LOG=debug` shows what it
does.

Layout: `crates/` (libraries and `nuggetd`; [crate map](../architecture/crates.md)),
`crates/nugget-plugin/sdk` (plugin SDK, `wasm32-wasip2`), `apps/desktop` (Tauri shell),
`extensions/browser`, `bench`, `manifests` (signed model manifest), `packaging`, `docs`.

CI runs the commands above on all three OSes plus the DCO check; `docs.yml` builds the book;
`release.yml` builds installers ([`docs/release.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/release.md)).
