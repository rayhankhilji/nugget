# GG1 area E: insertion, read-ahead, latency plumbing (explore layer)

Files: `qa/GG1/world/E-insert.json` (persona `ins-mara`, synthetic, reserved domains),
`qa/GG1/scenarios/E-insert.json` (31 scenarios, ids `ins-*`, area `insertion`),
`qa/GG1/probes/E-insert/` (Rust probe over `nugget-a11y` and `nugget-draft`, plus
`join_check.py`, which checks `result.inserted_field` and read-ahead outcomes against the scenario
keys `inserted_required_regex`, `inserted_forbidden_regex` and `read_ahead`; `check.py` ignores those keys).

## How to reproduce

```sh
cd /home/user/nugget
G=/home/user/gg1-target/release/gg1
$G draft --scenarios qa/GG1/scenarios/E-insert.json --out /tmp/e-off.jsonl --label ra-off
$G draft --scenarios qa/GG1/scenarios/E-insert.json --out /tmp/e-on.jsonl  --label ra-on --read-ahead on
python3 qa/GG1/check.py --scenarios qa/GG1/scenarios/E-insert.json /tmp/e-off.jsonl /tmp/e-on.jsonl
python3 qa/GG1/probes/E-insert/join_check.py /tmp/e-off.jsonl /tmp/e-on.jsonl
# probe (own workspace)
cd qa/GG1/probes/E-insert && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/home/user/gg1-target cargo run --release
```

Scripted-provider results (commit under test: the `qa/doubletap` checkout, 2026-10-30):
- `check.py`: 31/31 pass in both modes. It does not look at `inserted_field`.
- `join_check.py`: 21/31 pass per mode (42/62). All 10 failures come from the join. The same 10 fail with read-ahead on and off.
- Read-ahead on: 24 done, 4 `refused` (secure, search, terminal, excluded), 3 `unchanged`. The 3 `unchanged` are scenarios whose prompt matches the scenario just before it, which is correct.

### Checking the harness's insertion model against the product

The harness computes `inserted_field = before + draft + after` from `split_value(value, selection)`
(`qa/GG1/harness/src/main.rs:598`). The product does this:
- `Effect::Insert(final_text)` (overlay.rs:347). The text is the overlay text, which is the
  engine's post-processed stream, or the user's edit of it.
- `flow.rs:479-512` `insert()`. It hides the overlay and calls `restore_focus` (60 ms sleep, then
  `open -b <bundle>` on macOS). `wait_frontmost` (up to 1.5 s) checks the app only. Then
  `a11y.api().insert(&text)` (flow.rs:491-492).
- `nugget-a11y/src/engine.rs:225` `insert_inner`. It re-reads the field that is focused **now**,
  refuses secure fields and denied apps, and tries accessibility insertion first:
  - macOS: sets `AXSelectedTextRange` (best effort), then sets `AXSelectedText`.
  - Linux: `DeleteText` of the selection, then `InsertText`. When no selection is reported it
    inserts at the end (`platform/linux/mod.rs:658`).
  - Windows: `ValuePattern.SetValue(expected_after_insert)`, and only when the selection is known.

  It then verifies by reading the field back for up to 600 ms. If accessibility insertion fails,
  or the field did not change, it falls back to the clipboard: save every format, paste, verify,
  restore.

So the harness's model of caret insertion that replaces the selection is **accurate** whenever a
selection is reported. It differs only when no selection is reported: macOS and Windows then insert
at the real caret, while Linux and the harness append (finding E-5). Text is typed once, never
key-by-key, and `\r` and control characters are stripped (`clean_insert_text`).

## Candidate findings

### E-1: The draft is joined to half-typed text with no separator ("good,I'll", "doThursday", "Hi Ana,The total"). Severity guess: **High**
- **Repro:** run the commands above. `join_check.py` fails on `ins-join-comma-nospace`,
  `ins-join-half-sentence-word`, `ins-capital-after-half-sentence`, `ins-greeting-typed-no-newline`,
  `ins-email-join-comma`, `ins-rtl-join` (and `ins-no-selection-known`; see E-5). The probe's case A1
  shows the product inserter does the same and reports the result as **verified**.
