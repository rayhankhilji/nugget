# QA JJ1: the wave II features and HH2 triage, as a real user

Branch `qa/wave-ii`. Brief: `docs/waves/wave-jj/JJ1.md`. The product was tested at efebc0c, and the
local repros were rerun at the latest integration head **44b9e11** (merged as b91dccd). Between the
two, none of the tested code changed (only the updater, desktop `daemon.rs`, settings and docs), and
every rerun gave the same result. Harness, fixtures and outputs: `qa/JJ1/`. All data is synthetic and
written for this package.

**How it was tested**
- **Daemon:** the real `nuggetd` over IPC (`qa/JJ1/harness/nq.py`).
- **Desktop:** the real debug `nugget-desktop` under Xvfb + openbox with AT-SPI. The target app is the
  GTK fixture, the provider a controllable mock (`qa/JJ1/overlay/`). The frontend also ran in Chromium
  through the e2e bridge (`qa/JJ1/search/desktop.mjs`).
- **Real models:** `qwen3-4b-q4km` and `nomic-embed-text-v1.5-q8` from the signed manifest, on
  GitHub `ubuntu-latest` (4 vCPU AMD EPYC 9V74), via `.github/workflows/qa-JJ1.yml`:
  - run 1, [41250457739](https://github.com/rayhankhilji/nugget/actions/runs/41250457739): refine and triage;
  - run 2, [41266509561](https://github.com/rayhankhilji/nugget/actions/runs/41266509561): messaging Ask and a repeat of refine.
- **Verification:** a second agent, which did not write the scenarios, reproduced every finding below
  from its written repro in two rounds. The second round was at 44b9e11. Findings that did not
  reproduce were dropped. Column **V** in the findings table: ✔ reproduced, ◐ partly (corrected),
  — explore-only (not re-run).

## Verdicts

| Capability | Verdict | Evidence |
|---|---|---|
| **Message imports (II1)**: WhatsApp, Telegram, iMessage parsing | **works with caveats** | 41/57 scenario checks, identical over 2 runs. iOS/Android en/de/es/ru, Telegram single and full exports, and iMessage (Apple epoch, `attributedBody`, WAL) are right. Unparseable locales are skipped silently (JJ1-5). Dates are wrong for day-first 12-hour locales (JJ1-6) and Telegram uses UTC (JJ1-12). |
| II1: private by default, kept from cloud models | **works** | Ask and Double Tap with a `cloud` provider: 0 chat canaries sent, `excluded_sensitive` 6 and 4. With `private: false` (plus `assume_unflagged_safe`) chats are sent, as chosen. A card/IBAN/API key re-marks an item either way (`qa/JJ1/messaging/cloud_exclusion.py`). |
| II1: Ask answers from chats with citations | **works** | 4B, 13 questions ("When does Sam's flight from Lisbon land?" → "18:40 on Friday [1]"): **13/13 correct, 13/13 cite the right chat month** (run 2). First token 36–63 s on CPU. |
| **Search (II2)**: facets, filters, pagination, preview, saved/recent | **works with caveats** | 27/36 scenarios, 372/383 checks. Facet counts, all 18 filter combinations and counts while an option is selected are exact. Paging has no gaps; 41 hostile queries are fine; a preview never shows more than the item. Saved and recent searches survive a daemon restart and a new browser context. The keyboard-only path works. Dates of zoned events (JJ1-10) and across DST (JJ1-11) are wrong. |
| II2: timeline of a person | **works with caveats** | In order, truncation keeps the newest, bounds are respected. Only items linked by headers, attendees or participants appear, not plain mentions (JJ1-24). |
| **Double Tap overlay (II3)**: edit, insert as is, retry, Esc, placement, practice field | **works** | Real app, 2 repeats: edit+Ctrl+Enter, Ctrl+Shift+Enter, R/S/L/F/C (one request per burst), Esc restores focus (`"Hello z"`), 6 edge/corner positions stay on screen with a 420 px max and scroll, the practice field touches no other app and stays out of history. |
| II3: losing focus while a draft is open | **broken** | The draft can go into a different field (JJ1-3). The overlay gets stuck and Double Tap stops responding (JJ1-2). |
| II3: short one-line drafts | **broken** | 3 of 4 complete one-line replies arrive as an empty draft that cannot be inserted (JJ1-4). |
| II3: refinements with the real 4B (en, de) | **broken** | 96 refinements over 2 runs: **21 returned the draft unchanged**; "Shorter" was shorter in 13/24; "More casual" (en) passed 3/12. A German chat was drafted and refined in English 4/4 (JJ1-7, JJ1-8). |
| II3: recent drafts | **works with caveats** | Card numbers in every format are dropped, practice drafts too. A spaced IBAN, a dashed SSN and API keys are kept and re-insertable (JJ1-9). |
| **Platform (II4)**: `nugget://` links, `.nuggetplugin`, single instance | **works** | Second launch hands over and exits in ~45 ms, 9/9 cases (`qa/JJ1/platform/out/single-instance.tsv`). 52 hostile links: credentials, ports, repeats, unknown pages and hosts, over 500 chars, RLO in ids, and a symlinked plugin file are all refused. Item and plugin links ask first. Ask links are typed, never sent. A link received during onboarding is applied after Skip setup. 4 invisible characters pass (JJ1-22). |
| II4: quick-ask window | **works** | Ctrl+Shift+Space opens focused, streams with citations and Sources, Esc returns focus, blur hides it, one window only. |
| II4: tray fallback, launch at login, notifications | **works** | Starts and runs with no StatusNotifier host. The autostart entry is written and removed with `--minimized`. Notifications carry fixed text only, are gated per kind with quiet hours (code review), and were not shown on a real desktop. **macOS/Windows not run** (time); Linux X11 only. |
| **Ingest triage (HH2)**: searchable within seconds | **works** | 2000 mails searchable in 5–8 s without models (local). 1200 mails with the 4B: first search at 5.2 s, full triage 74 min. |
| HH2: Ask during a large sync | **works with caveats** | 4 questions: 3/4 correct both during sync and idle. First token 17–27 s during sync vs 13–16 s idle (about 1.6×). |
| HH2: junk labelled, never hidden | **works with caveats** | 21/21 junk labelled, all still searchable, 0.7% of 1157 normal mails labelled junk. The label is invisible to the user (JJ1-19). |
| HH2: sensitive rules (health, money, credentials, legal; en + de/es/fr) | **broken** | Rules alone catch de 0/6, es 1/4, fr 1/4, en 6/8. After 4B triage only 9/22 stay sensitive: the model flagged credentials only, and its "not sensitive" cleared the rest for cloud use (JJ1-1). |

## Findings

Owners: II1 `nugget-connector-messaging`, II2 search UI + `features/search_ui`, II3 overlay
(`apps/desktop/src-tauri/src/doubletap/*`, `src/overlay/*`), II4 shell, HH2 `nugget-ingest`, EE2
`nugget-draft`, extraction `nugget-extract`, EE1 knowledge contacts.

| Id | Sev | Finding | Area (owner) | V |
|---|---|---|---|---|
| JJ1-1 | High | Model triage clears health, money and legal mail; rules miss non-English | HH2 | ✔ |
| JJ1-2 | High | Overlay stuck after the target regains focus; Double Tap stops responding | II3 | ◐ |
| JJ1-3 | High | Draft inserted into a different field than it was written for | II3 | ✔ |
| JJ1-4 | High | Complete one-line replies become an empty, un-insertable draft | EE2 (+II3) | ✔ |
| JJ1-5 | High | WhatsApp exports with an unreadable date line import nothing, status "Up to date" | II1 | ✔ |
| JJ1-6 | High | Day-first 12-hour WhatsApp exports dated wrong (future dates), duplicated on re-export | II1 | ✔ |
| JJ1-7 | High | German chat drafted (and refined) in English | EE2 | ✔ |
| JJ1-8 | Medium | Refinements often return the draft unchanged; "Shorter" and "More casual" unreliable with the 4B | II3/EE2 | ◐ |
| JJ1-9 | Medium | Draft history keeps IBANs, SSNs and API keys | II3 | ✔ |
| JJ1-10 | Medium | Events with `TZID` get the wrong instant | extraction / II2 | ✔ |
| JJ1-11 | Medium | Date filters and month counts use today's offset for every date (DST) | II2 | ✔ |
| JJ1-12 | Medium | Telegram times shown and split by month in UTC | II1 | ✔ |
| JJ1-13 | Medium | Phone-only participants never become people | EE1 knowledge / II1 | ✔ |
| JJ1-14 | Medium | One invalid UTF-8 iMessage row fails the whole source | II1 | ✔ |
| JJ1-15 | Medium | Telegram follows a symlinked export outside the chosen folder | II1 | ✔ |
| JJ1-16 | Low | Media placeholders in other languages kept as text | II1 | ✔ |
| JJ1-17 | Low | Telegram message edited between two exports shows twice | II1 | ✔ |
| JJ1-18 | Low | Refining while streaming rewrites only the visible part | II3 | ◐ |
| JJ1-19 | Low | The junk label is invisible to the user | HH2 / II2 | ✔ |
| JJ1-20 | Low | Preview's "Open in app" fails axe `label-content-name-mismatch` | II2 | ✔ |
| JJ1-21 | Low | `to=2026-02-30` in saved state reaches the daemon (-32602) | II2 | ✔ |
| JJ1-22 | Low | Link sanitiser lets U+061C, U+00AD, U+2060, U+2028 through | II4 | ✔ |
| JJ1-23 | Low | Autostart entry unusable under GLib when the path contains `%` | II4 | ◐ |
| JJ1-24 | Low | Smaller gaps (timeline mentions, date phrases, unreadable exports, docs) | II1/II2 | — |

### JJ1-1 · High · Model triage clears health, money and legal mail; the rules miss non-English (HH2)
- **Repro (rules):**
  ```
  qa/JJ1/triage/gen_corpus.py --out C --n 2000
  qa/JJ1/triage/rules_check.py --corpus C --home H --out r.json --assume-safe
  ```
  The corpus has 22 planted sensitive mails (health, money, credentials and legal; 8 en, 6 de, 4 es, 4 fr), written like real mail in a neutral `Inbox` folder.
- **Repro (model):** `realmodel.py --part triage` (run 1, job "triage"): 1200 mails, the 4B as the triage model, default config. Labels are read via `export.run` → `items.jsonl`.
- **Expected:** all 22 stay sensitive (kept from cloud models).
- **Actual:**
  - Rules: `sensitive caught 8/22`; en 6/8, **de 0/6**, es 1/4, fr 1/4; 0/27 false positives. Missed:
    - "Ihr Termin in der Praxis Dr. Kaya", "Kontoauszug Oktober", "Gehaltsabrechnung September"
    - "Zugangsdaten … Kennwort: Lindenblatt#77"
    - "Resultados de su análisis", "Votre relevé de compte", "Convocation au tribunal"
    - en: "mood journal" (therapy follow-up), the Wi-Fi passphrase
  - After triage: **9/22 sensitive**. The model said `sensitive: true` for 4 of the 5 credentials mails and for **0 of 18 health, money and legal mails** in any language, including English. Every unmatched item started sensitive (unknown counts as sensitive), and the model's `false` then cleared it (`triage.rs` `pii || t.sensitive`).
  - So a German bank statement, payslip, diagnosis or lawsuit becomes eligible for a cloud provider once triage runs.
- **Frequency:** rules deterministic (3 runs); model 1 run of 74 min (`qa/JJ1/results/run1-41250457739/triage.json`).
- **Fix direction:**
  - Don't let a model "not sensitive" verdict lift the fail-closed default by itself for categories the user expects protected. For example, require the model to name a non-sensitive category, or keep items that a cheap multilingual classifier flags.
  - Extend `sensitivity.rs` WORDS and PHRASES per language (Kontostand, Gehaltsabrechnung, Kennwort, Befund, Praxis, Klage, Amtsgericht, saldo, nómina, juzgado, relevé, solde, tribunal, …).
  - Triage one item per request, or with fewer items per batch, and measure per category.

### JJ1-2 · High · Overlay stuck after the target regains focus; Double Tap stops responding (II3)
- **Repro:** `NUGGET_E2E_DISPLAY=:117 qa/JJ1/overlay/run.sh --out O --only s9` (part 9c). Trigger, wait for `done`, then activate the target window (click or alt-tab).
- **Expected:** the overlay hides or cancels, or the next double tap brings it back.
- **Actual:**
  ```
  "esc_in_target_hides_overlay":false, "third_trigger_fired":1, "third_trigger_views":[], "overlay_still_visible":true
  ```
  The overlay stayed on top for more than 4 minutes. In the first full run this broke all 13 later scenarios. The only exit is to click the overlay, then press Esc.
- **Frequency:** 2/2 explore, 1/1 verify.
- **Cause:** the overlay is `focusable(false)`, so Esc in the target never reaches it, and `fire()` returns early while `busy()` (`overlay.rs:299`), which is true in Done. Ignoring the trigger while the overlay holds the keyboard is intended; the combination is the bug.
- **Fix direction:** cancel or hide on focus loss (as `quickask.rs` does), or make a trigger while busy refocus or restart the overlay.

### JJ1-3 · High · Draft inserted into a different field than it was written for (II3)
- **Repro:** `run.sh --only s9` (part 9a). Trigger in the composer, wait for done, click the fixture's Search entry (the overlay stays), click the overlay, press Enter.
- **Expected:** insert into the composer, or refuse.
- **Actual:** `"composer":"Hello ","search_entry":"Thanks, see you at noon in the lobby."`, phase `inserted`.
- **Frequency:** 3/3 explore, 1/1 verify.
- **Cause:** `Target` stores only the app (`flow.rs:55`). `insert()` refocuses the app, and `a11y.insert` writes into whatever element has focus.
- **Fix direction:** record the field's identity at trigger time (AX element, AT-SPI path, UIA runtime id); at insert, refocus it or refuse with a message.

### JJ1-4 · High · Complete one-line replies become an empty, un-insertable draft (EE2 `nugget-draft`, II3)
- **Repro:** `run.sh --only s10`. The mock streams `"Sure, I'll email you the slides tomorrow."`, `"Okay, I will reply to Sam today."` or `"Great, the new version looks good to me!"`. The verifier also confirmed it at unit level against `crates/nugget-draft/src/post.rs`.
- **Expected:** that text.
- **Actual:** `"overlay_done_text":""`. The overlay shows "Draft ready" with no text (`qa/JJ1/overlay/evidence/s10-empty-draft.png`), Enter does nothing, and only Esc exits.
- **Frequency:** 3 of 4 one-liners, every run. The control `"Here is the address: 12 Elm Street."` and `"Sure, see you tomorrow."` survive.
- **Cause:** `preamble_len` treats the whole reply as a preamble when it is one line of ≤ 120 chars, starts like a preamble, contains a meta word (email/reply/version) and ends in `.` or `!`.
- **Fix direction:**
  - Never strip a line unless real content follows it.
  - The overlay should treat an empty `done` as an error with Retry.

### JJ1-5 · High · WhatsApp exports with an unreadable date line import nothing, and the status says "Up to date" (II1)
- **Repro:** `qa/JJ1/messaging/run_all.py locales`, fixture styles `and_fr` (`14/03/2026 à 09:06 - Chloé: …`), `and_ar` (Arabic comma) and `and_ar_eastern` (Eastern Arabic digits). The exact device formats are taken from open parsers and are unverified; the silent skip is the point.
- **Expected:** imported, or the source reports which file it could not read.
- **Actual:** `search "Chloé"` → 0 hits, `items: 0`, health `{"code":"ok","reason":"Up to date."}`. The skip is logged at debug only.
- **Frequency:** 2/2 explore, 2/2 verify (also at 44b9e11).
- **Owner:** `whatsapp.rs::parse_stamp` (only `,` or a space between date and time; ASCII digits only) and the `Ok(None)` branch in `WhatsAppReader::read`.
- **Fix direction:**
  - Accept any short non-digit separator, and map Arabic-Indic digits.
  - Report an export with zero readable lines as an item error.

### JJ1-6 · High · Day-first 12-hour WhatsApp exports are dated wrong (into the future) and duplicated on re-export (II1)
- **Repro:** `run_all.py ambiguous`: Android en_IN `03/05/26, 9:00 pm - Priya: …`, with every day ≤ 12.
- **Expected:** 3 May 2026.
- **Actual:**
  - `"Priya — March 2026" / **Priya** · 2026-03-05 21:00`.
  - `11/06/26` → `2026-11-06 09:30`, five days after the test date.
  - A re-export with `25/06/26` next to the old export: `occurrences: 2` across "March" and "May".
- **Frequency:** 2/2 + verify.
- **Owner:** `whatsapp.rs::infer_day_first` (per file).
- **Fix direction:**
  - Infer the date order per conversation across all of its exports.
  - Reject an order that yields future dates.
  - Prefer day-first for lower-case `am/pm` with `dd/mm/yy`.

### JJ1-7 · High · A German chat is drafted and refined in English (EE2 `nugget-draft`)
- **Repro:** `realmodel.py --part refine`, context `de-chat-umzug`: app "Signal", a `text_field`, and the thread `Jonas: "Hey, kannst du mir am Samstag beim Umzug helfen? Wir fangen so um 10 an, Pizza gibt's danach 🍕"`.
- **Expected:** a German reply.
- **Actual:** `"Sure, I can help with the move on Saturday. Let me know if there's anything specific you need assistance with."`
- **Frequency:** 4/4 base drafts over 2 runs, and every refinement of them. German mails were answered in German 8/8.
- **Owner:** EE2 prompt and field kind for chat messages.
- **Fix direction:** state the thread's language explicitly in the chat prompt; check the reply language and retry once.

### JJ1-8 · Medium · Refinements often return the draft unchanged; "Shorter" and "More casual" are unreliable with the 4B (II3 refine request / EE2)
- **Repro:** `realmodel.py --part refine`, 6 contexts (3 en, 3 de) × 2 reps × 4 refinements per run. The requests are built exactly as `flow.rs refined_request` builds them (the draft as the field's selection plus the overlay's instruction).
- **Actual:**
  - Byte-identical to the base draft: 7/48 (run 1) and 14/48 (run 2). In run 1, 3 of the 7 came from a base draft that was itself a refusal (`"I'm sorry, but I can't complete the email…"`).
  - "Shorter" was shorter by word count in 6/12 and 7/12.
  - "More casual" (en) passed 3/6 and 0/6.
  - "Longer" (de) invented a fact: "…wenn wir uns dann in der Praxis treffen könnten".
- **Data:** `qa/JJ1/results/run*/refine-stats.json`.
- **Fix direction:**
  - Send a refinement as an explicit rewrite task (the draft as the text to rewrite, not as a selection in a field).
  - Treat identical output as a failure and retry or say so.
  - Check word count for Shorter and Longer.

### JJ1-9 · Medium · Draft history keeps IBANs, SSNs and API keys (II3)
- **Repro:** `qa/JJ1/overlay/history-probe` (`cargo run --offline` prints the table), plus end to end in `run.sh --only s6`.
- **Actual:** kept and re-insertable:
  - `Bitte überweise an DE89 3704 0044 0532 0130 00`
  - `My SSN is 123-45-6789`
  - `sk-live-…`
  - `AKIA…`

  The daemon's own PII scan flags all four. Cards are dropped in every format.
- **Owner:** `doubletap/history.rs looks_sensitive` (`mask_digits` only).
- **Fix direction:** use the same detector as `nugget_ingest::scan_pii`.

### JJ1-10 · Medium · An event with `TZID` gets the wrong instant (extraction `nugget-extract` ics, shown by II2)
- **Repro:** index one `.ics` with `DTSTART;TZID=America/New_York:20260415T200000`, then call `search.faceted`.
- **Expected:** `2026-04-16T00:00:00Z`.
- **Actual:** `"date":"2026-04-15T20:00:00Z"` in `search.faceted`, `search.preview` and `search.timeline`. A user at +120 filtering 16 April gets `[]`.
- **Frequency:** every time (efebc0c and 44b9e11).
- **Owner:** `nugget-extract/src/ics.rs` stores the time without its zone; `search_ui::logic::dates_of` drops the zone.
- **Fix direction:** convert to UTC at extraction.

### JJ1-11 · Medium · Date filters and month counts use today's UTC offset for every date (II2)
- **Repro:** a Berlin mail sent at 23:30 CET on 15 Jan.
  - Daemon: `search.faceted` with from = to = 2026-01-15: offset 60 → 1 hit, offset 120 (today, CEST) → 0.
  - UI: a New York user filtering 31 Jan misses a mail the list itself dates 31 Jan.
- **Owner:** `api.ts` `utcOffsetMinutes()` and `logic::day_range`/`month_of`. The daemon applies the offset it is given correctly; the contract has one offset per request.
- **Fix direction:** send an IANA zone, or instants for the bounds.

### JJ1-12 · Medium · Telegram times are shown and split by month in UTC (II1)
- **Repro:** `run_all.py tg_single`.
- **Actual:** local 09:12 (UTC+2) is shown as `**Sam Lee** · 2026-05-03 07:12`; 00:30 on 1 June is filed in "May 2026" as `2026-05-31 22:30`.
- **Owner:** `telegram.rs::message_time` uses `date_unixtime`, rendered in UTC. Old exports without unixtime are fine.
- **Fix direction:** use the export's local `date` for display and the month split, as WhatsApp does.

### JJ1-13 · Medium · Phone-only participants never become people (EE1 knowledge contacts / II1)
- **Repro:** `run_all.py people`; the WhatsApp sender is `+49 151 23456789`.
- **Actual:**
  - The hit has `people: []` and there is no entity.
  - The same happens for an iMessage handle with Contacts off.
  - Telegram's `contacts.list` phone numbers are ignored.
  - Contrary to the guide ("one person when the phone number matches").
- **Owner:** `nugget-knowledge/src/extract/contacts.rs` (`clean_person_name` → None for a number, so the contact is skipped).
- **Fix direction:** create a person named by the number with a trusted phone identifier. Also say in the guide that WhatsApp exports carry no numbers for saved contacts.

### JJ1-14 · Medium · One invalid UTF-8 iMessage row fails the whole source (II1)
- **Repro:** `run_all.py wal_and_bad`.
- **Actual:** `state: error`, `"invalid utf-8 sequence of 1 bytes from index 3"`, 0 items from any chat.
- **Owner:** `imessage.rs::read_conversations` (`text` read as a `String`).
- **Fix direction:** decode lossily, or skip the row as an item error.

### JJ1-15 · Medium · Telegram follows a symlinked export folder outside the chosen folder (II1)
- **Repro:** `run_all.py tg_symlink`.
- **Actual:** the outside export is imported (1 hit). WhatsApp correctly refuses the same layout.
- **Owner:** `telegram.rs::find_results` (`is_dir`/`is_file` follow links).
- **Fix direction:** reuse WhatsApp's `list_dir` containment check.

### Low
- **JJ1-16** (II1, `run_all.py locales`): media placeholders in other languages are mishandled.
  - Android `<Medien ausgeschlossen>` and `<Без медиафайлов>` stay as quoted text.
  - iOS Russian `‎изображение отсутствует` becomes an authorless system line.
  - The Russian "message deleted" notice is shown as a message.
  - Fix: treat any `<…>` Android line as media, and extend `OMITTED`/`DELETED`.
- **JJ1-17** (II1, `run_all.py tg`): a message edited between two Telegram exports shows twice (`"…lands at 18:40"`, `"…19:10 now"`) while both exports exist. Dedupe by message `id`.
- **JJ1-18** (II3, `run.sh --only s3`): S while streaming sends only the visible text (`"One two three"`) and cuts the stream. The guide's "rewrites the current one" may intend this. Suggest allowing refinements only once done, or saying so.
- **JJ1-19** (HH2/II2): `junk` appears in no `search`/`search.faceted` hit and in no UI. "Labelled, never hidden" leaves the label with no visible effect; show a "Likely junk" badge or facet.
- **JJ1-20** (II2): axe `label-content-name-mismatch` (serious) on the preview's "Open in app" button, whose accessible name is "Open {title}" (`Preview.tsx`). This fails WCAG 2.5.3 for speech input; start the name with the visible text.
- **JJ1-21** (II2): a saved or pasted state with `to=2026-02-30` passes `searchState.isDate` (`Date.parse` rolls the day over), and the daemon answers `-32602`. Validate the round trip.
- **JJ1-22** (II4, `qa/JJ1/platform/deeplink-probe` + `links.tsv`): `nugget://search?q=pay%D8%9Cme` → `Search { query: "pay\u{61c}me" }`, and likewise U+00AD, U+2060 and U+2028. `deeplink.rs is_invisible` covers only 200B–200F, 202A–202E, 2066–2069 and FEFF; use the Unicode `Default_Ignorable` and bidi sets.
- **JJ1-23** (II4): an install path containing `%` (written correctly as `%%`) makes `Gio.DesktopAppInfo` reject the autostart entry, so the app doesn't start at login on GLib desktops. Mostly GLib behaviour; avoid or warn on `%` in the path.
- **JJ1-24** (explore only):
  - The timeline misses items that only mention a person in their text.
  - `"quokka last month"` is silently ignored as a date phrase.
  - Refused or unreadable exports still say "Up to date".
  - `reference/connectors.md` lacks the messaging settings.
  - Escape doesn't close a timeline.

## What works well (protect these)
- **Messaging privacy:** chats are sensitive by default, and toggling `private` re-marks existing items both ways. With a cloud provider, Ask and Double Tap sent no chat text. Attachments are never opened; the live `chat.db` is never touched (a temp copy, removed after).
- **Messaging robustness:**
  - Re-exports of the same chat don't duplicate, and deleting the old export is safe.
  - A half-written `.txt` or a zip copied in two steps recovers.
  - A zip `../` entry and a WhatsApp symlink outside the folder are refused; a 300 MB entry is refused at about 600 MB peak memory.
  - A 40 MB chat month is split into 30 parts.
  - A new export is searchable in about 1 s; `sources.remove` purges.
- **Messaging Ask:** with a 4B on CPU, 13/13 correct with the right chat month cited.
- **Search:**
  - Exact facet counts, including counts while another option is selected; an unknown or merged person filter still works.
  - Inclusive end day; all-day events stay on their day; bad dates and offsets are refused cleanly.
  - Hostile queries (FTS syntax, NUL, RLO, 4 KB) work; previews are bounded to 20,000 chars and flagged truncated.
  - A removed source disappears everywhere.
  - Saved and recent searches persist and Clear sticks.
  - The full keyboard path works with focus restored and counts announced.
- **Overlay:** edit before insert, insert as is, retry and refinement keys with no double requests, and Esc that restores focus to the right field. On-screen placement at every edge with max height and scroll. A practice field that is truly sandboxed. Cards never enter history.
- **Platform:**
  - Strict link validation, with confirmation before opening an item or installing a plugin; Ask links never auto-send.
  - Single instance hands over in about 45 ms.
  - Links survive onboarding.
  - The quick-ask window has a focus model that is right (Esc and blur).
  - The tray has a fallback, and autostart toggles cleanly.
  - Notifications carry no reminder or agent text.
- **Triage pipeline:** indexing never waits for triage: 2000 mails searchable in seconds, junk never hidden. Ask stays correct during a sync.

## Reproduce
- **Daemon:** `qa/JJ1/{messaging,search}/run_all.py`, `triage/rules_check.py`, `messaging/cloud_exclusion.py`.
- **Desktop:** `qa/JJ1/overlay/run.sh`, `qa/JJ1/platform/{single_instance,tray}.sh` (Linux packages: `apps/desktop/e2e-native/README.md`).
- **Real models:** dispatch `qa-JJ1.yml`. Full outputs are in the run artifacts (kept 30 days); compact copies in `qa/JJ1/results/`.
