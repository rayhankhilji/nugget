# GG2 explore: time-sensitive questions (draft, explore layer)

Product code at 2b85228 (no crates/ change since). Today = 2026-10-30 (Fri, UTC). Corpus from
`gen_corpus.py --today 2026-10-30`, 315 items. Retrieval was text-only. The answering model was replaced
by `capture_llm.py`, a fake OpenAI-compatible `local` provider, so the exact model requests were captured.
Results are deterministic.

Files: `time_probe.py` (setup, ingest, `retrieve.context`, `retrieve.ask`, per-citation in-range check),
`search_probe.py`, `queries.json` (20 queries with ranges for 2026-10-30), `out-main/` (original mtimes),
`out-copied/` (the same corpus copied and touched today).

```
W=/tmp/claude-0/gg2-time; python3 qa/GG2/harness/gen_corpus.py --out $W/corpus --questions $W/q.jsonl --today $(date -u +%F)
target/release/nuggetd --home $W/h & sleep 8; kill -INT $!
python3 qa/GG2/explore/time/capture_llm.py --port 18731 --log $W/cap.jsonl &
python3 qa/GG2/explore/time/time_probe.py --home $W/h --corpus $W/corpus --queries qa/GG2/explore/time/queries.json \
  --out $W/out-main --ask --capture-port 18731 --capture-log $W/cap.jsonl
# copied folder: cp -r $W/corpus $W/corpus-copied && find $W/corpus-copied -type f -exec touch {} + ; repeat with home h2
```

## T-F1 (High): the answering model is never told today's date
- **Evidence:** none of the 20 captured requests contains the date, weekday or "today is".
- **Q10 "When is my next dentist appointment?":** the context holds the past visit (2026-05-02), the stale
  original booking (3 Nov 9:00) and the real one (5 Nov 10:30). Nothing tells the model which one is next.
- **Q12 "Which bills are due soon?":** both due dates (8 Oct and 7 Nov) are in the context, and "soon" can't be decided.
- **Q08 "What do I have tomorrow?":** nothing was retrieved, so the request is a bare user message with no
  system prompt and no date.
- **Owner:** `crates/nuggetd/src/handler.rs` (`ask_system_prompt`, `ask_request`).
- **Fix:** give the local date, weekday and time zone in every ask request, including the general path.

## T-F2 (High): email passages carry no date or sender
- **Evidence:** 0 of 99 email passages contain the Date or From text. Calendar passages do have `Start:` (14 of 14).
- **Cause:** the header is `[n] Title (source)` (`pack.rs` `header()`), and `nugget-extract/src/mail.rs` keeps
  Date and From only in metadata. `Citation.date` reaches the client, not the model.
- **Effect:** the model can't tell an original booking from its reschedule, offer v1 from v3, or an old bill from a new one.
- **Fix:** render the item's date (and the sender, for mail) in the passage header.

## T-F3 (High): date phrases filter on file mtime, not on event or mail dates
- **Cause:** the filter is `source_modified_at.unwrap_or(updated_at)` (`retriever.rs` ~492–501). The item time comes
  only from the connector's `modified_at` (`nugget-ingest/src/pipeline.rs:628`). ICS `start` and mail `date` are
  extracted but never used.
- **Q13 "What meetings did I have last week?":** returns a 7 Nov event, because its file was written last week.
  It misses the Atlas steering meeting held Mon 19 Oct, because that file was written 9 Oct.
- **Q16:** "dentist last week" returns next week's appointment.
- **Q18 "on 2026-11-05":** no file has that mtime, so the query silently falls back to an unfiltered search.
- **Google Calendar (code reading only):** the connector sets `modified_at` from the event's `updated` time
  (`gcal.rs:872`), so the same problem applies there.
- **Fix:** give each item an effective date (event start, mail Date, else mtime) and filter on it.

## T-F4 (High): a folder copied today breaks every date query
- **Setup:** after `cp -r` and `touch`, every item's date is 2026-10-30.
- **"This week":** 10 of 10 citations count as in range, including the August Lisbon note.
- **"Last week":** 0 of 10 in range, because the search falls back to unfiltered.
- **Fix:** same as T-F3, since content dates survive copying.

## T-F5 (Medium): no future date phrases
- **Not parsed** (`dates.rs`): "next week", "tomorrow", "next month", "upcoming", "soon", "this weekend",
  and long-form dates such as "Thursday 5 November 2026".
- **"What do I have next week?":** the context is newsletters that match the word "week", with 0 of 10 items about next week.

## T-F6 (High in text-only mode; to be confirmed with embeddings): generic period questions silently fall back to unfiltered keyword search
- **Cause:** `retriever.rs` `rank` (~351–381) removes the date phrase and searches the rest within the range.
  If nothing in range matches, it searches the original query with no filter.
- **Q01 "Summarise last week":** 1 of 10 citations in range, although 33 items fall in that week. The context is
  newsletters from July to September.
- **Q06 "What happened yesterday?":** 0 of 10 in range.
- **Q05 "What do I have this week?":** 0 of 10 in range.
- **Fix:** when the remaining words are generic, list the period's items by date. Never fall back silently;
  tell the model and the client.

## T-F7 (Low): the date filter result can't be seen over IPC
- `ContextResponse` and `AskStarted` don't say which range was parsed or whether the search fell back
  (`nugget-core/src/ipc.rs`).

## Works well
- **Past phrases parse exactly:** "this month" and "in October" stay in range, and "last week" does too whenever
  the remaining words match something.
- **Calendar passages always show times:** `Start:`/`End:` lines include the time zone.

## Harness note
- **Bug in run 1's corpus:** `gen_corpus.py` gave some filler items future mtimes and Date headers.
- **Fixed** after the first runner run: filler is now clamped to today.