- **Actual (from `inserted_field`):**
  - `"Sounds good,I'll send the deck by Friday."`
  - `'I can doThursday at 3pm if that works for you.'`
  - `'Thanks Ana, the plan looks good andHappy to start on Monday.'`
  - `'Hi Ana,The total is $8,750 and delivery is on November 21.\n\nBest,\nMara'`. The model wrote
    `"Hi Ana,\n\nThe total…"`. The greeting strip removed the greeting **and** the blank line.
  - `'شكرا،سأرسل الملف غدا.'`
  - Probe A1: `insert -> Ok((Accessibility, true))`, `"Hi Ana,\n\nThanks for the update,I'll move our prep meeting to Monday."`
- **Expected:** a space when the text before the caret ends in a word character or punctuation and
  the draft starts with a word, in Latin, Cyrillic, Arabic and similar scripts. No space for
  CJK: `ins-cjk-no-space` is already correct. A paragraph break after a greeting line that has no
  newline. Lower case when continuing mid-sentence: the model's choice, but the join should not
  glue the words.
- **Why:** the post-processor always strips leading whitespace from the draft:
  - `post.rs:179` `let mut pos = skip_ws(raw, 0);`
  - `post.rs:227` `head.pos = skip_ws(raw, head.pos + k);`, which strips the greeting and the
    whitespace after it
  - `post.rs:497` `let trimmed = out.trim_start().len();`

  Nothing downstream looks at the character before the caret: `flow.rs` passes `final_text`
  unchanged and `engine.rs insert_inner` inserts it as is. The prompt only says "Write only what
  goes at the caret" (`prompt.rs:495`).
- **Owner:** `crates/nugget-draft` (post.rs / engine.rs `post_options`). Alternatively the desktop
  `doubletap/flow.rs` just before `Effect::Insert`.
- **Fix direction:** compute a join prefix in the engine from `split_value(...).0`:
  - if `before` ends with a non-whitespace character that is not an opening bracket, and the
    first character of the draft is not whitespace, closing punctuation or CJK, prepend one
    space;
  - if `strip_greeting` fired and `before` does not end with `\n`, prepend `\n\n`.

  Emit the join prefix as part of the first delta, so the overlay shows exactly what will be
  inserted. Do the same check on the `after` side: a space before a following word.

### E-2: No overlap removal when the model repeats what the user already typed ("ThursThursday", "we shouldI think we should"). Severity guess: **Medium** (depends on how often the real model does it; small models often do)
- **Repro:** `ins-join-midword`, `ins-repeat-half-sentence` (scripted replies that imitate this failure).
- **Actual:**
  - `'I can do ThursThursday at 3pm works for me.'`
  - `'Hi Ana,\n\nThanks for the update. I think we shouldI think we should move the review to Monday…'`
- **Expected:** the repeated prefix (or partial word) is dropped: `I can do Thursday at 3pm…`, `…I think we should move…`.
- **Why:** the post-processor has no overlap handling. It only drops a duplicated greeting or
  sign-off. The prompt instruction (`prompt.rs:495`) is the only guard.
- **Owner:** `crates/nugget-draft/src/post.rs`.
- **Fix direction:** hold the head until the first ~40 characters are known. Find the longest
  suffix of `before` (at least one word, case-insensitive, whitespace-normalised) that is a prefix
  of the draft and drop it. When `before` ends inside a word and the draft starts with a word that
  extends it (`Thurs` / `Thursday`), emit only the completion (`day`).
- **Real models:** watch `ins-join-*` and `ins-repeat-*` in the 4B and 1.7B runs.
- **Harness note:** `check.py`'s `no_repeat_prefix` misses this case. It needs the last 25
  characters of the field to occur in the draft (`check.py:159` `tail = before[-25:]`), but
  `"update. I think we should"` is not in the draft. Use a word-level suffix/prefix overlap instead.

### E-3: The insertion target is any focused field of the same app, not the field the draft was written for. Severity guess: **Medium**
- **Repro:** probe case A2. The draft is read from "Message body". The user clicks into "Subject"
  (same Mail window), then presses Enter on the overlay.
- **Actual:** `[A2 focus moved to Subject] insert -> Ok((Accessibility, true))`,
  `Subject = "Re: orderThe total is $8,750 and delivery is on November 21.\n\nBest,\nMara"`: a
  multi-line email body in a single-line field, reported verified. Probe A3: the field changed
  after the read (user typed "The tot"). The draft is inserted at the new caret:
  `"Hi Ana,\n\nThe totThe total is $8,750."`.
