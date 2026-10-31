# GG4 QA: proactive cards and the morning brief

Package GG4 (wave GG). The question: do Today cards, the morning brief, card actions with dry-run diffs,
learning from Accept/Dismiss (BB2) and the optional LLM judge work for a real user?

- Branch `qa/proactive`. Product code tested is the integration branch at `5f1998e`.
- Method: the real `nuggetd` was run on a simulated clock and driven over its IPC. The input was a
  synthetic two-week persona, replayed one morning at a time.
- Real local models: `qwen3-1.7b-q8` (chat, judge, drafting) and `nomic-embed-text-v1.5-q8`, from the
  signed manifest, on GitHub `ubuntu-latest` runners (4 vCPU Xeon Platinum 8573C, 16 GB, CPU only).
  - Run 3, [41115038648](https://github.com/rayhankhilji/nugget/actions/runs/41115038648): replay and judge.
  - Run 4, [41116756315](https://github.com/rayhankhilji/nugget/actions/runs/41116756315): replay, judge,
    actions, brief and learning.
  - The final run on the head commit is linked in the orchestrator message.
- Everything else ran locally with no model: judge off, or a fake OpenAI-compatible server for drafts.
- Every finding below was reproduced by a second, fresh verifier subagent from the written repro alone,
  unless marked otherwise. The verifier reports are `qa/GG4/out/verify/V1.md`–`V3.md`.

## Verdict

| Capability | Verdict | Evidence |
|---|---|---|
| Today cards from mail (unanswered, follow-ups, awaiting reply, renewals, note to-dos) | **Works with caveats** | 26/29 required in-window expectations hit over 14 mornings (no model). 0 duplicate cards. Noise blocked on 14/14 mornings. All 6 handled situations left the inbox the next morning. Caveats: GG4-6, GG4-7, GG4-9, GG4-11, GG4-12, GG4-13. |
| Meeting-prep cards | **Broken** for `.ics` calendars | 0 of 4 external meetings got a prep card on 14/14 mornings, because attendees are lost in parsing (GG4-1). |
| Morning brief: timing | **Works** | `brief.ready` arrived at 08:00:00–08:00:01 local on 14/14 persona mornings, in New York and London, 3 times over. It fires once per day, including across the DST change while running (sped-up clock, 08:00 EST), and after a restart. |
| Morning brief: content | **Broken** for calendar content; **works** for cards | 85/85 brief cards trace to real evidence, and none are invented. But: the recurring standup is missing on 10/10 weekdays (GG4-3); a moved meeting is listed twice; a cancelled meeting is listed, twice (GG4-2); cross-zone times are off by hours (GG4-4). |
| Card actions with dry-run diffs | **Works with caveats** | The diff matches what was done for reminders, `.ics` events and drafts (all times, text and recipients). Approval binding refused 10/10 attack kinds: wrong, foreign, reused, expired or restarted digests, closed cards, bad indexes, concurrent execution. Nothing ran without the approval digest. Caveats: GG4-14, GG4-15, GG4-16, GG4-17. |
| Learning from Accept/Dismiss (BB2) | **Broken** in practice | Per-scope learning and its explanations are exact, but every feedback also retrains a shared model that demotes every card. After 6 dismissals of one recruiter, the user's friend and manager fall below the offer threshold (0.60 → 0.076), and scoped resets don't undo it (GG4-5, GG4-8). Learning scenario: 37/50 checks, 3/3 local runs and 1 runner run. |
| Optional LLM judge (real model) | **Works with caveats** | 34 card enrichments over 6 mornings. 14/14 unique summaries use only names and numbers present in the evidence. It never added a card. No sensitive word appeared in summaries. But 3/15 unique summaries invert who owes whom a reply, and 2/2 summaries of due items still read "upcoming" days after the due date (GG4-10). Refresh with the judge took up to 76 s on the runner. |

## Findings

Severity is the impact on a real user. "V" marks a finding reproduced by the verification layer, with
the verifier's file. Detailed evidence and quoted outputs are in the explorer reports
(`qa/GG4/out/A1.md` daily cards, `A2.md` learning, `A3.md` actions, `A4.md` brief/time, `L.md` lead).

Common setup, from the repo root:

```sh
apt-get install faketime
CARGO_INCREMENTAL=0 cargo build -p nuggetd
cargo build --manifest-path qa/GG4/probe/Cargo.toml --target-dir target          # gg4-probe
cargo build --manifest-path qa/GG4/scenarios/a2audit/Cargo.toml --target-dir target  # gg4-audit
python3 qa/GG4/persona/gen.py                                                    # writes persona/world
```

`gg4-probe HOME NOW OFFSET_SECS` prints what `nugget-proactive` parsed from a stopped daemon's store
(mail, events, notes, candidates).

### GG4-1 — High — `.ics` events lose attendees, organizer, status and location: no meeting-prep cards; cancelled and declined events stay in the brief (V1, V3)
- **Repro:**
  1. `python3 qa/GG4/scenarios/replay.py --out W --days 1-2 --judge off`.
  2. `target/debug/gg4-probe W/home 2026-10-27T11:59:00Z -14400`.
- **Expected:** "Northwind checkout review" (Oct 28 14:00, attendees Dana and Marcus at
  northwind-outfitters.example) has 3 attendees and produces a `meeting_prep` card on Oct 27/28.
  The Kelsey coffee, cancelled with `STATUS:CANCELLED` / `METHOD:CANCEL`, is absent from the Nov 6 brief.
- **Actual:**
  - The probe prints `Northwind checkout review 2026-10-28T14:00:00-04:00 ... attendees []`.
  - No `meeting_prep` card appears on any of 14 mornings, for any of the 4 external meetings.
  - Brief Nov 6 lists "Coffee with Kelsey (TalentBridge)" twice: the invite and the cancellation itself.
  - `location` is always null.
  - A declined event is still listed.
- **Frequency:** 14/14 mornings, every `.ics` event.
- **Owner:** `crates/nugget-extract/src/ics.rs:329-336` copies only uid/start/end/timezone/all_day of
  the first event to top-level metadata; attendees, organizer, status and location live in
  `extracted.events[]`. `crates/nugget-proactive/src/corpus.rs:637-707` (`parse_event`) reads them from
  the top level. Today: the extract/proactive owners. CalDAV goes through the same extractor, so it is
  affected too.
- **Fix:** read the event fields from `extracted.events[i]` (one `CalEvent` per VEVENT), or copy them up.
  Add a corpus test with a real `.ics` from the files connector.

### GG4-2 — High — A moved meeting appears twice in the brief, at its old and new time (V3)
- **Repro:** `python3 qa/GG4/scenarios/brief.py --out W --only accuracy`; look at the 2026-10-30
  brief. Input: `ev-nw-qa-v0.ics` (13:00) and `ev-nw-qa-v1.ics` (same UID, `SEQUENCE:1`, 15:00).
- **Expected:** one "Northwind design QA" at 15:00.
- **Actual:** two entries, 13:00 and 15:00.
- **Frequency:** 2/2 mornings with a moved or cancelled event.
- **Owner:** nothing groups events by UID and keeps the highest SEQUENCE (`corpus.rs:255`,
  `brief.rs:102`).
- **Fix:** dedup by UID (plus RECURRENCE-ID), keep the highest SEQUENCE/DTSTAMP, and drop the event
  when that version is cancelled.

### GG4-3 — High — Recurring events (RRULE) are never expanded: the daily standup is never in the brief (V1, V3)
- **Repro:**
  1. Base run as in GG4-1.
  2. `brief.get` on Mon 2026-10-26 (`W/day-01.json`).
  3. Probe the store.
- **Expected:** "Design standup 09:30" on every weekday (`ev-standup.ics`, `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR`
  from Oct 5).
- **Actual:**
  - `meetings: []`.
  - The probe has a single event on 2026-10-05.
  - Missing on 10/10 weekdays: 10 of the 17 meeting occurrences the brief should have listed.
- **Owner:** `crates/nugget-proactive/src/corpus.rs` (no RRULE handling; the extractor keeps `rrule`).
- **Fix:** expand RRULE/RDATE/EXDATE within the brief and meeting horizon, in the event's own zone.

### GG4-4 — High — The event's TZID is ignored: event times are read in the user's current UTC offset (V1, V3)
- **Repro:** probe as in GG4-1.
- **Expected:**
  - Flight BA178 ends `2026-11-04T07:00 Europe/London` (02:00 New York).
  - "Foundry workshop" at 10:00 London.
  - The studio retro (Nov 6 16:00 New York) at 21:00 in London's brief.
- **Actual:**
  - `end 2026-11-04T07:00:00-04:00`.
  - Foundry `10:00:00-05:00` when the OS zone is New York.
  - Retro listed at 16:00 London.
  - After the DST change, New York events created earlier shift by an hour too
    (`2026-10-28T14:00:00-05:00`).
  - One wrong-time meeting per mode: 1/14 mornings (OS zone), 1/4 (fixed zone), 1/4 (explicit `utc_offset`).
- **Owner:** the extractor stores `start` without an offset plus `timezone`/`end_timezone`;
  `corpus.rs:588` `parse_time` assumes the user's offset.
- **Fix:** resolve TZID with a tz database (the extractor already has the zone name).

### GG4-5 — High — Every Accept/Dismiss also retrains a shared model that moves every card; one recruiter dismissal demotes friends and the manager (V1)
- **Repro:** `python3 qa/GG4/scenarios/learning.py --out W --only minimal` (5 s). It runs two homes with
  identical mail at the same simulated time: one gets feedback, the control gets none.
- **Expected:**
  - Dismissing recruiter Jo (`not_relevant`) lowers Jo's cards.
  - Sofia (a gmail.com friend the user never touched) moves by at most the small per-kind effect shown
    in her card's `learned`.
- **Actual:**
  - One dismissal moves Sofia −0.96 log-odds, and her `learned` explains −0.11.
  - `already_handled` −0.43, `wrong_info` −0.27, a snooze −0.35.
  - After 6 dismissals, Sofia and the manager drop from 0.60 to 0.076, below `min_score` 0.45, so they
    are not offered.
  - Over a simulated week, untouched friends sit −2.2 to −2.7 below the control.
  - The manager accepted daily ranks below the control on day 1.
  - Consequences, same root cause:
    - The shift never decays: still −2.3 to −2.6 31 days later.
    - An "URGENT … sign-off today?" from a manager dismissed 5× as not_important ranks 0.0075 against
      0.926 in the control, and 0.025 after 185 days. The learned layer's 4 log-odds cap doesn't apply
      to the shared model.
    - A cold recruiter mail climbs to the top of the inbox (0.92) as the user accepts other reply cards.
- **Frequency:** 4/4 runs (3 local, 1 runner).
- **Owner:** `crates/nugget-proactive/src/lib.rs:406` (`model.update_weighted` on every feedback),
  `score.rs:172-191`, learning rate 0.3 (`score.rs:95`), unregularised bias (`score.rs:180`).
  BB2 / proactive owners.
- **Fix:**
  - Let the BB2 learned layer carry feedback (it is scoped, explained, bounded and decaying).
  - Freeze the shared model, or update it with a far smaller rate and no bias term, with decay and the
    same cap.
  - Show any remaining shared effect in `learned`.

### GG4-6 — High — Sensitive mail is never flagged: therapy, clinic, HR salary and fraud all show `sensitive: false`, with or without the local model (V1; real-model run 3)
- **Repro:**
  - `python3 qa/GG4/scenarios/replay.py --out W --judge off` (no model).
  - The runner replay with `--runtime` (`qa/GG4/out/real-run3/replay-summary.json`).
- **Expected:** "Reply to Rebecca Moss: Thursday session" (mossplace-therapy), the HR compensation
  letter and the clinic nurse's metformin mail are `sensitive: true`. They are then withheld from a
  cloud judge, and their thread labels are not learned.
- **Actual:**
  - 0 of about 80 cards are sensitive in the no-model runs.
  - 0 cards are sensitive in the real-model replay (qwen3-1.7b running).
- **Owner:** `crates/nugget-ingest/src/pipeline.rs:519` sets the flag only from PII patterns or model
  triage.
- **Impact:** with `judge = "cloud"`, `judge.rs:70` would send health and salary content to a cloud model.
- **Caveat:** the real-model run did not wait for triage before the first sync. Items that arrived on
  later days (therapy Nov 2, HR Oct 30) were still unflagged.
- **Fix:** a deterministic sensitive classifier (health, therapy, salary/compensation, bank, legal)
  that runs without a model, and treat role mailboxes like `hr@` as sensitive senders.

### GG4-7 — High — An HR "Confidential: your 2027 compensation" letter becomes a "Reply to" card, top-5 in the brief, salary quoted (V1)
- **Repro:** the no-model replay above, mornings Nov 1–8. Input `s6-hr.eml` from `hr@larkspur.example`:
  "your salary for 2027 will be $118,500 (+6%). Please don't share this letter."
- **Expected:** no card (nothing is asked), or at most a sensitive, low-ranked FYI.
- **Actual:**
  - "Reply to Larkspur People Team: Confidential: your 2027 compensation" appears 8/8 mornings, with
    the salary in the evidence quote.
  - It is in the `brief.ready` top 5 on 5 mornings.
  - With the real-model judge: "A thread regarding compensation for 2027 has not been responded to,
    and a reply is needed."
- **Owner:**
  - "Please don't share" counts as an ask (`crates/nugget-proactive/src/text.rs:172,195`).
  - `hr` is missing from the role-mailbox list (`corpus.rs` `automated_local`).
- **Fix:** imperatives that forbid ("don't share/forward") are not requests; add `hr`/`people`/`payroll`.

### GG4-8 — High — Scoped learning resets don't restore the cards they should (V1)
- **Repro:** `learning.py` full run, `report.scoped_resets_restore_jo_cards_to_no_feedback_rank`.
- **Expected:** after resetting Jo's sender, his domain, his thread and the card kind, Jo's cards rank
  as in the control.
- **Actual:** 0.357 against 0.789. `learned` is empty, so the user sees no reason and has nothing left
  to reset but "all". "Reset all" does restore the control ranks exactly.
- **Owner:** `lib.rs:543-557` (`ResetScope::One(..) => {}` leaves the shared model).
- **Fix:** follows from GG4-5.

### GG4-9 — Medium — An unquoted comma in a From name ("Paula Grant, RN") silently drops the thread: the clinic's "please call" never becomes a card (V1)
- **Repro:** no-model replay, mornings Oct 30–Nov 3. Input
  `qa/GG4/persona/world/files/Mail/Inbox/20261029-1015-s6-nurse.eml`.
- **Expected:** "Reply to Paula Grant, RN: Your results - please call".
- **Actual:**
  - No card, 5/5 mornings.
  - With the display name quoted, the card appears (but is not sensitive, GG4-6).
- **Owner:** `crates/nugget-extract/src/mail.rs:85` keeps only the first comma-separated mailbox
  ("Paula Grant", no address). `generators.rs:136` then skips the thread without logging.
- **Fix:** parse the address list leniently: take the address in `<…>`, and log skipped threads.
- **Severity:** the header is technically malformed, but common in real mail; the verifier lowered it to
  Medium.

### GG4-10 — Medium — LLM judge summaries invert who owes whom and go stale after the due date (real model, run 3; not re-verified by a second agent)
- **Repro:** runner suite `judge` (`python3 qa/GG4/scenarios/judge.py --out W --runtime --chat-model
  qwen3-1.7b-q8 --embed-model nomic-embed-text-v1.5-q8 --models-home M`). Evidence:
  `qa/GG4/out/real-run3/judge-summary.json`.
- **Expected:** the summary says why the card matters now, from the evidence.
- **Actual (quoted, 3/15 unique summaries wrong):**
  - Mom's "are you coming home for Thanksgiving?" → "Ngozi Okafor has not responded to the thank-you
    message".
  - Maya's unanswered "Brunch next weekend?" → "The user hasn't responded to a request for a brunch".
  - Maya's question to Jonah → "The user has not responded to Jonah Pike's request for brochure quotes".
  - On Nov 2, the Friday-Oct-30 budget promise still reads "will send ... by Friday, which is important
    for ensuring timely delivery".
  - "Overdue since 2026-11-04" is summarised as "it is important to ensure the task is completed on
    time" (2/2 overdue items).
  - These summaries replace the card detail in the brief (`brief.rs` `BriefCard::of`).
- **Frequency:** the same 2 temporal failures in runs 3 and 4.
- **Owner:**
  - `crates/nugget-proactive/src/judge.rs`: the input has no current date, no "who sent it", and no
    direction (`JudgeInput`).
  - Enrichment is computed once per card and never refreshed (`lib.rs` refresh filter
    `card.enrichment.is_none()`).
- **Fix:**
  - Give the judge today's date, the direction ("you asked them" / "they asked you") and the sender.
  - Re-judge when the due state changes.
  - Prefer the deterministic detail in the brief when the judge's confidence is low.

### GG4-11 — Medium — "1 day" / "3 days" thresholds are measured in hours, so yesterday's asks miss the 08:00 brief (V1)
- **Repro:** `replay.py --out W --judge off --days 2-2 --start 10:00:00` against the default 07:59.
- **Expected:** Dana's Monday 09:14 "send revised mockups by Thursday?" is in Tuesday's brief.
- **Actual:**
  - Absent at 07:59; present at 10:00.
  - Same for Jonah (day 4→5), Gus (6→7) and Rebecca (9→10).
  - Every required miss outside GG4-1/GG4-9 is this.
- **Owner:** `generators.rs:138,159-163` (`unanswered_min_days`, `awaiting_reply_days` compared as
  durations).
- **Fix:** compare local calendar days ("sent before today"), or evaluate at the brief time.

### GG4-12 — Medium — A nudge in the same thread makes the card disappear for a day and return under a new id, losing snooze and feedback (V1)
- **Repro:**
  1. `python3 qa/GG4/persona/gen_extra_a1.py W/world_a1`.
  2. `python3 qa/GG4/scenarios/a1_replay.py W/world_a1 --out W/extra --judge off --days 1-11`.
  3. Look at X6 (Priya asks Tue, nudges Thu "Any update?").
- **Actual:** card `d04812050c1a357a` expires on day 5; new card `9cb212d800aa05b6` appears on day 6.
- **Owner:** the dedup key includes the last message id, and the age gate applies to the newest message
  (`generators.rs:138,159-163`).
- **Fix:** key on the thread; keep the card open while the newest inbound message is younger than the gate.

### GG4-13 — Medium — A question addressed to someone else, with the user on Cc, becomes "Reply to" (V1)
- **Repro:** the X4 extra run as in GG4-12. Priya to Theo, Cc Maya: "Theo, can you approve the
  illustration contractor invoice by Friday?"
- **Actual:** "Reply to Priya Raman: Illustration invoice" (.72), 7/7 mornings.
- **Owner:** `generators.rs:141-144`.
- **Fix:** require the user in To:, or that the ask names or addresses the user; a Cc-only message is FYI.

### GG4-14 — Medium — A refresh between preview and Approve breaks the approval for any "in 2 hours" reminder (V2)
- **Repro:** `python3 qa/GG4/scenarios/actions.py --out W --only exec`: preview a reminder,
  `proactive.refresh`, execute.
- **Actual:**
  - The label moves from `…15:00:01.227Z` to `…15:00:02.961Z`.
  - Execute returns `-32005 card fc6406a22e182988 does not offer this action`.
- **Frequency:** 4/4.
- **Owner:** `now + 2h` is baked into the offered action on every refresh (`generators.rs:450,990,1097,1169`).
  The comparison is at `lib.rs:501`, and the actions are replaced at `cards.rs:183`.
- **Fix:** round reminder times to a stable slot, and keep a card's offered actions stable while a
  preview is outstanding.

### GG4-15 — Medium — Local reminders and focus events ask for "send on your behalf" / "spend money" confirmation (V2)
- **Repro:** `actions.py --only exec`; execute with `confirmed=false` on the reminder for
  "Submit the October expense report" and on the Pixelboard "renew" reminder.
- **Actual:**
  - `needs_confirmation`: "about to send something on your behalf … (keyword "submit")", and
    "spend money" (keyword "renew").
  - The preview says risk `edit` and gives no hint a confirmation is coming.
  - 4 of 5 local actions in the corpus hit this.
- **Owner:** `crates/nugget-policy/src/classify.rs:205-215`, keyword scan over the descriptor built by
  `actions.rs:312`.
- **Fix:** classify reminders and local events by their typed risk, not by keywords in user text; or
  show the confirmation reason in the preview.

### GG4-16 — Medium — Follow-up drafts tell the model the user's own words came from the other person (V2)
- **Repro:** `actions.py --only exec`; the fake LLM log `qa/GG4/out/a3/fake_llm.jsonl` shows
  `<message from="other">Sure, I'll send the vendor shortlist by Friday.</message>`, which is Maya's own
  promise.
- **Owner:** `crates/nuggetd/src/features/proactive/exec/draft.rs:108-113` sets `from_me: Some(false)`
  for every quote.
- **Fix:** carry the evidence's direction (the corpus knows `from_user`).
- **Not tested:** the effect on a real draft.

### GG4-17 — Medium (V2: Low–Medium) — Times built with a fixed UTC offset are off by an hour across DST: snooze, renewal reminders, deadlines (V2, V3)
- **Repro:**
  - `actions.py --only tz`: renewal Nov 3, computed Thu Oct 29 → `Reminder at 2026-11-02T09:00:00-04:00`,
    which is 08:00 EST.
  - `brief.py --only dst`: snooze on Sat evening → `2026-11-01T08:00:00-04:00`, which is 07:00 EST,
    an hour before the brief.
- **Owner:** `crates/nugget-proactive/src/generators.rs:103-107,846-849,880`,
  `crates/nuggetd/src/features/proactive/settings.rs:135-138`.
- **Fix:** keep an IANA zone (from the OS, or `[proactive] timezone`) and compute local wall times with it.

### GG4-18 — Medium — An invalid `[proactive]` value silently removes cards and the brief (V3)
- **Repro:** `python3 qa/GG4/scenarios/brief.py --out W --only config`, with e.g. `brief_time = "25:00"`
  or `utc_offset = "America/New_York"`.
- **Actual:**
  - The daemon runs.
  - `cards.list` and `brief.get` answer `-32601 method not found`.
  - `status` shows nothing; the reason is only in the log.
  - 9/9 invalid values behave like this.
  - IANA names are rejected, so a traveller can only pin a fixed offset. Left at `+00:00` after flying
    home, the Nov 8 brief fires at 03:00 EST.
- **Owner:** `crates/nuggetd/src/features/mod.rs:139`, `features/proactive/settings.rs`.
- **Fix:** report a failed feature in `status`; accept IANA zone names.

### GG4-19 — Medium — Brief meetings carry no item ref, uid or url (V3)
- **Repro:** any `brief.get`; `BriefMeetingView` (`crates/nugget-core/src/ipc.rs:1660`).
- **Actual:** 12/12 listed meetings can't be opened or traced to their source. Brief cards, by contrast,
  trace 85/85.
- **Fix:** add the event's `ItemRef` and url.

### Low
- **GG4-20 (V1, V2):** action labels show raw RFC 3339 UTC with nanoseconds, e.g.
  `Remind me 2026-10-26T14:00:00.002070842Z` for 10:00 New York.
  Owner: `crates/nugget-proactive/src/actions.rs:169,265`.
- **GG4-21 (V2):** `archive_thread` shows `supported:false` on a files source, yet execute asks to confirm
  "delete or overwrite data in mail", then fails `-32002`. Preview and execute don't check
  `exec.supports()`. Owner: `nuggetd/src/features/proactive.rs:565-616`.
- **GG4-22 (V1, V3):**
  - The Pixelboard renewal is still the top card (.97) the day after it renewed, still "renews 2026-11-03".
  - The brief also lists it twice (Top and Renewals).
  - Owner: `generators.rs:852,893`; `brief.rs:147` has no lower bound.
- **GG4-23 (V2):** `draft_reply` says `supported:true` with no model, then fails with "no provider is
  available for the Synthesis tier" (jargon). Owner: `exec.rs:179,254`.
- **GG4-24 (V2):** `reminders.list` / `reminder.due` always have `card_id: null`. Owner: `exec.rs:564`.
- **GG4-25 (V2):** a preview while the kill switch is on still calls the model (1 request measured); only
  execute checks the switch. Owner: `actions.rs:634`.
- **GG4-26 (V1):** the user's own company domain becomes a learning key: two `not_important` dismissals of
  one coworker give `larkspur.example` −0.126 for everyone there. Owner: `feedback.rs:415`.
- **GG4-27 (V1):** resetting a key that doesn't exist returns `{"cleared":0}`, broadcasts and audits.
  Defensible, but it could say "nothing to reset". Owner: `convert.rs:102-131`.
- **GG4-28 (V1):**
  - The learned half-life is 60 days (`feedback.rs:55`); the crate docs say 30 (`lib.rs` header,
    "30-day half-life").
  - `docs/book/src/guide/today.md` doesn't describe per-sender learning, decay or reset.
- **GG4-29 (V3):**
  - On a fresh install, the first morning's brief is spent on an empty corpus: marked fired, nothing sent.
  - A brief missed with the machine off fires at whatever hour the daemon starts (22:30 observed).
  - Owner: `proactive.rs:741-756`, `settings.rs:143-162`.
- **GG4-30 (V3):** a multi-day all-day event appears only on its first day (`brief.rs:103`).
- **GG4-31 (V1):** polish:
  - Handled and timed-out cards both end as `expired`.
  - "wrote N days ago" floors.
  - The quote splitter breaks at "Dr.".
  - The commitment title repeats its date.
- **GG4-32 (V1):** no controllable clock in the daemon. Every path uses `OffsetDateTime::now_utc()`
  (`proactive.rs:888,900,908,937`, `exec.rs`). QA had to run `nuggetd` under libfaketime. Suggest a
  test-only clock override in `nuggetd-e2e`, like its `RuntimeOverrides`.
- **GG4-33 (observed once, run 4; outside GG4 scope, not re-verified):** after both `llama-server`
  processes were killed, the chat server came back `ready`, but the embedding server stayed `failed`
  ("llama-server exited unexpectedly (signal: 9)"). Owner: `nugget-runtime` supervisor.

Dropped or downgraded by verification:
- **Sofia "drinks" follow-up:** a test-world artefact. The world never contains Sofia's reply to "7pm?".
- **Bank fraud alert suppressed as automated:** a judgement call, not a defect. It is listed here so
  product can decide.
- **Judge failure fallback:** inconclusive. With every card already enriched, the refresh after killing
  the model server made no judge call.

## What works well (protect this)

- **Lifecycle:**
  - Handled situations leave the inbox the next morning: Maya's reply, a delivered promise, a vendor's
    answer (6/6).
  - Accepted cards move to `done`.
  - Dismissed cards stay gone.
- **Promise follow-ups:**
  - They appear 2 days ahead and stay overdue until delivered.
  - Late-evening promises get the right deadline: "by Friday" sent Thu 21:40 EDT is due Oct 30.
- **Noise filtering:** 0 cards over 14 days from newsletters, promos, CI mail, the auto-reply,
  `Accepted:`, the airline check-in, the lab-portal notice and the one-time code. An FYI forward makes
  no card.
- **Dedup:** the same Message-ID saved in two folders gives 1 card. A late Archive copy doesn't revive
  an answered thread. 0 duplicate cards in any run.
- **Renewals need proof of payment:** Pixelboard is shown because of its receipt; the never-paid
  Cloudvault trial is never shown.
- **Brief clock:**
  - It fires at 08:00 local every day, in New York and London.
  - It fires once a day across restarts, midnight and the DST change, including a 01:30 brief time on
    the night 01:30 happens twice.
  - It follows an OS zone change while running.
  - A changed `brief_time` takes effect.
- **Brief honesty:** every brief card traces to a real file and a real quote (85/85). No invented events.
  The overdue section is exact.
- **Approval binding:**
  - A digest is tied to card, action and preview.
  - It is refused when wrong, foreign, reused, older than 1 h, or from before a restart.
  - Concurrent executes run exactly once.
  - The kill switch halts execution with nothing created, and the same digest works after resume.
  - `needs_confirmation` has no side effects.
- **Dry runs match reality:**
  - reminders: time and text match.
  - `.ics` events: start, end, title and description match.
  - drafts: the `mailto:` to/cc/subject/body match, and nothing is sent.
  - `reminder.due` fires once, never early.
- **Learned layer (BB2) on its own:**
  - Counts and explanations on cards are exact.
  - Repeated accepts are no-ops.
  - Public mail domains are never keys.
  - The scopes are ordered: sender is stronger than domain.
  - `already_handled` suppresses only the thread, without penalising the sender.
  - Sensitive titles never become thread labels.
  - Learning persists across restarts.
  - Resets are audited (actor `user`, target, audit chain verifies) and broadcast `reset_learning`.
  - Reset works with `[proactive] enabled=false`.
  - Bad reset input gets a clear `-32602`.
  - Learning never changes an action or its risk.
- **Judge guard rails:**
  - It never added a card, and unknown keys are dropped.
  - Names and numbers come from the evidence (14/14).
  - No sensitive words appeared in summaries.
  - The ranking still works when the judge has no provider.

## How to run it

The harness lives in `qa/GG4/`:

- `persona/gen.py`: the persona, its timeline and its truth file.
- `harness/nq.py`: the daemon under libfaketime, plus the IPC client.
- `harness/fake_llm.py`: a fake OpenAI-compatible server for drafts.
- `probe/`: the store probe.
- `scenarios/`:
  - `replay.py`, `a1_replay.py`: the daily replay.
  - `learning.py`, `actions.py`, `brief.py`, `judge.py`.
  - `a2audit/`: the audit-log reader.
- `ci/run.sh` and `.github/workflows/qa-GG4.yml`: real models on GitHub runners.

Explorer and verifier reports, and small evidence files, are in `qa/GG4/out/`.

Note on libfaketime: `FAKETIME="@T"` read from the environment makes the clock start at T, ticking in
real time. With `FAKETIME_DONT_FAKE_MONOTONIC=1`, tokio timers run on the real monotonic clock.
Sped-up runs (`"@T x600"`) need the monotonic clock faked too.
