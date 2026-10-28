# M0: Foundations spec

M0 delivers a daemon (`nuggetd`) that runs on macOS, Windows and Linux. It opens an encrypted store, talks to local
and cloud models through one provider layer, installs verified local models, and serves a desktop app over local
IPC. CI runs on all three OSes.

Shared contracts live in `crates/nugget-core` (ids, errors, capability tiers, egress events, paths, the IPC protocol).
**Do not edit `nugget-core` from a work package.** Propose changes in your report instead.

## Ground rules (all work packages)
- **Ownership.** Edit only the paths your package owns. Add dependencies to *your* crate's `Cargo.toml`, not the root.
- **Scoped builds.** Build and test only your crate: `cargo test -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo fmt`. The target directory is shared with other agents, so waiting on its lock is normal.
- **Git.** Do not commit, push or run any git command that changes state. The orchestrator integrates.
- **Clean room.** Do not read the upstream Sentient OS source. `docs/research/*.md` is the only allowed reference to it.
- **`unsafe`.** It is denied workspace-wide. If FFI is unavoidable (for example registering a SQLite extension), use one minimal function with `#[allow(unsafe_code)]` and a `// SAFETY:` comment.
- **No hard-coded model ids in logic.** Model ids belong only in config, examples and manifests.
- **Tests.** Every public behaviour gets a test. Network is mocked (`wiremock` or a local server). No test may need real credentials.
- **Cross-platform.** Code must compile on macOS, Windows and Linux. Gate OS-specific code with `cfg`, and keep a compiling fallback on every target.

---

## WP-A: `nugget-store` (owns `crates/nugget-store/`)
Encrypted local store: items, chunks, full-text and vector search, cursors, egress ledger, audit log.

- **SQLite** via `rusqlite` with **SQLCipher** (`bundled-sqlcipher-vendored-openssl`), **FTS5**, and **`sqlite-vec`** for vectors.
- **Key.** `StoreKey::load_or_create(&Paths)` tries the OS keychain (`keyring` crate) first. If no keychain is usable (headless Linux, CI), it falls back to a 0600 key file under `Paths::data` and logs a warning. `StoreKey::from_bytes([u8; 32])` is for tests.
- **API** (sync, `Store: Clone + Send + Sync`, internally pooled or behind a mutex):
  - `Store::open(path, &StoreKey)`, and `Store::open_in_memory()` (unencrypted, for tests)
  - `upsert_item(&NewItem) -> UpsertOutcome { Inserted | Updated | Unchanged }`. Duplicates are detected by `content_hash`.
  - `delete_item(&ItemRef) -> bool`. This cascades to chunks and vectors.
  - `get_item(&ItemRef) -> Option<Item>`
  - `replace_chunks(&ItemRef, &[NewChunk { text, embedding: Option<Vec<f32>> }])`
  - `search_text(query, limit)`: FTS5 bm25. `search_vector(&[f32], limit)`. `search_hybrid(query, Option<&[f32]>, limit)` with reciprocal-rank fusion. All return `Vec<Hit { item: ItemRef, chunk_id, title, snippet, score }>`.
  - The embedding dimension is fixed per database, in a `meta` table. A mismatch returns `Error::Invalid`.
  - `get_cursor(&SourceId) -> Option<serde_json::Value>` and `set_cursor(&SourceId, Value)`
  - `counts() -> Counts { items, chunks }`
  - `impl EgressSink for Store`, plus `recent_egress(limit) -> Vec<EgressEvent>`
  - `append_audit(&AuditEntry { actor, action, target, detail: Value })`. Each entry carries a SHA-256 of the previous entry, making a hash chain. `verify_audit_chain() -> Result<()>` detects tampering.
- **Schema migrations** are versioned through `PRAGMA user_version` and idempotent.
- **Tests:**
  - CRUD and dedup by hash
  - delete cascade
  - FTS ranking
  - vector kNN
  - hybrid fusion
  - cursor round-trip
  - an encrypted DB can't be opened with the wrong key
  - the audit chain detects a tampered row
  - migration re-run is a no-op

## WP-B: `nugget-infer` (owns `crates/nugget-infer/`)
One provider layer for every model, local or cloud.

- **Load the `claude-api` skill before writing the Anthropic adapter.**
- `#[async_trait] trait Provider: Send + Sync`:
  - `id() -> &ProviderId`
  - `meta() -> ProviderMeta { kind, model, locality }`
  - `chat(ChatRequest) -> Result<ChatStream>`
  - `embed(&[String]) -> Result<Vec<Vec<f32>>>`, returning `Error::Unsupported` where not supported
- `ChatRequest`:
  - `system: Option<String>`
  - `messages: Vec<Message { role, content: Vec<Part::Text | Part::Image { media_type, data_base64 }> }>`
  - `tools: Vec<ToolSpec { name, description, input_schema }>`
  - `json_schema: Option<Value>`
  - `max_tokens`, `temperature: Option<f32>`
