# GG5 verification: real-model findings (`qa/GG5/findings/real-model.md`)

Verifier: fresh pass over the committed artifacts in `qa/GG5/runs/` (4B and 8B from run 41117293540,
EE1 from run 41117587773). I did not re-run any model. HEAD is `fb542d2` on qa/knowledge, and
`git diff e7299584 HEAD -- crates` is empty, so the code I read is the code the 4B and 8B runs
tested. The EE1 run tested `a670826`. Note that `origin/ee/entity-resolution` has since moved past it
(`4c7e27b nugget-knowledge: internal mail names colleagues…`, `10202d7`), so read EE1 code at
`a670826`, not at the branch tip.

My own checks (stdlib Python, run with `python3 -I`, in `qa/GG5/verify/real-model/`):
- `claims.py RUN LABEL [--pred P] [--status S] [--name X] [--item X] [--count]`: flat list of a snapshot's claims, with subject kind, confidence, items and first quote.
- `items.py CORPUS RUN`: for each snapshot, which corpus files have any mention or evidence. It lists the missing files by name for phase 1 and phase 2.
- `claim_diff.py RUN A B [-v]`: claims that are gone between two snapshots, plus status changes.
- `idem.py RUN A B [--ext .eml,.ics]`: re-extraction idempotence restricted to model claims (confidence 0.70, the `DEFAULT_CONFIDENCE` at `llm.rs:83`).
- `quotes.py CORPUS RUN LABEL`: every evidence quote checked against the raw corpus file.

I also used grep over the `vault/` trees and read the daemon logs and `log.jsonl` directly. Of the
author's scripts I read `sensitive_claims.py`, `coverage.py` and `item_claims_diff.py`, and ran only
`item_claims_diff.py`, to compare its number with mine. Every `file:line` cited in the findings was
opened and read.

**Run 2 (41192088580): not finished.** At 10:37 UTC its `knowledge (qwen3-4b-q4km)` job (started 10:19)
was still in "Run the GG5 scenarios", and the run had no artifacts yet. A 4B run takes about 65 min plus
the 11-minute `restart-later` wait, so it will not be done before about 11:40 UTC. I did not wait for it.
Nothing below uses run 2, and the question "do the lost facts come back at `restart-later`?" is still open.

---

### 1. Model extraction copies the card number, mortgage account number, balances and salary (4B) and the diagnosis and prescription (EE1) into plain-text vault pages
**VERDICT: REPRODUCED** (with small corrections)

What I checked: a grep for each sensitive string in every `vault/` of every snapshot, `claims.py` on Finance/ and Health/, and the cited code.

| string | 4B | 8B | EE1 |
|---|---|---|---|
| `4000 0566 5566 5556` | phase1 → restart | none | phase1 → restart |
| `NCU-4471`, `214,880`, `210,437` | phase2 → restart | none | phase1 → restart |
| `7,450` | phase1 → restart | none | none |
| `anaemia`, `ferrous` | none | none | phase1 → restart (3 files with `anaemia`) |

- 4B `reextract/vault/Commitment/replacement Northshore Credit Union debit card (ent_aj45dskmnbf4sed2).md`:
  `- replacement Northshore Credit Union debit card has title 4000 0566 5566 5556. [^2] <!-- c:24 -->`, footnote `"Card number: 4000 0566 5566 5556"`, plus `has title 05/30`.
