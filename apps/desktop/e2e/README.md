# Desktop end-to-end test (real daemon)

`pnpm run e2e` runs the real `nuggetd` on the fictional corpus in
`bench/fixtures/knowledge/items` and drives the real desktop frontend (`dist/`) in headless
Chromium against it. Nothing is mocked except the Tauri shell itself, and it makes no network calls
besides localhost. The vitest suites in `../tests` cover the UI against a mocked daemon; this test
covers the wire between the two.

It runs five scenarios:

- **pages**: a home with the corpus already indexed; every page is opened and checked.
- **i18n**: on the same indexed home (indexed once, shared with pages), every page is opened in
  the pseudo-locale, German, Japanese and Arabic and checked for text that doesn't fit.
- **first-run**: its own empty home; the test goes through onboarding like a new user, connects the
  corpus folder, and times app start to a cited answer on Search. It fails above a budget
  (2 minutes by default), the promise of "useful search in under two minutes from install".
- **ai-setup**: the same first run, but choosing "On this device" in onboarding: the recommended
  model is installed from a local mock download server (a test manifest signed with a throwaway
  key), served by the repo's fake llama-server, and Chat answers with `[n]` citations streamed by
  it. Times app start to the first cited model answer; also drafts through `draft.begin`.
- **byok**: onboarding's "Bring your own key" form against a mock OpenAI-compatible server on
  localhost: bad key, wrong URL and no network are reported, a good key is saved to the secret
  store (never to `config.toml`) and the egress ledger shows the checks.
- **a11y**: on the indexed home, axe-core audits every page and the main dialogs in light and dark
  and fails on serious or critical violations; each page's accessibility tree is checked too (see
  "What happens: a11y").
- **prefs**: on the indexed home, display preferences on every page: with reduced motion nothing
  animates (no infinite or longer-than-instant animation or transition, spinners still but
  visible) and the knowledge graph draws once and stops, even after a node is dragged (a control
  run without the preference sees both move); with more contrast and with forced colours the
  pages render and the overrides in `src/styles-a11y.css` apply (screenshots `prefs-contrast-*`,
  `prefs-forced-*`); at 200% zoom (a 680×430 viewport at device scale 2) neither the window nor
  `main` scrolls sideways and i18n.mjs's `measure` finds no label overflowing
  (`zoom200-<page>.png`, `prefs-zoom-offenders.json`).

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
(a no-op when up to date) unless `NUGGETD` names a binary. ai-setup also needs `nuggetd-e2e` and
`nuggetd-fake-llama-server`: it runs `cargo build --locked -p nuggetd --features fake-runtime --bins`
unless `NUGGETD_E2E` and `NUGGET_FAKE_LLAMA_SERVER` name them (CI builds that way once and sets all three). A run takes about 35 s once both are
built, plus about 60 s for the i18n scenario. Linux and macOS only, because the bridge speaks to the daemon's Unix socket.

| Variable | Default | Meaning |
| --- | --- | --- |
| `NUGGETD` | build with cargo | Path to a built `nuggetd` (skips the cargo build). |
| `NUGGETD_E2E`, `NUGGET_FAKE_LLAMA_SERVER` | build with cargo | ai-setup: paths to `nuggetd-e2e` and `nuggetd-fake-llama-server` (both, to skip the build). |
| `CARGO_TARGET_DIR` | `<repo>/target` | Where the cargo build looks for the binary. |
| `E2E_ARTIFACTS` | `e2e/artifacts/` | Output directory, wiped at the start of each run. |
| `E2E_BUILD=1` | off | Rebuild the frontend even when `dist/` exists (it may be stale). |
| `E2E_KEEP=1` | off | Keep the daemon's temp home (database, vault, logs) after the run. |
| `E2E_INDEX_TIMEOUT_MS` | 180000 | How long indexing and extraction may take. |
| `E2E_STEP_TIMEOUT_MS` | 15000 | How long each UI expectation waits. |
| `E2E_SCENARIOS` | `pages,i18n,first-run,ai-setup,byok` | Which scenarios to run, comma-separated. |
| `E2E_I18N_LOCALES` | `en-XA,de,ja,ar` | i18n: which locales to check, comma-separated (any code in `LOCALES`). |
| `E2E_FIRST_RUN_BUDGET_MS` | 120000 | First run: the most app start to a cited answer may take. |
| `E2E_AI_BUDGET_MS` | 180000 | ai-setup: the most app start to a cited model answer may take. |
| `E2E_MODEL_BYTES`, `E2E_MODEL_BYTES_PER_SEC` | 4 MiB, 1.5 MiB/s | ai-setup: size of the fake model and the mock server's speed (progress shows for size / speed seconds). |
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

