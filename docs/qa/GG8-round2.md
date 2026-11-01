# GG8 round 2: privacy and egress, re-verified from the outside

This round tests the integration branch at **e0d44e65**, merged into `qa/privacy` as f746575d. The
method is the same as round 1 ([GG8.md](GG8.md)):

- `nuggetd` runs in a network namespace; every packet is captured and every DNS query logged.
- A recording TLS sink impersonates the real API hosts.
- Observed traffic is compared with the egress ledger.
- Canary strings are searched for in requests, logs and files.

GitHub Actions was unavailable, so everything ran locally (4 vCPU container, debug build). This time
real models ran locally too. `qwen3-1.7b-q8` (chat and triage) and `nomic-embed-text-v1.5-q8` were
installed once by the daemon's own `models.install`, then copied into each lab home, and served by
the managed `llama-server`.

**Where things are:**

| What | Where |
|---|---|
| Scenarios | `qa/GG8/round2/scenarios/` (r01–r05), plus round 1's `qa/GG8/scenarios/` for regressions |
| Messaging fixtures | `qa/GG8/round2/fixtures/` |
| Evidence | `qa/GG8/round2/results/` (requests trimmed, keys removed; 223 KB) |
| Verification sheet | `qa/GG8/round2/VERIFY.md` |
| Command | `sudo GG8_LAB=<0-3> python3 qa/GG8/harness/lab.py run <scenario> --out /tmp/x` |

r03 and r04 expect the models in `/tmp/gg8-models` (`GG8_MODELS_HOME`).

## Verdict

| # | Check asked for | Verdict | Evidence |
|---|---|---|---|
| 1 | Local-only use sends nothing: first run, idle, new features | **Works** | r01: no model installed. It covers folders, WhatsApp and Telegram imports, faceted search, `draft.check_sensitive`, `plugins.catalog`, the knowledge graph, a quick-ask style Ask, Today, 120 s idle (300 s in round 1) and a restart. Result: 0 DNS queries, 0 requests, 0 ledger rows, no packet off the namespace. With real local models (r03), quick ask (`provider: null`) and read-ahead sent nothing |
| 2 | With a cloud provider, unsorted and confirmed-sensitive items never reach it | **Works, except text sent through Double Tap (R2-1) and the Double Tap thread itself (R2-2)** | See the table below |
| 3 | Every request is in the ledger, including EE5's app-reported update rows | **Works** | r03 and r04: 39/39 requests had a row, with no unledgered host. `egress.record` (r05): accepts `update_check`/`update_download` only; refuses `ask`, `model_download`, `sync:gmail` and `draft` (`unknown feature`); refuses URL-shaped hosts, times in the future, times over 30 days old and negative bytes; rate limit 59 accepted out of 80, then `-32010`. Labels: a probe's embeddings test is ledgered as `embed` (R2-4); `prewarm` rows are undocumented (R2-3) |
| 4 | GG8-1 to GG8-12 re-tested | **7 fixed, 2 not fixed, 1 partly, 1 untestable, 1 code-reviewed** | Table below |

Check 2 in detail:

| Path | Result |
|---|---|
| Ask before triage | Withheld all 9 items (`excluded_sensitive: 9`) |
| Ask after real triage | Sent only the triaged-normal notes (`GG8CANARY-NORMAL-1`, `-2`); withheld 7, including both private chats |
| Quick ask handed over to a cloud follow-up | The real local answer quoted card `4111 …`; the cloud follow-up got a placeholder instead |
| Cloud embeddings (r04) | 29 requests, only normal-item text, at index and at search time |
| Knowledge and Today judge with the cloud allowed | Sent nothing in r03 and r04: local models took extraction, and the judge was not called |

## GG8-1 to GG8-12

