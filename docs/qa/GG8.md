# GG8: privacy and egress, verified from the outside

QA package GG8 (wave GG), branch `qa/privacy`. The binary under test is the integration branch at
**3aeda23** (DD2 and DD4 included), merged into `qa/privacy`. Every verdict below comes from watching
traffic, not from reading the code.

## How it was tested

`qa/GG8/harness/` runs `nuggetd` in a Linux network namespace whose only way out is one veth pair:

- **DNS.** A logging DNS server sees every query. In *sinkhole* mode it answers every name with the
  sink, 203.0.113.10 (TEST-NET-3, which Nugget classifies as `cloud`). In *internet* mode it forwards
  to a real resolver.
- **The fake internet.** In sinkhole mode a TLS sink terminates HTTPS for any name, using a test CA
  installed in the system trust store. It records every request in full (headers and body) and answers
  from mocks of the real APIs at their real hosts:
  - `api.anthropic.com`, `api.openai.com`, and an OpenAI-compatible cloud;
  - a LAN model server on 192.168.88.1;
  - `slack.com`, `api.notion.com`, CalDAV, OAuth token endpoints, and raw TCP loggers for IMAP and SMTP ports.
- **Packet capture.** `tcpdump` records every packet that leaves the namespace.
- **Comparison.** Each run compares the observed hosts with the egress ledger (`egress.recent`). It
  also searches requests, DNS, logs, every plaintext file under the data root, and the decrypted store
  (`sqlcipher` with the store key) for **canary strings** planted in the synthetic corpus:
  - `GG8CANARY-SENS-*` items carry a Luhn-valid card number, an SSN, an IBAN, and API and AWS keys;
  - `GG8CANARY-NORMAL-*` items are ordinary notes;
  - API keys, tokens, passwords, draft text and MCP queries carry canaries too.

