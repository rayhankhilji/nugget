# Verification: time and retrieval (V1–V8)

A fresh agent reproduced these from the written repros in `qa/GG2/explore/{time,retrieval}/FINDINGS.md`.

Setup:
- Corpus: `gen_corpus.py --today 2026-10-30`, 315 items.
- Homes: `/tmp/claude-0/gg2-verify-a/h`, and `h2` for the copied corpus.
- Model requests captured with `capture_llm.py` on port 18797.

| # | Verdict | Evidence |
|---|---|---|
| V1 no date in prompt | REPRODUCED | `2026-10-30` appears in none of the 20 captured requests. The system prompt is only the fixed instructions plus the passages. |
| V2 mail passages lack Date/From | REPRODUCED | 0 of 99 `.eml` passages have a `^(Date\|From):` line; 14 of 14 `.ics` passages have `Start:`. |
| V3a date = mtime | REPRODUCED | "What meetings did I have last week?" returns `onboarding-call.ics` (DTSTART 2026-11-07, mtime 10-23). "dentist last week" returns `dentist.ics` (DTSTART 11-05). `atlas-steering.ics` (DTSTART 10-19, mtime 10-09) is missing. |
| V3b copied folder | REPRODUCED | All 167 citations are dated 2026-10-30. "this week": 10 of 10 in range, Lisbon note included. "Summarise last week": 0 of 10. |
| V4 silent fallback | REPRODUCED | "Summarise last week": 1 of 10 citations in range, against 33 items in that week. "What happened yesterday?": 0 of 10. |
| V5 no future phrases | REPRODUCED (behaviour; parser claim from code) | "What do I have next week?" returns 9 newsletters and `kyoto-flight.eml`, 0 of 10 in range. `dates.rs` `anchor()` has no next/tomorrow form. |
| V6 oldest bill first | REPRODUCED | The order is July, October, phish, August, September. |
| V7 0 passages means no system prompt | REPRODUCED | "When do I fly to Japan?" and "What am I allergic to?": 0 citations, and the request roles are `['user']` only. The control "When is my Kyoto flight?" gets a system prompt. |
| V8 item cap binds | REPRODUCED | Contexts are identical at budgets 2000 and 8000 (86 of 86). 39 of 86 contexts have 10 items. |