- **Why:**
  - `flow.rs:561-571` `wait_frontmost` only checks `same_app(f, target)`.
  - `Target` stores only `app` and `caret` (flow.rs:40-43).
  - `engine.rs:227` `insert_inner` re-resolves `self.platform.focus()` at insert time and checks
    only secure, deny and editable.
- **Fix direction:** keep a field identity from the trigger read: role, label/name, bounds, and a
  value hash or length. At insert time, if the focused field differs, show `insert_failed` /
  "field changed" instead of inserting. If only the value changed, insert anyway but recompute
  the join (E-1). If the target is single-line, fold newlines.
- **Owner:** `apps/desktop/src-tauri/src/doubletap/flow.rs` and `nugget-a11y` engine.

### E-4: A one-word selection replaced by a sentence-punctuated draft ("meet Friday. at 3?"). Severity guess: **Low–Medium** (model-dependent)
- **Repro:** `ins-selection-one-word` (selection on `Thursday` in `can we meet Thursday at 3?`, reply `Friday.`).
- **Actual:** `'can we meet Friday. at 3?'`. **Expected:** `can we meet Friday at 3?`.
- **Why:** the post-processor does not know that the selection is mid-sentence. `post_options`
  only uses `before` and `after` for the greeting and sign-off strips (engine.rs:1442-1466).
- **Fix direction:** if `after` starts with a space and a lower-case word or digit (the sentence
  continues), and the selection had no terminal punctuation, strip the draft's trailing `.!?`.
  Optionally match the case of the replaced text.

### E-5: When no selection is reported, behaviour differs by platform; on Linux the draft goes after the sign-off. Severity guess: **Low**
- **Repro:** `ins-no-selection-known`; probe A4.
- **Actual:** `'Hi Ana,\n\nBest,\nMaraYes, delivery is still on November 21.'`.
- **Why:**
  - `split_value` treats `None` as "caret at end" (prompt.rs:351-354), so the prompt shows `‸` at the end.
  - Linux inserts at `value.chars().count()` (`platform/linux/mod.rs:658-661`).
  - macOS `AXSelectedText` inserts at the real caret, which the prompt did not show.
  - Windows refuses accessibility insertion ("selection unknown") and pastes at the real caret.
- **Fix direction:** on AT-SPI, read `Text.caret_offset` when the selection is empty or unknown.
  In the prompt, mark the caret position as unknown instead of placing it at the end. Plus the E-1 join.

### E-6: Read-ahead misses the thread the user actually replies to after a quick switch (engine 5 s limit plus desktop 10 s re-read). Severity guess: **Medium** (latency: first character at the trigger)
- **Repro:** probe part B, with product `DraftConfig::default()`:
  ```
  default read_ahead config: ReadAheadConfig { enabled: true, min_interval_ms: 5000 }
  [B1] read_ahead(thread A)                         -> done (prompt_chars=1906)
  [B1] read_ahead(thread B) right after (switched)   -> skipped: rate_limited
  [B1] read_ahead(thread A + typed text)             -> skipped: rate_limited
  ```
  Timeline on the desktop:
  1. The user opens thread A. After `DWELL` 1.5 s (polled every 1 s), A is read and sent.
  2. The user switches to thread B 2–3 s later. The window title changes, so `surface` changes;
     the planner cancels A and, after the dwell, reads B.
  3. The engine answers `rate_limited`: `engine.rs:832` `if st.last_work.is_some_and(|t| t.elapsed() < min_interval)`
     with `min_interval_ms: 5_000` (warm.rs:38). Cancelling A does not reset `last_work`.
  4. The planner clears `last_sent` (readahead.rs:340), but `last_read` was set when B was read
     (readahead.rs:286), so B is not read again for `REREAD` = 10 s (readahead.rs:203).

  A user who switches thread and replies within ~10 s gets no read-ahead for that thread. The
  same happens after typing into a field within 5 s of a read-ahead.
- **Expected:** a read-ahead for a **different** context (fingerprint) is not rate-limited by one
  that was cancelled because focus moved.
- **Fix direction (engine, `nugget-draft` engine.rs/warm.rs):** apply `min_interval` only to the
  same app and surface, or reset `last_work` when `cancel_in_flight` cancels a running one.
  Desktop side (readahead.rs): after a `rate_limited` outcome, retry after the remaining interval
  instead of the full `REREAD`.
- Confirm on real runs by sending A then B 2 s apart: B's `cached_prompt_tokens` should be near the full prompt.

