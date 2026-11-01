# Rules: DCO, clean room, tests

## Developer Certificate of Origin

Nugget uses the [DCO 1.1](https://developercertificate.org/), not a CLA. Sign off every commit
(`git commit -s`); the `Signed-off-by` line must match the author, and CI checks it. To fix a branch:
`git rebase --signoff origin/main`, then `git push --force-with-lease`. Contributions are accepted under
Apache-2.0.

## The clean-room rule

Sentient OS is AGPL-licensed. To keep Nugget cleanly Apache-2.0:

- **Never read, copy, translate or paraphrase Sentient OS source**, including prompts, schemas, tests
  and distinctive text.
- [`docs/research/`](https://github.com/rayhankhilji/nugget/tree/main/docs/research) is the **only**
  bridge: observed behaviour in our own words. Work from it and public platform documentation.
- If you have read the Sentient source, say so in your pull request and stay off the matching area.
- The same applies to any GPL, AGPL or LGPL code. `deny.toml` allows only permissive licences (MPL-2.0
  only unmodified); npm packages are checked by hand.

## Tests and code

- Every public behaviour has a test, on Linux, macOS and Windows (gate OS code with `cfg`, keep a
  fallback). No real network, credentials, keychain or home: use `wiremock`, fixtures, `NUGGET_HOME`.
- Parsers cap sizes, time out and return errors. Claims of "faster" or "better" come with a
  `nugget-bench` metric.
- `unsafe` only in minimal, commented FFI. Route models by tier; record every egress; never log
  secrets, or content at `info` or above.
- New crates use `[lints] workspace = true` and get a row in the [crate map](../architecture/crates.md);
  daemon subsystems are modules under `crates/nuggetd/src/features/`.

## Commits and pull requests

[Conventional Commits](https://www.conventionalcommits.org/) with the crate as scope
(`fix(store): …`). Keep pull requests focused and CI green. A change to a config key, CLI flag, IPC
method, MCP tool or connector setting updates this book in the same pull request
([Working on these docs](docs.md)). Report security issues privately per
[`SECURITY.md`](https://github.com/rayhankhilji/nugget/blob/main/SECURITY.md).
