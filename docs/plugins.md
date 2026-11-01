# Writing a Nugget plugin

A plugin is a WebAssembly component that adds a **connector** (a new source kind), read-only
agent **tools**, or both. It runs inside `nuggetd` under wasmtime, sees the host only through the
`nugget:plugin` imports, and gets only the capabilities its manifest declares and the user approved.
Using plugins in the app: [Plugins guide](book/src/guide/plugins.md).

| Path | What it is |
|---|---|
| `crates/nugget-plugin/wit/plugin.wit` | The interface: `nugget:plugin@0.1.0`, world `plugin` |
| `crates/nugget-plugin/sdk/` | Rust guest SDK (`nugget-plugin-sdk`), its own workspace, built for `wasm32-wasip2` |
| `crates/nugget-plugin/sdk/examples/` | `feed-reader` (RSS/Atom connector + `feed_headlines`), `text-tools` (`text_stats`, `top_words`, no capabilities) |
| `crates/nugget-plugin/examples/sign-plugin.rs` | Signs a package directory and bundles a `.nuggetplugin` file |
| `plugins/template/` | Starting point: one connector, one tool, native tests, a README |
| `plugins/rss/`, `obsidian/`, `read-later/` | First-party plugins (`dev.nugget.*`), see below |
| `plugins/build.sh` | Builds, packages and (with a key) signs the first-party and example plugins |

## Quickstart

```sh
rustup target add wasm32-wasip2
cp -r plugins/template ~/my-plugin && cd ~/my-plugin
# rename the crate in Cargo.toml and the `id` in nugget-plugin.toml
cargo test-native                 # native tests against the SDK's TestHost
cargo build --release             # -> target/wasm32-wasip2/release/my_nugget_plugin.wasm
```

