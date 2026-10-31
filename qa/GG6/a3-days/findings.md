# GG6 area A3: days 2–7 resilience (daemon level)

Build: debug `nuggetd`, `nuggetd-e2e`, `nuggetd-fake-llama-server` at 62b538f (s1 and s3 also on 5f1998e, same results).
Perf numbers are from debug builds: read them as trends. Run each script with `node qa/GG6/a3-days/<script>` from the
repo root (debug binaries from `cargo build -p nuggetd --features fake-runtime --bins`; s4 needs root for tmpfs; s5
uses `unshare -rn`).

## Results

| # | Scenario | Result | Numbers |
|---|---|---|---|
| 1 | Clean restart (`s1-restart.mjs 3000`) | Pass | first index of 3,000 files 11.2 s; ready 214 ms after restart with all items; re-scan 1.5 s, 0 upserts; 6/6 searches identical |
| 1b | SIGKILL mid-index (same script) | Pass | killed at 929/3,000; complete 10.6 s after restart; 0 duplicates, 0 missing in a 60-item sample; knowledge equal to a clean run; audit chain OK |
| 2 | SIGSTOP 180 s with file changes, then SIGCONT (`s2-sleep.mjs`) | Pass | new/modified/deleted file and a new 20-file subtree all searchable 0.8 s after SIGCONT |
| 2b | Wall-clock jump | not run (no faketime); code reading only | F9 |
| 3a | kill -9 llama-server while idle (`s3-model-crash.mjs`) | Pass | `failed` at 2 ms, `starting` 1.33 s, `ready` 1.43 s; next Ask works |
| 3b | kill -9 mid-answer | Partial | raw transport error after 1.5 s, no retry (F4) |
| 3c | hung llama-server (SIGSTOP) | Fail | no reply for >200 s while `runtime.status` says `ready` (F2) |
| 3d | crash loop (server exits 1) | Partial | backoff +2.1/6.3/14.5/30.6 s then gives up; a request blocks 31 s and gets a 400+ char self-nested error (F5); works 174 ms after the fix |
| 4 | Disk full, home on a 30 MB tmpfs (`s4-disk-full.mjs`) | Partial | no crash, no corruption; stuck at 597/4,000 with `syncing` and no error for ~23 s, then `error`; recovers 59 s after space is freed (F6) |
| 5 | Offline, empty network namespace (`s5-offline.mjs`) | Partial | search and indexing work; cloud Ask/chat raw errors in ~2.2 s; model install fails after 98 s (F7) |
| 6 | Big folder, no model (`s6-big-folder.mjs 20000`): 20,001 files 15 deep, 1.6 GB incl. a 500 MB .log, nine 100 MB binaries, a symlink loop, edits during indexing | Pass | settled in 62 s; peak RSS 285 MB; DB 65 MB + 6 MB WAL; search p50 3 ms, max 7 ms while indexing; progress `total` always null (F10) |
| 7 | Idle soak, 2,000 items (`s7-soak.mjs`), 30 min | Pass | CPU 0.03–0.13%; RSS 166→186→174 MB; DB, WAL, log, threads, fds flat; scheduled work: 4 knowledge runs, 1 voice build, 1 brief |
| L1 | Default config's Ollama provider after restart (`s8-default-config-restart.mjs`) | partly reproduced | F3 |
| L2 | Slow indexing with a model (`s9-slow-index.mjs 5000 ABCD`) | reproduced, root cause found | F1 |

## Findings

