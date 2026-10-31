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
