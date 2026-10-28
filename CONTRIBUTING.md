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
cargo test -p nugget-desktop
cd apps/desktop && pnpm test && cd ../..        # frontend unit tests (vitest)

# Benchmarks (use --release for numbers you intend to quote)
cargo run -p nugget-bench -- --list
cargo run -p nugget-bench -- all --scale smoke --no-write
cargo run --release -p nugget-bench -- memory --backend real

# The book against the code (needs only Python 3)
python3 docs/book/check-refs.py
```

Note that `--workspace` includes `nugget-desktop`. Pass `--exclude nugget-desktop` unless its
system libraries are installed and the frontend has been built.

CI runs these workflows:

- [`ci.yml`](.github/workflows/ci.yml), on Linux, macOS and Windows: `cargo fmt`, clippy with
  `-D warnings`, the tests, and a bench smoke run (`all --scale smoke`); on Linux also the
  real-stack `memory` smoke run and a check of every report against
  `bench/schema/report.schema.json`. Then the desktop frontend build, clippy, build and tests on
  all three OSes, `cargo deny` (advisories; bans, licenses and sources), and the DCO check on pull
  requests.
- [`docs.yml`](.github/workflows/docs.yml): `python3 docs/book/check-refs.py`, then builds the
  mdBook in `docs/book` and checks its links. It runs on every change, because renaming a config
  key, CLI flag, IPC method, MCP tool or connector setting must update the book.
- [`release.yml`](.github/workflows/release.yml): installers and signed updates, on version tags
  only; see [`docs/release.md`](docs/release.md).

## Repository layout

| Path | What lives there |
|---|---|
| `crates/nugget-core` | Shared contracts: ids, errors, capability tiers, egress events, paths, the IPC protocol. Changes need agreement from every crate owner. |
| `crates/nugget-store` | Encrypted local store: SQLCipher, FTS5, `sqlite-vec`, cursors, egress ledger, audit chain, knowledge-graph tables |
| `crates/nugget-infer` | One `Provider` trait over local and cloud models; capability probe; tier router |
| `crates/nugget-models` | Signed model manifest, verified downloads, hardware-aware recommendation |
| `crates/nugget-runtime` | Supervised local `llama-server` processes |
| `crates/nugget-ipc` | JSON-RPC 2.0 over local sockets and named pipes |
| `crates/nugget-connector`, `nugget-extract`, `nugget-ingest`, `nugget-oauth` | The connector API, text extraction and chunking, the ingest pipeline (including the PII scan), OAuth |
| `crates/nugget-connector-*` | Connectors: `files` (and Obsidian), `google`, `imap`, `caldav`, `microsoft`, `saas` (Slack, Notion, GitHub, Linear) |
| `crates/nugget-retrieve` | Hybrid search and cited, token-budgeted context |
| `crates/nugget-graph`, `nugget-knowledge`, `nugget-vault`, `nugget-mcp` | The knowledge graph, extraction, the Markdown vault, the local MCP server |
| `crates/nugget-a11y`, `nugget-hotkey`, `nugget-draft`, `nugget-voice` | Double Tap: accessibility, the global trigger, drafting, the writing-voice profile |
| `crates/nugget-policy`, `nugget-agent`, `nugget-browser` | Safety policy, the agent loop, browser control (native-messaging host) |
| `crates/nugget-proactive` | Proactive cards and the morning brief |
| `crates/nugget-plugin` | WASM component plugins; the guest SDK is a separate cargo workspace in `crates/nugget-plugin/sdk` |
| `crates/nuggetd` | The daemon; post-M2 subsystems plug in through `src/features/` |
| `apps/desktop` | Tauri 2 shell (`nugget-desktop`): React, TypeScript, Vite |
| `extensions/browser` | The browser extension that talks to `nugget-browser` |
| `bench` | `nugget-bench`: benchmark harness, suites and report schema; see [`bench/README.md`](bench/README.md) |
| `manifests` | The signed model and runtime manifest |
| `packaging` | Installer, sidecar and update-manifest scripts; see [`docs/release.md`](docs/release.md) |
| `docs` | [`PLAN.md`](docs/PLAN.md), milestone specs, clean-room research, wave briefs and the mdBook (`docs/book`) |

The [crate map](docs/book/src/architecture/crates.md) says what each crate does and whether the
daemon runs it yet. New crates go under `crates/`, are added to `members` (and usually
`default-members`) in the root `Cargo.toml`, use `version.workspace = true` (and the other
inherited package fields), and include `[lints] workspace = true`. Add dependencies to your crate's
`Cargo.toml`. Only add them to the root `[workspace.dependencies]` when several crates share them.
A new crate also needs a row in the crate map, or the docs check fails.

### Daemon features

Subsystems wired into the daemon after M2 (Double Tap, the agent and safety policy, the browser
bridge, proactive cards, plugins) each live in one module under
[`crates/nuggetd/src/features/`](crates/nuggetd/src/features/mod.rs). A feature owns its state,
its IPC methods, its background tasks and its `[section]` of `config.toml`. To add one, implement
the `Feature` trait in `features/<name>.rs`, declare the module in `features/mod.rs` and add one
line to `build` there. Do not edit `State` or `handler.rs` for it. A feature that fails to start
is logged and left out; its methods then answer "method not found".

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
