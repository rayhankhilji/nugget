# Nugget plugin template

A working plugin with one **connector** (pulls items from a JSON API into Nugget) and one **tool**
(`word_count`, for the agent), native tests, and a manifest. Start here; the author guide is
[`docs/plugins.md`](../../docs/plugins.md).

```text
template/
  Cargo.toml            the crate (a cdylib) and its own workspace
  .cargo/config.toml    builds for wasm32-wasip2; `cargo test-native` runs tests natively
  nugget-plugin.toml    the manifest: id, kind, capabilities, connector settings schema
  src/lib.rs            MyConnector + MyTools, exported with export_plugin!
  src/tests.rs          tests against the SDK's TestHost (no wasm, no network)
```

## Five minutes

You need Rust (stable, 1.88 or newer) and the wasm target: `rustup target add wasm32-wasip2`.

**1. Copy and rename.**

```sh
cp -r plugins/template ~/my-plugin && cd ~/my-plugin
```

- In `Cargo.toml`, rename the crate (`name = "my-nugget-plugin"`). Outside the Nugget repository,
  switch the SDK dependency to the git line in the comment above it.
- In `nugget-plugin.toml`, set your own reverse-DNS `id` (say `com.yourname.notes`), `name` and
  `description`, and list the hosts you will really call under `capabilities.hosts`.

**2. Test natively.**

```sh
cargo test-native
```

This runs `src/tests.rs` on your machine's own target. Every SDK host call goes to
`nugget_plugin_sdk::testing::TestHost`, built from your `nugget-plugin.toml`: canned HTTP responses
(an undeclared host is denied, as in Nugget), an in-memory key-value store and secrets, a fake
clock, file roots and a sink that records what the connector sent. Nothing touches the network.

**3. Build the component.**

```sh
cargo build --release
# -> target/wasm32-wasip2/release/my_nugget_plugin.wasm
```

**4. Package and sign** with a throwaway development key. From a checkout of the Nugget repository:

```sh
mkdir -p dist/my-plugin
cp ~/my-plugin/nugget-plugin.toml dist/my-plugin/
cp ~/my-plugin/target/wasm32-wasip2/release/my_nugget_plugin.wasm dist/my-plugin/plugin.wasm

export NUGGET_PLUGIN_SIGNING_KEY=$(openssl rand -hex 32)   # throwaway: never reuse it for releases
cargo run -p nugget-plugin --example sign-plugin -- dist/my-plugin --out dist/my-plugin.nuggetplugin
```

`sign-plugin` validates the manifest with the same parser the daemon uses, writes
`dist/my-plugin.nuggetplugin` and prints the signer's public key.

**5. Install.** Add the printed public key to `trusted_keys` in the `[plugins]` section of Nugget's
`config.toml` (see the data-directory reference for where it lives) and restart the daemon:

```toml
[plugins]
enabled = true
trusted_keys = ["<the printed public key>"]
```

Then open **Plugins** in the app, choose `dist/my-plugin.nuggetplugin`, review the requested
capabilities and approve. The tool is available to the agent at once as
`plugin.<id>.word_count`; the connector appears in **Sources** as `plugin:<id>` after the daemon
restarts.

For quick iteration you can skip signing: with `dev_mode = true` under `[plugins]`, the daemon
accepts an unsigned package directory (`nugget-plugin.toml` + `plugin.wasm`, with an empty
`plugin.sig`) and signs it with its local dev key. Turn `dev_mode` off again when you are done.

## Next steps

- Read files the user picks instead of calling an API: declare `file_roots` (see
  [`plugins/obsidian`](../obsidian) and [`plugins/read-later`](../read-later)).
- Keep per-source state between passes in the key-value store (`kv_max_bytes`, see
  [`plugins/rss`](../rss)).
- Only tools? Use `kind = ["tool"]`, drop `[connector]` and `export_plugin!(tools: MyTools)`.

Remember the plugin sandbox has no WASI: `std::fs`, `std::env`, `SystemTime`, `println!` and
`HashMap::new()` trap at run time. Use the SDK's `fs`, `now_ms`, `log` and `BTreeMap`. (In native
tests they work, so a test passing natively is not proof; the guide lists what to avoid.)