- `ChatStream = Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>`, where `ChatEvent` is one of:
  - `TextDelta(String)`
  - `ToolCall { id, name, arguments: Value }`
  - `Done { stop_reason, input_tokens, output_tokens }`
- **Adapters** (streaming with SSE; `reqwest` with rustls):
  - `anthropic`: Messages API, covering streaming, tools, images and structured output.
  - `openai_responses`: the OpenAI Responses API.
  - `openai_compatible`: `/v1/chat/completions` plus `/v1/embeddings`. This covers Ollama, LM Studio, `llama-server`, OpenRouter and vLLM.
- **Config.** `ProviderConfig` is serde-loaded from TOML: `id`, `kind`, `base_url`, `model`, `api_key` (`env:VAR` or `keychain:NAME`), `tiers`. `build_provider(&ProviderConfig, Arc<dyn EgressSink>)` constructs one.
- **Locality** comes from the base URL host: loopback is `OnDevice`, private ranges are `LocalNetwork`, anything else is `Cloud`. Every non-`OnDevice` request records an `EgressEvent` (host, feature, bytes).
- **Probe.** `probe(&dyn Provider) -> Capabilities`:
  - streaming
  - vision: render a random 4-digit code into a PNG (the `image` plus `ab_glyph` or a built-in bitmap font) and require it to be read back
  - tools: the model must call a given tool with a given argument
  - json_schema: the output must validate
  - embeddings
- **Router.** `Router::new(providers, tier_map)`, and `route(Tier, &Capabilities required) -> Result<Arc<dyn Provider>>` tries candidates in order.
- **Tests:** `wiremock` SSE fixtures for each adapter covering text, tool calls, errors and HTTP 429 or 5xx; locality classification; egress recorded only for non-local providers; router fallback; probe logic against mocked servers.

## WP-C: `nugget-models` (owns `crates/nugget-models/`, `manifests/`)
Verified local model distribution and hardware-aware choice.

- **Manifest.** `Manifest { version, models: Vec<ModelEntry> }`. Each `ModelEntry` has `id, family, file_name, url, mirrors, revision (an exact commit; never "main"), size, sha256, min_ram_gb, tiers, license, min_app_version`.
- **Signing.** The manifest is signed with a detached **ed25519** signature over its exact bytes (`ed25519-dalek`). `verify_manifest(bytes, sig, &[VerifyingKey]) -> Result<Manifest>` must be checked before anything is used.
  - Ship `manifests/default.json`, with **real** GGUF entries: one small chat or triage model (≈3–4B, Q4_K_M), one larger chat model (≈8B), and one embedding model.
  - Use pinned Hugging Face commit revisions, with sizes and SHA-256 taken from the HF API (`/api/models/<repo>/revision/<sha>?blobs=true`).
  - Only Apache-2.0, MIT or similarly permissive licenses, or clearly redistributable ones. Record the license.
  - Include a dev signing tool (`cargo run -p nugget-models --example sign-manifest`). The dev **public** key is embedded; the private key is never committed (read it from an env var or a path).
- **`ModelStore`** (root = `Paths::models_dir()`):
  - Storage is content-addressed (`blobs/sha256/<hex>`), plus `slots.json` mapping `model_id` to `{ active, previous }`.
  - `install(&ModelEntry, on_progress)`, in order:
    1. Disk preflight that **fails closed**: needs size + 10% + 1 GiB free, and an unreadable free-space value is an error.
    2. Resumable parallel range download (N parts, each with retry and backoff).
    3. Streaming SHA-256 check.
    4. Atomic rename into place, then update the slot (keeping the previous one) and record egress.
  - `path(id)`, `verify(id)` (re-hash), `rollback(id)`, `gc()` (removes unreferenced blobs).
- **Hardware.** `detect_hardware() -> Hardware { total_ram, available_ram, cpu_cores, gpu: GpuKind }` (best effort, through `sysinfo`). `recommend(&Manifest, &Hardware, Tier) -> Option<&ModelEntry>`.
- **Tests:** signature accept and reject; a tampered manifest is rejected; download against a local HTTP server, covering range resume, hash mismatch wiping staging, and parallel parts; fail-closed preflight; rollback; gc; recommend by RAM.

## WP-D: `nugget-ipc` + desktop app (owns `crates/nugget-ipc/`, `apps/desktop/`)

### `nugget-ipc`
JSON-RPC 2.0, one object per line, over `interprocess` local sockets (tokio). The protocol types are in `nugget_core::ipc`.

