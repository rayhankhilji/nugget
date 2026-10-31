# GG2 explore: retrieval quality and citation mechanics (draft, explore layer)

Commit 01f9e95, release nuggetd. Text-only retrieval (no embeddings). Home `/tmp/claude-0/gg2-retrieval2/home`.
Corpus `qa/GG2/corpus`: 315 items, one chunk each.

Probes:
- `probe.py HOME OUT QUERYFILE [budget]`: dumps `retrieve.context` (set `CORPUS=` to add and ingest a source first).
  Query files: `q1.txt`, `q2.txt`, `q_mtime.txt`.
- `ctx_stats.py`: statistics over the packed contexts.
- Outputs: `out/`.

Baseline (`ask_qa.py --retrieval-only` + `score.py`), share of turns whose packed context holds a supporting item:

| category | retrieved |
|---|---|
| overall (149 turns) | 87.4% |
| lookup | 93.8% |
| time | 76.9% |
| follow-up | 80.0% |

## R-1 (High; same root cause as T-F3/T-F4): item dates come from file mtime, not the mail Date or the event DTSTART
- **Where:** `nugget-connector-files/src/scan.rs:513`, `nugget-ingest/src/pipeline.rs:628`,
  `nugget-retrieve/src/retriever.rs:546` (`Citation.date`).
- **Copied files:** after copying, all five Lumen bills are dated 2026-10-30. "Lumen bill this month" returns all five,
  July's first (`out/mtime-contexts.json`).
- **Main corpus:** `dentist.ics` is dated 2026-10-21, not its 2026-11-05 event. "dentist in november 2026" falls back
  to unfiltered search and ranks last May's visit first.

## R-2 (High; same as T-F1/T-F2): the model never sees a date, today's or any item's
- **What the passages look like:** `[3] Appointment confirmation (file:///…dentist-booked.eml)` followed by the body only.
  An email has no sender and no sent date. Calendar passages do have `Start: … (UTC)`, in UTC only.

## R-3 (Medium; same as T-F5): future phrases are not parsed
- **"What do I have next week?":** returns 5 newsletters that match "week" (T02).
- **"What's on my calendar this week?":** also newsletters, because "calendar" is not in the .ics text.
- **"What's on my calendar on Thursday 5 November 2026?":** `dentist.ics` is not in the top 10 (T12).
- **"in november" asked on 2026-10-30:** resolves to November 2025.

## R-4 (Medium): keyword mismatch gives 0 passages, and Ask then drops its system prompt
- **L09 "When do I fly to Japan?":** 0 passages. Neither "Japan" nor "fly" appears in the corpus;
  "When is my Kyoto flight?" works.
- **L35 "What am I allergic to?":** 0 passages. The porter stemmer turns "allergic" into "allerg" and "allergies" into "allergi".
- **L16 "How much do I pay for internet now?":** the only "internet" item is the phishing mail, which comes back
  first; none of the four real bills appears.
- **X08 "upcoming appointments":** only items containing the word "appointment"; the physio and calendar events are missing.
- **Zero passages:** `ask_request` sends no system prompt at all. The "(no relevant passages were found)" text in
  `ask_system_prompt` is never used.
- **To re-check with embeddings on the runner:** L09 and L16.

## R-5 (Medium): near-identical recurring items rank oldest first; "now" and "latest" have no effect
- **"How much is my Lumen Fiber bill now?":** ranks July, then October, then the phish, then August and September.
- **Cause:** recency is off by default (`config.rs` `recency: None`).
- **Fix:** break ties toward newer items, add a recency boost for "now/latest/current", and give the model the item dates.

## R-6 (Medium; same as FU-2): when the rewrite model fails, follow-ups are searched as typed
- **Examples:** "What time?" returns 5 "Focus time" calendar entries; "Who is it with?" returns unrelated mail.
  8 of 41 follow-up turns missed their supporting item in the text-only baseline.

## R-7 (Low): the 2,000-token budget is never the limit; the 10-item cap is
- **Evidence:** budgets of 2000 and 8000 give identical contexts. The median context is 8 items and about 2,533 characters;
  65 of 145 contexts hit exactly 10 items.
- **Impact:** list questions ("all upcoming appointments", "summarise last week") are capped at 10 items.
- **Fix:** scale `context_max_items` with the budget.

## R-8 (Low): short items (including the injection fixtures) rank high under BM25 length normalisation
- **Occurrences in 145 contexts:** `book club.md` in 32, `onboarding-call.ics` in 19 (ranked [1] nine times),
  `dentist-spoof.eml` in 14 (ranked [1] for "When is my dentist appointment?").

## Works well
- **Citation numbering:** `[n]` headers are exactly 1..k and match `citations[].n` and `url`, with no duplicate items
  in a context (0 mismatches over 145 contexts).
- **Titles:** Subject for mail, SUMMARY for events, stem or H1 for notes.
- **.ics passages:** they carry Event/Start/End/Location.
- **"last week":** filters correctly, by mtime.
- **Keyword lookups with matching words are strong:** budget questions, and offer letters v1/v2/v3 together with their
  "Version: vN, issued <date>" lines.
- **Packing:** it never exceeds the budget and degrades smoothly at budget 300.