- 4B `Person/Mortgage account (…).md` (`kind: "person"`): `- Mortgage account has title NCU-4471-0093-218.` The `Balance at …` and `Gross salary (monthly` pages read as quoted in the finding, and the salary page's quote is only `"Gross salary (monthly)"`.
- EE1: `Organization/Northshore Credit Union (…).md:15` `has title 4000 0566 5566 5556`. `Person/Mara Okafor (…).md:60,61,63` `The user has title NCU-4471-0093-218.` / `£214,880.12` / `£210,437.55` (under "Earlier"). `Dr. Lena Brandt … dislikes [[Preference/iron-deficiency anaemia …]]`. `README.md:61` lists the anaemia page.
- `llm_claim_quality.py`'s count is right: 4B `reextract` has 67 active `has_title` claims, 8B has 12 and EE1 has 16. I listed the 67 by hand: about 4 are real job titles (`Staff Engineer, Platform`, `VP Engineering`, `HR Business Partner`). The rest are dates, places, amounts, identifiers and other entities' names, so "63 junk" holds.
- Code: `subject_kind` `llm.rs:157-170` (`has_title` → Person), `object_kind` `llm.rs:172-186` (`has_title` → None), the literal fallback at `llm.rs:1697`, and `clean_title` at `llm.rs:1751`. `clean_title` (`llm.rs:1104`) only strips articles. `is_date_or_amount` is called only from `clean_entity` (`llm.rs:1277`), never on literal objects. The "names subject or object" quote check is at `llm.rs:1822-1826`. The sensitivity code at `llm.rs:188-214` only excludes cloud providers. All as claimed.

Corrections:
- **Wrong citation.** The triage timeout is `crates/nugget-ingest/src/config.rs:36` (field) and `:88` (`triage_timeout_ms: 30_000`). Line 64 is `error_ring`.
- **"Not 8B: 8B made no finance or health claims" is too strong.** 8B made model claims from the payslip (`Mara Okafor has_title Staff Engineer`, superseded; `Mara Okafor part_of Northshore Credit Union`, active). It also has deterministic claims from the counselling .ics. It made no claim that carries a figure, an identifier or a diagnosis. Reword to "8B leaked no figures, identifiers or diagnoses".
- **Title wording.** The card number is attached to a *commitment* entity ("replacement … debit card"), and in EE1 to the *organization* Northshore Credit Union, not to a made-up person. "Made-up people" fits the mortgage, balance and salary pages.
- **4B timing.** The card number and salary are there from phase1. The mortgage figures appear only from phase2, because the mortgage statement timed out in phase 1 (finding 4). "Every snapshot from phase1" is true only for the card and the salary.
- **EE1 prescription.** `ferrous sulfate 200 mg` is in the *Earlier* (superseded history) section of `Person/Prescription (…).md:18`. The current claim is `has title Repeat bloods in 12 weeks`. Its footnote quotes the whole prescription line, so the string is on the page either way.
- **Unsupported objects (point 4).** My own count of model claims whose object is not in their quote at 4B `reextract` is 7 strict ones: `1:1 Mara / Bob → Mara Okafor`, `Gross salary`, `Income tax`, `National Insurance`, `Harbor has_title Sam Patel`, and 2 alias-only ones. The finding says 8, which is close enough.

Severity: I agree with Blocker for the 4B and EE1 runs. A Luhn-valid card number on a page in a folder the docs say may be synced is a release stopper, whatever the model.

---

### 2. "Up to date" while files are still unextracted: commits that arrive during a carried run chain are never planned until something else wakes the worker
**VERDICT: REPRODUCED**

What I checked: `items.py` (my own script) gives exactly the author's coverage table: 4B 19/24/24/42/42, 8B 7/19/19/42/42, EE1 11/34/36/41/42.
- 4B `phase2`: 1 of 19 phase-2 files is present (`dana-farewell.ics`). The 18 missing ones include `dana-new-role.eml`, `Notes/People/Dana-Whitfield.md`, `harbor-launch-moved.eml`, `fwd-data-weekly.eml` and both `Work/` files. `log.jsonl` `settled` shows `pending_items: 0`.
- **The finding understates 8B.** At the 8B `phase2` and `edits` snapshots, **0 of 19** phase-2 files were in the graph. The 19 files present are all phase-1 files. The whole phase-2 window (20:34:47 → 20:51:55) went on the phase-1 carry chain (`remaining 13 → 0` at 20:51:30). The first plan that contained phase-2 files came after the touch (`20:55:22 remaining 25`). This is the clearest example of the mechanism.
- 4B `phase1` already shows it: 4 phase-1 notes were still missing (`Journal/2026-07-05.md`, `1on1-with-Bob.md`, `People/Ife.md`, `Projects/Harbor.md`) while the last run reported `remaining 0` and status said `pending_items 0`.
- 8B phase-1 settle in the middle of a run: the run logged at 20:36:21 with `took_ms 220642`, so it started at 20:32:40, and the harness settled at 20:34:47. Confirmed. EE1 `reextract` also settled at 21:01:14 while a run was in flight. That run is the one interrupted by SIGTERM in finding 3.
- Code: `handler_m2.rs:341` `pending = st.last_run…remaining`. Confirmed: the IPC status has no `running`, although the library has `KnowledgeStatus.running` (`nugget-knowledge/src/knowledge.rs:70`). Carry take/set at `knowledge.rs:213-220` and `292-298`, cursor set only when `remaining == 0` (`:274`). In the worker (`nuggetd/src/knowledge.rs:186-250`), `committed.changed()` is polled only while `due.is_none()`. After a run with `remaining > 0`, `due = Some(now)`, so the next loop does `borrow_and_update()` (`:227`), which consumes any new commit and only moves the next run back by the debounce. That run takes the carry, not a new plan. When the carry ends, `due = None` and the loop waits for a *new* commit. The finding's reading is correct.
- EE1 "4 fresh plans during reextract": `remaining` jumps up at 20:46:07, 20:51:10 and 20:56:44, plus the run in flight at SIGTERM. That makes 4. Correct.

