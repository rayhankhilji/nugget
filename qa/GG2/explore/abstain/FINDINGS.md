# GG2 explore: "not in your items", the Chat flow and edge cases (draft, explore layer)

Commit 3593ef7. Release nuggetd, text-only retrieval. Home `/tmp/claude-0/gg2-abstain2/h` with knowledge, vault
and MCP HTTP off. Corpus `qa/GG2/corpus` (315 items), ingested through a `files` source. `slow_llm.py` is a fake
OpenAI-compatible model: it logs requests and streams N deltas with a delay, so streams can be cancelled.

```
cd qa/GG2/explore/abstain
H=/tmp/claude-0/gg2-abstain2/h; mkdir -p $H
timeout 20 ../../../../target/release/nuggetd --home $H
python3 slow_llm.py --port 18793 --log $PWD/out/requests.jsonl --chunks 12 --delay 0.1 &
python3 abstain_probe.py --home $H --corpus /home/user/nugget/qa/GG2/corpus \
  --questions /home/user/nugget/qa/GG2/questions.jsonl --out out --log out/requests.jsonl --port 18793
```

## GG2-ab1 (High): every "not in your items" question gets irrelevant passages, because there is no relevance floor
- **Result:** 0 of 20 abstain questions (A01–A20) went general. All 20 sent the sourced prompt with 1–10 unrelated passages.
- **A01 "What is my passport number?":** top passage is "Your City Power bill is ready", at normalised score 1.0.
- **A03 "How much was the hotel in Madrid?":** only passage is the Porto hotel invoice.
- **A05 "When is my flight to Berlin?":** only passage is the WhatsApp chat holding the Kyoto booking
  ("QX7H2P, both of us, $2,318 total"). This is a direct trap for a wrong answer.
- **A16 "Which hotel did I book in Tokyo?":** bookstore receipts and the Lisbon booking.
- **Cause:** `nugget-retrieve/src/rank.rs` `normalise()` makes the top hit 1.0. `pack_context_with` (`retriever.rs:289`)
  packs up to `context_max_items` with no absolute floor.
- **Fix:** keep an absolute relevance signal (raw BM25 or cosine) and drop passages under a floor. If nothing passes,
  answer as general with an explicit "not in your items" instruction.

## GG2-ab2 (Medium): general requests are forced into sourced mode whenever any word matches
- **Example:** "Write a haiku about autumn" packs 10 passages (autumn newsletters, standup notes). The prompt then tells
  the model to answer only from them. "What is the capital of France?" went general.
- **Fix:** the same floor, plus a prompt that allows questions that aren't about the user's data.

## GG2-ab3 (Medium; to re-check with embeddings): non-English questions find nothing and become a general answer
- **Example:** "¿Cuándo es mi cita con el dentista?" and "歯医者の予約はいつですか" both went general with 0 citations,
  while the English control gets 6 dentist passages.
- **What the model gets:** a bare user message with no system prompt, so it may invent an appointment.
- **Fix:** a multilingual embedding model, or a query rewrite for retrieval. At minimum, a general-path system prompt
  that says the user's items held nothing.

## GG2-ab4 (Low): an unbounded question length goes straight to the model
- **Example:** a 200,000-char query was accepted and became a 201,448-byte model request. History turns, by contrast,
  are capped at 4,000 chars.
- **Owner:** `handler.rs` `retrieve_ask`.
- **Fix:** cap the length with an `invalid_params` error such as "question too long".

## GG2-ab5 (Low): the general label is misleading when there are no sources yet or ingest is still running
- **Example:** asked before any source existed, or right after `sources.add`, Chat shows "No matching passages were
  found in your sources, so this is a general answer."
- **Gap:** the Search page has `search.indexingHint` (`pages/Search.tsx:47`); Chat (`pages/Chat.tsx:749`) has no
  equivalent.

## GG2-ab6 (Low, code review): with a cloud model and only sensitive matches, two notes contradict each other
- **Example:** `general` plus `excluded_sensitive > 0` renders "No matching passages were found…" together with
  "N sensitive items were left out…". Not run.

## GG2-ab7 (Low): IPC validation errors are raw serde text
- **Example:** `"invalid value: integer \`-5\`, expected u32"`, with no field name. Not user-facing today.

## GG2-ab8 (Low, docs): `AskStarted.general` is undocumented
- **Where:** `docs/book/src/reference/ipc.md:81`. Neither the field nor the no-system-prompt behaviour is described.

## Desktop review notes (code reading)
- **No `[n]` markers in the reply:** every passage goes under "N more passages the model saw". Reasonable.
- **Out-of-range `[12]`:** left as plain text, no crash. Good.
- **General answers:** the label is muted small text below the reply, so the user reads the possibly confident
  answer first. For personal-data questions it doesn't say the answer doesn't use their data.

## Works well
- **Bad input errors:** an empty or whitespace question gives "the question is empty"; `budget_tokens 0` gives
  "must be positive"; an unknown provider gives a friendly -32001.
- **Budget extremes:** `budget_tokens` 10^9 is clamped.
- **Odd query text:** FTS operators and SQL-ish input are harmless.
- **`chat.cancel` mid-stream:** immediate `chat.done {"error":"cancelled"}` and the upstream HTTP connection closed;
  cancelling is idempotent, and the next ask works.
- **Concurrency:** 4 parallel asks on one connection all succeeded.
- **History bounding:** 100 turns become 12 history messages; huge turns are cut to 4,000 chars; system turns are dropped.
- **During ingest:** asks never errored.
