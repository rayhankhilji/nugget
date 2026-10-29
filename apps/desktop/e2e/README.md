# Desktop end-to-end test (real daemon)

`pnpm run e2e` runs the real `nuggetd` on the fictional corpus in
`bench/fixtures/knowledge/items` and drives the real desktop frontend (`dist/`) in headless
Chromium against it. Nothing is mocked except the Tauri shell itself, and it makes no network calls
besides localhost. The vitest suites in `../tests` cover the UI against a mocked daemon; this test
covers the wire between the two.

It runs three scenarios:

- **pages**: a home with the corpus already indexed; every page is opened and checked.
- **i18n**: on the same indexed home (indexed once, shared with pages), every page is opened in
  the pseudo-locale, German, Japanese and Arabic and checked for text that doesn't fit.
- **first-run**: its own empty home; the test goes through onboarding like a new user, connects the
  corpus folder, and times app start to a cited answer on Search. It fails above a budget
  (2 minutes by default), the promise of "useful search in under two minutes from install".

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
(a no-op when up to date) unless `NUGGETD` names a binary. A run takes about 35 s once both are
built, plus about 60 s for the i18n scenario. Linux and macOS only, because the bridge speaks to the daemon's Unix socket.

| Variable | Default | Meaning |
| --- | --- | --- |
| `NUGGETD` | build with cargo | Path to a built `nuggetd` (skips the cargo build). |
| `CARGO_TARGET_DIR` | `<repo>/target` | Where the cargo build looks for the binary. |
| `E2E_ARTIFACTS` | `e2e/artifacts/` | Output directory, wiped at the start of each run. |
| `E2E_BUILD=1` | off | Rebuild the frontend even when `dist/` exists (it may be stale). |
| `E2E_KEEP=1` | off | Keep the daemon's temp home (database, vault, logs) after the run. |
| `E2E_INDEX_TIMEOUT_MS` | 180000 | How long indexing and extraction may take. |
| `E2E_STEP_TIMEOUT_MS` | 15000 | How long each UI expectation waits. |
| `E2E_SCENARIOS` | `pages,i18n,first-run` | Which scenarios to run, comma-separated. |
| `E2E_I18N_LOCALES` | `en-XA,de,ja,ar` | i18n: which locales to check, comma-separated (any code in `LOCALES`). |
| `E2E_FIRST_RUN_BUDGET_MS` | 120000 | First run: the most app start to a cited answer may take. |
| `E2E_FIRST_RUN_NO_CONFIG=1` | off | First run: write no `config.toml` at all (binds MCP HTTP on its fixed port 7319 and starts the local runtime). |
| `E2E_CHROMIUM` | Playwright's | Path to a Chromium binary to launch instead of Playwright's download. |

## What happens: pages

1. `daemon.mjs` creates a temp home with a fixed `config/config.toml` (local runtime off, MCP HTTP
   off, `[knowledge.me]` set to the corpus owner Robin Ashgrove), starts
   `NUGGET_KEYCHAIN=off nuggetd --home <tmp>`, waits for the `nuggetd ready` log line (it names the
   socket), adds the corpus as a `files` source with `sources.add`, and polls `status` and
   `knowledge.status` until there are more than 50 entities, no pending items and the counts are
   stable. Extraction is deterministic (no LLM): 93 items, 74 entities and 199 facts.
2. `bridge.mjs` serves `dist/` on a free localhost port and answers `POST /invoke {cmd, args}` like
   `src-tauri/src/commands.rs`: `knowledge_status` becomes the daemon method `knowledge.status`,
   `{ request }` args are sent as the params, other args are snake-cased. Commands the shell answers
   itself (connection, pickers, chat history, updates, Double Tap, overlay) return fixed stubs
   (`startBridge({ pickFolders })` sets what the folder picker "chooses"; by default nothing).
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
| Knowledge | at least 60 entities; Ada Wexford shows facts; "Why?" shows a quoted source titled by its calendar SUMMARY (`cal-011.ics`: "Wren Mobile usability test"); the Graph tab draws a canvas with nodes |
| Sources | the `files-…` source lists more than 0 items |
| Safety | the built-in rules table has rows |
| Agent | the empty task history |
| Plugins, Today | the empty plugin list and the morning brief, without an error state |
| wire | no daemon command the UI called was "method not found" or "invalid params" |
| vault | the daemon's vault has an `Ada Wexford` page with `[^1]` footnote citations |

UI strings come from `src/i18n/locales/en.json`, so copy changes don't break the test.
There is no screenshot diffing.

## What happens: first run

