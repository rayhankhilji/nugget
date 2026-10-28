# Environment variables

## The daemon

<!-- check-refs: env.daemon -->
| Variable | Meaning |
|---|---|
| `NUGGET_HOME` | Keep all data, config and cache under this directory (`data/`, `config/`, `cache/`). The `--home` flag overrides it. Useful for portable installs, tests and running a second, separate daemon |
| `NUGGET_LOG` | Log filter, in `tracing` `EnvFilter` syntax (`debug`, `info,nugget_ingest=debug`). Default `info`. Falls back to `RUST_LOG` |
| `NUGGET_KEYCHAIN` | `0`, `off`, `false` or `no` turns the OS keychain off. The store key then lives in a `store.key` file readable only by you, and source passwords and tokens are kept **in memory only**: you must set them again after every restart |
| `NUGGET_OCR_MODELS` | A directory holding the two `ocrs` models, `text-detection.rten` and `text-recognition.rten`. Image OCR runs only when they are present. Nugget never downloads them |

Variables that `config.toml` references as `env:VAR` (API keys) must be set in the **daemon's**
environment. A service started at login does not read your shell profile.

## MCP clients

<!-- check-refs: env.mcp -->
| Variable | Meaning |
|---|---|
| `NUGGET_MCP_TOKEN` | The client token for `nuggetd mcp --client <id>`, set in the AI app's MCP config by `nuggetd mcp install` or the setup snippet. Kept out of the command line so it does not appear in process listings. The bridge refuses to start without it |

When the daemon runs with a non-default data root, the setup snippets also set `NUGGET_HOME` for the
bridge, so it finds the same daemon.

## Desktop app

<!-- check-refs: env.desktop -->
| Variable | Meaning |
|---|---|
| `NUGGET_HOTKEY_SESSION` | Linux only: `x11` or `wayland` overrides session detection for the global shortcut (`nugget-hotkey`). Status: Double Tap is being wired in (wave W) |
| `NUGGET_UPDATER_PUBKEY` | Build time: the minisign public key the updater trusts. Without it the build has no updater |
| `NUGGET_UPDATER_FEED` | Build time: where the updater manifests live, for forks (default: this repository's `updater-feed` release) |

## Development

<!-- check-refs: env.dev -->
| Variable | Meaning |
|---|---|
| `NUGGET_BENCH_RESULTS_DIR` | Where `nugget-bench` writes reports (default `bench/results/`) |
| `NUGGET_BENCH_COMMIT` | Commit recorded in benchmark reports (falls back to `GITHUB_SHA`, then `.git/HEAD`) |
| `NUGGET_MANIFEST_SIGNING_KEY` | Hex ed25519 private key for the dev manifest signing tool (`cargo run -p nugget-models --example sign-manifest`). Never commit it |
| `NUGGET_MANIFEST_SIGNING_KEY_FILE` | The same key, read from a file |
| `NUGGET_PLUGIN_SIGNING_KEY` | Signing key for the plugin signing example (`cargo run -p nugget-plugin --example sign-plugin -- <package dir>`). Never commit it |
| `NUGGET_PLUGIN_SIGNING_KEY_FILE` | The same key, read from a file |
| `NUGGET_PLUGIN_REQUIRE_GUESTS` | `1` makes the `nugget-plugin` tests fail, instead of skipping, when the WebAssembly guest plugins cannot be built |
| `NUGGET_A11Y_IT` | `1` runs the Linux accessibility integration tests in `nugget-a11y` |
| `NUGGET_HOTKEY_IT` | `1` runs the X11 integration test in `nugget-hotkey` (needs `DISPLAY`) |
| `NUGGET_CUA_DISPLAY` | An X display for the computer-use benchmark, instead of starting Xvfb |
| `NUGGET_CUA_CHROMIUM` | The Chromium binary for the computer-use benchmark |
| `NUGGET_BENCH_REGEN` | Set to regenerate the proactive benchmark fixtures instead of comparing them |

Build-time: on Windows, `OPENSSL_SRC_PERL` points the vendored OpenSSL build at a native Perl (see
[Development setup](../contributing/setup.md)).
