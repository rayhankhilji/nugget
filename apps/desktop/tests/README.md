# Desktop UI tests

Headless UI tests for the desktop app: React components run in jsdom against a mocked daemon
(the Tauri IPC is replaced by `@tauri-apps/api/mocks`). No browser, no network, no Rust.
For the real-app test (real `nuggetd`, headless Chromium, `pnpm run e2e`) see `../e2e/README.md`.

- `src/**/*.test.ts(x)`: unit and component tests, next to the code.
- `tests/flows/*.test.tsx`: user flows through the full `<App/>` (onboarding, search, sources,
  knowledge, status, the app shell).
- `tests/*.test.tsx`: shell, palette, chat and per-page state tests.
- `tests/harness.ts`: the helpers below. `src/test/render.tsx` and `src/test/fixtures.ts` are
  the lower-level pieces it builds on.

## Running

```sh
cd apps/desktop
npm test                         # everything once (CI=1 friendly); same as pnpm test
npm run test:watch               # re-run on change
npm run test:flows               # only tests/
npx vitest run tests/flows/search.test.tsx -t "retries"   # one file / one test
npm run typecheck                # tsc for src and tests
```

Every test file starts with `// @vitest-environment jsdom`.

## Writing a flow

```tsx
// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { click, daemon, errors, findByText, getByRole, press, renderApp, setupHarness, t, type, within } from "../harness";

setupHarness(); // DOM shims before each test; unmount, clear mocks and localStorage after

describe("search flow", () => {
  it("retries after an error", async () => {
    const d = daemon().failOnce("search", errors.rpc("index is locked"));
    await renderApp({ page: "search" }); // onboarding skipped, opens on Search

    const box = getByRole("searchbox", { name: t("search.query") });
    await type(box, "engine");
    await press(box, "Enter"); // submits the form
    await click(within(getByRole("alert")).getByRole("button", { name: t("common.retry") }));

    await findByText("Notes");
    expect(d.callsTo("search")).toEqual([
      { query: "engine", limit: 25 },
      { query: "engine", limit: 25 },
    ]);
  });
});
```

Guidelines:

- Drive the UI the way a user does (`type`, `press`, `click`, `selectOption`) and find things
  by role and accessible name (`getByRole("button", { name })`), not CSS classes. Use `t(key)`
  for UI strings so copy changes don't break tests; use fixture text for daemon data.
- Assert both what the user sees and what reached the daemon (`d.callsTo(cmd)` gives the exact
  arguments).
- Render the whole app with `renderApp({ page })` for flows; `render(<Component/>)` is fine for
  a focused component test. `renderApp({ onboarding: true })` starts at first-run setup.
- Waiting: interactions already settle React. For debounces and timers use
  `await waitFor(() => expect(...))` or `findBy*` (they poll until a timeout, 1.5 s by default).

## Mocking the daemon

`daemon(responses?)` installs a fresh mock and returns a `Daemon`. Commands answer, in order of
precedence, from one-shot steps, the persistent reply, then `RESPONSES` in
`src/test/fixtures.ts`. Unknown commands reject with JSON-RPC "method not found" (what an older
daemon does), so a page's "not available" path is the default for commands without a fixture.

```ts
const d = daemon({ search: hits });               // persistent reply (value or (args, n) => value)
d.on("status", (args, n) => (n === 1 ? a : b));  // compute per call; n counts calls from 1
d.once("sources_list", []).once("sources_list", more); // a sequence, then back to the default
d.fail("providers_probe", errors.rpc("timed out")); // always reject
d.failOnce("search");                             // reject the next call only
const p = d.hang("start_daemon");                 // hold the next call open…
await p.resolve({ connected: true, socket: "", error: null }); // …then settle it (or p.reject(err))
d.offline();                                      // connection_info reports disconnected
d.reset("search");                                // back to the fixture

d.callsTo("search");   // [{ query, limit }, ...] in call order
d.last("sources_sync"); // { request: { id: "files-1" } }
d.count("status");
d.commands;            // ["connection_info", "status", ...]
d.clearCalls();
```

Errors use the shape the Rust shell rejects with: `errors.rpc(message, code)`,
`errors.methodNotFound()`, `errors.notRunning()`, `errors.disconnected()`, or any
`{ kind, message, code? }`.

Events from the shell: `await emitEvent(EVENT.sourcesChanged, payload)` (names in
`EVENT` from `src/api.ts`), or `emitConnection({ connected, socket, error })`.

Shared data lives in `src/test/fixtures.ts`; add new fixtures there with small edits (other
tests use it too) and keep them out of `RESPONSES` unless every test should see them.

## Query helpers

`getByRole / getAllByRole / queryByRole / queryAllByRole / findByRole / findAllByRole` take
`{ name, level, root, hidden }`; `name` is a string (exact, whitespace-normalised) or RegExp.
`getByText / queryByText / findByText / queryAllByText` match the innermost element whose whole
text matches, optionally limited by `selector`. `within(el)` scopes any of them. The role and
name rules are a pragmatic subset of ARIA (explicit roles, common implicit roles, labels,
`aria-label(ledby)`, content); a failing `getByRole` lists what it did find.

Keyboard: `press(el, key, { ctrl, meta, shift, alt })` dispatches keydown/keyup; Enter in a form
field submits the form and Escape cancels an open `<dialog>`, as a browser would.

## Adding a flow

1. Create `tests/flows/<area>.test.tsx` with the jsdom pragma and `setupHarness()`.
2. Add any fixtures the flow needs to `src/test/fixtures.ts`.
3. One `it` per user goal: start from `renderApp`, act like a user, assert the UI and the
   daemon calls. Cover the unhappy paths too (daemon error, empty data, offline).
4. `npm test` must pass headless.
