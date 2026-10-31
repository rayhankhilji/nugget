# Fuzzing

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) targets for the parsers that
read untrusted input. This directory is its own cargo workspace with its own `Cargo.lock`, so it
is not built, linted or `cargo deny`-checked with the root workspace. It needs a nightly toolchain;
the root `rust-toolchain.toml` stays on stable, so name `+nightly` explicitly.

| Target | What it feeds | Entry point |
|---|---|---|
| `mail` | RFC 822 / MIME mail (`.eml`), as the files connector extracts it | `nugget_extract::extract("message/rfc822", ..)` |
| `calendar` | iCalendar (`.ics`), as the files connector extracts it | `nugget_extract::extract("text/calendar", ..)` |
| `ipc_frame` | Bytes from the local socket: line framing, JSON-RPC decoding, reply encoding | `nugget_ipc::fuzz_frames` (hidden hook) |
| `model_manifest` | Model manifest JSON (signed here with a test key) or a detached signature file | `nugget_models::verify_manifest` |
| `plugin_manifest` | `nugget-plugin.toml` and its host rules | `nugget_plugin::Manifest::parse` |
| `facts_block` | Retrieved context text (attacker-written passages, headers, dates) turned into the draft facts block | `nugget_draft::prompt_facts::{facts, facts_section}` |
| `secret_stream` | Model output streamed in arbitrary chunks past the credential stop | `nugget_draft::Grounder` (`GroundingCheck::Secrets`/`Sentences`) |
| `drive_export` | Google Drive file entries, Docs/Sheets/Slides exports and sheet titles | `nugget_connector_google::fuzz_export` (hidden hook) |
| `plugin_package` | `.nuggetplugin` files: container decoding, signature check against a fixed test key (seeds signed with it), store install/load | `nugget_plugin::PluginPackage::{from_bytes, verify}`, `PluginStore` |
| `plugin_caps` | Host rules against URLs, capability subsets, `[capabilities]` validation | `nugget_plugin::HostRule::{parse, matches}`, `Capabilities::is_subset_of` |
| `plugin_fs_path` | Relative paths a plugin passes to `nugget:plugin/fs`, under a scratch root with symlinks | `nugget_plugin::fs::{check_relative, resolve}` |

Always pass `-O`. Without it cargo-fuzz turns on debug assertions in every crate, and third-party
`debug_assert!`s that are no-ops in shipped builds (mail-parser 0.11 has a `debug_assert!(false)`
on malformed nested multipart, kept as `corpus/mail/seed-regress-nested-rfc822.eml`) stop each
run. `Cargo.toml` turns debug assertions and overflow checks on for the Nugget crates only.

Extraction runs each extractor under `catch_unwind`; libfuzzer-sys's panic hook aborts first, so a
panic in the mail or calendar code still shows up as a crash here.

## Running

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked --version 0.13.2

cd fuzz
cargo +nightly fuzz list
cargo +nightly fuzz run -O mail -- -max_total_time=60
```

`cargo fuzz run` starts from `corpus/<target>/`. The handmade `seed-*` files there are committed;
anything libFuzzer adds next to them is ignored by `.gitignore`. Crashes are written to
`artifacts/<target>/`; reproduce one with

```sh
cargo +nightly fuzz run -O <target> artifacts/<target>/crash-<hash>
```

then fix it in the owning crate with a regression test built from the reproducer.

CI (`.github/workflows/fuzz.yml`) runs every target for 60 s on pull requests that touch these
parsers and for 300 s nightly (configurable on manual runs), and uploads `artifacts/<target>` when
a target fails. After changing dependencies of the fuzzed crates, refresh the lockfile with
`cargo +nightly update -w` here.
