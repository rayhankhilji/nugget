# Writing a Nugget plugin

A plugin is a WebAssembly component that adds a **connector** (a new source kind in the ingest
pipeline), read-only **tools** for the agent, or both. It runs inside `nuggetd` under wasmtime, sees
the host only through the `nugget:plugin` imports, and gets only the capabilities its manifest
declares and the user approved.

The pieces live in `crates/nugget-plugin`:

| Path | What it is |
|---|---|
| `wit/plugin.wit` | The interface: package `nugget:plugin@0.1.0`, world `plugin` |
| `sdk/` | The Rust guest SDK (`nugget-plugin-sdk`), its own cargo workspace built for `wasm32-wasip2` |
| `sdk/examples/feed-reader/` | RSS/Atom connector plus a `feed_headlines` tool (declares one host and a key-value store) |
| `sdk/examples/text-tools/` | Two pure-computation tools, `text_stats` and `top_words`, with no capabilities at all |
| `examples/sign-plugin.rs` | Signs a package directory and bundles it into a `.nuggetplugin` file |

First-party plugins and the template live in `plugins/` at the repository root (its own cargo
workspace, depending on the SDK by path):

| Path | What it is |
|---|---|
| `plugins/template/` | The starting point: one connector, one tool, native tests, a five-minute README |
| `plugins/rss/` | `dev.nugget.rss`: RSS 2.0 / Atom / RSS 1.0 feeds with conditional GETs, plus `feed_headlines` |
| `plugins/obsidian/` | `dev.nugget.obsidian`: an Obsidian vault importer over a file root |
| `plugins/read-later/` | `dev.nugget.read-later`: Pocket, Instapaper and bookmark exports over a file root |
| `plugins/build.sh` | Builds, packages and (with a key) signs the first-party and example plugins |

## Quickstart

```sh
rustup target add wasm32-wasip2
cp -r plugins/template ~/my-plugin && cd ~/my-plugin
# rename the crate in Cargo.toml and the `id` in nugget-plugin.toml
cargo test-native                 # tests run natively against the SDK's TestHost
cargo build --release             # -> target/wasm32-wasip2/release/my_nugget_plugin.wasm
```

