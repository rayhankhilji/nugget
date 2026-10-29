# Model manifest trust: signing keys and rollback protection

The model and runtime manifest (`manifests/default.json`) says which model files and
`llama-server` builds Nugget may download: exact upstream commits, sizes and SHA-256 hashes. It is
compiled into `nuggetd` together with a detached ed25519 signature (`manifests/default.json.sig`)
and is only used after `nugget-models` has verified it. This page describes which keys a build
trusts and how an older, correctly signed manifest is refused.

Code: `crates/nugget-models/src/manifest.rs` (keys, signature check) and
`crates/nugget-models/src/trust.rs` (rollback protection).

## Key slots

`trusted_keys()` returns, in order:

| Slot | Source | Trusted when |
|---|---|---|
| Release key | `NUGGET_MANIFEST_RELEASE_PUBKEY` at **build time** (64 hex chars, an ed25519 public key), read with `option_env!` | The variable was set (non-empty) when `nugget-models` was compiled |
| Development key | `DEV_PUBLIC_KEY_HEX`, committed in `manifest.rs` | Debug builds (`cfg(debug_assertions)`), or a build made with `NUGGET_TRUST_DEV_MANIFEST_KEY=1` |

- Both variables are read when the crate is **built**, not when the program runs. Nothing a user
  or another process sets at runtime adds a key. `crates/nugget-models/build.rs` declares them
  with `cargo:rerun-if-env-changed` and fails the build on a malformed release key, or on an
  opt-in value other than `1`/`true`/`yes`/`0`/`false`/`no`/empty.
- A release build that trusts the development key logs a warning once
  (`this release build trusts the DEVELOPMENT manifest key`).
- **A release build with neither slot filled trusts no key.** `trusted_keys()` fails with
  "no trusted manifest keys in this build ...", the daemon logs "bundled model manifest failed
  verification" and offers no model or runtime downloads. It still starts; everything that does
  not need the manifest keeps working. This is deliberate (fail closed): a release must never
  trust the development key by accident.
- The pure function `select_trusted_keys(release, allow_dev)` makes the choice and is unit-tested
  for every combination (`crates/nugget-models/tests/trust.rs`).

### Several signatures in one `.sig`

A signature file may hold several hex signatures, one per line (blank lines and `#` comments are
ignored, at most 8). Verification succeeds if any trusted key verifies any of them; keys are tried
release first. This lets one committed manifest carry a development signature (for debug builds
and tests) and a release signature (for release builds) at the same time:

```sh
# development signature (replaces the file)
NUGGET_MANIFEST_SIGNING_KEY_FILE=~/.config/nugget/manifest-dev.key \
  cargo run -p nugget-models --example sign-manifest -- manifests/default.json
# release signature, added next to it, on the offline machine that holds the release key
cargo run -p nugget-models --example sign-manifest -- --append --key-file release.key manifests/default.json
# check against a specific public key
cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json --pubkey <hex>
```

Any change to `default.json` invalidates every signature, so both must be redone.

### How CI builds get their keys

See `docs/release.md` §4 (*Manifest release key*). In short: the release workflow passes the
repository variable `NUGGET_MANIFEST_RELEASE_PUBKEY` to the sidecar build, and sets
`NUGGET_TRUST_DEV_MANIFEST_KEY=1` only for dry runs, so dry-run installers keep a working
(development-signed) manifest while real releases trust only the release key. Debug builds (CI
tests, the e2e suite) trust the development key without any setting.

## Rollback protection

`Manifest::version` is a monotonic serial. Without a check, anyone able to feed the daemon an old
but correctly signed manifest could pin it to withdrawn or vulnerable files (a rollback or freeze
attack). `ManifestTrust` stores, **per trusted key** (by hex public key), the highest version this
installation has accepted:

```json
{ "format": 1, "keys": { "a56054ce...d9b3": 1 } }
```

in `<data>/models/manifest-trust.json` (the model store root, `Paths::models_dir()`).

| Case | Result |
|---|---|
| No file, or no entry for the key (fresh install, first manifest from a new key) | Accepted and recorded |
| Same version as recorded | Accepted (the bundled manifest is re-checked on every start) |
| Higher version | Accepted, new version recorded |
| Lower version | Refused: `integrity check failed: manifest version 3 is older than version 5 already accepted from key ab12...; refusing a rollback` |
| File unreadable, not valid JSON, unknown format, unknown fields or a malformed key | Refused (fail closed): "manifest trust state ... is corrupt ...; refusing to accept any manifest" |

- Versions are per key because the release key and the development key sign independent series;
  a development manifest at version 40 says nothing about the release series.
- The record is written atomically: a temporary file in the same directory, `fsync`, then rename.
  A refused or unverifiable manifest never changes the record.
- The check runs after the signature and entry checks, against the key that actually verified the
  signature (`verify_manifest_with_key`).
- **API.** `verify_manifest_checked(bytes, sig, keys, &trust)` for any manifest (embedded or
  downloaded); `default_manifest_checked(&paths)` for the bundled one, which the daemon uses at
  start. `verify_manifest` / `default_manifest` still exist and do **not** check rollback; use
  them only where no installation state applies (signing tools, tests, the runtime supervisor's
  fallback when it is not given a manifest).

### Operational notes

- **Downgrading the app.** Installing an older Nugget whose bundled manifest has a lower version
  than one already accepted makes the daemon refuse that manifest: no model downloads until the
  app is upgraded again. Installed models keep working. This is the intended behaviour.
- **Recovering.** Deleting `manifest-trust.json` resets protection to a fresh install; the next
  start records the bundled version again. Do this only after a deliberate downgrade, or to clear
  a corrupt file.
- **Bump the version** in `manifests/default.json` whenever its content changes, so a newer
  manifest supersedes the old one.

## Known gaps

- The manifest is still only bundled; there is no download path for newer manifests yet. The
  check is ready for one (`verify_manifest_checked`).
- The state file sits in the user's data directory. Malware running as the user can delete or
  edit it; rollback protection defends against a replayed manifest, not a compromised account.
- The runtime supervisor (`nugget-runtime`) falls back to the unchecked `default_manifest()` when
  the daemon does not hand it a manifest. Inside the daemon it receives the checked one.
- The release key has not been generated yet. Until `manifests/default.json.sig` carries a
  release signature, a real release build with `NUGGET_MANIFEST_RELEASE_PUBKEY` set (and no
  opt-in) rejects the bundled manifest and offers no downloads.