### E-7: Read-ahead does not notice focus moving within a window, and backs off to 60 s while the focus is outside a text field. Severity guess: **Low–Medium** (latency)
- **Why (file:line):**
  - The planner's notion of "where the user is" is app + window title + URL only
    (`readahead.rs:361` `pub fn surface`).
  - A full read that finds no draftable field records fingerprint 0 (`readahead.rs:299`
    `let seen = fingerprint.unwrap_or(0)`). Each repeat counts as "unchanged" (`readahead.rs:300`).
  - `reread_after` doubles: 10, 20, 40, then a cap of 60 s (`readahead.rs:203`
    `let factor = 1u32 << self.unchanged.min(8)`).

  A user who reads a Slack channel for a minute (focus in the message list) and then clicks into
  the composer can wait up to 60 s before any read-ahead. Moving from To to Body in a mail window
  is not detected either.
- **Fix direction:** add the focused element's role and label (or its bounds) to the cheap poll.
  The frontmost read already resolves the focused element (`frontmost_app` calls
  `platform.focus()`). Reset `unchanged` when it changes.
- **Owner:** `apps/desktop/src-tauri/src/doubletap/readahead.rs` (and `A11y::frontmost` in flow.rs).

### E-8: Nothing is visible between the double tap and `draft.begin` answering. Severity guess: **Medium** (perceived latency and "did it work?")
- **Why:**
  - `Event::Begin` changes the view but emits no `Show` (overlay.rs:244-257, `Step::changed(Vec::new())`).
  - The window is first shown on `Started` (overlay.rs:270 `Step::changed(vec![Effect::Show { focus: true }])`).
  - `fire()` awaits the full field read first: `focused()`, with a tree walk of up to 600 ms
    (`tree.rs` `WalkLimits::deadline`), then `draft.begin`.
  - `begin` awaits retrieval, voice and `open_stream` before it answers (engine.rs:664-720).
  - With a busy daemon, `retry_busy` adds 100/300/600 ms (daemon.rs:116-119).

  During that time the overlay is hidden, and a second double tap is swallowed (`busy()`).
- **Fix direction:** on `Begin`, show the overlay without focus in a "reading" state at the caret
  (or after ~150 ms, to avoid a flash on fast paths).
- **Owner:** `doubletap/overlay.rs` / `flow.rs`.

### E-9 (harness/report, not product): `first_char_ms_from_trigger` is not what the user sees. Severity guess: **Low** (affects the report's latency claims)
- `gg1` starts the clock at `drafter.prewarm(&hint)` (`harness/src/main.rs:852`
  `let trigger = Instant::now()`) and stops at the first non-whitespace engine delta. The real path also includes:
  1. the a11y `focused()` read, field plus thread walk, up to ~600 ms on large trees, which runs before `draft.begin`;
  2. the `draft.begin` JSON-RPC round trip (and `retry_busy`);
  3. `chat.delta` notification → `daemon.rs` re-emit → Tauri event → state machine → `emit_to` overlay → webview paint;
  4. the overlay being shown only on `Started` (E-8).
- Items 2–3 are small but not zero. Item 1 can dominate on Chromium-based apps (Slack, Gmail).
- **Suggestion:** state the latency as "engine first character". For first character on screen,
  use `apps/desktop/e2e-native` with `NUGGET_E2E_PROBE` (debug builds record the timestamps
  `trigger`, `view` and `show`; `doubletap/probe.rs`) on runners. That harness also inserts for real.
  Its fixture starts with `"Hello "` (trailing space) and it checks `includes` (run.mjs:272-280),
  so it cannot catch E-1. A second fixture value `"Hello"`, plus an exact read-back comparison,
  would.
- The harness's `--read-ahead on` waits for the read-ahead to finish. This is the best case: the
  desktop sends after a 1.5 s dwell and may be rate-limited (E-6, E-7).

### E-10 (observation, Low): the trigger's prefix warm is skipped when any recent read-ahead for the same app ran, even for another thread
- **Repro:** probe B2. After read-ahead(A), the trigger in thread B gives `connection warms +1, prefix warms +0`.
  `engine.rs:559` `} else if self.read_ahead_covers(app.as_deref(), p.id())`: the match is on app
  and provider only, with a TTL of 30 s (`prewarm_ttl_ms`).
