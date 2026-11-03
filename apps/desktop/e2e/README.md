# Desktop end-to-end test (real daemon)

`pnpm run e2e` runs the real `nuggetd` on the fictional corpus in `bench/fixtures/knowledge/items`
and drives the real frontend (`dist/`) in headless Chromium against it. Only the Tauri shell is
replaced, by `bridge.mjs`; nothing leaves localhost. The vitest suites in `../tests` cover the UI
against a mocked daemon; this covers the wire between the two.

```
Chromium (dist/ + tauri-shim.js) --POST /invoke--> bridge.mjs --JSON-RPC over the Unix socket--> nuggetd
```

## Scenarios

Indexed home (indexed once, shared): **pages** (every page of `PAGES` in `src/App.tsx` opens with
real data and no errors; Knowledge's "Why?" quotes a source; the vault page has footnotes),
**i18n** (every page in en-XA, de, ja and ar: no label cut off or overflowing, RTL in ar),
**a11y** (axe-core over every page, the main dialogs, onboarding, quick ask and the Double Tap
overlay, light and dark; serious/critical fails; plus `a11y-tree.mjs`), **prefs** (reduced motion,
more contrast, forced colours, 200% zoom) and **search** (filters, saved searches, preview,
timeline).

Own home: **first-run** (onboarding to a cited "Find context" answer), **ai-setup** (installs the
recommended model from a mock download server with a throwaway signed manifest, then a cited Chat
answer and a `draft.begin` from the fake llama-server), **byok** and **byok-keys** (API keys against
a mock OpenAI-compatible server: errors, secret store only, replace and remove; GG6-7/11/15),
**keyboard** (core flows with the keyboard alone: focus rings, no traps, dialogs return focus),
**backup** (back up, export, wipe, restore with the passphrase), **messaging** (WhatsApp export
folder, watched) and **sources-setup** (onboarding regressions GG6-2/12/13/14/16/21/31).

Opt-in (not in the default run): **cloud**, Heed Cloud end to end against a running self-hosted
stack with fake Composio, Anthropic, OpenAI and Stripe. The cloud commands run in the real shell
client (`cloud-shell.mjs` spawns `crates/nugget-cloud-client/examples/e2e_shell.rs`, which shares
its command code and validators with `src-tauri`), nuggetd gets the session's model token as the
shell hands it over, and the scenario plays the browser (Mailpit's magic link, the consent and
Checkout pages). Covered: magic-link sign-in, connecting Gmail and Calendar with the sync pill and
progress, the brief's author citation chips and cards, actions proposed from cards (edit with E,
approve with the Undo hold, exactly one Composio execute with the edited body, a stale digest
refused, snooze with H, reject with Backspace), a cited quick ask and ghost text through `/llm/v1`,
plan/trial/usage with the Checkout and portal URLs (or the self-hosted plan with billing off), and
deleting the account by typing the email, after which none of the user's rows are left in Postgres
(every RLS table with a `user_id`, and the waitlist). Run it with one command, which starts all of
that: `selfhost/e2e.sh` (see the self-host guide). Against a stack you started yourself, set the
`E2E_CLOUD_*` variables below and run `E2E_SCENARIOS=cloud node e2e/run.mjs`; without
`E2E_CLOUD_URL` it skips itself.

## Running

```sh
cd apps/desktop
pnpm install
pnpm exec playwright install chromium       # once (CI adds --with-deps)
pnpm run e2e
```

It builds `dist/` when missing and runs `cargo build --locked -p nuggetd` unless `NUGGETD` is set;
ai-setup and keyboard also build `nuggetd-e2e` and `nuggetd-fake-llama-server` (`--features
fake-runtime --bins`) unless `NUGGETD_E2E` and `NUGGET_FAKE_LLAMA_SERVER` are set. Linux and macOS
only (the bridge speaks to the daemon's Unix socket).

| Variable | Default | Meaning |
| --- | --- | --- |
| `NUGGETD`, `NUGGETD_E2E`, `NUGGET_FAKE_LLAMA_SERVER` | build with cargo | Prebuilt binaries. |
| `E2E_SCENARIOS` | all | Comma-separated scenarios to run. |
| `E2E_ARTIFACTS` | `e2e/artifacts/` | Output (screenshots, `summary.json`, daemon logs); wiped per run. |
| `E2E_BUILD=1` | off | Rebuild `dist/` even when it exists. |
| `E2E_KEEP=1` | off | Keep the daemons' temp homes. |
| `E2E_INDEX_TIMEOUT_MS`, `E2E_STEP_TIMEOUT_MS` | 180000, 15000 | Indexing and per-step waits. |
| `E2E_I18N_LOCALES` | `en-XA,de,ja,ar` | i18n locales. |
| `E2E_CHROMIUM` | Playwright's | A Chromium binary to launch instead. |
| `E2E_CLOUD_URL`, `E2E_CLOUD_ANON_KEY` | unset (cloud skips) | The stack's gateway and its anon key. |
| `E2E_CLOUD_SHELL` | unset | The built `e2e_shell` example of nugget-cloud-client. |
| `E2E_MAILPIT_URL`, `E2E_FAKE_URL` | `:8025`, `127.0.0.1:9100` | Mailpit, and the fake providers (`fake_providers` example). |
| `E2E_CLOUD_PSQL` | unset | A command that runs SQL from stdin as postgres in the stack's database. |
| `E2E_CLOUD_BILLING` | `on` | `off` when the server runs with `HEED_BILLING=off`. |

## Layout

- `run.mjs`: the scenario table, the shared indexed home and the pages checks.
- `harness.mjs`: `check`/`until`/`shot`, UI strings from `en.json` (`t`, `tPattern`), `newContext`,
  `checkWire` (every daemon command the UI called exists and took its params), `stopDaemon`.
- `daemon.mjs`: temp homes, `startDaemon`, `rpc`/`rpcStream`, indexing the corpus (also used by
  `crates/nugget-mcp/compat`).
- `bridge.mjs`: serves `dist/`, forwards `POST /invoke` to the daemon like `src-tauri/src/commands.rs`,
  stubs shell-only commands, and with `deliverEvents(page)` turns daemon notifications into the
  shell's Tauri events. `tauri-shim.js` is the page side.
- `cloud.mjs`, `cloud-shell.mjs`: the cloud scenario and the shell's cloud half (above).
- `screenshots.mjs` (`pnpm run screenshots`): refreshes `docs/book/src/images/screenshots/`; it
  checks nothing.

To add a check, put it in the scenario that already has the state it needs, look strings up with
`t()` rather than hard-coding English, and wait with `until()`. To poke at the app by hand: `E2E_KEEP=1
pnpm run e2e`, restart the kept home with `HEED_KEYCHAIN=off nuggetd --home <home>`, and run
`node e2e/bridge.mjs <socket> 8765`.
