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

## Layout

- `run.mjs`: the scenario table, the shared indexed home and the pages checks.
- `harness.mjs`: `check`/`until`/`shot`, UI strings from `en.json` (`t`, `tPattern`), `newContext`,
  `checkWire` (every daemon command the UI called exists and took its params), `stopDaemon`.
- `daemon.mjs`: temp homes, `startDaemon`, `rpc`/`rpcStream`, indexing the corpus (also used by
  `crates/nugget-mcp/compat`).
- `bridge.mjs`: serves `dist/`, forwards `POST /invoke` to the daemon like `src-tauri/src/commands.rs`,
  stubs shell-only commands, and with `deliverEvents(page)` turns daemon notifications into the
  shell's Tauri events. `tauri-shim.js` is the page side.
- `screenshots.mjs` (`pnpm run screenshots`): refreshes `docs/book/src/images/screenshots/`; it
  checks nothing.

To add a check, put it in the scenario that already has the state it needs, look strings up with
`t()` rather than hard-coding English, and wait with `until()`. To poke at the app by hand: `E2E_KEEP=1
pnpm run e2e`, restart the kept home with `NUGGET_KEYCHAIN=off nuggetd --home <home>`, and run
`node e2e/bridge.mjs <socket> 8765`.