| Id | Round 1 | Now | Evidence |
|---|---|---|---|
| GG8-1 | High: follow-up history to the cloud unscanned | **Fixed** | s53 3/3 (2 regression runs and 1 verifier run, plus the real model in r03). The cloud body holds `"(An earlier message is left out here: it held private details.)"` where the card was |
| GG8-2 | High: cloud embeddings of sensitive notes at search | **Fixed** | r04 with real triage and a cloud embedder: 29 `/v1/embeddings` requests, none with card, SSN, IBAN, keys or `MAILSENS` text; canaries seen are `NORMAL-1`, `NORMAL-2`, `MAILNORMAL-1` and `MAILNORMAL-2` only. (s25 as written no longer embeds anything: every item is unsorted) |
| GG8-3 | High: sent Double Tap texts in a hidden source, to MCP and cloud Ask | **Not fixed, now the one exception (R2-1)** | s52 2/2 + 2/2 verifier, s21 |
| GG8-4 | Medium: card number in the Double Tap thread to the cloud | **Not fixed (R2-2)** | s21 2/2 + 2/2 verifier: the body has `Use card 4111 1111 1111 1111 for the deposit.` |
| GG8-5 | Medium: desktop update check by default, unledgered | **Fixed (code review only; the desktop app was not run)** | `Settings::default()` is `channel_chosen: false`; `active_channel()` returns `None` until the user chooses. Checks are recorded through `egress.record`, and r05 verified that path. privacy.md documents both |
| GG8-6 | Medium: ledger rows without items | **Partly fixed** | Ask follow-up: correct now, since nothing from history is sent. `embed`: every row carrying item text lists its item (r04); query embeddings correctly list none. `proactive_judge`: **not testable**, because the judge was not called in r03, r04 or s27 (0 requests, though r04 had 3 cards and a probed cloud judge) |
| GG8-7 | Low: debug logs held MCP content | **Fixed** | s50 2/2: no query or passage text in about 205 DEBUG lines |
| GG8-8 | Low: audit kept MCP queries verbatim | **Fixed** | `"args":{"query":{"chars":32,"hmac":"ca8d1a511e609a82"}}` |
| GG8-9 | Low: absolute `file://` paths to the cloud | **Fixed** | r03's cloud Ask body: passages carry title and date only, no path |
| GG8-10 | Low: wrong read-ahead/prewarm status with cloud only | **Fixed** | `read_ahead` → `{'skipped': 'cloud'}`; `prewarm` → `{'provider': 'anthropic', 'locality': 'cloud'}` |
| GG8-11 | Low: IMAP ledger gaps | **Fixed** | s41: 8 connections, 8 rows; port-143 attempts get 0-byte rows; bare host |
| GG8-12 | Low: `extractor_provider` null during cloud extraction | **Not testable** | No cloud extraction happened (s23: 0 requests; r03: local extraction). The code now reports the last run's provider |

## New findings

All four were reproduced by an independent verifier from `qa/GG8/round2/VERIFY.md`, 2/2 runs each.

### R2-1 (High): Double Tap's sent texts bypass the new "unsorted counts as sensitive" rule

This is GG8-3, still open. It now stands out, because it is the one kind of user content that still
reaches the cloud before triage.

**Repro:** `qa/GG8/scenarios/s52_voice_items_exposure.py`, which calls `draft.feedback` with
`final_text: "Thanks doctor, I will call about the biopsy … GG8CANARY-VOICE-final"`, and
`qa/GG8/scenarios/s21_cloud_draft.py`.

**Expected:** text from other apps is private by default. Every untriaged item is withheld from cloud
models and from MCP clients without the sensitive scope, as imported chats now are.

**Actual:**

- `sources.list` shows only `files-1`.
- A default-scope MCP `search` returns `<untrusted-content source="nugget-voice" …>Thanks doctor, … biopsy …`.
- The cloud Ask body contains the same text, and its ledger row lists `{"source":"nugget-voice","key":"feedback/…"}`.
- In s21 a cloud draft cites the previous draft's final text with `"sensitive": false`, while `excluded_sensitive: 7`.