No AI is set up ("Decide later"), so the "cited answer" is the context an answer or a draft is
grounded on, not model output; the ai-setup scenario covers the path with a local model and adds
its time as `first_run.time_to_cited_model_answer_ms`. The test
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

## What happens: a11y

`a11y.mjs` injects axe-core (a devDependency, MPL-2.0) into the page and runs it with the tags
`wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa` and `best-practice`, colour contrast included. The app
follows the system theme, so each check runs with Playwright's emulated `prefers-color-scheme`,
light and then dark. One check per theme and:

| What | Audited |
| --- | --- |
| every page of the `PAGES` registry | the whole document; Search again with results, Knowledge with Ada Wexford's facts open and on the Graph tab, Connected apps with its connect dialog; in light also `treeChecks` from `a11y-tree.mjs` (landmarks, headings, names) |
| Add source | the kind step and the folder step, opened from Sources |
| Merge and Split entity | from Ada Wexford on Knowledge: Merge as it opens and with a candidate picked (preview shown), Split with a fact ticked |
| a Today card | its sources (expanded) and its action's review dialog (a dry-run preview) |
| command palette | Ctrl+K, empty and filtered |
| onboarding | every step walked with Continue, the AI step with each choice, and the folder form from the source step |

Dialogs are audited alone (`dialog[open]`, the page behind hidden), so page violations aren't
counted twice. Before each run the viewport grows to the tallest scroll content, so text below the
fold is measured instead of reported as obscured. Today's card comes from the corpus by date (an
unanswered mail); when the daemon has none, `cards_list` answers with one fixed card for the
sources check and the action dialog is skipped. A first check plants a faint paragraph and an
unnamed button and expects axe to report both, so a broken injection can't pass silently.

Every finding, any impact, is written to `a11y-violations.json` (`{ check, theme, scope, id,
impact, nodes }`); what axe could not decide is recorded with impact `needs-review` and never
fails. `summary.json` has `a11y` with the counts by rule. Accepted violations go in `ALLOW` in
`a11y.mjs` (rule id and selector) with the reason; it is empty.

## Events

`bridge.mjs` keeps one connection to the daemon open, like the shell, and turns its notifications
into the shell's Tauri events (`chat.delta` becomes `nugget:chat-delta`, and so on, plus
`nugget:notification`). A scenario calls `bridge.deliverEvents(page)` to have them dispatched to the
page's listeners (`window.__E2E_EMIT__` in `tauri-shim.js`, through `page.evaluate`, in order; no
long-lived request, so `networkidle` still works). ai-setup and byok do; pages and first-run don't
need them. `bridge.notifications` records every notification seen.

## What happens: ai-setup

Before the clock starts, the scenario builds or locates the binaries, generates an ed25519 key pair
with `node:crypto` (`generateKeyPairSync("ed25519")`), writes a manifest (version 1) with two chat
models (`e2e-chat-q4`: 4 MiB of deterministic bytes starting with `GGUF`, `min_ram_gb = 4`;
`e2e-chat-big-q8`: 64 GB, never recommended, never served) whose URLs are
`http://127.0.0.1:<port>/<40-hex revision>/<file>.gguf`, signs its exact bytes and starts the
throttled mock download server (Range support, 1.5 MiB/s). Then it starts `nuggetd-e2e` with
`[runtime] enabled = true`, `idle_stop_minutes = 0`, `[mcp] http = false` and:

| Variable | Value |
| --- | --- |
| `NUGGET_E2E_MANIFEST`, `NUGGET_E2E_MANIFEST_SIG` | the manifest and its hex signature |
| `NUGGET_E2E_MANIFEST_PUBKEY` | the throwaway public key (hex); the only key `nuggetd-e2e` trusts, and it refuses to start if the manifest doesn't verify |
| `NUGGET_E2E_LLAMA_SERVER` | `nuggetd-fake-llama-server` (run as a CPU llama-server) |
| `NUGGET_E2E_RAM_GB`, `NUGGET_E2E_GPU` | `16`, `unknown`: the machine the recommendation sees |