Then [package and sign](#packaging-and-signing) it with a throwaway key, add that key to
`trusted_keys` (or use `dev_mode`), and install the `.nuggetplugin` file from the Plugins page.

## What a plugin can do

- **Connector** (`kind = ["connector"]`): exports `connector.validate(settings)` and
  `connector.sync(settings, cursor) -> cursor`, sending upserts, deletes and checkpoints through
  `sink` under the native connector contract ([Writing a connector](book/src/contributing/connectors.md)).
  Registers as source kind **`plugin:<id>`**.
- **Tools** (`kind = ["tool"]`): exports `tools.list-tools()` and `tools.call(name, input)`, JSON in
  and out. Offered to the agent as **`plugin.<id>.<tool>`** (lowercased, other characters than
  `[a-z0-9_.-]` → `_`, names over 64 chars cut and suffixed with `-` + 8 hex of a hash). Tools are
  **read-only** (HTTP `GET`/`HEAD`, kv and secret reads, no sink, no files) and the lowest risk class.

The host refuses to call an export the manifest does not list.

## Imports

```wit
world plugin {
    import http;     // send(request) -> response; declared hosts only
    import kv;       // get / set / delete / keys; scoped to the plugin
    import secrets;  // get / set; declared names only, stored in the OS keychain
    import log;      // into Nugget's log, tagged with the plugin id
    import clock;    // now() -> ms since the Unix epoch; the only clock
    import fs;       // read-dir / read-file / stat below user-chosen paths; connectors only
    import sink;     // upsert / delete / checkpoint / known-items / cancelled; during sync only
    export connector;
    export tools;
}
```

Every call is checked against approved capabilities, else `plugin-error::denied`. Errors:
`invalid`, `unauthorized`, `network`, `rate-limited(ms)`, `not-found`, `denied`, `cancelled`,
`other`. **No WASI**: WASI imports are linked to trapping functions, so `std::fs`, `std::env`,
`SystemTime`, `println!` and `HashMap::new()` trap; use `nugget_plugin_sdk::now_ms`, `log` and
`BTreeMap`. Imports outside `nugget:plugin` and WASI are refused at load.

- **http**: absolute URL whose scheme, host and port match a declared host. No redirects (3xx is
  returned). Hop-by-hop, `host`, `content-length` and `proxy-*` headers refused. Recorded in the
  egress ledger as `sync:plugin:<id>` or `tool:plugin:<id>`. Hosts resolving to loopback, private,
  link-local, unique-local, CGNAT, unspecified, multicast or documentation addresses are **always
  refused**, and the checked addresses are the ones connected to.
- **kv**: at most `kv_max_bytes`. One namespace per connector source; tools share a read-only one.
- **secrets**: keychain account `plugin:<id>:<name>`, declared names only; tools read only.
- **sink**: only in `sync`. `checkpoint(cursor)` commits once everything before it is stored;
  `known-items` lists stored keys and versions; after `cancelled()` returns true, return
  `cancelled` promptly. Keys 1 to 1024 bytes.
- **validate** runs with no network, secrets, kv or sink, within 2 s.

## The manifest: `nugget-plugin.toml`

Unknown fields are errors.

```toml
id = "dev.example.feeds"            # reverse-DNS: lowercase [a-z0-9-] labels, >= 2 labels, <= 64 chars
name = "RSS and Atom feeds"          # 1-80 characters
version = "0.1.0"                    # semver; an update must raise it
description = "Indexes one feed."    # 0-2000 characters
kind = ["connector", "tool"]         # at least one; no duplicates

[capabilities]                       # everything is denied unless listed here
hosts = ["feeds.example.com"]        # at most 64
secrets = ["api_token"]              # at most 16, [a-z0-9_-], 1-64 chars each
kv_max_bytes = 4096                  # 0 (default) = no key-value store; at most 16 MiB
file_roots = ["folder"]              # connectors only: settings fields holding a user-chosen path

[connector]                          # required with kind connector, forbidden otherwise
display_name = "RSS / Atom feed"     # shown in Sources; 1-80 characters
auth = "none"                        # "none" (default) or "password" (needs a declared secret)
poll_interval_secs = 1800            # default 1800, at least 60

[connector.settings_schema]          # JSON Schema of the source settings object
type = "object"
required = ["url"]
```

| `capabilities.hosts` entry | Allows |
|---|---|
| `example.com` | `https://example.com` on the default port |
| `example.com:8443` | `https://example.com:8443` only |
| `*.example.com` | any subdomain over https, not `example.com` itself (must cover a registered domain, not a TLD) |
| `http://example.com` | plain `http`, only when spelled out |

No paths, queries, credentials or other wildcards. With no capabilities a plugin can only log and
read the clock.

## Limits

A fresh instance per call (only kv persists). Defaults (`nugget_plugin::Limits`):

| Limit | Default |
|---|---|
| Linear memory | 64 MiB |
| Fuel per tool call or `validate` / per sync pass | 5 × 10⁹ / 2 × 10¹¹ |
| Wall clock: tool call / `validate` / sync pass | 15 s / 2 s / 30 min |
| HTTP response / request body | 16 MiB / 4 MiB |
| HTTP requests per tool call / per sync pass | 32 / 10 000 |
| Document content; tool input or output (JSON) | 16 MiB; 1 MiB |
| Tools per plugin; log lines per call | 64; 1000 |
| File size; entries per folder listing | 16 MiB; 10 000 |
| File bytes read per `validate` / per sync pass | 4 MiB / 4 GiB |
| File operations per `validate` / per sync pass | 1000 / 1 000 000 |

Running out traps the instance and the call fails; an SDK panic is logged as an error.

## Writing one in Rust

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
nugget-plugin-sdk = { path = ".../crates/nugget-plugin/sdk" }
```

Implement `Tools { fn tools() -> Vec<Tool>; fn call(&str, Value) -> Result<Value> }` and/or
`Connector { fn validate(&Value) -> Result<()>; fn sync(&Value, Option<Value>) -> Result<Value> }`,
then `export_plugin!(tools: T)`, `(connector: C)` or `(connector: C, tools: T)`. `plugins/template`
and `sdk/examples/text-tools` are complete examples.

Tool names are `[a-z0-9_]`, 1 to 64 characters, unique; input schemas are JSON Schema objects.
Build with `--target wasm32-wasip2` (the template and SDK set it in `.cargo/config.toml`); the
output must be a component, core modules are refused.

## Files the user chose (`fs`)

`capabilities.file_roots` names **settings fields** that hold a path (folder or file); each must be
a settings property. The plugin reads only below those paths:

```toml
[capabilities]
file_roots = ["vault"]

[connector.settings_schema.properties.vault]
type = "string"
title = "Vault folder"
```

From Rust: `fs::read_dir("vault", "Daily")`, `fs::read_to_string(root, path)`, `fs::stat(root, "")` (`""` is the root).

- Read-only (`read-dir` sorted, hidden entries included; `read-file`; `stat`), connectors only.
- Paths are relative and `/`-separated: no leading `/`, empty, `.`, `..`, `\`, `:`, control
  characters, trailing `.` or space, or Windows device names. The configured root must be absolute.
- Symlinks are followed only inside the root; links out are hidden and `denied`.
- Undeclared roots or empty settings fields fail (`denied` / `invalid`). The approval dialog shows
  the roots.

Use the root-relative path as key and `<mtime>:<size>` as version so passes read only what changed.
If a listing fails part-way, skip deletes for that pass.

## Testing natively

On non-wasm targets the SDK's host calls go to `nugget_plugin_sdk::testing::TestHost`, so
`cargo test-native` (`cargo test --target host-tuple`, defined in the SDK, `plugins/` and the
template) tests plugin logic on Linux, macOS or Windows with no runtime or network.

See `plugins/template` for a complete test.


| `TestHost` | What it does |
|---|---|
| `new()`, `from_manifest(toml)` | Empty host, or one with the manifest's capabilities |
| `allow_host`, `allow_secret`, `set_kv_max_bytes`, `allow_file_root`, `set_limits` | Adjust capabilities and limits |
| `respond`, `respond_with`, `requests()` | Canned HTTP by method and exact URL (unmatched → `network`; private IPs denied unless `allow_private_network(true)`); every request sent |
| `kv_get`, `kv_json`, `kv_set`, `kv_keys`, `tools_kv_set`, `set_secret`, `secret` | Seed or inspect kv and secrets, with the host's namespaces |
| `set_now`, `advance` | Fake clock (default 1 700 000 000 000) |
| `mount`, `mount_path`, `edit_mount` | File roots in memory (`MemFs`) or on disk |
| `seed_item`, `known_items`, `documents`, `document`, `sink_events` | The store behind the sink |
| `set_cancelled`, `cancel_after_upserts(n)` | Drive `sink::cancelled()` |
| `validate`, `sync -> SyncPass`, `tools`, `call` | Run the plugin with the host's mode restrictions and checks |
| `logs()`, `logged(level, text)` | Captured log lines |

It cannot catch WASI traps or fuel, memory and time limits: build the wasm and try it in the app.

## First-party plugins

- **`plugins/rss`** (connector + tool): a reviewed list of feed hosts (`*.substack.com`,
  `*.github.io`, `hnrss.org`, …); other feeds fail `denied` (the README explains adding one).
  Conditional GETs and entry versions in kv, ignoring state from uncommitted passes.
- **`plugins/obsidian`** (`file_roots = ["vault"]`): versions notes by `<mtime>:<size>`, parses
  front matter, tags and wikilinks, deletes only after a complete walk.
- **`plugins/read-later`** (`file_roots = ["export"]`): Pocket HTML/CSV, Instapaper CSV, Netscape
  bookmarks, keyed by normalised URL.

`plugins/build.sh [--out DIR] [--check] [--sign]` builds these and the SDK examples into
`DIR/<id>/{nugget-plugin.toml,plugin.wasm}` (default `target/plugin-packages`), and with
`NUGGET_PLUGIN_SIGNING_KEY` (or `_FILE`) signs each into `DIR/<id>.nuggetplugin`. `--check` runs
fmt, wasm clippy and native tests first. CI signs with a per-run throwaway key.

## Packaging and signing

A package is the manifest, the component and an ed25519 signature over
`nugget-plugin-package/v1` + SHA-256 of the manifest bytes + SHA-256 of the component bytes,
checked before anything is parsed. As a directory: `nugget-plugin.toml`, `plugin.wasm`,
`plugin.sig`. As one **`.nuggetplugin`** file:

```text
b"NUGGETPKG\x01"                    10-byte magic (format version 1)
u32 LE length + manifest bytes      at most 64 KiB
u32 LE length + component bytes     at most 32 MiB
u32 LE length + signature bytes     at most 1024 (hex or 64 raw bytes; empty = unsigned)
```

Put `nugget-plugin.toml` and the built `.wasm` (as `plugin.wasm`) in one directory, then:

```sh
# hex ed25519 seed (32 bytes), e.g. `openssl rand -hex 32`; keep it secret
NUGGET_PLUGIN_SIGNING_KEY=<64 hex chars> \
  cargo run -p nugget-plugin --example sign-plugin -- dist/text-tools --out dist/text-tools.nuggetplugin
```

`sign-plugin` validates the manifest, writes `plugin.sig` (and the package with `--out`/`-o`), and
prints the public key for `trusted_keys`. `NUGGET_PLUGIN_SIGNING_KEY_FILE` works instead.

## Trust model

```toml
[plugins]
enabled = true                         # false: no plugin loads
trusted_keys = ["<hex ed25519 public key>"]
dev_mode = false
```

- A plugin loads only if its signer is in `trusted_keys`, or is the local dev key while `dev_mode`
  is on. Others are listed as `untrusted` with an `error`, not loaded.
- **Dev mode** accepts unsigned packages and signs them with a local dev key in the keychain; they
  stop loading when it is off. Point `plugins.install` at your build directory.
- Installing shows the signer and requested capabilities; nothing installs until approved (per
  version).
- **Updates** need the same key and a higher version. An update within the approved capabilities
  installs silently; one asking for more is approved again. The host enforces the intersection of
  approved and declared capabilities.
- Packages are stored content-addressed under `<data>/plugins/` and re-verified on load.

## Installing

In the app: **Plugins** → choose a `.nuggetplugin` → review → approve. Over IPC, `plugins.install`
takes a path the daemon can read, verifies and stages it under a `token` for 10 minutes; if
`needs_approval` is false it installs at once, otherwise call `plugins.approve`. Method list:
[IPC reference](book/src/reference/ipc.md). Install, approve, reject, remove, enable and disable
are audited.

- A connector appears in **Sources** as `plugin:<id>` only **after the daemon restarts**.
- Tools are available immediately and vanish when the plugin is disabled or removed.
- Removing a plugin deletes its kv state.

## Untrusted content

Everything a plugin returns (documents, tool results, error messages) is untrusted. Tool output is
fenced as data and scanned for injected instructions. Plugins cannot act (tools are read-only),
but their output can still try to steer the model: check a plugin's hosts and signer before
trusting it.
