# Contributing to Nugget

Nugget is licensed under [Apache-2.0](LICENSE). The book's
[contributor pages](docs/book/src/contributing/setup.md) cover setup, layout and CI in more depth.

## Ground rules

### Developer Certificate of Origin

Nugget uses the [DCO 1.1](https://developercertificate.org/), not a CLA. Signing off certifies
that you wrote the change or have the right to submit it under the project's license. Sign off
every commit:

```sh
git commit -s -m "feat(store): add cursor round-trip"
# adds: Signed-off-by: Ada Lovelace <ada@example.com>   (must match the commit author)
```

CI's `DCO sign-off` job checks every commit in a pull request (bots such as Dependabot are exempt).
To fix a branch: `git rebase --signoff origin/main` and force-push. Contributions are accepted
under Apache-2.0 (inbound = outbound, section 5 of the license).

### Clean room

Nugget competes with Sentient OS, which is AGPL-licensed. To keep Nugget cleanly Apache-2.0:

- **Never read, copy, translate or paraphrase Sentient OS source code**, including its prompts,
  schemas, tests and distinctive text, not even to "just look at how they did it".
- [`docs/research/`](docs/research/) is the **only** bridge: it describes observed behaviour, not
  code. Work from it and from public platform API documentation.
- If you have read the Sentient OS source, say so in your pull request and don't work on the
  matching area.
- Don't copy any other GPL, AGPL or LGPL code into this repository either.

Reviewers reject changes that look derived from copyleft sources.

### Dependency licenses

Dependencies must be Apache-2.0-compatible; [`deny.toml`](deny.toml) holds the allow list (MPL-2.0
only unmodified; GPL, AGPL, LGPL rejected). Before adding a crate run
`cargo deny check licenses bans sources` (`cargo install cargo-deny --locked`). Check npm package
licenses in `apps/desktop` by hand.

## Setup

- **Rust** via [rustup](https://rustup.rs/); [`rust-toolchain.toml`](rust-toolchain.toml) selects
  stable with `rustfmt` and `clippy`; MSRV is `rust-version` in [`Cargo.toml`](Cargo.toml).
- **C toolchain** for SQLCipher and vendored OpenSSL: a C compiler, `make`, `perl`. On Windows, MSVC
  Build Tools and [Strawberry Perl](https://strawberryperl.com/) (Git's MSYS Perl fails; set
  `OPENSSL_SRC_PERL=C:\Strawberry\perl\bin\perl.exe` if it comes first on `PATH`).
- **Desktop app**: Node.js 22, pnpm (`corepack enable`), and the
  [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/). On Debian/Ubuntu:

  ```sh
  sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
    librsvg2-dev libxdo-dev libsoup-3.0-dev libssl-dev build-essential file
  ```

## Build and test only what you touch

`default-members` exclude the desktop crate, so plain `cargo build`/`cargo test` need no webview
libraries; `--workspace` includes it, so pass `--exclude nugget-desktop` unless it is set up.

```sh
cargo fmt -p nugget-store
cargo clippy -p nugget-store --all-targets -- -D warnings
cargo test -p nugget-store

# everything CI checks except the desktop app
cargo fmt --all --check
cargo clippy --workspace --all-targets --exclude nugget-desktop -- -D warnings
cargo test --workspace --exclude nugget-desktop

# desktop app
cd apps/desktop && pnpm install --frozen-lockfile && pnpm build && pnpm test && cd ../..
cargo clippy -p nugget-desktop --all-targets -- -D warnings && cargo test -p nugget-desktop

cargo run -p nugget-bench -- all --scale smoke --no-write   # --release for numbers you quote
python3 docs/book/check-refs.py                             # the book against the code
```

CI: [`ci.yml`](.github/workflows/ci.yml) (fmt, clippy, tests, bench smoke and desktop on all three
OSes; `cargo deny`; DCO), [`docs.yml`](.github/workflows/docs.yml) (`check-refs.py`, mdBook build
and links; renaming a config key, CLI flag, IPC method, MCP tool or connector setting must update
the book), [`release.yml`](.github/workflows/release.yml) ([docs/release.md](docs/release.md)).

## Where code lives

The [crate map](docs/book/src/architecture/crates.md) lists every crate. `apps/desktop` is the Tauri
shell, `extensions/browser` the browser extension, `bench` the benchmark harness, `manifests` the
signed model manifest, `packaging` installer and update scripts. `nugget-core` holds shared
contracts; changes there need agreement from every crate owner.

- **New crates** go under `crates/`, in `members` (usually `default-members`) of the root
  `Cargo.toml`, with `version.workspace = true`, the other inherited fields and
  `[lints] workspace = true`, and need a row in the crate map (the docs check fails otherwise).
  Put dependencies in the crate's `Cargo.toml`, in `[workspace.dependencies]` only when shared.
- **Daemon features** (Double Tap, agent, browser bridge, proactive, plugins) each live in one
  module under [`crates/nuggetd/src/features/`](crates/nuggetd/src/features/mod.rs) owning its
  state, IPC methods, tasks and `config.toml` section. Implement `Feature` in
  `features/<name>.rs`, declare it in `features/mod.rs` and add a line to `build`; don't edit
  `State` or `handler.rs`. A feature that fails to start is logged and its methods answer
  "method not found".

## Code expectations

- **Tests** for every public behaviour (unit tests beside the code, integration tests in
  `tests/`), with no real network (`wiremock` or a local server), no real credentials, no user
  keychain or home (`NUGGET_HOME`, temp dirs), passing on all three OSes.
- **Cross-platform**: gate OS code with `cfg` and keep a compiling fallback.
- **`unsafe`** is denied; unavoidable FFI gets one minimal `#[allow(unsafe_code)]` function with a
  `// SAFETY:` comment.
- **No hard-coded model ids in logic**: route by capability tier.
- **Privacy**: every off-device request records an `EgressEvent`; never log secrets or user
  content at `info` or above.
- **Errors**: convert to `nugget_core::Error` at public boundaries; never panic on bad input.
- **Claims need numbers**: performance or quality changes add or update a `nugget-bench` metric.

## Commits and pull requests

[Conventional Commits](https://www.conventionalcommits.org/) with the crate as scope
(`feat(infer): ...`). Keep PRs focused, fill in the template, keep CI green. Report security
issues privately ([SECURITY.md](SECURITY.md)), never in a public issue.
