# Lead findings (GG4), written repros

Common setup (repo root, commit = branch qa/proactive): `CARGO_INCREMENTAL=0 cargo build -p nuggetd`,
`cargo build --manifest-path qa/GG4/probe/Cargo.toml --target-dir target`, `apt-get install faketime`,
`python3 qa/GG4/persona/gen.py`.
Base run: `python3 qa/GG4/scenarios/replay.py --out W --days 1-2 --judge off` (W = any new dir), then
`target/debug/gg4-probe W/home 2026-10-27T11:59:00Z -14400` (daemon stopped) prints the parsed corpus.

## L1 — .ics events have no attendees in the proactive corpus → no meeting-prep cards (High/Blocker for .ics users)
Probe output: `EV Northwind checkout review 2026-10-28T14:00:00-04:00 ... attendees []` though
qa/GG4/persona/world/files/Calendar/ev-nw-review.ics has ATTENDEE lines for Dana and Marcus (external).
Expected: attendees parsed; a meeting_prep card on the mornings of Oct 27/28. Actual: no meeting_prep card on
any of 14 mornings for 4 external meetings. Cause: the ics extractor (crates/nugget-extract/src/ics.rs ~330) copies
only uid/start/end/timezone/all_day of the first event to top-level metadata (attendees, organizer, status live in
`extracted.events[]`); corpus `parse_event` (crates/nugget-proactive/src/corpus.rs ~637, ~673) reads
`extracted.attendees` / `extracted.organizer` / status. Fix: read from `extracted.events[0]` (or copy those keys up).

## L2 — TZID ignored: event times read in the user's current offset
Probe: `Flight BA178 JFK to LHR start 2026-11-03T19:00:00-04:00 end 2026-11-04T07:00:00-04:00`; the .ics says
DTEND;TZID=Europe/London:20261104T070000 (= 02:00 New York). Also ev-foundry.ics (TZID Europe/London 10:00) viewed
from New York before the trip reads as 10:00 -04:00. Cause: extractor `start` iso has no offset (tz kept in
`timezone`/`end_timezone`), corpus `parse_time` (corpus.rs ~588) assumes `offset` (user's). Fix: resolve TZID with a tz database.

## L3 — RRULE not expanded
ev-standup.ics: weekly MO–FR from 2026-10-05 09:30. Probe lists one event on 2026-10-05; brief.get on 2026-10-26
meetings = [] (expected "Design standup 09:30"). Cause: corpus does not expand `rrule`. Fix: expand within the brief/horizon window.

## L4 — No controllable clock in the daemon
crates/nuggetd/src/features/proactive.rs uses OffsetDateTime::now_utc() for refresh (888), clock loop (900, 908) and
every IPC call (937); exec.rs too. There is no debug hook to set "now", so simulating days needs LD_PRELOAD libfaketime
(qa/GG4/harness/nq.py). Fix direction: a test-only clock source (env var on nuggetd-e2e, like its RuntimeOverrides).

## L5 — Reminder action label shows a raw UTC timestamp with nanoseconds (Low)
Day 1 card "No answer from Sofia Alvarez: brunch?" offers action label `Remind me 2026-10-26T14:00:00.002070842Z`
(user is in New York, so it means 10:00 local). Expected: "Remind me today at 10:00". Owner: action label in
crates/nugget-proactive (Reminder label) / convert.rs.