Then package and sign it (see [Packaging and signing](#packaging-and-signing)) with a throwaway key,
add that key to `trusted_keys` and install the `.nuggetplugin` file from the Plugins page.
`plugins/template/README.md` walks through every step.

## What a plugin can do

- **Connector** (`kind = ["connector"]`). Exports `connector.validate(settings)` and
  `connector.sync(settings, cursor) -> cursor`. A sync pass sends upserts, deletes and checkpoints
  through the `sink` import, exactly like a native connector (see
  [Writing a connector](book/src/contributing/connectors.md) for the contract: checkpoints after
  persistence, a pass without a cursor reconciles, versions make re-sends cheap). The connector
  registers as source kind **`plugin:<id>`** with the manifest's `display_name` and settings schema.
- **Tools** (`kind = ["tool"]`). Exports `tools.list-tools()` and `tools.call(name, input)`, both JSON
  in and out. Each tool is offered to the agent as **`plugin.<id>.<tool>`** (lowercased, characters
  outside `[a-z0-9_.-]` replaced by `_`; names over 64 characters are cut and end in `-` plus 8 hex
  digits of a hash). Tools are **read-only**: HTTP `GET`/`HEAD` only, key-value and secret reads only,
  no sink. The agent treats them as the lowest risk class.

A plugin can be both (`kind = ["connector", "tool"]`); the host refuses to call an export the
manifest does not list.

## The world and its imports

```wit
world plugin {
    import http;     // send(request) -> response; declared hosts only
    import kv;       // get / set / delete / keys; scoped to the plugin
    import secrets;  // get / set; declared names only, stored in the OS keychain
    import log;      // log(level, message), into Nugget's log, tagged with the plugin id
    import clock;    // now() -> ms since the Unix epoch; the only clock
    import fs;       // read-dir / read-file / stat below user-chosen paths; connectors only
    import sink;     // upsert / delete / checkpoint / known-items / cancelled; during sync only

    export connector;
    export tools;
}
```

Every import call is checked against the approved capabilities; anything else fails with
`plugin-error::denied`. Errors map onto Nugget's error kinds: `invalid`, `unauthorized`, `network`,
`rate-limited(ms)`, `not-found`, `denied`, `cancelled`, `other`.

There is **no WASI**: no direct filesystem access (only the `fs` import), sockets, environment
variables, stdio, random seed or system clock.
A component may import WASI interfaces (Rust's `std` adds them), but they are linked to functions
that trap. In Rust that means `std::fs`, `std::env`, `SystemTime`, `println!` and
`HashMap::new()` trap; use `nugget_plugin_sdk::now_ms`, the `log` module and `BTreeMap`. A component
that imports anything outside `nugget:plugin` and WASI is refused at load.

Details of the imports:

- **http.** The URL must be absolute and its scheme, host and port must match a declared host.
  Redirects are never followed (a 3xx comes back as-is). The host refuses hop-by-hop headers and
  `host`, `content-length`, `proxy-*`. Response bodies are capped. Every exchange is recorded in the
  egress ledger as `sync:plugin:<id>` (connectors) or `tool:plugin:<id>` (tools). Requests to a host
  that resolves to a loopback, private, link-local, unique-local, CGNAT, unspecified, multicast or
  documentation address are **always refused**, even when the manifest lists that host; the
  connection uses the addresses that were checked, so DNS cannot be switched between check and
  connect.
- **kv.** A small persistent store under the data dir, at most `kv_max_bytes` in total. Connectors
  get one namespace per configured source; tools share one namespace and may only read it.
- **secrets.** Stored in the OS keychain under the account `plugin:<id>:<name>`. Only declared names
  are reachable. Tools may only read.
- **fs.** Read-only access to the files and folders the user chose for a connector source; see
  [Files the user chose](#files-the-user-chose-fs). Tools are always denied.
- **sink.** Available only inside `connector.sync`. `upsert` waits while the pipeline is busy;
  `checkpoint(cursor)` commits once everything sent before it is stored; `known-items` lists the
  keys and versions the store holds for this source; `cancelled()` says the host asked the pass to
  stop, after which `sync` should return `cancelled` promptly. Emitted documents are validated (key 1
  to 1024 bytes, size cap).
- **validate** runs with no network, secrets, key-value store or sink, and a 2 second limit.

## The manifest: `nugget-plugin.toml`

Unknown fields are errors, so a typo cannot silently change what the user approves.

```toml
id = "dev.example.feeds"            # reverse-DNS: lowercase [a-z0-9-] labels, >= 2 labels, <= 64 chars
name = "RSS and Atom feeds"          # 1-80 characters
version = "0.1.0"                    # semver; an update must raise it
description = "Indexes one feed."    # 0-2000 characters
kind = ["connector", "tool"]         # at least one of connector, tool; no duplicates

[capabilities]                       # everything is denied unless listed here
hosts = ["feeds.example.com"]        # at most 64
secrets = ["api_token"]              # at most 16, [a-z0-9_-], 1-64 chars each
kv_max_bytes = 4096                  # 0 (default) = no key-value store; at most 16 MiB
file_roots = ["folder"]              # connectors only: settings fields holding a user-chosen path

[connector]                          # required with kind connector, forbidden otherwise
display_name = "RSS / Atom feed"     # shown in Sources; 1-80 characters
auth = "none"                        # "none" (default) or "password" (needs a declared secret)
poll_interval_secs = 1800            # default 1800, at least 60

[connector.settings_schema]          # JSON Schema of the source settings object (a table)
type = "object"
required = ["url"]
```

**Host rules** (`capabilities.hosts`):

| Entry | Allows |
|---|---|
| `example.com` | `https://example.com` on the default port |
| `example.com:8443` | `https://example.com:8443` only |
| `*.example.com` | any subdomain of `example.com` over https, not `example.com` itself |
| `http://example.com` | plain `http` (only when the scheme is spelled out) |

Entries are bare hosts: no path, query, credentials or other wildcards. A wildcard must cover a
registered domain, not a TLD. IP literals are accepted by the parser but private and loopback
addresses are refused at request time anyway (see above).

A plugin with no capabilities at all (like `text-tools`) can still log and read the clock; every
other import fails with `denied`.

## Limits

Every call gets a fresh instance from a cached, pre-linked component, so no state survives between
calls except the key-value store. Defaults (`nugget_plugin::Limits`):

| Limit | Default |
|---|---|
| Linear memory | 64 MiB |
| Fuel per tool call or `validate` | 5 × 10⁹ |
| Fuel per sync pass | 2 × 10¹¹ |
| Wall clock: tool call / `validate` / sync pass | 15 s / 2 s / 30 min |
| HTTP response body / request body | 16 MiB / 4 MiB |
| HTTP requests per tool call / per sync pass | 32 / 10 000 |
| Document content | 16 MiB |
| Tool input or output (JSON) | 1 MiB |
| Tools per plugin | 64 |
| Log lines per call | 1000 (the rest are dropped) |
| File size (`fs.read-file`) | 16 MiB |
| Entries per folder listing | 10 000 |
| File bytes read per `validate` / per sync pass | 4 MiB / 4 GiB |
| File operations per `validate` / per sync pass | 1000 / 1 000 000 |

Running out of fuel, memory or time traps the instance and the call fails; a panic in the SDK is
logged and becomes an error.

## Writing one in Rust

```toml
# Cargo.toml
[lib]
crate-type = ["cdylib"]

[dependencies]
nugget-plugin-sdk = { path = ".../crates/nugget-plugin/sdk" }
```

```rust,ignore
use nugget_plugin_sdk::{Error, Result, Tool, Tools, Value, export_plugin, json};

struct MyTools;

impl Tools for MyTools {
    fn tools() -> Vec<Tool> {
        vec![Tool::new("shout", "Upper-case a text.", json!({
            "type": "object", "required": ["text"],
            "properties": { "text": { "type": "string" } }
        }))]
    }

    fn call(name: &str, input: Value) -> Result<Value> {
        match name {
            "shout" => Ok(json!({ "text": input["text"].as_str().unwrap_or_default().to_uppercase() })),
            _ => Err(Error::NotFound(format!("no tool `{name}`"))),
        }
    }
}

export_plugin!(tools: MyTools);   // or (connector: C) or (connector: C, tools: T)
```

A connector implements `Connector { fn validate(&Value) -> Result<()>; fn sync(&Value, Option<Value>) -> Result<Value> }`
and uses `http`, `kv`, `secrets`, `fs`, `sink`, `log` and `now_ms` from the SDK. `plugins/rss`
shows conditional GETs, incremental state in the key-value store, reconciling deletes and a tool
sharing the parser; `examples/feed-reader` is a smaller version of it.

Tool names are `[a-z0-9_]`, 1 to 64 characters, unique within the plugin. Input schemas are JSON
Schema objects.

## Files the user chose (`fs`)

A connector that imports local data declares, in `capabilities.file_roots`, the **names of
settings fields** that hold a path (a folder or a single file). The user types or picks those
paths when adding the source, and the plugin can read only below them:

```toml
[capabilities]
file_roots = ["vault"]

[connector.settings_schema.properties.vault]   # every root must be a settings property
type = "string"
title = "Vault folder"
```

```rust,ignore
use nugget_plugin_sdk::fs;

for e in fs::read_dir("vault", "Daily")? {          // root name, relative `/` path
    if !e.is_dir && e.name.ends_with(".md") {
        let text = fs::read_to_string("vault", &fs::join("Daily", &e.name))?;
    }
}
let root = fs::stat("vault", "")?;                    // "" is the root itself
```

- **Read-only.** `read-dir` (sorted by name, hidden entries included), `read-file` (whole file)
  and `stat`. Nothing writes.
- **Connectors only**, during `validate` and `sync`. Tools are always denied.
- **Paths** are relative and `/`-separated on every OS: no leading `/`, no empty, `.` or `..`
  segments, no `\`, `:` or control characters, no segment ending in `.` or a space, no Windows
  device names. The root's own path in the settings must be absolute.
- **Symlinks** are followed only while they stay inside the root: a link out of it is left out of
  listings and reading it is `denied`. Names a plugin could not address are left out of listings.
- **Caps**: files over 16 MiB fail with `invalid`; listings over 10 000 entries fail; bytes read
  and operations are budgeted per call and per sync (see [Limits](#limits)), `denied` when spent.
- The approval dialog shows the file roots. A root that is not declared, or whose settings field
  is empty, fails (`denied` / `invalid`).

Keys are usually the root-relative path, and `<mtime>:<size>` makes a cheap version, so a pass
only reads what changed (see `plugins/obsidian`). When a listing fails part-way, skip deletes for
that pass rather than deleting what you could not see.

## Testing natively

On any target other than `wasm32`, the SDK's host calls go to an in-process fake host,
`nugget_plugin_sdk::testing::TestHost`, so plugin logic is tested with plain `cargo test` on your
own machine: no wasm runtime, no network. The SDK workspace, `plugins/` and the template define
`cargo test-native` (`cargo test --target host-tuple`, since their `.cargo/config.toml` builds for
`wasm32-wasip2` by default); it works on Linux, macOS and Windows.

```rust,ignore
use nugget_plugin_sdk::testing::{MemFs, TestHost};
use nugget_plugin_sdk::{http, json};

#[test]
fn syncs() {
    // Capabilities and kinds come from the manifest, enforced like the real host does.
    let host = TestHost::from_manifest(include_str!("../nugget-plugin.toml"));
    host.respond("GET", "https://api.example.com/items", http::Response::ok(r#"{"items":[]}"#));
    host.set_secret("api_token", "t");
    host.mount("vault", MemFs::new().file("a.md", "# A"));   // or a temp dir via the settings path
    host.seed_item("old-key", Some("v1"));                    // what known_items returns

    let pass = host.sync::<MyConnector>(json!({ "endpoint": "…", "vault": "/v" }), None).unwrap();
    assert_eq!(pass.upserted_keys(), ["a.md"]);
    assert_eq!(pass.deletes, ["old-key"]);
    let next = host.sync::<MyConnector>(json!({ "…": "…" }), Some(pass.cursor)).unwrap();

    assert_eq!(host.call::<MyTools>("word_count", json!({ "text": "a b" })).unwrap()["words"], 2);
}
```

| `TestHost` | What it does |
|---|---|
| `new()`, `from_manifest(toml)` | An empty host, or one with the manifest's kinds, hosts, secrets, kv size and file roots (malformed manifests panic) |
| `allow_host`, `allow_secret`, `set_kv_max_bytes`, `allow_file_root`, `set_limits` | Adjust capabilities and limits (defaults are the host's) |
| `respond(method, url, Response)`, `respond_with(method, url, fn)` | Canned HTTP keyed by method and exact URL; unmatched requests fail with `network` |
| `requests()` | Every request sent, including denied and unmatched ones |
| `kv_get`, `kv_json`, `kv_set`, `kv_keys`, `set_secret`, `secret` | Inspect or seed the key-value store and secrets |
| `set_now`, `advance` | The fake clock (`now_ms`), default 1 700 000 000 000 |
| `mount(root, MemFs)`, `mount_path(root, path)`, `edit_mount` | File roots: in memory (`MemFs::new().file(..).file_at(.., mtime).dir(..).fail(..)`, `MemFs::single_file`) or on disk; unmounted roots use the settings path like the host |
| `seed_item`, `known_items`, `documents`, `document`, `sink_events` | The store behind the sink: seeded items plus upserts, minus deletes |
| `set_cancelled`, `cancel_after_upserts(n)` | Drive `sink::cancelled()` |
| `validate::<C>(settings)`, `sync::<C>(settings, cursor) -> SyncPass`, `tools::<T>()`, `call::<T>(name, input)` | Run the plugin as the host does: JSON round trips, the mode's restrictions (no network/kv/secrets/sink in `validate`; tools read-only with no files; 32 requests per call), document and tool-I/O checks |
| `logs()`, `logged(level, text)` | Captured log lines (also printed to the test output) |

`SyncPass` holds the returned `cursor` and the pass's `upserts`, `deletes` and `checkpoints`
(`upserted_keys()`, `upserted(key)`). The fake host belongs to the thread that created it, so tests
run in parallel; plugin code called directly from a test (outside `sync`/`validate`/`call`) uses it
without mode restrictions. What it cannot catch: WASI-only failures (`std::fs`, `SystemTime`,
`println!`, `HashMap::new()` work natively but trap in the plugin), fuel, memory and wall-clock
limits. Build the wasm and try it in the app before shipping.

## First-party plugins as worked examples

- **`plugins/rss`** (connector + tool, hosts + key-value store). Feed URLs are user-chosen but host
  rules are static, so it declares a reviewed list of feed platforms (`*.substack.com`,
  `*.github.io`, `hnrss.org`, …); a feed elsewhere fails with `denied` for that feed only, and the
  README explains how to add a host and reinstall. Per-feed `ETag`/`Last-Modified` and entry
  versions live in the key-value store, tagged with the pass that wrote them so state from a pass
  whose cursor never committed is ignored.
- **`plugins/obsidian`** (connector, `file_roots = ["vault"]`). Walks the vault, skips dot-folders,
  versions notes by `<mtime>:<size>` against `known_items`, parses front matter, tags and
  wikilinks into metadata, and deletes only after a complete walk.
- **`plugins/read-later`** (connector, `file_roots = ["export"]`, root may be a file or folder).
  Parses Pocket HTML/CSV, Instapaper CSV and Netscape bookmarks, keys articles by normalised URL,
  and skips work with a fingerprint of the export files in the cursor.

`plugins/build.sh [--out DIR] [--check]` builds the SDK examples and these three as release
components and stages each as `DIR/<id>/{nugget-plugin.toml,plugin.wasm}` (default
`target/plugin-packages`). With `NUGGET_PLUGIN_SIGNING_KEY` set it also signs each into
`DIR/<id>.nuggetplugin`; CI does that with a throwaway key generated per run. `--check` first runs
`fmt --check`, wasm clippy and the native tests in the SDK, `plugins/` and the template.

## Building

```sh
rustup target add wasm32-wasip2
cd crates/nugget-plugin/sdk
cargo build --release            # target comes from .cargo/config.toml
# -> target/wasm32-wasip2/release/<crate>.wasm (a component)
```

For your own crate outside the SDK workspace, build with `--target wasm32-wasip2` (the template's
`.cargo/config.toml` sets it). The output must
be a component (`wasm32-wasip2` produces one); core modules are refused.

## Packaging and signing

A package is three parts: the manifest, the component and an ed25519 signature over
`nugget-plugin-package/v1`, the SHA-256 of the exact manifest bytes and the SHA-256 of the exact
component bytes. The signature is checked before anything in the package is parsed.

It is distributed as one **`.nuggetplugin`** file:

```text
b"NUGGETPKG\x01"                    10-byte magic (format version 1)
u32 LE length + manifest bytes      at most 64 KiB
u32 LE length + component bytes     at most 32 MiB
u32 LE length + signature bytes     at most 1024 (hex or 64 raw bytes; empty = unsigned)
```

Nothing may follow the signature. The same parts can also be a directory holding
`nugget-plugin.toml`, `plugin.wasm` and `plugin.sig`.

To sign and bundle:

```sh
mkdir -p dist/text-tools
cp crates/nugget-plugin/sdk/examples/text-tools/nugget-plugin.toml dist/text-tools/
cp crates/nugget-plugin/sdk/target/wasm32-wasip2/release/text_tools.wasm dist/text-tools/plugin.wasm

# The key is a hex ed25519 seed (32 bytes), e.g. from `openssl rand -hex 32`. Keep it secret.
NUGGET_PLUGIN_SIGNING_KEY=<64 hex chars> \
  cargo run -p nugget-plugin --example sign-plugin -- dist/text-tools --out dist/text-tools.nuggetplugin
```

`sign-plugin` validates the manifest, writes `plugin.sig` into the directory and, with `--out` (or
`-o`), the single-file package (`.nuggetplugin` is added when the name has no extension). It prints
the public key; that is what users add to `trusted_keys`. The seed can also come from a file named
by `NUGGET_PLUGIN_SIGNING_KEY_FILE`; it is never written anywhere.

## Trust model

```toml
# config.toml
[plugins]
enabled = true                         # false: no plugin is loaded and the feature does not start
trusted_keys = ["<hex ed25519 public key>"]
dev_mode = false
```

- **Signers.** A plugin is loaded only when its signer is in `trusted_keys`, or is the daemon's local
  dev key while `dev_mode` is on. Any other plugin is still listed (signature `untrusted`, with an
  `error` saying why) but not loaded.
- **Dev mode.** With `dev_mode = true` the daemon accepts unsigned packages (an empty signature
  section, or a package directory with an empty `plugin.sig`) and signs them with a local dev key kept
  in the OS keychain. Those plugins stop loading when `dev_mode` is turned off. Use it for your own
  builds only: point `plugins.install` at the build directory, no signing key needed.
- **Capability approval.** Installing shows the requested capabilities (hosts, secrets, key-value
  size) and the signer; nothing is installed until the user approves. Approval is per version.
- **Updates** must be signed by the **same key** as the installed version and must have a **higher
  version** (no downgrades or reinstalls of the same version). An update that requests nothing beyond
  what was approved installs without asking; one whose capabilities grow is asked again, showing what
  was approved before. The host always enforces the intersection of what was approved and what the
  current manifest declares.
- **On disk.** Packages are stored content-addressed under `<data>/plugins/`; loading re-hashes the
  blobs and re-checks the signature, so a component altered on disk is refused.

## Installing

**In the app**, open **Plugins**, choose a `.nuggetplugin` file, review the signer and the requested
capabilities, and approve. The page lists installed plugins with their status, signer, approved
capabilities and tools; plugins can be disabled, enabled or removed there.

**Over IPC** (see [IPC protocol](book/src/reference/ipc.md) for the transport):

| Method | Params | Result |
|---|---|---|
| `plugins.list` | `{}` | `{ plugins: [PluginInfo] }` |
| `plugins.inspect` | `{ id }` | `{ plugin, requested, connector_display_name }` |
| `plugins.install` | `{ path }` | `{ token, id, name, version, description, kinds, signature, signer, requested, previously_approved, installed_version, needs_approval, installed }` |
| `plugins.approve` | `{ token, approve }` | `{ plugin }` (`null` when rejected) |
| `plugins.remove` | `{ id }` | `{ removed }` |
| `plugins.enable` | `{ id, enabled }` | `PluginInfo` |

`plugins.install` takes a path the daemon can read (a `.nuggetplugin` file or a package directory),
verifies it and stages it under `token` for 10 minutes. When `needs_approval` is `false` (an update
within the approved capabilities) it is installed at once and `installed` is `true`; otherwise call
`plugins.approve` with the token. `PluginInfo` carries `id`, `name`, `version`, `description`, `kinds`,
`enabled`, `loaded`, `error`, `signature` (`trusted`, `dev` or `untrusted`), `signer`, `approved`,
`installed_at`, `connector_kind` and `tools`. Install, approve, reject, remove, enable and disable are
written to the audit log.

**After installing:**

- A connector shows up in **Sources** as kind `plugin:<id>` **after the daemon restarts**. Connector
  kinds are registered when ingest starts, so a plugin installed while the daemon runs is not
  available as a source until then.
- Tools are available to the agent right away as `plugin.<id>.<tool>`, and disappear when the plugin
  is disabled or removed.
- Removing a plugin deletes its key-value state.

## Untrusted content

Everything a plugin returns is untrusted: documents it indexes, tool results and its error messages.
Tool output is marked untrusted, so the agent fences it as data and scans it for injected
instructions, the same as web or email content. A plugin cannot run actions itself (its tools are
read-only), but its output can still try to steer the model. Review what a plugin's hosts are and
who signed it before trusting the signer.
