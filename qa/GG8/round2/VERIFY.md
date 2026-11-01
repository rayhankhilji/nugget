# GG8 round 2 verification sheet

Binary: `target/debug/nuggetd` built from integration e0d44e65. Run:
`GG8_LAB=<k> python3 qa/GG8/harness/lab.py run <scenario> --out <dir>`; evidence in
`<dir>/<scenario>/` (requests.jsonl = every request the fake internet received, full bodies;
ledger-final.json = egress ledger; summary.json = checks and notes).

| Id | Claim | Repro |
|---|---|---|
| R2-1 | Text sent through Double Tap (`draft.feedback` final_text, a reply about a biopsy) is still indexed under a `nugget-voice` source missing from `sources.list`, is returned to a default-scope MCP client, and is sent to a cloud model as Ask context and as a draft citation (`sensitive: false`), although every other untriaged item is now withheld (`excluded_sensitive` > 0) | `qa/GG8/scenarios/s52_voice_items_exposure.py` (all three checks); `qa/GG8/scenarios/s21_cloud_draft.py` (the draft request body and the `draft` ledger row's items) |
| R2-2 | A card number typed in the Double Tap thread is still sent to the cloud drafting model | `qa/GG8/scenarios/s21_cloud_draft.py`, check `draft.thread_with_card_number` |
| R2-3 | `draft.prewarm` with a cloud provider sends `GET https://api.anthropic.com/v1/models` with the API key (no content), ledgered under feature `prewarm`, which privacy.md does not list | `qa/GG8/scenarios/s21_cloud_draft.py`; requests.jsonl and ledger-final.json |
| R2-4 | A provider probe's embeddings test request is ledgered under feature `embed`, not `probe` | `qa/GG8/scenarios/s25_cloud_embed_triage.py`; ledger rows at the probe's timestamp |
| R2-5 (fixed?) | GG8-1 is fixed: an Ask follow-up's history that quotes a card number reaches the cloud with that turn replaced by a placeholder | `qa/GG8/scenarios/s53_followup_history_to_cloud.py`, check `followup.history_to_cloud_has_no_sensitive` should now PASS |