- **Impact:** probably small, because the system prefix of the A prefill matches B's when the kind
  and voice are the same, so llama-server reuses it. But a stale read-ahead still running on the
  slot is not cancelled by `begin` (begin never touches `inner.read_ahead`), so the trigger can
  queue behind a prefill of the wrong thread. Check `cached_prompt_tokens` and first-character
  time in the real runs.
- **Fix direction:** compare the read-ahead's fingerprint, or at least its thread or surface, in
  `read_ahead_covers`. Have `begin` cancel an in-flight read-ahead whose fingerprint differs from
  the request.

### E-11 (observation, Low): no power or battery awareness in read-ahead
The read-ahead loop (readahead.rs) does a full a11y read every 10–60 s in any allowed window. It
can send up to 4 local-model prefills per minute (`RATE_MAX`). No battery, low-power or idle check
was found (`grep -i battery` in `doubletap/` finds nothing). Suggest pausing on battery saver or
low battery, or making it a setting.

## What works well (protect this)
- **Clipboard fallback is careful.**
  - Every format and item is snapshotted and restored on every path, including errors.
  - A 256 MB cap refuses rather than risking clipboard loss.
  - After an unverified paste it waits `unverified_grace` (1.5 s) and verifies again before
    restoring, so a slow app does not paste the old clipboard (`clipboard.rs:paste_with_restore`).
  - Clipboard managers are told to skip the draft: on macOS (`org.nspasteboard.TransientType`,
    `ConcealedType`), on KDE (`x-kde-passwordManagerHint`) and on Windows
    (`ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory`).
- **No double insertion.** If accessibility insertion changed the field but could not be
  verified, the engine does not also paste (engine.rs: "never insert a second copy").
- **Re-checks at insert time.** Secure fields and denied apps are refused again at insertion
  (`insert_inner`), not only at the trigger. `\r` and control characters are stripped from
  inserted text.
- **Join cases that already work** (scripted runs; `join_check` passes):
  - typed greeting with a blank line (`ins-greeting-typed`);
  - caret between a typed greeting and a typed sign-off: only the body is inserted (`ins-caret-middle-between-greeting-signoff`);
  - sign-off already below the caret is dropped (`ins-signoff-already-below`);
  - a "Best," inside a quoted message is **not** mistaken for the user's sign-off (`ins-caret-above-quote`);
  - an inline chat greeting is deduplicated (`ins-greeting-typed-chat`);
  - a trailing space plus a model reply starting with a space gives no double space (`ins-join-model-leading-space`);
  - a selection replacing one sentence, including reversed offsets (`ins-selection-replace-*`);
  - emoji with a skin-tone modifier before the caret (`ins-emoji-*`);
  - CJK joined without a space (`ins-cjk-no-space`);
  - preamble and quotes stripped (`ins-preamble-and-quotes`);
  - no trailing newline in a chat composer (`ins-trailing-newline-chat`);
  - a multi-paragraph reply folded to one line in a single-line field (`ins-single-line-field`).
- **Offsets are consistent.** macOS converts UTF-16 offsets to character offsets in both
  directions (`utf16_to_chars` / `chars_to_utf16`). `split_value`, `expected_after_insert` and
  the harness all use character offsets.
- **Read-ahead privacy and gating hold.**
  - Secure, search, terminal and excluded fields skip as `refused` with no prompt built (`ins-ra-*`; probe B3).
  - The desktop never reads a secure field's value and filters with `draftable()`.
  - Cloud providers get nothing at all.
  - An identical prompt skips as `unchanged`.
  - The read-ahead request goes through the same `draft_request` code path as `begin`, so the
    prefill matches the draft's prompt exactly.
  - The planner cancels a read-ahead when focus leaves the window.
- **The trigger itself adds little.** The double tap fires on the second release with no extra
  wait (nugget-hotkey `tap.rs`), and prewarm starts before the field read (flow.rs:211-217).

## Notes for the lead
- Real-model runs: `ins-join-*`, `ins-repeat-half-sentence`, `ins-capital-after-half-sentence`
  and `ins-selection-one-word` test what the model does with a half sentence. The scripted
  replies imitate likely outputs. E-1 is deterministic and does not depend on the model; E-2 and
  E-4 do.
- The scratchpad directory is shared between explore agents. I once ran a `gen.py` there that
  turned out to be area D's generator. It rewrote `qa/GG1/scenarios/D-safety.json` from D's
  current generator source, so the content should be unchanged, but D may want to regenerate. I
  now keep my files in `scratchpad/E-insert/`.
