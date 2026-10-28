# Rules: DCO, clean room, tests

## Developer Certificate of Origin

Nugget uses the [Developer Certificate of Origin](https://developercertificate.org/) (DCO 1.1)
instead of a Contributor Licence Agreement. There is no CLA. Sign off every commit:

```sh
git commit -s -m "feat(store): add cursor round-trip"
```

This adds `Signed-off-by: Your Name <you@example.com>`, which must match the commit author. By adding
it you certify that you wrote the change or otherwise have the right to submit it under Apache-2.0. The
`DCO sign-off` CI job checks every commit in a pull request. To fix a branch:

```sh
git rebase --signoff origin/main
git push --force-with-lease
```

Contributions are accepted under Apache-2.0 (inbound = outbound).

## The clean-room rule

Nugget competes with Sentient OS, which is AGPL-licensed. To keep Nugget cleanly Apache-2.0:

- **Never read, copy, translate or paraphrase Sentient OS source code**, including its prompts,
  schemas, tests and distinctive text, not even "just to see how they did it".
- The documents in [`docs/research/`](https://github.com/rayhankhilji/nugget/tree/main/docs/research)
  are the **only** bridge. They describe observed behaviour and architecture in our own words. Work
  from them and from the public documentation of platform APIs.
- If you have read the Sentient OS source, say so in your pull request and do not work on the matching
  area of Nugget.
- The same applies to any GPL, AGPL or LGPL code.

## Licences of dependencies

Every dependency must be Apache-2.0 compatible. `deny.toml` holds the allow list (Apache-2.0, MIT, BSD,
ISC, Zlib, Unicode and similar; MPL-2.0 only for unmodified dependencies). GPL, AGPL and LGPL crates
are rejected. npm packages for the desktop app are checked by hand. Build tools that are not linked
into the product, such as mdBook (MPL-2.0) for these docs, are fine.

## Testing expectations

- **Every public behaviour has a test.** Unit tests sit next to the code; integration tests go in
  `tests/`.
- **No real network.** Use `wiremock` or a local server. Connectors are tested against recorded
  fixtures and fake servers.
- **No real credentials, keychain or home directory.** Use `NUGGET_HOME` and temporary directories.
- **All three OSes.** Tests must pass on Linux, macOS and Windows. Gate OS-specific code with `cfg` and
  keep a compiling fallback for every other target.
- **Hostile input.** Parsers enforce size caps and timeouts, return errors instead of panicking, and
  get fuzz-style tests on malformed input.
- **Claims need numbers.** A change meant to make Nugget faster, fresher or better adds or updates a
  `nugget-bench` metric and quotes it in the pull request. Release builds only for quoted numbers.

## Code expectations

- `unsafe` is denied workspace-wide. Unavoidable FFI gets one minimal function with
  `#[allow(unsafe_code)]` and a `// SAFETY:` comment.
- No hard-coded model ids in logic: route by tier.
- Every request that leaves the device records an `EgressEvent`. Never log secrets, and never log user
  content at `info` level or above.
- Convert errors to `nugget_core::Error` at public crate boundaries.

## Commits and pull requests

Use [Conventional Commits](https://www.conventionalcommits.org/) with the crate as the scope
(`feat(infer): …`, `fix(store): …`, `docs(book): …`). Keep pull requests focused, fill in the template,
and keep CI green. If a change alters a config key, CLI flag, IPC method, MCP tool or connector setting,
update this book in the same pull request: the docs check fails otherwise
([Working on these docs](docs.md)).

Report security issues privately, as described in
[`SECURITY.md`](https://github.com/rayhankhilji/nugget/blob/main/SECURITY.md).
