# Contributing to Nugget

Nugget is licensed under [Apache-2.0](LICENSE). More detail: the book's
[contributor pages](docs/book/src/contributing/setup.md).

## Ground rules

### Developer Certificate of Origin

Nugget uses the [DCO 1.1](https://developercertificate.org/), not a CLA. Signing off certifies
that you wrote the change or have the right to submit it under the project's license. Sign off
every commit:

```sh
git commit -s -m "feat(store): add cursor round-trip"
# adds: Signed-off-by: Ada Lovelace <ada@example.com>   (must match the commit author)
```

CI's DCO step checks every commit pushed to the integration branch or `main` (bots such as
Dependabot are exempt).
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

Rust via [rustup](https://rustup.rs/) ([`rust-toolchain.toml`](rust-toolchain.toml); MSRV is
`rust-version` in [`Cargo.toml`](Cargo.toml)); a C compiler, `make` and `perl` for SQLCipher and
vendored OpenSSL (Windows: MSVC Build Tools and [Strawberry Perl](https://strawberryperl.com/); if
Git's MSYS Perl comes first, set `OPENSSL_SRC_PERL=C:\Strawberry\perl\bin\perl.exe`). For the
desktop app: Node.js 22, pnpm (`corepack enable`) and the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/); on Debian/Ubuntu:

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

CI: see [CI on the free tier](#ci-on-the-free-tier) below.

## Where code lives

The [crate map](docs/book/src/architecture/crates.md) lists every crate (`nugget-core` changes need
every crate owner's agreement). New crates go under `crates/`, in `members` (usually
`default-members`), with `version.workspace = true`, the other inherited fields and
`[lints] workspace = true`, plus a crate-map row (the docs check enforces it). Shared
dependencies only go in `[workspace.dependencies]`.

Daemon features (Double Tap, agent, browser bridge, proactive, plugins) each live in one module
under [`crates/nuggetd/src/features/`](crates/nuggetd/src/features/mod.rs) that owns its state, IPC
methods, tasks and `config.toml` section: implement `Feature` in `features/<name>.rs`, declare it
in `features/mod.rs` and add a line to `build`, without touching `State` or `handler.rs`. A
feature that fails to start is logged and its methods answer "method not found".

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

## CI on the free tier

The repository is private on GitHub Free. Its Actions allowance is a monthly amount of compute
(US$12, which is 2,000 minutes of a 2-core Linux runner). Windows minutes cost about 1.7 times as
much as Linux minutes, and macOS about 10 times as much
([billing docs](https://docs.github.com/en/billing/concepts/product-billing/github-actions)).
When the allowance runs out, every job fails within seconds with no runner until the next billing
cycle. Self-hosted runners use no minutes. CI is set up to fit the allowance:

- **Automatic:** only [`ci.yml`](.github/workflows/ci.yml). It is one Linux job (about 15 min) on
  each push to `claude/inspiring-wozniak-z3gg3n` or `main`, and it skips docs-only pushes. A newer
  push cancels the older run. The job runs fmt, clippy (the workspace, plus `nuggetd` with
  `fake-runtime`), the workspace tests, the desktop build and vitest, cargo-deny,
  `check-refs.py`, the bench-table checks, and DCO. Pull requests and Dependabot get no automatic
  runs.
- **Manual (`workflow_dispatch`, no schedules):** everything else. That includes
  [`ci-full.yml`](.github/workflows/ci-full.yml), which runs the old macOS/Windows matrix, MSRV and
  bench smoke. It also includes `e2e`, `real-os`, `fuzz`, `nightly`, `perf`, `evals`,
  `mcp-compat`, `package-smoke`, `crash`, `plugins`, `docs`, `reproducible`,
  `release-rehearsal`, `windows-signing` and `release`. Run the suites that match what you
  changed before you merge:

  ```sh
  gh workflow run ci.yml       --ref <branch>                  # the lean check, e.g. on a PR branch
  gh workflow run ci-full.yml  --ref <branch>                  # macOS/Windows on your self-hosted runner
  gh workflow run real-os.yml  --ref <branch> -f runner=github-hosted   # paid images: about 750 Linux-min
  gh workflow run fuzz.yml     --ref <branch> -f seconds=120
  gh run watch                                                 # follow the latest run
  ```

  Only workflow files on the default branch can be dispatched. In a workflow with macOS or Windows
  jobs, those jobs go to a self-hosted runner by default. The `runner` input picks the label, with
  the OS label added automatically, and `github-hosted` uses GitHub's images instead. Linux jobs
  always run on GitHub-hosted runners.
- **Don't add workflows that trigger on `push`, `pull_request` or `schedule`,** even
  temporary ones on a feature branch. GitHub runs them from whichever branch holds the file. The
  temporary QA and eval workflows on worker branches were the largest single cost before the
  minutes ran out. Use `workflow_dispatch` and run the workflow by hand.
- **Usage and reset date:** go to your account's Settings → Billing and licensing → Usage
  (`https://github.com/settings/billing`) and filter by Actions. The billing overview there shows
  the current cycle; the allowance resets when the next one starts. A single run's billable time is
  under the run's **Usage** link.

### Your own machine as a free runner

A Mac, a Windows PC or a Linux box can run the manual workflows' jobs for free. **The runner
executes this repository's code, and any workflow pushed to it, on your machine as the user it runs
as.** Use a dedicated user account (or a VM), keep no other secrets in that account, and only
register it to this private repository. State persists between jobs.

1. Go to the repository → **Settings → Actions → Runners → New self-hosted runner**. Pick the OS
   and architecture, then run the download commands the page shows in an empty folder
   (`~/actions-runner`, or `C:\actions-runner` on Windows). Then configure the runner. The
   registration token expires after one hour, and `gh` can mint one:

   ```sh
   TOKEN=$(gh api -X POST repos/rayhankhilji/nugget/actions/runners/registration-token --jq .token)
   ./config.sh --url https://github.com/rayhankhilji/nugget --token "$TOKEN" --unattended
   ./run.sh                                  # foreground; Ctrl-C stops it
   sudo ./svc.sh install && sudo ./svc.sh start   # or as a service (Linux; on macOS without sudo)
   ```

   On Windows, in an administrator PowerShell, run
   `.\config.cmd --url https://github.com/rayhankhilji/nugget --token <TOKEN> --runasservice`.
   The runner gets the labels `self-hosted`, its OS (`macOS`, `Windows`, `Linux`) and its
   architecture. Those labels are what the workflows' default `runner: self-hosted` targets. Add
   `--labels mymac` and pass `-f runner=mymac` to target one machine.
2. Install what GitHub's images normally provide: Git (Git Bash on `PATH` on Windows), rustup,
   Python 3 and Node 22 (`setup-node` can download it). On Windows, also install the Visual
   Studio C++ Build Tools and Strawberry Perl. On macOS, install the Xcode command-line tools. The
   real-OS accessibility and hotkey jobs also need the runner process to have Accessibility (and
   Input Monitoring) permission on macOS, and a logged-in desktop session (not a service) on
   Windows.
3. To remove the runner, use `./config.sh remove --token <token from the same page>` or delete it
   under Settings → Actions → Runners.

## Commits and pull requests

[Conventional Commits](https://www.conventionalcommits.org/) with the crate as scope
(`feat(infer): ...`). Keep PRs focused, fill in the template, keep CI green. Report security
issues privately ([SECURITY.md](SECURITY.md)), never in a public issue.
