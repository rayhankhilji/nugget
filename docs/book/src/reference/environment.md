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

## MCP clients (planned)

<!-- check-refs: env.mcp -->
| Variable | Meaning |
|---|---|
| `NUGGET_MCP_TOKEN` | The client token for `nuggetd mcp --client <id>`, set by the MCP app's config. Kept out of the command line so it does not appear in process listings. The bridge command itself is not in the daemon yet |

## Development

<!-- check-refs: env.dev -->
| Variable | Meaning |
|---|---|
| `NUGGET_BENCH_RESULTS_DIR` | Where `nugget-bench` writes reports (default `bench/results/`) |
| `NUGGET_BENCH_COMMIT` | Commit recorded in benchmark reports (falls back to `GITHUB_SHA`, then `.git/HEAD`) |
| `NUGGET_MANIFEST_SIGNING_KEY` | Hex ed25519 private key for the dev manifest signing tool (`cargo run -p nugget-models --example sign-manifest`). Never commit it |
| `NUGGET_MANIFEST_SIGNING_KEY_FILE` | The same key, read from a file |

Build-time: on Windows, `OPENSSL_SRC_PERL` points the vendored OpenSSL build at a native Perl (see
[Development setup](../contributing/setup.md)).
