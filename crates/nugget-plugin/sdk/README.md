# nugget-plugin-sdk

Write Nugget connectors and read-only agent tools as WebAssembly components. This folder is its
own cargo workspace, built for `wasm32-wasip2`:

```sh
rustup target add wasm32-wasip2
cd crates/nugget-plugin/sdk
cargo build --release            # target comes from .cargo/config.toml
cargo test-native                # tests on the host triple, against testing::TestHost
```

- `src/`: implement `Connector` and/or `Tools`, then `export_plugin!`. Host access through `http`,
  `kv`, `secrets`, `log`, `now_ms`, `fs` and `sink`; natively these go to `testing::TestHost`. WASI
  is not linked: `std::fs`, `std::env`, `SystemTime`, stdout and `HashMap::new` trap.
- `examples/feed-reader/`: small RSS/Atom connector + `feed_headlines` tool (full version: `plugins/rss`).
- `examples/text-tools/`: two tools with no capabilities.
- `test-guests/hostile/`: a misbehaving plugin for the host's security tests.

Start from `plugins/template`. Interface: `../wit/plugin.wit`. The author guide (manifest, limits,
packaging, signing, trust) is [`docs/plugins.md`](../../../docs/plugins.md).