- `socket_name(&Paths)`: a Unix socket in a user-private 0700 directory, or a Windows named pipe scoped to the user.
- **Server.** `serve(listener, Arc<dyn Handler>)`. `#[async_trait] trait Handler { async fn call(&self, method: &str, params: Value, notify: Notifier) -> Result<Value, RpcError>; }`. `Notifier::send(method, params)` pushes a notification to that connection.
- **Client.** `Client::connect(&Paths)`, `call<P: Serialize, R: DeserializeOwned>(method, &P) -> Result<R>`, and `notifications() -> impl Stream<Item = Notification>`. Calls may run concurrently with id correlation.
- **Limits.** Maximum 16 MiB per message, a typed `RpcError` with JSON-RPC error codes, and graceful disconnect handling.
- **Tests:** round-trip, concurrent calls, notifications, error mapping, an oversized message, and the server dropping.

### `apps/desktop`
Tauri 2 shell: React, TypeScript, Vite, pnpm. The crate is named `nugget-desktop` and is already a workspace member.

- **Tray or menu-bar icon** with Open, Status and Quit.
- **Main window pages:**
  - **Chat**: pick a tier or provider, see streamed output.
  - **Search**.
  - **Models**: list, install with a progress bar, recommended badge.
  - **Privacy**: the egress ledger table.
  - **Status**.
- **Rust side.** Tauri commands proxy to `nugget-ipc::Client`, and notifications are re-emitted as Tauri events. If the daemon isn't running, the window shows "Start Nugget". Starting means spawning `nuggetd` from next to the app binary or from `PATH`, then retrying.
- **Frontend.**
  - All IPC goes through a typed `api.ts` layer.
  - No remote fonts or CDNs, since everything must work offline.
  - Light and dark themes via `prefers-color-scheme`; keyboard accessible.
- **Must pass:** `pnpm install && pnpm build`, `cargo build -p nugget-desktop` on Linux (the system libraries are installed), and `cargo clippy -p nugget-desktop -- -D warnings`.

## WP-E: CI, repo hygiene, bench harness (owns `.github/`, `bench/`, the root dotfiles listed here, `CONTRIBUTING.md`, `SECURITY.md`, `deny.toml`, `rustfmt.toml`, `.editorconfig`)

- **`ci.yml`** runs on a matrix of `ubuntu-latest`, `macos-latest` and `windows-latest`:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`, excluding the desktop crate on runners where its system libraries aren't installed
  - `cargo test --workspace`
  - a desktop job that installs the Tauri system dependencies, then runs `pnpm build` and `cargo build -p nugget-desktop`
  - `cargo deny check`, which needs `deny.toml` to allow only Apache-2.0-compatible licenses (deny GPL, AGPL and LGPL for linked code)
  - rust-cache and concurrency groups
- **`dependabot.yml`**, plus PR and issue templates.
- **`CONTRIBUTING.md`**: DCO sign-off, the clean-room rule, scoped build commands, and the expected crate layout. **`SECURITY.md`**: how to report issues, and the threat-model outline.
- **`bench/`** (`nugget-bench`): a CLI harness with a `Benchmark` trait, a JSON report schema (`bench/schema/report.schema.json`), and suites registered as `memory`, `perf`, `draft` and `cua`. In M0, the `perf` and `memory` suites run real micro-benchmarks against an in-crate fake, and `draft` and `cua` are declared as "not yet implemented" with documented plans. Reports go to `bench/results/<suite>-<timestamp>.json`.

## WP-F: `nuggetd` (wave 2, after A–D land; owns `crates/nuggetd/`)
- Loads `config.toml` (providers, tiers, model selections) and creates default config on first run.
- Opens the store, builds the router, starts the IPC server, and implements every `nugget_core::ipc::method`.
- Uses `tracing` with JSON logs to a file under `Paths::data`.
- Ships user-level service install and uninstall for launchd, a Windows scheduled task and a systemd user unit.
- Handles graceful shutdown.
- End-to-end test: start the daemon on a temp `NUGGET_HOME`, connect with the client, then check status, chat (against a wiremock provider), search and the egress list.

## Contract notes after wave 1
- **IPC results.** Every method's result type is listed in the doc comment on `nugget_core::ipc::method`. `chat.cancel` and `models.install` return `null`; progress for those calls arrives as notifications.
- **Long-running handlers.** `nugget-ipc` drops a connection's in-flight handler futures when the client disconnects. Work that must outlive the call (chat streams, model installs) is `tokio::spawn`ed with a cloned `Notifier`. The spawned task stops when `Notifier::send` returns `Disconnected`.
- **Running `nuggetd`.** Started with no arguments, `nuggetd` runs in the foreground and binds with `nugget_ipc::bind(&Paths)`. If `bind` returns `AddrInUse`, a daemon is already running: exit 0 with a message.
- **Router.** A provider that hasn't been probed only serves plain chat. Probe results are persisted by `nuggetd`, under the store's `meta` table or the config dir, and loaded at start into `Router::set_capabilities`.
- **Keychain naming.** Use `nugget_core::secrets` for all keychain entries: the service is `dev.nugget.Nugget`, and provider keys use the account `provider:NAME`.
- **New error variants.** `Error::{Network, RateLimited, InsufficientSpace}` exist; use them instead of `Provider`/`Other`/`Storage` strings.