**A3-F1 (Blocker). With a local model installed, indexing runs at LLM speed: one serialized triage call per file.**
- Repro: `node qa/GG6/a3-days/s9-slow-index.mjs 5000 ABCD` (1/1). A: no big files, no model: 5,000 items in 17.1 s. B: plus a 200 MB .log, 200 MB .txt and 50 MB .bin, no model: 17.1 s (big files are not the bottleneck). C: B plus a local model whose llama-server is frozen: **8 items in 240 s**, one every 30 s, source `syncing`, `queue_depth` 66, nothing logged at warn. D: B plus an instant fake model: 25.1 s.
- Cause: ingest stage 4 calls the Triage-tier chat model inline before each item is stored, one at a time, with a 30 s timeout. At qwen3-4b CPU speed (~10–15 s per 6,000-char prompt) that is ~240–360 items/h, matching the real-OS Linux run (25 → 244 items in 60 min for a 20k-file folder).
- Owning code: `crates/nugget-ingest/src/pipeline.rs:489-516`; `crates/nugget-ingest/src/config.rs:87-89` (`triage: true`, 30 s, 6,000 chars); failures logged at debug only (`pipeline.rs:512`).
- Fix: store and index first, triage in a background queue (like knowledge extraction) or skip triage during backfills; report pending triage in `ingest.status`; warn on timeouts.

**A3-F2 (High). A hung local model server blocks Ask/chat for up to 10 min while status says `ready`.**
- Repro: `s3-model-crash.mjs` step "hung" (2/2). Client: `retrieve.ask: not done within 200000 ms`; `runtime.status` `{"state":"ready","error":null}` throughout.
- Cause: the supervisor only checks for process exit (`crates/nugget-runtime/src/supervisor.rs:1253`); `provider_config` (`supervisor.rs:1071-1117`) sets no `idle_timeout_secs`, so reads wait 600 s (`crates/nugget-infer/src/provider.rs:174`) with 2 retries (the 10 min is from code reading).
- Fix: a short first-token timeout for `local-*`; poll `/health` while ready and restart after N failures; say "model server stopped responding".

**A3-F3 (High). After a restart, the first chat goes to the default config's dead Ollama provider.**
- Repro: `s8-default-config-restart.mjs` (no config.toml; install the fake model; chat; restart; chat) (1/1). Before any model: `chat failed: local: connection failed: error sending request for url (http://127.0.0.1:11434/v1/chat/completions)` in 1.97 s. After restart: `nuggetd ready … "providers":1`; the first chat retries `127.0.0.1:11434` twice, then falls back to `local-chat` (1,804 ms vs 3 ms for the next). With a real model still `starting`, the fallback fails outright (the real-OS Linux run).
- Owning code: `crates/nuggetd/src/config.rs:98-105` (default config enables Ollama); `crates/nuggetd/src/local.rs:378-388` (`local-chat` registered only on activation).
- Fix: comment out the Ollama provider in the default config (or probe and mark unavailable); register `local-chat` at startup, ahead of `local`.

**A3-F4 (Medium). A model crash mid-answer gives a raw error and no retry.** `s3-model-crash.mjs` mid-answer (4/4):
`retrieve.ask failed: local-chat: request failed: error sending request for url (http://localhost/v1/chat/completions): client error (SendRequest) (code -32003)`.
Fix: retry once when nothing has streamed yet; a human message. (`crates/nugget-infer/src/http.rs`, Ask/chat handlers; code reading.)

**A3-F5 (Medium). A crash loop blocks a request 31 s and returns a self-nested 400+ char error.** `s3-model-crash.mjs`
crash-loop (2/2): `llama-server exited during start-up (exit status: 1); last output: simulated crash | start failed: provider error: llama-server exited during start-up … | start failed: …`, also in `status.providers[].error`. Cause:
`crates/nugget-runtime/src/health.rs:161` embeds the runtime log tail, which holds the supervisor's own `start failed:`
notes (`supervisor.rs:1204`); `wait_ready_state` (`supervisor.rs:684-724`) waits out the whole backoff. Fix: separate the
server's stderr from supervisor notes; a short message; fail fast while backing off.