`first-run.mjs` starts the clock, starts `nuggetd` on an empty temp home (the only config is
`[runtime] enabled = false` and `[mcp] http = false`, so CI starts no model runtime and binds no
fixed port; there is no `[knowledge.me]`, which onboarding doesn't ask for), and opens the app
without any saved onboarding state. Then, through the real UI:

| Mark | What the test does |
| --- | --- |
| `daemon_ready` | the daemon answers on its socket |
| `app_open` | the window shows onboarding's Welcome step (not preset: nothing is in `localStorage`) |
| `ai_step` | Continue; "Decide later" on Choose your AI; Continue |
| `source_added` | "Add a folder" opens the folder form; "Choose folders…" (the bridge's picker returns the corpus folder); "Add folder" |
| `source_searchable` | the step's live progress goes from indexing to "93 items indexed. Search works now." |
| `setup_done` | Continue through Permissions and Double Tap; Done offers "Search your sources", which opens Search |
| `first_results` | "Tidewater Billing vendor contract": a "Tidewater Billing" hit with the answer (Vellum & Thistle) in the top three |
| `cited_answer` | Ask with context, Find context: numbered citations ([1] …) with source titles, "Tidewater Billing" among them, and the answer in the cited passages |

Checks: the flow completes with no error notes or page errors; `cited_answer` is within
`E2E_FIRST_RUN_BUDGET_MS` of the clock start; every daemon command the UI called exists.
`summary.json` has `first_run` with `total_ms`, `from_app_open_ms`, `marks` (ms since the clock
start), `steps` (ms since the previous mark), the indexing states the user saw, the top titles
and the citations; the log prints the same breakdown.

No AI is set up: onboarding can't configure a provider by itself (it shows a config snippet to
paste), and a model's streamed answer needs Tauri events the bridge doesn't deliver. So the
"cited answer" is the context an answer or a draft is grounded on, not model output. The test
clicks as fast as the UI allows, so the time is the software's share of the two minutes, not a
person's reading time.

## What happens: i18n

`i18n.mjs` checks that translations fit the layout. For each locale in `E2E_I18N_LOCALES`
(default `en-XA`, the pseudo-locale: English accented and about 35% longer, built by
`src/i18n/pseudo.ts`; `de`, long compound words; `ja`, no spaces to break at; `ar`, right to
left) and each page of the `PAGES` registry, it sets `localStorage` `nugget.locale` (plus the
finished onboarding and `nugget.page`), waits for `<html lang>` to switch and the page to settle,
writes `i18n-<locale>-<page>.png`, and measures every visible labelled element: buttons, links,
nav items, headings, labels, `legend`, `summary`, `th`, `dt`, `select`, tabs, options, menu
items, badges, chips, pills and anything else with an `aria-label` and text (landmarks and
lists excepted). An element is an offender when, by more than 1px:

| Kind | Meaning |
| --- | --- |
| `clipped` | it, or an element inside it, clips (`overflow: hidden`/`clip`, which ellipsis needs) and its content is wider or taller than its box |
| `escapes` | a `white-space: nowrap` element inside it is wider than its nearest block container |
| `spills` | it, a box inside it, or its nearest block/flex/grid ancestor doesn't clip and its content is wider than it (a hint pushed out of its button, a button row past its card) |
| `page-scrolls` | the window scrolls sideways |

Scroll containers (`overflow: auto`/`scroll`) end the search: scrolling, as in a wide table's
wrapper, is intended. Places where overflow is intended are allow-listed by selector in `ALLOW`
in `i18n.mjs`, each with its reason (the graph canvas, `pre`/`code`, chat history titles and
palette labels cut with an ellipsis on purpose, screen-reader-only text, form fields). Each
locale and page is one check that fails with its offenders; in Arabic it also checks
`<html dir="rtl">`. `summary.json` has `i18n.offenders` (`{ locale, page, kind, path, text,
scrollWidth, clientWidth, … }`), also written to `i18n-offenders.json`.

To fix an offender, let the text wrap (`min-width: 0` on flex items, `flex-wrap: wrap`,
`overflow-wrap: anywhere`, no fixed widths); cut it with `text-overflow: ellipsis` and a `title`
only where truncation is acceptable, and then add the selector to `ALLOW` with the reason.

## Artifacts

`e2e/artifacts/` (git-ignored; CI uploads it as `desktop-e2e`): a numbered screenshot per page and
per step (`FAIL-<page>.png` when a page fails; `first-run-*.png` for each onboarding step, `i18n-<locale>-<page>.png` per locale and page),
`summary.json` (checks with timings, every command that returned an error, console errors per page,
the commands the UI called, `i18n` and `first_run`), `daemon-logs/`, `nuggetd.out.log`, the first run's
`first-run-daemon-logs/` and `first-run-nuggetd.out.log`, and the rendered `vault-Ada-Wexford.md`.

`harness.mjs` holds what both scenarios share: settings from the environment, the log, `t(key)`
for UI strings, `check`, `until`, `shot` and the browser context setup.

## Adding a check

Add a function for the page id to `pageChecks()` in `run.mjs`. Find elements by role and
accessible name, wait with `until(what, probe)` (it polls and fails with what it last saw), and
call `shot(page, name)` for an extra screenshot. Assert on corpus facts, not on dates: the Today
page depends on the current date.

To poke at the app by hand: `E2E_KEEP=1 pnpm run e2e`, start the kept home again with
`NUGGET_KEYCHAIN=off nuggetd --home <home>`, run `node e2e/bridge.mjs <socket> 8765` (the socket
is in the `nuggetd ready` line of `<home>/data/logs`) and open `http://127.0.0.1:8765/index.html`.
Standalone, the bridge inlines `tauri-shim.js` into `index.html`.
