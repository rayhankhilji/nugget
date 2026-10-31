# GG6 verification layer

Each finding was re-run by a fresh agent from its written repro only, on the current build, with no access to the
explorer's reasoning. Verdicts and quoted evidence below; the report (`docs/qa/GG6.md`) marks each finding accordingly.

## Verifier 1: A1, A2 and the default-provider finding (build 74b7fdf = integration ec35d39 + QA files)

Unmodified copies of the scripts, run from a mirror checkout (so results did not overwrite the committed ones).

| ID | Verdict | Evidence (quoted) |
|---|---|---|
| A1-F1 | reproduced 4/4 | `waiting after 25877ms: locked: getting ready to index…`; source `{"state":"watching","items":0,"last_error":null,"health":{"code":"ok",…,"reason":"Up to date."}}`; 200 MB folder `waiting after 91033ms`; Done: `Every step is done.` |
| A1-F2 | reproduced | `{"sources":[{"id":"files-1","name":"resume-src","items":40},{"id":"files-2","name":"resume-src","items":40}]}`; search `{"items":80,"hits":["n7","n7",…]}` |
| A1-F3 | reproduced 3/3 | back: `{"before":[{"phase":"ready","text":"back-small: 1 item indexed. Search works now."}],"after":[],"btnLabel":"Add a folder"}` |
| A1-F4 | reproduced | `no provider is available for the synthesis tier`, red box, only "Ask again". On a real first run the text is `local model qwen3-4b-q4km is not installed` instead (see the default-provider row) |
| A1-F6 | reproduced | after "Decide later": `Nothing set up here yet: Continue skips this step.`; Done: `You skipped one step. Go back to: Choose your AI` |
| A2-F1 | reproduced | `{"ok":false,"reason":"exists","message":"A provider named \"custom\" is already set up. Pick another name."}`; only `ai.key.save`, `ai.key.saved`, `ai.key.test` exist in `crates/nuggetd/src` |
| A2-F2 | partly | IPC: `ai.key.save failed: keychain: Platform secure storage failure: no secret service provider or dbus session found (code -32004)`; the form was not driven in that state (code: `ByokForm.tsx:138-139,252` renders the error as is) |
| A2-F4 | reproduced | `"Key checked and saved. Restart Nugget to start using it.","restart_required":true`; `providers.list` right after: `[]` |
| A2-F6 | reproduced 4/4 | offline 100.5 s → `network error: download failed: error sending request for url (http://127.0.0.1:41195/<rev>/qa-offline.gguf)`; HTTP 500 99.7 s; HTML page 104.4 s → `connection closed after 53 of 8388608 bytes`; server down at 50% → failure reports `"downloaded":0` |
| A2-F7 | reproduced | `chat failed: llama-server exited during start-up (exit status: 1); last output: starting qa-chat on /tmp/nugget-run-…/chat.sock (ctx 1x8192, ngl 0, accel cpu) \| fake: exiting immediately with 1 (code -32003)`; `starting qa-chat` 5 times in the UI alert; no Retry |
| A2-F8 | reproduced | `4 GB unknown: nomic-embed-text-v1.5-q8` (same for apple_silicon, nvidia); only a "Search model" section under "On this device [Recommended]" |
| GG6 default provider | reproduced | fresh home, no config: `config.toml` has `[[providers]] id = "local" … base_url = "http://127.0.0.1:11434/v1" model = "qwen3:4b"`; `providers.list` lists it with `"error":null`; log `"retrying","provider":"local",…11434…` twice, `ask: provider failed; trying next`; client gets `{"code":-32002,"message":"local model qwen3-4b-q4km is not installed"}` after 1908 ms; `nuggetd ready … "providers":1,"unavailable":0` |

## Verifier 2: A3 (build 6050a7e = integration ec35d39 + QA files)

| ID | Verdict | Evidence (quoted) | Cause check |
|---|---|---|---|
| A3-F1 | reproduced (variants A, C, 1,000 files) | A: `{"items":1000,"queue":0,"states":"files-1:watching:1000","ms":5022}`; C (frozen model): `{"items":8,"queue":66,"states":"files-1:syncing:8","ms":240881,"timeout":true}`, no warn lines; +1 item every ~30 s | one worker per source awaits upsert → prepare → inline `ai::triage` (`pipeline.rs:489-516`), 30 s / 6,000 chars (`config.rs:87-89`), failure at debug only (`:512`) |
| A3-F2 | reproduced (wait cut to 90 s) | `retrieve.ask: not done within 90000 ms; … 0 notification(s)`; `runtime.status` `{"state":"ready","error":null}` in all 5 samples | watch loop only `try_wait()`s for exit (`supervisor.rs:~1253-1285`); no `idle_timeout_secs` (`:1071-1117`) → 600 s default (`provider.rs:174`), 2 retries (the 10 min is code reading) |
| A3-F3 | reproduced | before any model: `chat failed: local: connection failed: … 127.0.0.1:11434 … (code -32003)` in 2269 ms; after restart the first chat retries `local` twice then succeeds on `local-chat` in 2340 ms; the next takes 3 ms | correction: `local-chat` is listed at startup; what waits for activation is its routing capabilities (`local.rs:386` `router.set_capabilities`), so it is not `eligible()` until then |
| A3-F5 | reproduced with a targeted run (the explorer's script has a timing bug: its 5 s sleep is shorter than the backoff) | Ask blocked **32,216 ms**, then a **2,007-char** error with 5 nested `start failed`; `status.providers[].error` 1,988 chars; launches at 1.3/3.4/7.6/15.7/31.9 s; after give-up every request starts a new launch with no backoff (~53 launches in ~9 s during a knowledge run) | `health.rs:159-161` puts `log.tail(5)` (which holds `supervisor.rs:1204`'s own notes) in the error; `wait_ready_state` (`:684-724`) waits out the backoff |
| A3-F8 | reproduced 4/4 | after every clean stop (exit 0): `"write-ahead log has an uncommitted or torn tail (a crash); SQLite rolls it back"`; WAL left ~4.2 MB; the trailing 432,600 bytes are exactly 105 old-salt frames, 0 partial frames | `integrity.rs:262-268`; `wal_checkpoint(TRUNCATE)` only in `vector.rs:1175`; logged at INFO |
| A3-F6 | not re-run (needs a root tmpfs) | | |
