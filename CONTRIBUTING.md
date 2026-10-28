# Contributing to Nugget

Thanks for helping build Nugget, a local-first, cross-platform personal AI licensed under
[Apache-2.0](LICENSE). This guide covers the rules every change must follow, how to build and test
only what you touch, and where code lives.

## Ground rules

### Developer Certificate of Origin

Nugget uses the [Developer Certificate of Origin](https://developercertificate.org/) (DCO 1.1)
instead of a Contributor License Agreement. There is no CLA. By adding a `Signed-off-by` line to a
commit, you certify that you wrote the change or otherwise have the right to submit it under the
project's license.

Sign off every commit with `-s`:

```sh
git commit -s -m "feat(store): add cursor round-trip"
```

This adds a trailer that must match the commit author:

```text
Signed-off-by: Ada Lovelace <ada@example.com>
```

The `DCO sign-off` CI job checks every commit in a pull request. To fix a branch that is missing
sign-offs, run `git rebase --signoff origin/main` and force-push. Commits by GitHub bots such as
Dependabot are exempt.

Contributions are accepted under Apache-2.0 (inbound = outbound, as in section 5 of the license).

### Clean room

Nugget competes with Sentient OS, which is AGPL-licensed. To keep Nugget's code cleanly
Apache-2.0:

- **Never read, copy, translate or paraphrase the Sentient OS source code**, including its prompts,
  schemas, tests and distinctive text. This applies even to "just looking at how they did it".
- The documents in [`docs/research/`](docs/research/) are the **only** bridge. They describe
  observed behaviour and architecture, not code. Work from those documents and from public
  documentation of platform APIs.
- If you have already read the Sentient OS source, say so in your pull request, and do not work on
  the matching area of Nugget.
- The same applies to any other GPL, AGPL or LGPL code: do not copy it into this repository.

Reviewers will reject changes that look derived from copyleft sources.

### Licenses of dependencies

Every dependency must be compatible with Apache-2.0. [`deny.toml`](deny.toml) holds the allow list
(Apache-2.0, MIT, BSD, ISC, Zlib, Unicode, and similar permissive licenses; MPL-2.0 only for
unmodified dependencies). GPL, AGPL and LGPL crates are rejected. Run this before adding a crate:

```sh
cargo deny check licenses bans sources
```

You can install cargo-deny with `cargo install cargo-deny --locked`. For npm packages in
`apps/desktop`, check the license by hand: it must be permissive.

## Getting set up

- **Rust**: install [rustup](https://rustup.rs/). [`rust-toolchain.toml`](rust-toolchain.toml)
  selects stable with `rustfmt` and `clippy`. The minimum supported Rust version is the
  `rust-version` in [`Cargo.toml`](Cargo.toml).
- **C toolchain**: `nugget-store` builds SQLCipher and a vendored OpenSSL from source. On Linux
  and macOS you need a C compiler, `make` and `perl`, which are usually already installed. On Windows,
  use the MSVC Build Tools and [Strawberry Perl](https://strawberryperl.com/). Git's bundled MSYS
  Perl does not work; if it comes first on your `PATH`, set
  `OPENSSL_SRC_PERL=C:\Strawberry\perl\bin\perl.exe`.
- **Desktop app**: Node.js 22 and pnpm (the version is pinned by `packageManager` in
  `apps/desktop/package.json`; `corepack enable` picks it up). Also install the
  [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/). On Debian or Ubuntu:

  ```sh
  sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
    librsvg2-dev libxdo-dev libsoup-3.0-dev libssl-dev build-essential file
  ```

## Building and testing

The workspace's `default-members` leave out the desktop crate, so a plain `cargo build` or
`cargo test` works without webview libraries. **Build and test only the crates you change.** This
keeps feedback fast, and several people (and agents) often share a target directory. Waiting on
its lock is normal.

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

# Benchmarks (use --release for numbers you intend to quote)
cargo run --release -p nugget-bench -- perf memory
```

Note that `--workspace` includes `nugget-desktop`. Pass `--exclude nugget-desktop` unless its
system libraries are installed and the frontend has been built.

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs on Linux, macOS and Windows:

- fmt, clippy with `-D warnings`, the tests, and a bench smoke run
- the desktop build on all three OSes
- `cargo deny`
- the DCO check

## Repository layout

| Path | What lives there |
|---|---|
| `crates/nugget-core` | Shared contracts: ids, errors, capability tiers, egress events, paths, the IPC protocol. Changes need agreement from every crate owner. |
| `crates/nugget-store` | Encrypted local store: SQLCipher, FTS5, `sqlite-vec`, cursors, egress ledger, audit chain |
| `crates/nugget-infer` | One `Provider` trait over local and cloud models; capability probe; tier router |
| `crates/nugget-models` | Signed model manifest, verified downloads, hardware-aware recommendation |
| `crates/nugget-ipc` | JSON-RPC 2.0 over local sockets and named pipes |
| `crates/nuggetd` | The daemon |
| `apps/desktop` | Tauri 2 shell (`nugget-desktop`): React, TypeScript, Vite |
| `bench` | `nugget-bench`: benchmark harness and report schema; see [`bench/README.md`](bench/README.md) |
| `manifests` | Signed model manifests |
| `docs` | [`PLAN.md`](docs/PLAN.md), milestone specs, and clean-room research |

More crates arrive with later milestones (`nugget-ingest`, `nugget-knowledge`, `nugget-retrieve`,
`nugget-draft`, `nugget-agent`, `nugget-a11y`, `nugget-policy`, `nugget-privacy`, `nugget-mcp`,
`nugget-plugin`); see [docs/PLAN.md §3.1](docs/PLAN.md#31-crates-cargo-workspace). New crates go
under `crates/`, use `version.workspace = true` (and the other inherited package fields), and
include `[lints] workspace = true`. Add dependencies to your crate's `Cargo.toml`. Only add them to
the root `[workspace.dependencies]` when several crates share them.

## Code expectations

- **Tests.** Every public behaviour gets a test. Put unit tests next to the code and integration
  tests in `tests/`.
  - Tests never touch the real network. Use `wiremock` or a local server.
  - Tests never need real credentials.
  - Tests must not depend on the user's keychain or home directory. Use `NUGGET_HOME` and
    temporary directories instead.
  - Tests must pass on all three OSes.
- **Cross-platform.** Code must compile on macOS, Windows and Linux. Gate OS-specific code with
  `cfg`, and keep a compiling fallback for every other target.
- **`unsafe`** is denied workspace-wide. If FFI is unavoidable, use one minimal function with
  `#[allow(unsafe_code)]` and a `// SAFETY:` comment explaining why it is sound.
- **No hard-coded model ids in logic.** Route by capability tier. Model ids belong only in config,
  examples and manifests.
- **Privacy.** Every request that leaves the device records an `EgressEvent`. Never log secrets or
  user content at `info` level or above.
- **Errors.** Convert to `nugget_core::Error` at public crate boundaries. Library code must not
  panic on bad input.
- **Claims need numbers.** If a change is meant to make Nugget faster, fresher or better, add or
  update a `nugget-bench` metric and quote it in the pull request.

## Commits and pull requests

- Use [Conventional Commits](https://www.conventionalcommits.org/), with the crate as the scope
  when it helps: `feat(infer): ...`, `fix(store): ...`, `chore(deps): ...`.
- Keep pull requests focused, and fill in the template. CI must be green.
- Report security issues privately, as described in [SECURITY.md](SECURITY.md), never in a
  public issue.
