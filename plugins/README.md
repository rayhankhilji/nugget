# Nugget plugins

First-party plugins built with the plugin SDK (`crates/nugget-plugin/sdk`), and the template to
start your own. The author guide is [`docs/plugins.md`](../docs/plugins.md).

| Plugin | Id | Kind | Capabilities |
|---|---|---|---|
| [`rss`](rss) | `dev.nugget.rss` | connector + `feed_headlines` tool | feed-platform hosts, 4 MiB key-value store |
| [`obsidian`](obsidian) | `dev.nugget.obsidian` | connector | file root `vault` |
| [`read-later`](read-later) | `dev.nugget.read-later` | connector | file root `export` |
| [`template`](template) | `dev.example.my-plugin` | connector + tool | one host, one secret |

This folder is its own cargo workspace (`template/` is a separate one so it can be copied out
as-is). Everything builds for `wasm32-wasip2` by default:

```sh
rustup target add wasm32-wasip2
cd plugins
cargo test-native                 # native tests against the SDK's TestHost (no network)
cargo build --release             # -> target/wasm32-wasip2/release/nugget_*.wasm
./build.sh --check                # fmt + clippy + tests, then build and stage every package
```

`build.sh [--out DIR] [--check] [--sign]` stages each plugin (and the SDK examples) as
`DIR/<id>/{nugget-plugin.toml,plugin.wasm}`, default `target/plugin-packages`; with
`NUGGET_PLUGIN_SIGNING_KEY` set it signs each into `DIR/<id>.nuggetplugin`. Run `./build.sh --help`
for the environment variables.