**Owner:** the voice feedback items (`crates/nuggetd/src/features/draft`, `nugget-voice`; EE2 today)
and MCP scopes (`nugget-mcp`, FF4).

**Fix direction:**

- Give voice items the messaging connectors' rule: private and sensitive by default, never in
  general retrieval or MCP.
- Or show the source and let the user choose.
- At minimum, have the unsorted-is-sensitive check cover them.

### R2-2 (Medium): a card number in the Double Tap thread still goes to the cloud

This is GG8-4, unchanged.

**Repro:** `s21_cloud_draft.py`, check `draft.thread_with_card_number`.

**Actual:** the body to `api.anthropic.com` contains `<message from="other" author="Priya">\nUse card 4111 1111 1111 1111 for the deposit.`

`draft.check_sensitive` now exists and the desktop calls it before keeping a draft in history.
The daemon could run the same scan on field, thread and subject before a cloud draft.

**Owner:** `nugget-draft` / `features/draft` (EE2).

### R2-3 (Low): `prewarm` contacts the cloud and its ledger feature is undocumented

**Repro:** `s21_cloud_draft.py`.

**Actual:** `draft.prewarm` with a cloud provider sends `GET https://api.anthropic.com/v1/models`
with the API key and a 0-byte body. It is ledgered `{"feature":"prewarm","bytes_sent":0,"items":[]}`.
No content is sent, and the ledger is right. But privacy.md's feature tables do not list `prewarm`,
so a user reading the Privacy page cannot tell what it is.

**Owner:** docs (privacy.md feature table), `features/draft` (CC1/DD2).

### R2-4 (Low): a probe's embeddings test is ledgered as `embed`

**Repro:** `s25_cloud_embed_triage.py`.

**Actual:** a provider probe sends 5 chat requests, ledgered `probe`, and then
`/v1/embeddings` (`"The cat sat on the mat."`) about 1 ms later, ledgered `embed`. Where nothing
else was embedded (s25), this is the only `embed` row. The Privacy page then suggests an opted-in
embedding of user data that never happened.

**Owner:** the probe's egress feature label (`nugget-infer` probe / `crates/nuggetd/src/probes.rs`).

**Fix direction:** tag it `probe`.

## What works well (protect it)

- **"Unsorted counts as sensitive" works end to end.**
  - Before triage, a cloud Ask withheld all 9 items.
  - After real triage only the normal notes went.
  - Imported WhatsApp and Telegram chats (private by default) never reached the cloud, and faceted
    search marks them sensitive.
- **The GG8-1 fix holds with a real model.** The 1.7B model's local answer quoted the card number,
  and the cloud follow-up received a placeholder instead.
- **Cloud embeddings are clean after triage** (r04). Only normal items are embedded; sensitive notes
  and sensitive mail never are, at index or at search time.
- **The messaging importers make no network request at all**, as documented.
- **`egress.record` is tight:** a whitelist of two features, validated hosts and times, and a rate
  limit at the documented 60 a minute.
- **Logs and audit are clean.** Debug logs have no MCP content, and the audit log keeps queries as
  length plus keyed hash.
- **IMAP ledger rows are complete**, and read-ahead and prewarm report their locality truthfully.
- **The update check waits for the user's choice** (code review).

## Limits

- The desktop app (update checks, quick ask UI, deep links) was not run, because GitHub Actions was
  down and there is no desktop build here. Quick ask and its Chat handover were driven through the
  same IPC calls the app makes (`retrieve.ask` with `provider: null`, then a follow-up with
  `history`).
- The Today judge was never called in any run, so its payload and ledger items are untested this
  round.
- Cloud knowledge extraction did not happen: with a local model present it runs locally.
- The real models are one small pair on CPU.
- The injection neutraliser (EE3), deep links (II4) and env key references (KK2) have no egress of
  their own. API keys from `env:` references reached only their host in every run.
- Model download resume (KK1) was not exercised against the network.
- No macOS or Windows runs.
