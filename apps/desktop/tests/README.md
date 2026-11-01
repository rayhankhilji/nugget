# Desktop UI tests

Headless vitest tests: React components in jsdom against a mocked daemon (`@tauri-apps/api/mocks`).
No browser, network or Rust. The real-app test is in `../e2e/README.md`.

- `src/**/*.test.ts(x)`: unit and component tests, next to the code.
- `tests/flows/*.test.tsx`: user flows through the full `<App/>`.
- `tests/*.test.tsx`: shell, palette, chat, keyboard and per-page state tests.
- `tests/harness.ts`: `setupHarness()`, `renderApp({ page, onboarding })`, the `daemon()` mock and
  role/text queries (`getByRole`, `findByText`, `within`, …) and user actions (`type`, `press`,
  `click`). Shared data lives in `src/test/fixtures.ts`.

```sh
pnpm test            # everything once
pnpm test:flows      # only tests/
pnpm typecheck       # tsc for src and tests
```

Every test file starts with `// @vitest-environment jsdom` (if it renders). Drive the UI as a user
does, find elements by role and accessible name, use `t(key)` for UI strings, and assert both
what the user sees and what reached the daemon (`d.callsTo(cmd)`). The daemon mock answers from
one-shot steps (`once`, `failOnce`, `hang`), then the persistent reply (`daemon({ cmd: reply })`,
`on`, `fail`), then `RESPONSES` in the fixtures; unknown commands reject with "method not found".
