# GG2 explore: follow-up questions in Chat (draft, explore layer)

Setup:
- Commit 37ff801, release nuggetd, 2026-10-30.
- No LLM ran. `fake_llm.py` is an OpenAI-compatible stand-in that streams a canned answer. For rewrite requests
  it can answer well, slowly, with HTTP 500, or with `<think>` text.
- Corpus `qa/GG2/corpus` (315 items), ingested through the `files` source. Retrieval was keyword-only (no embedding model).
- "hit" means a support item was among the `AskStarted` citations.

```sh
H=/tmp/claude-0/gg2-followup/h
target/release/nuggetd --home $H       # once, to write config.toml; then stop it
cd qa/GG2/explore/followup
./restart.sh <variant> <tag>           # mkconfig.py variant + fake_llm.py on :18431 + nuggetd
python3 drive.py $H 18431 out/<tag>.jsonl [--ingest] [--extra] [--convs F01,X04] \
        [--rewrite-mode map|error|slow|think] [--delay S] [--gap S]
python3 summarize.py out/<tag>.jsonl
```

Variants:
- `heuristic`: no local chat provider, so the heuristic rewrite runs.
- `model`: the fake is a local provider on triage/light/synthesis.
- `default`: the shipped Ollama entry first, nothing listening on :11434, the fake on synthesis only.
- `default_rt`: dead Ollama first, then the fake on all chat tiers. This is the routing order a default-config
  user gets with the on-device runtime.

`drive.py` sends history the way `Chat.tsx` does: `[n]` markers stripped, last 24 messages. Per-turn table: `out/table.md`.

Hits over the 29 follow-up turns that have support items:
- original follow-up text: 20/29
- heuristic rewrite: 27/29
- good model rewrite: 29/29

## FU-1 (High): default config with Ollama not running: follow-ups aren't rewritten and pay ~1.9 s for the failed attempt
- **Repro:** `./restart.sh default_rt default_rt; python3 drive.py $H 18431 out/default_rt_gap35.jsonl --convs F01,F13,F04 --gap 35`.
- **35 s between turns:** 4/4 follow-ups had `query_rewrite` null, `started_ms` 3709–4357, and missed (e.g.
  "Who is it with?").
- **Log:**
  - `WARN query rewrite failed; searching the original query provider=local error="network error: ... 127.0.0.1:11434 ... (Connect)"`
  - then `WARN ask: provider failed; trying next provider=local`
- **No gap:** the same turns were rewritten in 5–9 ms and hit, because Ollama was still in the router's 30 s failure cooldown.
- **`default` variant:** 6/6 follow-ups were not rewritten, 1.7–2.1 s each.
- **Cause:** `Retriever::rewrite_query` (`crates/nugget-retrieve/src/retriever.rs`) uses only `rewrite_candidates().first()`.
  It never tries the next candidate or the heuristic. The shipped `[[providers]] id="local"` Ollama entry is ranked
  before the runtime's `local-chat`.
- **Owners:** nugget-retrieve; the default config template; routing order in `crates/nuggetd/src/local.rs`.
- **Fix:** try the candidates in order within one timeout budget, skipping providers in cooldown; fall back to the
  heuristic; use a short connect timeout for on-device providers; don't route to the example Ollama entry until a
  probe of it has succeeded.

## FU-2 (Medium): a slow or failing rewrite model searches the raw text, never the heuristic
- **Slow** (10 s delay): each follow-up waits the full 3 s timeout, then searches the raw text. 3/3 missed
  ("Who is it with?", "And where is it?", "What time?"). Log: `WARN query rewrite timed out`.
- **HTTP 500:** 9/9 follow-ups had `query_rewrite` null.
- **Fix:** use the heuristic on error, timeout and unusable output (`rewrite_query`).

## FU-3 (Medium): contractions defeat `looks_like_followup`
- **Cause:** `words()` splits "what's" into `what` + `s`, and `s` is not a stopword, so the query has two content words.
- **Effect:** "What's the address?" (after the dentist question) gets no rewrite and misses; "What is the address?"
  is rewritten and hits. "What's the salary?" and "What should I bring?" are not treated as follow-ups either.
- **Owners:** `crates/nugget-retrieve/src/rewrite.rs`, `STOPWORDS` in `rank.rs`.
- **Fix:** ignore one-letter tokens and contraction tails when counting content words; treat modal verbs as non-content.

## FU-4 (Medium, heuristic path): a follow-up to a follow-up loses the topic
- **Cause:** `heuristic_rewrite` takes words only from the last user turn.
- **Examples:** "And where is it?" after "Who is it with?" is left unchanged and misses. "What gift did I plan for her?"
  becomes "... should bring".
- **Fix:** walk back through earlier user turns, or carry the previous rewrite forward.

## FU-5 (Medium, heuristic path): a topic switch drags the old topic into the context
- **Example:** "What about Kyoto?" after the Lisbon hotel question becomes "What about Kyoto? much hotel lisbon". The Lisbon
  hotel mail (EUR 612.40) ranks above the Kyoto items. 2/2 topic switches.
- **Fix:** don't append the previous turn's words when the follow-up names its own new subject.

## FU-6 (Medium, only for servers that return reasoning in the answer text): `<think>` becomes the search query
- **What happens:** `clean_reply` takes the first non-empty line, so the rewritten query is literally `<think>`.
  5/5 follow-ups missed, and the UI would show "Searched for: <think>".
- **Scope:** the runtime's own slots disable thinking. User-configured providers, including the default Ollama
  `qwen3:4b` entry, do not.
- **Fix:** strip `<think>…</think>` blocks; reject replies that start with `<`.

## FU-7 (Low, code reading only): the rewrite request lists no items in the egress ledger
- The rewrite request carries assistant text taken from the user's items, but sets no `req.items`, so a LAN
  provider's ledger entry lists no items.

## Works well
- **Cloud is never used for rewrites:** 0 of 53 asks.
- **The rewrite prompt is fenced:** data `<<<`/`>>>`, temperature 0, 96 max tokens, last 6 turns, system turns dropped.
- **A working local model rewrites well:** 29/29 hits, at 3–13 ms with the fake model.
- **The heuristic handles pronouns and ellipsis:** "When is he on leave?" gets "+ bob lindqvist", "What time?" gets "+ farewell drinks".
- **A general answer doesn't stop the next question using the items:** after one, the next question still retrieves
  from them. Caveat: the unsupported general answer is passed on as an ordinary assistant turn.
