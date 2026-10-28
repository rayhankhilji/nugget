## What and why

<!-- What does this change do, and why? Link the issue or plan section (docs/PLAN.md, docs/m0-spec.md). -->

Closes #

## How it was tested

<!-- Commands you ran and what they showed. New public behaviour needs a test. -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy -p <crate> --all-targets -- -D warnings`
- [ ] `cargo test -p <crate>`
- [ ] Desktop changes: `pnpm build` in `apps/desktop` and `cargo build -p nugget-desktop`

## Checklist

- [ ] Every commit is signed off (`git commit -s`, see [CONTRIBUTING.md](../CONTRIBUTING.md#developer-certificate-of-origin)).
- [ ] Clean room: nothing here was read or copied from the Sentient OS source; `docs/research/` is the only reference.
- [ ] Works on macOS, Windows and Linux, or OS-specific code is gated with `cfg` and has a compiling fallback.
- [ ] No network access or real credentials in tests; no hard-coded model ids in logic.
- [ ] New dependencies are permissively licensed and pass `cargo deny check`.
- [ ] Privacy: any new network egress is recorded in the egress ledger.
- [ ] Performance or quality claims come with a `nugget-bench` number.