The scenarios are `qa/GG8/scenarios/s*.py` (cloud container, debug build, three runs each for every
failing check) and `qa/GG8/real/scenarios/` (GitHub runners, release build, real models). The real-model
workflow is `.github/workflows/qa-GG8.yml` (dispatch only). Run 1 is
[41117729320](https://github.com/rayhankhilji/nugget/actions/runs/41117729320) on ubuntu-latest (AMD
EPYC 7763, 4 vCPU, 16 GB) with `qwen3-1.7b-q8` (chat) and `nomic-embed-text-v1.5-q8` (embedding). Run 2
is [41189149310](https://github.com/rayhankhilji/nugget/actions/runs/41189149310) on the same runner
type with the same models.

Per-area details and evidence:

- `qa/GG8/results/{A,B,C,D}/`, with B and C in `FINDINGS.md`;
- `qa/GG8/real/FINDINGS.md` and `qa/GG8/real/results/`;
- the verification sheet, `qa/GG8/VERIFY.md`.

To run one scenario: `sudo GG8_LAB=<0-3> python3 qa/GG8/harness/lab.py run qa/GG8/scenarios/<file>.py --out /tmp/x`.

## Verdict

| Capability | Verdict | Evidence |
|---|---|---|
| Default install, local only: nothing leaves the device | **Works** | s01, s10: 0 DNS queries, 0 requests and 0 ledger rows over search, Ask, MCP, knowledge, Today, plugins list, 5 min idle and a restart. Runner s61 (real llama-server, 300 s idle): no DNS, no IPv4 packet off the namespace, no new ledger row |
| Onboarding with local models: downloads only, all ledgered | **Works** | Runner s60: DNS saw exactly `github.com`, `release-assets.githubusercontent.com`, `huggingface.co` and `us.aws.cdn.hf.co`, all with `model_download`/`runtime_download` rows (6 rows, 2.0 GB). s12: failed attempts are ledgered too, and downloads are pinned to a revision |
| Ledger completeness (every off-device request has a row with the right host and feature) | **Works with caveats** | Across all area-B and area-C runs there were no unledgered HTTP hosts (ask, chat, draft, knowledge, agent 30/30, probe, judge, embed, sync for Slack, Notion, CalDAV, GitHub and Gmail, and OAuth). Caveats: `items` is empty on several feature rows (GG8-6), IMAP STARTTLS attempts have no row (GG8-11), and desktop update checks have no row (GG8-5) |
| Ask and chat with a cloud model: no sensitive items | **Broken** | Context passages: 4/4 sensitive withheld, `excluded_sensitive` reported, every run. But a follow-up's history is forwarded unscanned (GG8-1): 3/3 local, 2/2 with a real model |
| Double Tap with a cloud model | **Works with caveats** | Sensitive passages withheld; secure fields and password managers refused before anything is sent; screenshots only when allowed. A card number in the thread goes out unscanned (GG8-4); wrong read-ahead status (GG8-10) |
| Double Tap read-ahead | **Works** | Runner s62: reached only the local server, and was `skipped: "cloud"` when the draft would go to the cloud. Sinkhole s21: nothing sent in 4/4 runs |
| Knowledge extraction | **Works** | Nothing to the cloud without `cloud_sources`; only non-sensitive items with them (3 runs). Status caveat GG8-12 |
| Today judge | **Works with caveats** | `off`/`local` send nothing; `cloud` leaves sensitive cards out (4 runs); ledger `items` empty (GG8-6) |
| Agent with a cloud model | **Works** | 30 model calls, 30 ledger rows; the context sent matches the docs |
| Embeddings and triage | **Broken with the cloud-embedding opt-in** | Triage never went to the cloud; no embeddings without the opt-in. With the opt-in, search sends sensitive notes (GG8-2), 3/3 |
| Local-network provider | **Works** | s11: LAN requests ledgered (`ask`, host `192.168.88.1:8080`) with all 7 items listed |
| Secrets (API keys, connector tokens, passwords, OAuth codes and client secrets) | **Works** | Each reached only its own host, in all runs; none in logs (info or debug), plaintext files or audit rows; `client_secret` refused in settings |
| Connectors against mocks (Slack, Notion, CalDAV, IMAP, GitHub, Google OAuth) | **Works** | 147/150 checks over 3 runs (the 3 failures are one non-privacy status check). CalDAV refuses to send credentials to a foreign calendar-home host; OAuth begin contacts nothing |
| MCP | **Works with caveats** | Listener on loopback only (unreachable from the host side of the veth); no egress; sensitive items hidden by default. But Double Tap's sent texts are visible by default (GG8-3) |
| Desktop app | **Works with caveats** | Runner s65, 2/2 runs: the only traffic is an update check on by default and unledgered (GG8-5); with the channel `off` it is silent |
| Logs, audit log, telemetry, crash reports | **Works with caveats** | No telemetry or crash-upload traffic in any run. Info logs are clean. Debug logs hold MCP content (GG8-7); audit holds MCP queries verbatim (GG8-8). The data dir is 0700, and config, store key and IPC are private |
| Plugins' network access | **Not tested** | No built `.nuggetplugin`; the guests need a `wasm32-wasip2` cargo build |
| macOS | **Not tested** | The pktap job uploaded an empty artifact in both runs (harness failure, not iterated within the shared usage limit) |
| Agent with local models | **Not tested** | Runner s61: `agent.run` refused with `no provider for the agentic tier can call tools` (the 1.7B chat model is not tool-capable), so no local agent traffic was observed; the cloud agent is covered by s24 |

## Findings

All findings marked **Verified** were reproduced by a separate verifier subagent from the written repro
alone (`qa/GG8/VERIFY.md`), 3/3 runs unless stated. The only candidate that did not reproduce (a
probe's embeddings check ledgered as `embed`) was dropped.

### GG8-1 (High): an Ask follow-up sends the earlier answer's sensitive content to the cloud

**Verified**, 3/3 sinkhole, 2/2 real model (runner s62 in runs 41117729320 and 41189149310).

**Repro (sinkhole):** `qa/GG8/scenarios/s53_followup_history_to_cloud.py`. The scenario configures an
`anthropic` provider and the corpus, then calls `retrieve.ask` with:

- `provider: "anthropic"`;
- `query: "And the wire details?"`;
- `history`: the user turn `"How will the Acme renewal be paid?"`, then an assistant turn (verbatim
  what the real local model `qwen3-1.7b-q8` answered in run 41117729320):
  `"The Acme renewal will be paid by card. The card number is 4111 1111 1111 1111, expiry 09/29, … GG8CANARY-SENS-1. [1]"`.

**Expected:** sensitive content never reaches a cloud model (privacy.md, "Sensitive items").

**Actual:** the body sent to `api.anthropic.com/v1/messages` contains
`{"role":"assistant","content":[{"text":"The Acme renewal will be paid by card. The card number is 4111 1111 1111 1111, …GG8CANARY-SENS-1. [1]"}]}`.
The passages themselves were withheld (`excluded_sensitive: 2`), and the ledger row is
`{"feature":"ask","host":"api.anthropic.com","items":[]}`.

**Real-model path (runner s62):** the user asks locally, the local model quotes the card from
`acme-payment.md`, and the follow-up goes to the cloud (fallback or explicit provider). This is the
normal Ask page flow with "local first, cloud fallback". The item's own text does not go; its content
goes through the history.

**Owner:** the `retrieve.ask` history path (`crates/nuggetd/src/handler.rs` and `nugget-retrieve`,
conversational search from X4; Ask quality is with EE3 today).

**Fix direction:**

- Before a cloud request, run the deterministic sensitive scanner over history turns.
- Drop (or replace with a placeholder) assistant turns whose citations included sensitive items. The
  daemon produced those citations, so it can keep them per stream, or the client can send them back.
- List those items in the ledger row.

### GG8-2 (High): with the cloud-embedding opt-in, search sends the sensitive notes to the cloud

**Verified**, 3/3.

**Repro:** `qa/GG8/scenarios/s25_cloud_embed_triage.py`, opt-in part: `[ingest] allow_cloud_embeddings = true`,
`[retrieve] allow_cloud_embedding = true`, and an OpenAI-compatible cloud embedding provider.

**Expected:** "sensitive items are never sent to a cloud model for … embedding".

**Actual:** indexing correctly leaves the 4 sensitive notes without vectors. About 0.1 s after the
query's own embedding, one `/v1/embeddings` request carries exactly those 4 notes in full:
`"input":["# Acme renewal: wire instructions … IBAN GB82 WEST …","… Card number 4111 1111 1111 1111 …","… AKIAIOSFODNN7GG8TEST …","… SSN 512-34-6789 …"]`.
All 6 `embed` ledger rows have `items: []`.

Two candidate sources: retrieval's MMR, which embeds "passages stored without a vector"
(`[retrieve] mmr_vectors`, `mmr_embed_chars`), or the re-embed pass that the opt-in enables. The
timing points to the search. Either way the sensitive notes are exactly the ones without vectors.

**Owner:** `nugget-retrieve` (MMR passage embedding; W12/DD4), and `nugget-ingest`'s re-embed if
that is the path.

**Fix direction:**

- Never send sensitive or unknown-status chunks to a non-local embedder anywhere.
- Fall back to word overlap for them in MMR.
- Add a regression test that asserts no sensitive chunk in any embeddings request body.

### GG8-3 (High): Double Tap's sent texts become a hidden source that MCP clients and cloud Ask read

**Verified**, 3/3.

**Repro:** `qa/GG8/scenarios/s52_voice_items_exposure.py`.

1. Call `draft.begin` on a thread ("Your biopsy results are back…").
2. Call `draft.feedback` with `outcome: edited` and `final_text: "Thanks doctor, I will call about the biopsy … GG8CANARY-VOICE-final"`.
3. Create an MCP client with default scopes (`{}`) and call `search`.
4. Ask with a cloud provider.

**Expected:** the user can see and control what is indexed; MCP clients get "all sources" the user
added; the voice profile is a drafting aid.

**Actual:**

- `sources.list` returns only `files-1`.
- The MCP `search` returns `<untrusted-content source="nugget-voice" item="feedback/…">Thanks doctor, … GG8CANARY-VOICE-final`.
- The cloud Ask body includes `[1] feedback/… (nugget-voice)\nThanks doctor, …`.
- In s21, the first citation of a cloud draft was also a `nugget-voice` item.

What the user typed in other apps (replies to a doctor, to a bank) is readable by any MCP client, which
may forward it to its own cloud model, and is sent as Ask context. No page shows this source, and no
scope excludes it.

**Owner:** `crates/nuggetd/src/features/draft` with `nugget-voice` (W1/CC1, EE2 for draft recall
today) for indexing feedback as items; `nugget-mcp` scopes (FF4).

**Fix direction:**

- Keep voice samples out of general retrieval and MCP, or make the source visible on the Sources page
  with its own switch, off for MCP by default.
- Run the sensitive scan on `final_text`.
- Document it in privacy.md.

### GG8-4 (Medium): a card number in the Double Tap thread goes to the cloud unscanned

**Verified**, 3/3.

**Repro:** `qa/GG8/scenarios/s21_cloud_draft.py`, check `draft.thread_with_card_number`: a thread message
`"Use card 4111 1111 1111 1111 for the deposit. GG8CANARY-THREADSENS-1"` and a cloud provider.

**Expected:** privacy.md lists the thread among what goes to a cloud drafter, but also says sensitive
content never goes to a cloud model, and the same deterministic scanner runs on items.

**Actual:** the body to `api.anthropic.com` contains
`<message from="other" author="Priya">\nUse card 4111 1111 1111 1111 for the deposit.` (retrieved
sensitive passages are withheld correctly).

**Owner:** `nugget-draft` / `features/draft` (W1, EE2).

**Fix direction:** scan field, thread and subject with the sensitive scanner when the provider is
cloud. Then either fall back to the local model, redact the matches, or ask the user. Say which in
privacy.md.

### GG8-5 (Medium): the desktop app checks for updates on github.com by default, unledgered

**Verified** from the runner logs (runs 41117729320 and 41189149310, job "Desktop app traffic (Linux)",
2/2) and the code; not re-runnable in the container.

**Repro:**

1. Build `nugget-desktop` with an updater key, as release builds have
   (`qa/GG8/real/scenarios/s65_desktop.py`, workflow job `desktop`).
2. Start it with default settings inside the namespace for 150 s.

**Expected:** privacy.md says "Nothing is sent to the Nugget project, ever", nothing beyond connector
syncs and model downloads leaves with the default configuration, and every request is in the ledger.
The M6-B plan said "signed opt-in updates".

**Actual:** the app sends `GET https://github.com/rayhankhilji/nugget/releases/download/updater-feed/stable.json`
(`user-agent: nugget-desktop/0.0.1`). The code sets `FIRST_CHECK_DELAY = 60 s` and the default
`channel: Channel::Stable` (`apps/desktop/src-tauri/src/updater.rs`, `settings.rs`). The ledger is
empty, and `unledgered: ['github.com']`. With the channel `off`: no DNS, no request.

**Owner:** `apps/desktop/src-tauri/src/updater.rs`, `settings.rs` (M6-B packaging; EE5 release
readiness), and privacy.md (docs).

**Fix direction:** decide and document. Either make the channel `off` until the user opts in (first
run asks), or keep it on but list the update check in privacy.md's defaults and record it in the ledger
(feature `update_check`, via the daemon, or a desktop-side ledger the Privacy page merges).

### GG8-6 (Medium): ledger rows without their items

**Verified** (V1, V3, V6), 3/3 each.

| Feature | Repro | What the request carries | Ledger `items` |
|---|---|---|---|
| `proactive_judge` | `s27_cloud_proactive_judge.py` | Card evidence from mail items, e.g. `"evidence": ["Our contract renews on 14 November 2026."]` | `[]` |
| `embed` | `s25`, with the opt-in | Chunk text, including the sensitive notes of GG8-2 | `[]` on all rows |
| `ask` follow-up | `s53` | History derived from `acme-payment.md` | `[]` |

**Expected:** "Items included: the items whose content (or derived text) was in the request".

**Owner:** the judge in `features/proactive` (W4/BB2); `nugget-ingest` and `nugget-retrieve` for
embed; the Ask handler for history.

**Fix direction:** pass the item refs the request was built from to the egress recorder, as
ask, chat and draft already do.

### GG8-7 (Low): debug logs contain MCP queries and full results

**Verified**, 3/3.

**Repro:** `s50_logs_audit.py` (`NUGGET_LOG=debug`) against `s51_logs_audit_info.py` (info).

**Actual:** at debug, the daemon log has
`"message":"received request", … CallToolRequest … arguments: Some({"query": String("GG8CANARY-QUERY-mcp …`
and `"message":"response message", … CallToolResult … text: "Text inside <untrusted-content> …`, with
passage text (also a sent Double Tap text, via GG8-3). Info is clean. Users are told to set
`NUGGET_LOG=debug` when troubleshooting and to attach logs.

**Owner:** `crates/nuggetd/src/mcp.rs` (the MCP SDK's debug spans; FF4).

**Fix direction:** filter the SDK's request and response targets to `info` in the default filter, or
wrap them to log method and size only.

### GG8-8 (Low): the audit log stores MCP queries verbatim

**Verified**, 3/3.

**Repro:** `s50`/`s51`, decrypted `audit` table.

**Actual:** `{"action":"mcp.tool_call","target":"search","detail":"{\"args\":{\"query\":\"GG8CANARY-QUERY-mcp Acme renewal\"},…`.
privacy.md says MCP calls are audited with "arguments summarised, never content". mcp.md says "long
strings clipped". A query can contain anything the AI app's user typed.

**Owner:** `crates/nuggetd/src/mcp.rs` audit summary (FF4); docs.

**Fix direction:** record the query's length and a keyed hash (or the first few words), or align the
docs with what is kept.

### GG8-9 (Low): Ask sends absolute local file paths to the cloud

**Verified**, 3/3.

**Repro:** `s20_cloud_ask_chat.py`, check `ask.no_local_paths`.

**Actual:** the system prompt has `[1] acme-renewal (file:///tmp/gg8/s20_cloud_ask_chat/Documents/acme-renewal.md)`.
On a real machine this is `file:///home/<user>/…` (user name and folder structure). This is not in
privacy.md's list for Ask.

**Owner:** Ask prompt building in `nugget-retrieve` (EE3).

**Fix direction:** send the title (or a path relative to the source root) to non-local providers.

### GG8-10 (Low): read-ahead and prewarm report a local provider when only the cloud is usable

**Verified**, 3/3.

**Repro:** `s21_cloud_draft.py`, check `read_ahead.skipped_cloud`, with only an anthropic provider
(the local runtime has no model installed).

**Actual:**

- `draft.read_ahead` returns `{"started": true, "done": true}`, not `{"skipped": "cloud"}`.
- `draft.prewarm` returns `{"provider": "local-chat", "locality": "on_device"}`.

Nothing was sent, so no leak, but the UI would tell the user drafts are local.

**Owner:** `features/draft` read-ahead and prewarm (CC1, DD2).

**Fix direction:** resolve the provider the way `draft.begin` does, skipping unavailable runtime
providers.

### GG8-11 (Low): IMAP ledger gaps

**Verified**, 3/3.

**Repro:** `s41_caldav_imap.py` against a hostile IMAP host.

**Actual:** each run made 8 connections and has 4 `sync:imap` rows, all `imap.gg8-synthetic.test:993`.
STARTTLS connections on port 143 (TCP connects, 0 bytes sent, no greeting) get no row. The host
carries the port, unlike other rows.

**Owner:** `crates/nugget-connector-imap` (FF2).

**Fix direction:** record a row at connect time (or zero-byte exchanges), and use the bare host with
the port only when it is non-default.

### GG8-12 (Low): `knowledge.status.extractor_provider` stays null during cloud extraction

**Verified** (s23, 3/3).

**Actual:** three extraction requests went to the cloud provider for the listed `cloud_sources`, but
`knowledge.status` reports `"extractor_provider": null`. Users can't see that extraction uses the
cloud.

**Owner:** `crates/nuggetd/src/knowledge.rs` (W7, EE1).

**Fix direction:** report the provider of the last run.

### Not privacy, for other teams

- **Onboarding (runner s60, 2/2 runs):** the first chat right after installing the models
  failed with `no provider for tier Synthesis has the required capabilities` (for GG6, first week).
- **CalDAV without a password** shows `idle`/`syncing` instead of `needs_auth` (area C, 3/3).

## What works well (protect it)

- **Local-only really is local.** Default config, search, Ask, MCP, knowledge, Today and 5 minutes
  idle with a restart: zero DNS queries and zero packets. With the real `llama-server` the same holds,
  and the server talks only over a Unix socket with `--offline`.
- **Downloads are honest.** Every model and runtime host is ledgered, including failed attempts, with
  pinned revisions. `models.list` is offline.
- **The ledger matches the wire** for ask, chat, draft, knowledge, agent, probe, judge, embed, sync
  and OAuth: `bytes_sent` equals the body size, and no HTTP host went unrecorded in any run. LAN
  providers are ledgered too.
- **Passage-level sensitive filtering** works everywhere it is applied: Ask, drafts, the Today judge,
  knowledge (`cloud_sources` only), and connector content (Slack and Notion sensitive items withheld).
- **Read-ahead and prewarm never sent anything** to a cloud provider, and with real models read-ahead
  reached only the local server.
- **Secrets are confined.** API keys, connector tokens, CalDAV passwords and OAuth codes and client
  secrets reach only their own host. They never appear in logs (even at debug), plaintext files or the
  audit log. A CalDAV server pointing at a foreign host gets no credentials. OAuth begin sends nothing.
- **Minimal metadata.** `user-agent: nugget/0.0.1`, no machine identifiers, `store: false` for OpenAI
  Responses. A loopback "local" server marked `locality = "cloud"` is treated and ledgered as cloud.
- **MCP** listens on loopback only, refuses other hosts, and hides sensitive items by default.
- **No telemetry, no crash upload.** The data dir is 0700; config, store key and IPC are private.
- **The desktop app** with the update channel `off` makes no request at all.

## Limits of this run

- Plugins' network capability was not exercised: no built plugin, and the guests need a wasm cargo
  build.
- macOS was not covered: the pktap job failed.
- Windows was out of scope.
- The real-model runs are one model pair (`qwen3-1.7b-q8`, `nomic-embed-text-v1.5-q8`) on ubuntu-latest, two runs.
- The "user name in paths" part of GG8-9 is inferred: the lab's home has none.
