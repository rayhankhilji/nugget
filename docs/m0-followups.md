# M0 follow-ups

Known gaps carried out of M0. These come from the work-package reports and the M0 security and correctness review.
Items are grouped by where they'll most likely land.

## Deferred review findings
| Finding | Severity | Why deferred | Plan |
|---|---|---|---|
| Egress is recorded only when an exchange ends (`Drop`), so a crash or SIGKILL mid-request leaves no ledger row; model downloads flush once at the end | Medium | Needs a two-phase `EgressSink` API (begin, then update) across core, infer, models and store | M1: write a row before the first byte is sent, update byte counts as the exchange progresses, flush periodically for downloads |
| Windows: the key file and data directories inherit ACLs (risky on non-system NTFS volumes and FAT/exFAT) | Low | Windows-only code, can't be exercised in CI until Windows tests exist | M1: set a protected DACL (owner and SYSTEM) on the data dir and key file; warn on filesystems without ACLs |
| Audit chain uses unkeyed SHA-256, so anyone holding the store key can rewrite the chain and its head | Low | Needs a design for keying and anchoring | M1: HMAC keyed from the store key, with the head anchored in the OS keychain or periodically signed |
| Ollama `-cloud` models are served from localhost but forward prompts to ollama.com, so they show as on-device | Design | Not detectable from the URL alone | Docs now; M1: a known-forwarder model list and a UI warning |

- **Chat start latency vs. cancel**: tier-routed chats reply `ChatStarted` only after the first token (needed so the reported provider is truthful after fallback), so Stop is unavailable until then. M1/M3: return a provisional stream id immediately and let `chat.cancel` abort the routing attempt.
- **Windows named-pipe server check** (`nugget-ipc`) is compile-checked only; the client also cannot set `SECURITY_IDENTIFICATION` QoS through interprocess.
- **Daemon lock** is held for the process lifetime (not the listener's), because `bind` returns interprocess's `Listener`.

## Platform verification
- Nothing has run on real macOS or Windows yet. The first CI run is the first real check of:
  - the keychain backends
  - the Windows named pipe, and its server-SID check from the review fixes
  - launchd and Task Scheduler service install
  - SQLCipher's vendored OpenSSL on MSVC
- Add Windows and macOS runtime tests for service install and uninstall, on CI runners.

## Per-crate gaps
- **nugget-store:** reader connection pool (WAL allows concurrent reads); key rotation (`PRAGMA rekey`) and migrating a file key into the keychain; FTS phrase and prefix syntax; an ANN index for large vector sets; avoid duplicating item text in both `items` and `chunks`.
- **nugget-infer:**
  - in-process llama.cpp/MLX backends (local inference currently goes through OpenAI-compatible servers);
  - prompt caching, token counting, `tool_choice`, stop sequences, images inside tool results;
  - OAuth bearer auth for Anthropic;
  - an IPC chat message type that can carry images, tools and reasoning replay.
- **nugget-models:**
  - an offline release signing key with rotation and revocation (only a dev key is trusted now; its private half is not in the repo);
  - remote signed-manifest updates with anti-rollback on `version`;
  - per-part mirror failover, a cross-process store lock, bandwidth limits and pause/resume;
  - scheduled lazy re-verification;
  - better GPU and VRAM detection;
  - make `include_bytes!` of the manifest packagable.
- **nugget-ipc:** a per-connection in-flight call limit; a Unix peer-credential check (`SO_PEERCRED`/`getpeereid`) in addition to the 0700 directory.
- **nuggetd:** vector and hybrid search once ingest exists; detached background installs; a GUI-subsystem launcher on Windows so no console window appears; auto-probe at startup; config hot-reload.
- **desktop:**
  - persisted chat history and Markdown rendering;
  - a macOS template tray icon and a fallback where AppIndicator is missing;
  - a single-instance guard;
  - bundle `nuggetd` as a Tauri sidecar, signing and notarisation, autostart;
  - Playwright UI tests.
- **bench:** the real `draft` and `cua` suites (M3/M4); run `memory`/`perf` against the real store once ingest lands.

## Local runtime
- Windows still serves `llama-server` on loopback TCP, where the listener is not ownership-checked
  (needs `GetExtendedTcpTable` FFI); it relies on liveness checks and per-restart key rotation. Unix
  uses a socket in a private 0700 directory.

## Dependencies
- Replace `starship-battery` in nugget-ingest's power policy (it pins plist 1.7 → quick-xml 0.38,
  RUSTSEC-2026-0194/0195, ignored as unreachable in `deny.toml`) with direct platform reads:
  `/sys/class/power_supply` on Linux, IOKit on macOS, `GetSystemPowerStatus` on Windows.

## Process
- The first five commits on this branch predate the DCO check and have no `Signed-off-by`. Add sign-off (rebase `--signoff`) before opening the first PR.
- A `deny.toml` duplicate-version warning comes mostly from Tauri, toml and windows-sys; revisit when those dependencies update.
