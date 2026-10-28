# nugget-plugin-sdk

Write Nugget connectors and read-only agent tools as WebAssembly components.

This folder is its own cargo workspace (not part of the root one). It builds for `wasm32-wasip2`:

```sh
rustup target add wasm32-wasip2
cd crates/nugget-plugin/sdk
cargo build --release            # target comes from .cargo/config.toml
```

- `src/`: the SDK. Implement `Connector` and/or `Tools`, then call `export_plugin!`. Host access
  goes through `http`, `kv`, `secrets`, `log`, `now_ms` and `sink`. WASI is not linked, so `std::fs`,
  `std::env`, `SystemTime` and stdout trap. Use `BTreeMap` rather than `HashMap::new`, because
  its random seed needs WASI.
- `examples/feed-reader/`: an RSS/Atom connector plus a `feed_headlines` tool, with its
  `nugget-plugin.toml`.
- `test-guests/hostile/`: a misbehaving plugin used by the host's security tests.

## Packaging

1. Put `nugget-plugin.toml` and the built `.wasm` (renamed to `plugin.wasm`) in one directory.
2. Sign the directory:
   `NUGGET_PLUGIN_SIGNING_KEY=<hex seed> cargo run -p nugget-plugin --example sign-plugin -- <dir>`.
   This writes `plugin.sig`, an ed25519 signature over the SHA-256 hashes of both files.
3. The host's `PluginStore::install` verifies the signature against trusted keys. It then asks the
   user to approve the declared capabilities.

The interface is `../wit/plugin.wit` (`nugget:plugin@0.1.0`, world `plugin`).
