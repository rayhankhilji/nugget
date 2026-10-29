# Desktop end-to-end test (real daemon)

`pnpm run e2e` runs the real `nuggetd` on the fictional corpus in
`bench/fixtures/knowledge/items` and drives the real desktop frontend (`dist/`) in headless
Chromium against it. Nothing is mocked except the Tauri shell itself, and it makes no network calls
besides localhost. The vitest suites in `../tests` cover the UI against a mocked daemon; this test
covers the wire between the two.

```
Chromium (dist/ + tauri-shim.js) --POST /invoke--> bridge.mjs --JSON-RPC over the Unix socket--> nuggetd
```

## Running

```sh
cd apps/desktop
pnpm install                                # once; brings in the pinned `playwright`
pnpm exec playwright install chromium       # once (CI adds --with-deps for the system libraries)
pnpm run e2e
```

It builds the frontend first when `dist/` is missing, and runs `cargo build --locked -p nuggetd`
(a no-op when up to date) unless `NUGGETD` names a binary. A run takes about 20 s once both are
built. Linux and macOS only, because the bridge speaks to the daemon's Unix socket.

| Variable | Default | Meaning |
| --- | --- | --- |
| `NUGGETD` | build with cargo | Path to a built `nuggetd` (skips the cargo build). |
| `CARGO_TARGET_DIR` | `<repo>/target` | Where the cargo build looks for the binary. |
| `E2E_ARTIFACTS` | `e2e/artifacts/` | Output directory, wiped at the start of each run. |
| `E2E_BUILD=1` | off | Rebuild the frontend even when `dist/` exists (it may be stale). |
| `E2E_KEEP=1` | off | Keep the daemon's temp home (database, vault, logs) after the run. |
| `E2E_INDEX_TIMEOUT_MS` | 180000 | How long indexing and extraction may take. |
| `E2E_STEP_TIMEOUT_MS` | 15000 | How long each UI expectation waits. |

## What happens

1. `daemon.mjs` creates a temp home with a fixed `config/config.toml` (local runtime off, MCP HTTP
   off, `[knowledge.me]` set to the corpus owner Robin Ashgrove), starts
   `NUGGET_KEYCHAIN=off nuggetd --home <tmp>`, waits for the `nuggetd ready` log line (it names the
   socket), adds the corpus as a `files` source with `sources.add`, and polls `status` and
   `knowledge.status` until there are more than 50 entities, no pending items and the counts are
   stable. Extraction is deterministic (no LLM): 93 items, 74 entities and 199 facts.
2. `bridge.mjs` serves `dist/` on a free localhost port and answers `POST /invoke {cmd, args}` like
   `src-tauri/src/commands.rs`: `knowledge_status` becomes the daemon method `knowledge.status`,
   `{ request }` args are sent as the params, other args are snake-cased. Commands the shell answers
   itself (connection, pickers, chat history, updates, Double Tap, overlay) return fixed stubs.
   Errors use the shell's `{ kind, message, code }` shape.
3. `tauri-shim.js` is injected before the app loads; it provides `window.__TAURI_INTERNALS__`
   with an `invoke` that posts to the bridge. Onboarding is marked finished in `localStorage`
   (`nugget.onboarding.v1`) and each page is opened through `nugget.page`.
4. `run.mjs` opens every page of the `PAGES` registry in `src/App.tsx` (parsed from the source, so
   new pages are covered automatically), screenshots it, and checks:

| Where | Check |
| --- | --- |
| every page | the page and its heading render, no uncaught page errors, no error notes (`.note-error`, `role=alert`) |
| Status | the Items card shows 93 (the number of files in the corpus) |
| Search | "Tidewater budget": the first hit mentions Tidewater and a "Tidewater…" title is in the top three |
| Knowledge | at least 60 entities; Ada Wexford shows facts; "Why?" shows a quoted source from `cal-011`; the Graph tab draws a canvas with nodes |
| Sources | the `files-…` source lists more than 0 items |
| Safety | the built-in rules table has rows |
| Agent | the empty task history |
| Plugins, Today | the empty plugin list and the morning brief, without an error state |
| wire | no daemon command the UI called was "method not found" or "invalid params" |
| vault | the daemon's vault has an `Ada Wexford` page with `[^1]` footnote citations |

UI strings come from `src/i18n/locales/en.json`, so copy changes don't break the test.
There is no screenshot diffing.

## Artifacts

`e2e/artifacts/` (git-ignored; CI uploads it as `desktop-e2e`): a numbered screenshot per page and
per step (`FAIL-<page>.png` when a page fails), `summary.json` (checks with timings, every command
that returned an error, console errors per page, the commands the UI called), `daemon-logs/`,
`nuggetd.out.log` and the rendered `vault-Ada-Wexford.md`.

## Adding a check

Add a function for the page id to `pageChecks()` in `run.mjs`. Find elements by role and
accessible name, wait with `until(what, probe)` (it polls and fails with what it last saw), and
call `shot(page, name)` for an extra screenshot. Assert on corpus facts, not on dates: the Today
page depends on the current date.

To poke at the app by hand: `E2E_KEEP=1 pnpm run e2e`, start the kept home again with
`NUGGET_KEYCHAIN=off nuggetd --home <home>`, run `node e2e/bridge.mjs <socket> 8765` (the socket
is in the `nuggetd ready` line of `<home>/data/logs`) and open `http://127.0.0.1:8765/index.html`.
Standalone, the bridge inlines `tauri-shim.js` into `index.html`.