Corrections: a precision point only. The commits are not lost. The cursor stays at the old plan's head, so the next fresh plan (periodic run, new commit, vault edit, restart) does pick them up. That is exactly why 19 and 14 files appear at startup in finding 3. The finding already says this. Severity High: agreed.

---

### 3. A run without the model replaces model-extracted facts: at startup before the chat model is up (4B, 8B) and during shutdown after llama-server is stopped (EE1)
**VERDICT: REPRODUCED** (several counts need correcting)

What I checked: the daemon logs, `claim_diff.py reextract restart -v`, the vault stubs, claim `updated_at` timestamps, and the cited code.
- 4B log: `21:28:32.804 knowledge run processed 19 … took_ms 111`, then `21:28:36.574 local runtime ready role chat`. 8B: `21:27:42.456 processed 14 took_ms 77`, then chat ready at `21:27:47.867`. EE1: `21:01:14.688 SIGTERM`, `21:01:19.689 background tasks did not stop in time remaining 1`, `21:01:24.944 LLM extraction failed item 0:Notes/Journal/2026-08-16.md … error decoding response body`, `21:01:24.951 stopped`. All exact.
- 4B: 20 claims gone (all active), 12 active → retracted and 5 superseded → retracted. All lost claims have confidence 0.70 (model). Status went from 152/116 to 120/86 (claims/entities). Exact.
- Code: `extract_and_apply` retracts first (`session.rs:776`), then `llm_step` returns `LLM_PENDING` with no provider (`:837`) or with `llm_left == 0` (`:839`). `record` sets `retry_llm` (`:414`), and `plan` retries only when `llm_ready` (`:230-262`). `wind_down` waits 5 s (`daemon.rs:38`, `669-683`), then `runtime.shutdown()` runs (`:654-656`). `on_ready` (`daemon.rs:450-454`) only re-embeds, for the Embedding role, so nothing wakes knowledge when the chat model is ready. All confirmed.
- EE1 shutdown mechanism, my own evidence (claim `updated_at` in EE1 `restart`): between `21:01:24.952` and `.971`, after the failure at `.944`, the remaining plan files were applied in about 20 ms with no further "LLM extraction failed" lines: `1on1-with-Bob.md`, `Dana-Whitfield.md`, `Harbor-launch-plan.md`, `Harbor.md`, `mid-year-review`, `promotion-case-note`. That is the rest of the plan in key order after `Journal/2026-08-16.md`. Their model claims were retracted (`R. Chen leads Harbor`, `Mara has_title Principal Engineer`, `R. Chen has_title VP Engineering`, …). So the "rest of the plan applied without the model" claim is supported.

