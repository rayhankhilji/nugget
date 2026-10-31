# Verification: follow-ups and abstention (W1–W7)

A fresh agent reproduced these from the written repros in `qa/GG2/explore/{followup,abstain}/FINDINGS.md`.

Setup:
- HEAD 92e18f0.
- Homes: `/tmp/claude-0/gg2-verify-b/{ha,hf}`, each with 315 items ingested through a `files` source.
- Fake model providers on ports 18531 and 18893.

| # | Verdict | Evidence |
|---|---|---|
| W1 FU-1 dead default Ollama | REPRODUCED | With a 35 s gap, 3 of 3 follow-ups got `query_rewrite` null, `started_ms` 3914–4280, and missed. The log reads `query rewrite failed … 127.0.0.1:11434 … (Connect)` then `ask: provider failed; trying next`. With no gap they were rewritten in 7–9 ms. **New:** first turns also pay about 1.6–2.1 s for the answer request failing over away from the dead Ollama entry. |
| W2 FU-2 no heuristic fallback | REPRODUCED | HTTP 500: 4 of 4 follow-ups not rewritten and missed. Slow model: `started_ms` about 3010, 3 of 3 missed (`query rewrite timed out`). With no rewrite model, the heuristic rewrites the same turns and they hit. |
| W3 FU-3 contractions | REPRODUCED | "What's the address?": not treated as a follow-up, and missed. "What is the address?": rewritten to "dentist appointment address", and hit. Same result with the model and with the heuristic. |
| W4 FU-6 `<think>` query | REPRODUCED | 4 of 4 follow-ups got `query_rewrite: "<think>"` and missed. |
| W5 ab1 no relevance floor | REPRODUCED | A01–A20: 0 of 20 went general. A05 "When is my flight to Berlin?" packs only `whatsapp-jun`, which holds the Kyoto booking ("QX7H2P, both of us, $2,318 total"). |
| W6 ab2/ab3 | REPRODUCED | "Write a haiku about autumn" is sourced, with 10 passages. "¿Cuándo es mi cita con el dentista?" goes general with 0 citations, and the request is `[{"role":"user",…}]` only. |
| W7 ab4 unbounded query | REPRODUCED | A 200,000-char query was accepted; the model request was 201,448 bytes. |