`nuggetd-e2e` (`crates/nuggetd/src/bin/nuggetd-e2e.rs`, built only with the `fake-runtime`
feature) is `nuggetd`'s default command with those `RuntimeOverrides`; production `nuggetd` reads
none of them. The fake llama-server answers a prompt holding numbered passages (a line starting
`[1] `) with `According to your notes, "<first words of passage 1>" [1].` in several stream chunks.

| Mark | What the test does |
| --- | --- |
| `daemon_ready`, `app_open` | as in the first run |
| `install_clicked` | "On this device"; the AI step lists `e2e-chat-q4` with the "Recommended" badge and not the big model; Install |
| `model_installed` | the card's `<progress>` showed a value between 0 and 100 with "Downloading · n% · …" at least once, then the "Installed" badge; the mock server sent the whole file |
| `ai_step`, `source_searchable`, `setup_done` | Continue; the corpus folder as in the first run; through to Done |
| `cited_model_answer` | Status lists `e2e-chat-q4` as the chat runtime (`local-chat`); Chat with "Use my sources" on asks "Tidewater Billing vendor contract": the reply from `local-chat` arrives in several `chat.delta`s, starts with "According to your notes", its `[n]` markers render as citation buttons, every marker is in the sources list and "Tidewater Billing" is a cited source |

Then: the cited answer is within `E2E_AI_BUDGET_MS`; Status shows the runtime "Ready"; `draft.begin`
(a mail reply context, over its own socket connection that collects the `chat.delta`/`chat.done`
notifications) produces a non-empty draft from `local-chat` without an error; `egress.recent` has a
`model_download` row for 127.0.0.1; no unknown daemon commands and no page errors. `summary.json`
has it as `first_run_ai` (`time_to_cited_model_answer_ms`, `marks`, `steps`, the progress states
seen, download requests, the answer, citations and the draft), and `first_run` gets
`time_to_cited_model_answer_ms` too; the log ends with both first-run times.

## What happens: byok

A mock OpenAI-compatible server on 127.0.0.1 answers `GET /v1/models` with
`{ object: "list", data: [{ id: "mock-model" }] }` only for `Authorization: Bearer sk-e2e-good`
(401 otherwise, 404 on other paths). The plain `nuggetd` runs with the first-run config. In
onboarding the test chooses "Bring your own key", selects the OpenAI-compatible kind in the form
(labels `onboarding.byok.form.kind`, `.baseUrl`, `.model`, `.key`, `.name`; buttons `.test`,
`.save`; the daemon's `ai.key.test` / `ai.key.save`) and checks:

| Case | Base URL, key | Expected note |
| --- | --- | --- |
| bad key | `http://127.0.0.1:<mock>/v1`, `sk-e2e-bad`, Test | `onboarding.byok.form.error.bad_key` (role=alert) |
| wrong URL | `http://127.0.0.1:<mock>/nope`, good key, Test | `onboarding.byok.form.error.wrong_url` |
| no network | a closed localhost port, good key, Test | `onboarding.byok.form.error.no_network` |
| good key | `http://127.0.0.1:<mock>/v1`, good key, Save | `onboarding.byok.form.saved` (role=status) |

Then `config.toml` has a `[[providers]]` table with `api_key = "keychain:…"` and neither it nor the
daemon logs contain the key; `egress.recent` and the Privacy page show requests to 127.0.0.1.

## Artifacts

`e2e/artifacts/` (git-ignored; CI uploads it as `desktop-e2e`): a numbered screenshot per page and
per step (`FAIL-<page>.png` when a page fails; `first-run-*.png` for each onboarding step, `i18n-<locale>-<page>.png` per locale and page),
`summary.json` (checks with timings, every command that returned an error, console errors per page,
the commands the UI called, `i18n`, `first_run`, `first_run_ai` and `byok`), `daemon-logs/`, `nuggetd.out.log`, the first run's
`first-run-daemon-logs/` and `first-run-nuggetd.out.log`, and the rendered `vault-Ada-Wexford.md`; also `ai-setup-*.png`, `ai-setup-daemon-logs/`,
`ai-setup-nuggetd.out.log`, `byok-*.png`, `byok-daemon-logs/` and `byok-config.toml` (with the key masked, if it ever leaked).
The a11y scenario adds `a11y-<theme>-<check>.png` and `a11y-violations.json`.

`harness.mjs` holds what the scenarios share: settings from the environment, the log, `t(key)`
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