Corrections:
- **Stub count.** There are **30** "no longer in your knowledge graph" stubs in 4B `restart`, not 32. The vault went from 116 to 86 pages, which is 30, and `grep -rl 'status: "retracted"'` also finds 30. 8B has 20 stubs and EE1 goes from 6 to 20.
- **8B.** 6 gone, and **7** retracted: 6 active plus 1 superseded.
- **EE1.** 12 gone, but 6 of those were already `retracted` at `reextract`, so 6 live claims vanished (5 active, 1 superseded). Transitions: 8 active → retracted, **4 superseded → retracted** (not mentioned in the finding), and 3 superseded → active. Those 3 are not only `R. Chen works at Brightwell`: the junk `Mara Okafor has_title neighbour of the year` and `Harbor has_title Thursday` also became **current** again. So the restart also brings junk back, not only true facts.
- **New observation (EE1).** Claim writes continue until `21:01:24.975`, which is 24 ms *after* the daemon logged `stopped` at `.951`. The blocking extraction task outlives the shutdown sequence, including `runtime.shutdown()` and the store `flush_egress`. That is a write-after-shutdown race in its own right. It also explains why the shutdown run never logs its "knowledge run" line.
- **New observation (4B).** The `restart` snapshot's status has `extractor_provider: null` at 21:29:07, about 30 s after `local runtime routed provider local-chat` at 21:28:36. The status field shows the last run's provider, not the current one.
- "The 19 are the re-saved files committed after the reextract plan": this is consistent with the data, but I could not prove it directly, because the artifacts do not record item versions. Most lost claims were last written before the 21:02:44 touch (1on1-bob.ics 20:29, harbor-kickoff 20:41, Journal 03-15 20:48, …), and all their files are in the `sources.touched` list. A few were written later (`harbor-launch-date.eml` 21:10:52, `tessellate-intro` 21:11:38). Those fit a phase-2 version planned before the touch's commit landed, but this is inference.
- Retry after 3 failures: in 4B, `gp-visit.md` and `counselling.ics` each timed out exactly 3 times (20:38/20:59/21:26 and 20:40/21:01/21:28). In 8B, `harbor-standup.ics` and `1on1-bob.ics` did too. Confirmed.

Severity High: agreed for the mechanism, which flips the vault and its stubs on every restart. As the finding says, most of what was lost was junk, and gold recall was unchanged (4B 10, 8B 8) or down by one (EE1 8 → 7).

---

### 4. Model calls time out at 120 s on CPU, and each timeout uses up a whole run
**VERDICT: REPRODUCED**

What I checked: I counted the "LLM extraction failed" lines per file myself.
- 4B: 10 timeouts, 5 files (gp-visit ×3, counselling ×3, harbor-standup ×2, mortgage ×1, go-no-go ×1). 8B: 13 timeouts, 8 files (harbor-standup ×3, 1on1-bob ×3, counselling ×2, pipeline-green, promotion-case-note, and dana-farewell, go-no-go and ife-visit .ics ×1 each). EE1: 0 timeouts and 1 network error at shutdown. Exact.
- Seconds per extracted file over all runs above 1 s (my sum of `took_ms` divided by `processed`): 4B 72.8, 8B 76.0, EE1 32.4. These match the finding's ranges. CPUs are confirmed in `hardware.txt` (7763, 7763, 9V45).
- Code: `tokio::time::timeout` around the whole stream (`llm.rs:258-262`), `temperature 0` (`:256`), `parse_json` tolerates truncation (`:294-321`), and the local `max_output_tokens: 1_024` / `timeout_ms: 120_000` (`nugget-knowledge/src/config.rs:137,152`). `extract_and_apply` applies the deterministic extraction whatever the model did. Confirmed. The 8B counselling .ics keeps its deterministic claims (`Jonas Albers organized Counselling session`, conf 0.95).

Corrections:
- "Each timeout uses up a whole run" is nearly but not always true. A timeout that starts late in a run pushes the run past 120 s (4B 20:59:43: 2 files in 226 s). "A timed-out request ends the run" is more exact.
- The tokens/s figure (8-10 tok/s for 4B) is not in any artifact. It is an estimate, and the finding should say so.
- The local limits also allow up to 4 requests per item (`max_requests_per_item: 4`, `max_chunks_per_request: 2`). The timeout applies per request, so a long item can time out on its 2nd request after spending more than 120 s.

Severity Medium: agreed.

---

