# Screenshots

The PNGs here are taken from the real app by `apps/desktop/e2e/screenshots.mjs`: the real `nuggetd`
indexes the fictional corpus in `bench/fixtures/knowledge/items`, and the real desktop frontend runs
in headless Chromium against it. Every shot exists twice, `<name>-light.png` and `<name>-dark.png`
(`prefers-color-scheme`), at 1280×800. Don't edit them by hand; regenerate them.

## Refreshing them

After a UI change:

```sh
cd apps/desktop
pnpm run screenshots    # about a minute once the daemon and dist/ are built
git diff --stat ../../docs/book/src/images/screenshots
```

Look at the changed images before committing them. A run deletes and rewrites the whole set, so it
is safe to repeat. `SHOTS_ONLY=chat,graph pnpm run screenshots` retakes only those shots. The
variables are listed at the top of the script.

No network is used. The Chat shot's answer comes from a fake OpenAI-compatible server on
`127.0.0.1` that the script starts and configures as the `local` provider; its wording is fixed in
the script, and its citations point at the passages the daemon really retrieved. The Today page
depends on the date of the run (the daemon reads the real clock), and the Status page shows the
daemon's uptime, so those two change on every run.

## The set

| Name | Page | Used in |
|---|---|---|
| `welcome` | Setup, first step | `guide/first-run.md` |
| `status` | Status | `guide/first-run.md` |
| `today` | Today, after a card refresh | `guide/today.md`, `README.md` |
| `search` | Search for "Tidewater budget" | `guide/search-ask.md`, `README.md` |
| `chat` | Chat, a cited answer | `guide/search-ask.md`, `README.md` |
| `knowledge` | Knowledge, Ada Wexford | `guide/knowledge.md` |
| `knowledge-why` | Knowledge, a fact's evidence (Based on N sources) | `guide/knowledge.md`, `README.md` |
| `graph` | Knowledge, the graph view | `guide/knowledge.md` |
| `sources` | Sources | `guide/sources.md` |
| `safety` | Safety | `guide/safety.md` |
| `double-tap` | Double Tap settings | `guide/double-tap.md` |

To add one, add an entry to `SHOTS` in the script and embed it with a `<picture>` element, like the
guide pages do (Markdown image links would fail `check-refs.py`, which treats every link as a page).
