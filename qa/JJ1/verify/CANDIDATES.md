# JJ1 candidate findings: written repros for the verification layer

Repo at the integration head efebc0c (+ QA-only commits on `qa/wave-ii`). Binaries:
`target/debug/nuggetd` (built with `--features fake-runtime --bins`), `target/debug/nugget-desktop`
(debug, `pnpm tauri build --debug --no-bundle`). IPC client: `qa/JJ1/harness/nq.py`. Each
candidate must be reproduced from this text alone; record the observed output.

## Messaging imports (II1)
- **C-M1** WhatsApp exports whose date line uses `à` (French Android `14/03/2026 à 09:06 - Chloé: …`),
  an Arabic comma `، `, or Eastern Arabic digits import nothing, and the source health still reads
  "Up to date". Repro: `python3 qa/JJ1/messaging/run_all.py locales` (fixture styles `and_fr`,
  `and_ar`, `and_ar_eastern` in `qa/JJ1/messaging/fx.py`). Check: `search "Chloé"` → 0 hits,
  `ingest.status` health `ok`. (The exact real-device formats are unverified; the silent skip is the point.)
- **C-M2** Day/month order guessed per file: an Android en_IN export `03/05/26, 9:00 pm - Priya: …`
  (all days ≤ 12) is dated 5 March 2026 and a `11/06/26` line lands on 2026-11-06 (a future date). A re-export
  with `25/06/26` next to the old export duplicates messages. Repro: `run_all.py ambiguous`.
- **C-M3** Telegram message times are rendered and split by month in UTC, not the export's local `date`.
  Repro: `run_all.py tg_single` (09:12 at UTC+2 shows as `07:12`; 00:30 on 1 June lands in "May 2026").
- **C-M4** A WhatsApp participant that is only a phone number (`+49 151 23456789`) becomes no person:
  `search.faceted` hit `people: []`, and the knowledge graph has no entity. Repro: `run_all.py people`.
- **C-M5** One iMessage row with invalid UTF-8 in `message.text` fails the whole source
  (`state: error`, "invalid utf-8 sequence"), and nothing is imported. Repro: `run_all.py wal_and_bad`.
- **C-M6** Media placeholders in other languages: iOS Russian `‎изображение отсутствует` becomes an
  authorless system line, and Android `<Medien ausgeschlossen>` / `<Без медиафайлов>` stay as message text.
  Repro: `run_all.py locales`; read the document text with `search.preview`.
- **C-M7** Telegram follows a symlinked export folder that points outside the chosen folder (WhatsApp does not).
  Repro: `run_all.py tg_symlink`.
- **C-M8** A Telegram message edited between two exports shows twice while both exports are in the folder.
  Repro: see `sc_telegram.py` (the edited-message scenario) via `run_all.py tg`.

## Search (II2)
- **C-S1** An event `DTSTART;TZID=America/New_York:20260415T200000` is reported as
  `2026-04-15T20:00:00Z` (should be 2026-04-16T00:00Z) in `search.faceted`, `search.preview` and
  `search.timeline`. Repro: `python3 qa/JJ1/search/run_all.py daemon`, scenario D2 (the corpus from
  `qa/JJ1/search/gen_corpus.py`), or index one .ics with that line via a `files` source and call `search.faceted`.
- **C-S2** Date filters and month counts use one UTC offset (today's) for every date, so across DST an
  item can fall outside its own day. Repro: D5 in `run_all.py daemon`: a Berlin mail sent 23:30 CET on 15 Jan,
  queried with `utc_offset_minutes: 120` (today's CEST) and from = to = 2026-01-15, is missing.
- **C-S5** A saved search with `to=2026-02-30` passes `searchState.isDate`, and the daemon then answers -32602.
  Repro: `node -e` importing `apps/desktop/src/searchState.ts` logic, or the desktop scenario in `qa/JJ1/search/desktop.mjs`.
- **C-S6** axe: `label-content-name-mismatch` (serious) on the preview's "Open in app" button, whose accessible
  name is "Open {title}" (`apps/desktop/src/components/search/Preview.tsx`). Repro: `node qa/JJ1/search/desktop.mjs`.

## Ingest triage (HH2)
- **C-T1** The deterministic sensitivity rules alone catch 8/22 planted sensitive mails: de 0/6, es 1/4,
  fr 1/4, en 6/8. Repro: `python3 qa/JJ1/triage/gen_corpus.py --out /tmp/v/tri --n 2000; python3
  qa/JJ1/triage/rules_check.py --corpus /tmp/v/tri --home /tmp/v/trih --out /tmp/v/r.json --assume-safe`.
- **C-T2** (real model, CI only) After the local 4B triage finishes, only 9/22 stay sensitive; the model flags only
  the 4 credentials mails, never health, money or legal. Evidence: run 41250457739, job "triage", artifact
  `qa-jj1-triage` (`triage.json` rows). Verify by reading the artifact JSON (`/tmp/jj1/art/triage/triage.json`
  if present) and confirming in `crates/nugget-ingest/src/triage.rs` that a model `sensitive:false` verdict clears an
  item that no rule marked (around the `pii || t.sensitive` logic).
- **C-T3** The junk label is never visible to the user: no `junk` field in `search` / `search.faceted` hits and no UI
  label. Repro: grep `junk` in `crates/nuggetd/src/features/search_ui.rs` and `apps/desktop/src` (non-test), and
  check a `search.faceted` hit's keys.

## Double Tap (II3 overlay / drafting)
- **C-D1** Draft history (`apps/desktop/src-tauri/src/doubletap/history.rs`, `looks_sensitive`) keeps drafts holding a
  spaced IBAN, a dashed SSN or API keys, all of which the daemon's PII scan flags. Repro:
  `cd qa/JJ1/overlay/history-probe && CARGO_TARGET_DIR=/home/user/nugget/target/jj1-probes cargo run --offline -q`.
- **C-D2** (real model, CI only) Refinements with the 4B: 7/48 came back byte-identical to the draft (4 of the 12
  "Shorter"); "Shorter" made the draft shorter in 5/12; a German Signal chat (`de-chat-umzug`) was drafted and refined in
  English (2/2 base drafts). Evidence: run 41250457739, job "refine", `/tmp/jj1/art/refine/refine.json`.

## Platform (II4)
- **C-P1** Some invisible/bidi characters pass the link sanitiser: U+061C, U+00AD, U+2060, U+2028 stay in a
  `nugget://search?q=` query. Repro: `cd qa/JJ1/platform/deeplink-probe && cargo build --offline -q &&
  ./target/debug/jj1-deeplink-probe < ../links.tsv | grep -E 'alm|softhyphen|wordjoiner|linesep'`.
