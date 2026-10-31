# GG8 area B: what is sent to cloud providers, per feature

Binary: target/debug/nuggetd built from 3aeda23. Sinkhole lab, no local model installed. Cloud
providers at their real default hosts answered by the sink mocks (plus `mock_schema.py` for structured
output and probes, `mock_agent.py` for agent tool calls). Repro: `sudo GG8_LAB=1 python3
qa/GG8/harness/lab.py run qa/GG8/scenarios/<file>.py --out /tmp/gg8-B`. Exact request bodies:
`<scenario>/bodies-*.json`.

| Scenario | Area | Verdict | Checks per run | Runs |
|---|---|---|---|---|
| s20 | Ask, follow-up, chat | works with caveat (B-3) | 15/16 | 4 |
| s21 | Double Tap draft | works with caveats (B-4, B-5) | 17/19 | 4 |
| s22 | Draft feedback | works with caveat (B-5) | 3/4 | 4 |
| s23 | Knowledge extraction | works, caveat B-6 | 6/6 | 3 |
| s24 | Agent | works (30 calls, 30 ledger rows) | 4/4 | 1 |
| s25 | Embeddings, triage | broken (B-1) | 6/8 | 4 |
| s26 | Probe, loopback `locality = "cloud"` | works, caveat B-7 | 11/11 | 1 |
| s27 | Today judge | works, caveat B-2 | 7/7 | 4 |
| s28 | `[retrieve] allow_cloud_embedding` alone | no request observed | 4/5 | 1 |

## Candidate findings (each failure reproduced in every run)

- **B-1 (High)**: with `[ingest] allow_cloud_embeddings` and `[retrieve] allow_cloud_embedding` on,
  indexing correctly leaves the four sensitive notes without vectors, but a search then sends their
  full text (card 4111 1111 1111 1111, SSN 512-34-6789, IBAN, API keys) to `/v1/embeddings`: the MMR
  step embeds "passages stored without a vector", which are exactly the sensitive ones. Owner
  `nugget-retrieve` (W12, DD4). Fix: never cloud-embed sensitive or unknown-status passages; fall back
  to word overlap for them.
- **B-2 (Medium)**: `embed` and `proactive_judge` ledger rows have empty `items` although the judge
  request quotes mail evidence and the embedding requests carry item text.
- **B-3 (Low)**: Ask sends each passage's absolute `file://` path (home directory, user name) to the
  cloud.
- **B-4 (Medium)**: a card number in the Double Tap thread is sent to the cloud model unscanned
  (`nugget-draft` / `features/draft`).
- **B-5 (Low)**: with only a cloud provider usable, `draft.prewarm` reports `local-chat`/`on_device`
  and `draft.read_ahead` answers `started`/`done` instead of `skipped: "cloud"`. Nothing was sent.
- **B-6 (Low)**: `knowledge.status.extractor_provider` stays null while cloud extraction runs
  (`crates/nuggetd/src/knowledge.rs`).
- **B-7 (Low)**: a probe's embeddings check is ledgered as `embed`, not `probe`.

## What works well

- Ask and drafts withhold all sensitive items and report `excluded_sensitive`; a follow-up's rewrite
  never goes to the cloud.
- Ledger rows for ask, chat, draft, knowledge, agent and probe match host and feature, with
  `bytes_sent` equal to the body size; item lists accurate except B-2.
- Secure fields and password-manager apps are refused before anything is sent; screenshots go only
  when allowed.
- Read-ahead and prewarm sent nothing to the cloud in any run.
- Knowledge sends nothing without `cloud_sources`, and only non-sensitive items with it.
- Triage never went to the cloud; no embeddings without the opt-in.
- The Today judge sends nothing with `off` or `local`, and leaves sensitive cards out with `cloud`.
- A loopback server marked `locality = "cloud"` is treated and ledgered as cloud.
- API keys reach only their own host; headers are minimal (`user-agent: nugget/0.0.1`);
  `openai_responses` sends `store: false`.