**A3-F6 (Medium). Disk full: no daemon-wide signal, the error appears ~23 s late, and the log loses the cause.**
`s4-disk-full.mjs 30` (1/1): 0 bytes free from ~11 s; until 34 s `sources.list` showed `syncing`, `last_error: null`,
`health.code: "ok"` at 597 items, then `error` "storage error: write cursor: database or disk is full"; `status` has no
field for it; the log ends mid-line at 4,096 bytes. Good: no crash, no corruption, self-recovery within 59 s, 4,000 items
after restart. Fix: `storage_full` in `status` and source health; pause ingest; keep a space reserve for logs.

**A3-F7 (Medium). Offline: raw errors, a 98 s model-install failure, no Ask fallback.** `s5-offline.mjs` (1/1):
`models.install` stays at 0 bytes for 97.9 s (8 retries), then `network error: download failed: error sending request
for url (https://huggingface.co/…)` (`crates/nugget-models/src/download.rs:563-580`); Ask with only a cloud provider:
`anthropic: connection failed: … client error (Connect) (code -32003)` and no passages although search works. Fix:
classify DNS/connect failures as "You're offline" and fail fast; Ask returns cited passages when no model is reachable.

**A3-F8 (Low). "WAL torn tail (a crash)" after every clean restart; WAL not truncated on stop.** 2/2 clean restarts
(`grep "torn tail"` in the home's logs); a 4.5 MB WAL remains. `crates/nugget-store/src/integrity.rs:262-268` treats
old-salt frames after a WAL reset as a crash. Fix: ignore them; `wal_checkpoint(TRUNCATE)` on shutdown.

**A3-F9 (Low, code reading). Schedules use the monotonic clock, which pauses in system sleep:** proactive refresh
(`crates/nuggetd/src/features/proactive.rs:870-889`), knowledge `interval_minutes` (`crates/nuggetd/src/knowledge.rs:192-196`),
runtime idle stop (`supervisor.rs:1289-1303`), the 1 h rescan. The morning brief and reminders re-read the wall clock
(`proactive.rs:895-917`). Fix: reconcile on resume or when wall and monotonic gaps diverge.

**A3-F10 (Low). No progress total; skipped files are invisible.** All 446 `ingest.progress` events had `total: null`;
a 60 MB .txt (over the 50 MiB cap) and `.log` files were skipped with health `ok`, `item_errors: 0`, nothing logged
(`crates/nugget-connector-files/src/config.rs:12`, `scan.rs`). Fix: count during the scan; report "N skipped (too large / unsupported)".

## Friction log

| Moment | Who |
|---|---|
| Installed the local model and added Documents; after an hour search finds almost nothing; Status says "syncing" (F1) | most |
| After a reboot the first question fails with an error about 127.0.0.1:11434; never installed Ollama (F3) | most local-model users |
| Chat hangs after the laptop wakes while Status says Ready (F2) | few |
| Answer dies with "client error (SendRequest)"; asking again works (F4) | few |
| A 30 s wait, then a wall of "start failed: … start failed: …" (F5) | few |
| On a plane, Download model sits at 0% for 1.5 min; Ask gives a connection error although search works (F7) | some |
| Disk full: indexing stops at 597, no explanation, nothing in the logs (F6) | few |
| No percentage on a big first index (F10) | most with big folders |

## What works well

- Store crash safety: SIGKILL mid-backfill lost nothing, no duplicates, knowledge identical to a clean run; a full disk did not corrupt the database.
- Restarts: ready in ~0.2 s; a 3,000-file re-scan does 0 upserts in 1.5 s; identical search results.
- File watcher: survives a 3-minute freeze and catches up in 0.8 s; handles edits, deletes and renames during a 20k backfill.
- llama-server supervisor: restarts a crashed server in 1.4 s; exponential backoff with a give-up; the next request after a fix works.
- Big messy folder (no model): 15-level tree and a symlink loop are harmless; binaries and oversized files skipped cheaply; search p95 6 ms during a 20k backfill.
- Idle: ~0.05% CPU; DB, WAL, fds and threads flat over 30–45 min.
- Offline basics: indexing and search work; connector errors are specific; attempts are in the egress ledger.