### 5. Re-extracting an unchanged file gives different model facts each time, and first names stay split
**VERDICT: PARTIALLY REPRODUCED** (the observation holds; the 19/34 figure mixes in other effects, and the nonce cause is unproven)

What I checked:
- The author's `item_claims_diff.py` on EE1 `edits` → `reextract` gives `identical 15, different 19`. That reproduces the number. But the script compares *all* claims citing an item (deterministic ones too, plus status changes caused by other items and entity renames). It also counts the `.md` notes, whose text the harness changed (the "(re-saved)" line).
- My `idem.py`, restricted to model claims (confidence 0.70) on `.eml`/`.ics` files only (mtime changed, text unchanged): **15 of 20** files with model claims changed. Examples (exact): `tessellate-intro.eml` `-(Mara Okafor, works_at, Tessellate Systems) +(Gus Ferreira, has_title, Tessellate Systems)`, and `harbor-launch-moved.eml` `-(Harbor launch, has_title, "3 December 2026") +(Harbor launch, due_on, "2026-12-03") +(Harbor launch, reports_to, "12 November")`. Four of the 15 are calendar files whose model claims went from 4-5 to 0. So the non-idempotence is real, and stronger on the clean subset than the 19/34 figure suggests.
- Code: the nonce (`llm.rs:464-472`, used at `:619`), thinking off (`supervisor.rs:1105-1112`), `adopt_known_kinds` (`:1995`) and `fold_first_names_in_graph` (`:2053`, `find_by_name` skip at `:2069-2073`). The in-response fold is at `llm.rs:1610-1650`. There is a second one, against the deterministic extraction, at `session.rs:1442`. `settle_short_names` exists in EE1's `session.rs:1899`.
- Splits, from my own counts in the entity list (person kind): 4B `reextract` has 4 Theo, 3 Dana, 3 Ife and Robert as `Bob`/`R. Chen`/`Rob Chen`. 8B has 5 Theo and 4 Dana. Matches. The junk entities `Event "Ife Okafor <ife.okafor@mailbox.example> (accepted"`, `Person "Cut-over window"` and `Person "Checklist owners"` exist in 4B `reextract`.

Corrections:
- The cause "fresh nonce → different prompt → drift" is a hypothesis. The artifacts cannot tell it apart from graph-dependent post-processing, which the finding also names, or from llama.cpp batch nondeterminism. The fix (a deterministic nonce) should be stated as worth testing, not as the fix.
- Replace "19 of 34 files changed their claims" with the model-only figure on the files whose text did not change (15 of 20), or explain what the 19 includes.

Severity Medium: agreed.

---

### 6. Wrong and invented facts the model makes current
**VERDICT: REPRODUCED** (one confound and some wording)

What I checked: `claims.py` on `reextract` with `--item Work/`, `--name Dana`, `--name Theo`, `--pred due_on` and `--item fwd-data-weekly`, plus grade.json `forbidden_current`.
- EE1 `R. Chen leads Harbor` (active), quote `"I asked her in March to lead Harbor, our data-platform migration"`. EE1 `Mara Okafor has_title Principal Engineer` is active, quote `"I support her case for promotion to Principal Engineer"`. 4B and EE1 have `R. Chen works_at VP Engineering`. 8B has `R. Chen has_title Priyanka Rao`. All confirmed.
- Dana: 4B has both `Dana works_at Corvid Labs` and `Dana works_at Brightwell Analytics` active on the bare `Dana`, from the same quote. 8B `Dana Whitfield has_title Design Lead` is active (grade.json `forbidden_current: "Dana still Design Lead"`), plus `has_title dana@corvidlabs.example`. EE1 has `Dana Whitfield works_at Corvid Labs` active (0.99), Brightwell superseded and the bare-Dana claims retracted. Confirmed.
- Injection: 4B `Mara Okafor works_at Corvid Labs` and 8B `Mara Okafor has_title Head of Platform` are both `superseded` with confidence 0.50, and EE1 has none. The cap is at `llm.rs:1492,1859`, and quotes *with* a marker are dropped at `:1807`. Confirmed.
- Theo: 4B `Theo located_in Eastgate`; EE1 `Told has_title Theo` (superseded, not current). No partner predicates. Confirmed.
- Launch dates: 8B has three current `due_on` (`Harbor launch` 2026-11-12, `Launch date` 2026-11-19, `Cut-over window` "overnight 18 to 19 November"). 4B `Harbor launch due_on 2026-12-03` is active. EE1 has `Harbor due_on "12th"` active, and that "Harbor" is an *event* entity, not the project. Confirmed.
- `first_person_is_me` (`llm.rs:1233`) is `!by_others(item) || …`, so any non-mail item the source does not mark `owned_by_me: false` counts as the user's. Confirmed as a latent risk.

