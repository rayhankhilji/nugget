# M0 follow-ups

Known gaps carried out of M0. These come from the work-package reports and the M0 security and correctness review.
Items are grouped by where they'll most likely land.

## Deferred review findings
| Finding | Severity | Why deferred | Plan |
|---|---|---|---|
| Ollama `-cloud` models are served from localhost but forward prompts to ollama.com, so they show as on-device | Design | Not detectable from the URL alone | Docs now; M1: a known-forwarder model list and a UI warning |

- **Chat start latency vs. cancel**: tier-routed chats reply `ChatStarted` only after the first token (needed so the reported provider is truthful after fallback), so Stop is unavailable until then. M1/M3: return a provisional stream id immediately and let `chat.cancel` abort the routing attempt.
- **Windows named-pipe server check** (`nugget-ipc`) is compile-checked only; the client also cannot set `SECURITY_IDENTIFICATION` QoS through interprocess.
- **Daemon lock** is held for the process lifetime (not the listener's), because `bind` returns interprocess's `Listener`.

## Closed in wave W (W9)
- Two-phase egress: rows are opened before the first byte, updated as bytes flow (infer every
  256 KiB / 1 s, downloads every 8 MiB / 2 s) and marked `interrupted` on the next open after a
  crash. Opening the store from a second process while the daemon runs briefly shows the daemon's
  live rows as `interrupted` until they finish.
- Audit chain: HMAC-SHA256 keyed from the store key for new entries (older SHA-256 entries still
  verified), a sealed head, and the head anchored in the OS keychain when the store key lives
  there. Remaining: with a key-file store, or if the keychain entry is deleted, there is no anchor
  and truncation of the newest entries is not detected.
- Windows protected DACL (owner and SYSTEM) on newly created data dirs, the key file and the
  database files, with a warning on volumes without ACLs. Existing directories are not re-ACL'd;
  compile-checked only until Windows CI runs.
- Store reader pool (four read-only connections); IPC peer uid check and per-connection in-flight
  limit; `RuntimeState::Stopping`; `llama-server` on a Unix socket in a 0700 dir.
- CI: fmt, clippy on Linux/macOS/Windows, full tests on Linux and platform crates on macOS/Windows,
  MSRV 1.88 check, cargo-deny, desktop build and tests, bench smoke on all three OSes.

## Platform verification
- Nothing has run on real macOS or Windows yet. The first CI run is the first real check of:
  - the keychain backends
  - the Windows named pipe, and its server-SID check from the review fixes
  - launchd and Task Scheduler service install
  - SQLCipher's vendored OpenSSL on MSVC
- Add Windows and macOS runtime tests for service install and uninstall, on CI runners.

## Per-crate gaps
- **nugget-store:** key rotation (`PRAGMA rekey`) and migrating a file key into the keychain; FTS phrase and prefix syntax; an ANN index for large vector sets; avoid duplicating item text in both `items` and `chunks`.
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
- **nugget-ipc:** a connection at its in-flight cap (64) stops reading, so it notices a client disconnect only when a call finishes, and a handler that waits for a later request on its own connection (e.g. a cancel) can deadlock at the cap.
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
- ~~Replace `starship-battery` in nugget-ingest's power policy with direct platform reads~~ (done
  in W5). quick-xml 0.38 (RUSTSEC-2026-0194/0195) is still pulled by tauri via plist 1.7, so the
  `deny.toml` ignores stay until tauri updates.

## Process
- The first five commits on this branch predate the DCO check and have no `Signed-off-by`. Add sign-off (rebase `--signoff`) before opening the first PR.
- A `deny.toml` duplicate-version warning comes mostly from Tauri, toml and windows-sys; revisit when those dependencies update.
