# GG8 verification sheet

Each candidate below is a claim plus a repro. A verifier runs the repro from a clean lab and says
**reproduced / not reproduced / partly**, with the quoted evidence (check line, request body excerpt,
ledger row). Lab runs need root: `GG8_LAB=<k> python3 qa/GG8/harness/lab.py run <scenario> --out <dir>`;
the binary is `target/debug/nuggetd` (built from 3aeda23). Run each failing scenario 3 times.
Evidence lands in `<dir>/<scenario>/` (requests.jsonl = every request the fake internet received,
with full bodies; ledger-final.json = the daemon's egress ledger; summary.json = checks).

| Id | Claim | Repro |
|---|---|---|
| V1 | An Ask follow-up to a cloud provider forwards the client-supplied `history` unscanned: an earlier local answer quoting a card number (4111 1111 1111 1111) and the sensitive item's text goes to api.anthropic.com, and the ledger row's `items` is empty | `qa/GG8/scenarios/s53_followup_history_to_cloud.py`; check `followup.history_to_cloud_has_no_sensitive` |
| V2 | Text the user sent through Double Tap (`draft.feedback` final_text) is stored as an item of a `nugget-voice` source that is absent from `sources.list`, is returned to an MCP client created with default scopes, and is put into a cloud Ask's context | `qa/GG8/scenarios/s52_voice_items_exposure.py`; checks `voice.source_visible_in_sources_list`, `mcp.default_scope_hides_sent_drafts`, `cloud_ask.no_sent_drafts` |
| V3 | With `[ingest] allow_cloud_embeddings = true` and `[retrieve] allow_cloud_embedding = true`, a search sends the full text of the sensitive notes (card, SSN, IBAN, API keys) to the cloud `/v1/embeddings`; ledger `embed` rows have empty `items` | `qa/GG8/scenarios/s25_cloud_embed_triage.py`; checks `optin.embed.no_sensitive_content`, `optin.ledger_items_accurate` |
| V4 | A card number in the Double Tap thread text goes to the cloud drafting model unscanned | `qa/GG8/scenarios/s21_cloud_draft.py`; check `draft.thread_with_card_number` |
| V5 | With only a cloud provider usable, `draft.read_ahead` answers `started/done` instead of `skipped: "cloud"` and `draft.prewarm` reports `local-chat`/`on_device` (nothing is sent) | `qa/GG8/scenarios/s21_cloud_draft.py`; check `read_ahead.skipped_cloud`, plus requests.jsonl |
| V6 | The Today judge on `judge = "cloud"`: ledger `proactive_judge` rows have empty `items` while the request body quotes card evidence | `qa/GG8/scenarios/s27_cloud_proactive_judge.py`; compare ledger-final.json with requests.jsonl |
| V7 | Ask sends each passage's absolute `file://` path (home directory, user name) to the cloud | `qa/GG8/scenarios/s20_cloud_ask_chat.py`; check `ask.no_local_paths` |
| V8 | At `NUGGET_LOG=debug`, MCP tool arguments and full tool results (passage text) are written to the daemon log; at `info` they are not. The audit log row of an MCP `tools/call` holds the query verbatim | `qa/GG8/scenarios/s50_logs_audit.py` and `s51_logs_audit_info.py`; checks `logs.no[...]`, `store.noncontent_tables_clean` |
| V9 | IMAP: STARTTLS attempts (port 143) get no ledger row; implicit-TLS rows name the host with `:993` | `qa/GG8/scenarios/s41_caldav_imap.py`; ledger-final.json vs. raw TCP entries in requests.jsonl |
| V10 | A probe's embeddings check is ledgered as `embed`, not `probe`; `knowledge.status.extractor_provider` stays null while cloud extraction runs | `qa/GG8/scenarios/s26_probe_locality.py`, `s23_cloud_knowledge.py` |
| V11 | The desktop app with default settings requests `https://github.com/rayhankhilji/nugget/releases/download/updater-feed/stable.json` about 60 s after launch; no ledger row; silent with the channel `off` | Runner only: workflow run 41117729320, job "Desktop app traffic (Linux)" (`qa/GG8/real/scenarios/s65*`); read the job log's GG8-SUMMARY block and `apps/desktop/src-tauri/src/updater.rs` defaults |