Corrections:
- **Confound on the Dana comparison.** In 4B and 8B the harness edit E1 was applied. It produced a `user_asserted` claim `Dana Whitfield works_at "Dana is Head of Product at Corvid Labs (fixed by me)"`, which *supersedes* `Dana Whitfield works_at Corvid Labs` in both runs. In EE1, E1 was `edit.skipped`. So "EE1 gets the employer right" against 4B/8B is not like for like: in 4B and 8B the user's own edit had replaced the correct claim with a junk literal (a vault-edits issue). Say so.
- "4B: four current `has_title` for Mara from the HR note's evidence list": 4B has four current `has_title` from the HR note, but only three come from the evidence list. The fourth is the correct `Staff Engineer, Platform`.
- Add: 4B and 8B extracted `Mara Okafor has_title Principal Engineer` too (superseded, not current). And EE1's current Principal Engineer claim is retracted at `restart` by the shutdown run (finding 3), so "current" holds only at `reextract`.

Severity Medium: agreed.

---

### 7. Grader and harness limits
**VERDICT: REPRODUCED** (one correction)

- `settle` (`nugget_qa.py:224-248`): the signature is queue depth, `pending_items`, counts, `last_run`, `last_render` and `busy`. A run in progress changes none of them, so the harness settles mid-run. Confirmed (8B phase1, EE1 reextract).
- The metrics table (entities, claims, fact recall, entity_ok, bad quotes, sensitive hits) matches grade.json for all 15 snapshots. `wrong_merges` and `uncited` are empty everywhere.
- Quote check, my `quotes.py` at `reextract`: every evidence quote not found verbatim in its raw file is either an `.ics` quote (57 in 4B, 33 in 8B, 36 in EE1) or one of 11 others per run. **Correction:** those 11 are not all frontmatter. 6 are deterministic mail-header quotes (`To: Sam Patel <…>`, `Cc: Dana Whitfield <…>`, re-synthesised for one recipient of a multi-recipient header). The other 5 are frontmatter (`members:`, `tags:`, `people:`). "No model quote outside .ics is missing from its file" holds.

---

## Summary

| # | Finding | Verdict | Main correction |
|---|---|---|---|
| 1 | PII from the model into vault pages | REPRODUCED | triage timeout is `ingest/config.rs:88`, not `:64`; 8B did make finance claims (no figures); card is on a commitment/org, not a person |
| 2 | Commits during a carried chain are not planned; `pending_items` 0 | REPRODUCED | 8B phase2/edits had 0/19 phase-2 files (stronger than stated) |
| 3 | Run without the model replaces model facts (startup and shutdown) | REPRODUCED | 30 stubs, not 32; 8B 7 retracted; EE1 also 4 superseded → retracted and junk restored to current; writes continue after `stopped` |
| 4 | 120 s timeouts on CPU | REPRODUCED | tokens/s is an estimate; a timeout ends the run rather than always being the whole run |
| 5 | Non-idempotent re-extraction, first-name splits | PARTIALLY REPRODUCED | 19/34 mixes in deterministic and cross-item changes; model-only on unchanged files is 15/20; the nonce cause is unproven |
| 6 | Wrong and invented current facts | REPRODUCED | E1 user edit confounds the Dana comparison; Principal Engineer also in 4B/8B (superseded) |
| 7 | Grader and harness limits | REPRODUCED | the non-.ics bad quotes include deterministic mail-header quotes, not only frontmatter |

Run 2 (41192088580) was still running when I finished (knowledge job started 10:19 UTC, not done at 10:37). There are no artifacts yet, so whether the lost facts come back at `restart-later` is still open.
