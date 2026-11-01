# Model manifest trust: signing keys and rollback protection

`manifests/default.json` pins which model files and `llama-server` builds Nugget may download
(upstream commits, sizes, SHA-256). It is compiled into `nuggetd` with a detached ed25519
signature (`manifests/default.json.sig`) and used only after `nugget-models` verifies it. Code:
`crates/nugget-models/src/manifest.rs` (keys, signatures) and `src/trust.rs` (rollback).

## Key slots

| Slot | Source | Trusted when |
|---|---|---|
| Release key | `NUGGET_MANIFEST_RELEASE_PUBKEY` at **build time** (64 hex chars), via `option_env!` | Set (non-empty) when `nugget-models` was compiled |
| Development key | `DEV_PUBLIC_KEY_HEX` in `manifest.rs` | Debug builds, or built with `NUGGET_TRUST_DEV_MANIFEST_KEY=1` |

- Both are read at **build** time; nothing set at runtime adds a key. `build.rs` fails on a
  malformed release key or an opt-in value other than `1`/`true`/`yes`/`0`/`false`/`no`/empty.
- A release build trusting the dev key logs a warning.
- **A release build with neither slot trusts no key** (fail closed): the daemon logs "bundled
  model manifest failed verification" and offers no model or runtime downloads; everything else works.
- CI: `release.yml` passes the repository variable to the sidecar build and sets the dev opt-in
  only for dry runs ([release.md §4](../release.md#manifest-release-key)).

### Several signatures in one `.sig`

One hex signature per line (blank lines and `#` comments ignored, at most 8). Any trusted key
verifying any line succeeds (release key tried first), so one file carries the dev and release
signatures. Any change to `default.json` invalidates both.

```sh
# development signature (replaces the file); create the key once with --generate-key <file>
NUGGET_MANIFEST_SIGNING_KEY_FILE=~/.config/nugget/manifest-dev.key \
  cargo run -p nugget-models --example sign-manifest -- manifests/default.json
# release signature, appended, on the offline machine holding the release key (release.md Step 5)
cargo run -p nugget-models --example sign-manifest -- --append --key-file release.key manifests/default.json
# check against a specific public key
cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json --pubkey <hex>
```

`NUGGET_MANIFEST_SIGNING_KEY` (64 hex) works instead of the key file.

## Rollback protection

`Manifest::version` is a monotonic serial. `ManifestTrust` records, **per trusted key**, the
highest version accepted, in `<data>/models/manifest-trust.json`
(`{ "format": 1, "keys": { "<hex key>": 1 } }`), so an old but validly signed manifest can't pin
the daemon to withdrawn files.

| Case | Result |
|---|---|
| No file or no entry for the key | Accepted and recorded |
| Same version | Accepted (re-checked every start) |
| Higher version | Accepted, recorded |
| Lower version | Refused: `... manifest version 3 is older than version 5 already accepted from key ab12...; refusing a rollback` |
| Unreadable, invalid JSON, unknown format or fields, malformed key | Refused (fail closed): "manifest trust state ... is corrupt ..." |

- Per key (release and dev keys sign independent series). Written atomically; a refused manifest
  never changes it. Every key with a valid signature must allow the version; all advance together.
- API: `verify_manifest_checked(bytes, sig, keys, &trust)` and `default_manifest_checked(&paths)`
  (used at daemon start). `verify_manifest` / `default_manifest` skip the rollback check; use them
  only without installation state (tools, tests).
- Installing an older app with a lower manifest version disables model downloads (installed
  models still work) until upgrading. Deleting `manifest-trust.json` resets protection; do it only
  after a deliberate downgrade or to clear a corrupt file.
- Bump `version` in `default.json` whenever its content changes.

## Known gaps

- The manifest is only bundled; there is no download path for newer manifests yet
  (`verify_manifest_checked` is ready for one).
- Malware running as the user can delete or edit the state file.
- `nugget-runtime` falls back to the unchecked `default_manifest()` when not given a manifest
  (inside the daemon it gets the checked one).
- The release key has not been generated yet; until `default.json.sig` carries a release
  signature, a real release build rejects the bundled manifest and offers no downloads.
