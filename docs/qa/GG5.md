# GG5 QA: knowledge graph and Markdown vault, with real models

Independent QA of entity and claim extraction, the typed graph, the rendered Markdown vault, merging
the user's vault edits back, and the Knowledge pages of the desktop app (entity view, "Why?", graph
view), used the way a person would on a realistic synthetic life.

- Product under test: integration branch at `e7299584` (harness commits up to `qa/knowledge` HEAD);
  EE1's unmerged branch `ee/entity-resolution` at `a670826` for comparison.
- Branch with everything: `qa/knowledge`. Persona and corpus: `qa/GG5/PERSONA.md`,
  `qa/GG5/corpus/` (42 items: 21 mails, 7 calendar files, 14 notes, in two phases; `gold.json` holds
  the ground truth). Harness: `qa/GG5/harness/nugget_qa.py` (drives a real `nuggetd` over IPC: a
  `files` source on the corpus, snapshots of every `knowledge.*` result and of the vault after each
  step), grader `qa/GG5/harness/grade.py`, workflow `.github/workflows/qa-GG5.yml`.
- Explore layer (5 areas): `qa/GG5/findings/{resolution,vault-edits,truth-sensitive,app,real-model}.md`,
  with their scenario scripts and captured outputs under `qa/GG5/scenarios/`. Verification layer: a
  fresh agent reproduced each finding from the written repro alone, `qa/GG5/verify/*.md`.
- Real-model runs (GitHub `ubuntu-latest`, 4 vCPU AMD EPYC, 16 GB, CPU only, the signed manifest's
  models installed and served by `nuggetd`'s own runtime): outputs in `qa/GG5/runs/`.

| Run | Product | Model | Runner CPU | Outputs |
|---|---|---|---|---|
| [41117293540](https://github.com/rayhankhilji/nugget/actions/runs/41117293540) | integration `e7299584` | qwen3-4b-q4km | AMD EPYC 7763, 4 vCPU | `qa/GG5/runs/41117293540-qwen3-4b/` |
| [41117293540](https://github.com/rayhankhilji/nugget/actions/runs/41117293540) | integration `e7299584` | qwen3-8b-q4km | AMD EPYC 7763, 4 vCPU | `qa/GG5/runs/41117293540-qwen3-8b/` |
| [41117587773](https://github.com/rayhankhilji/nugget/actions/runs/41117587773) | `ee/entity-resolution` `a670826` | qwen3-4b-q4km | AMD EPYC 9V45, 4 vCPU | `qa/GG5/runs/41117587773-qwen3-4b-ee1/` |
| [41192088580](https://github.com/rayhankhilji/nugget/actions/runs/41192088580) | integration (second run, adds `restart-later`) | qwen3-4b-q4km | RUN2_CPU | `qa/GG5/runs/41192088580-qwen3-4b/` |

Embeddings: nomic-embed-text-v1.5-q8 in every run. Extraction settings are the defaults (120 s per request, 120 s per run).

All data is synthetic (`.example` domains, invented people). The container cannot run
`llama-server`, so the deterministic-only scenarios ran locally against a debug `nuggetd`; every
real-model observation comes from the GitHub runs.

## Verdict

| Capability | Verdict | Evidence |
|---|---|---|
| Entity extraction (people, organisations) | Works with caveats | Every gold person and organisation is found by the deterministic extractors in all runs; no wrong merges in any run or snapshot (grade.json `wrong_merges` empty; the two Sams, Ife and Mara are never merged). Display names depend on file order (GG5-9), and model runs add junk entities and first-name duplicates (GG5-15, GG5-19). Gold entities resolved 1:1 at the last snapshot: deterministic 12/20, 4B 10/20, 8B 11/20, EE1 4B 13/20 |
| Claim extraction with a local model | **Broken** | 4B puts a card number, an account number, balances and a salary into the vault (GG5-1); 63 of its 67 model `has_title` claims are junk (8B 11/12, EE1 13/16); wrong facts become current (GG5-15). 8B is more precise but slow. Unsupported quotes: 0 in every run (the quote gate holds) |
| Citations and "Why?" | Works | Every active claim has evidence (`uncited: []` in all 3 real-model and all deterministic snapshots); no model quote is missing from its source; "Why?" opens the quote and a working link. Calendar/frontmatter quotes are reconstructed (GG5-25) and some deterministic facts overstate (GG5-16) |
| Currentness (time-aware facts) | Works with caveats | Mail is dated by its Date header and supersession works on it (Dana's employer, deterministic and EE1 runs). Notes and calendar files are dated by mtime (GG5-6); a user edit can lose to a revived fact (GG5-4, GG5-13); the model does not order two facts from one sentence (GG5-15) |
| Freshness of extraction with a CPU model | **Broken** | New files wait while status says nothing is pending (GG5-7: 4B phase2 24/42 files in the graph); every restart replaces model facts with deterministic ones (GG5-8: 152 → 120 claims); 70-93 s per file and 120 s timeouts on a 4-core runner (GG5-18) |
| Rendered vault | Works with caveats | Pages for every entity, valid (`vault.invalid: []` in every snapshot), links by short id, redirect stubs after merges. Frozen claim text (GG5-10), email addresses in footnotes (GG5-17), sensitive pages (GG5-3) |
| Merging the user's vault edits back | **Broken** | Deleting a line, adding bullets, Notes, offline and atomic saves all work, but an edited fact comes back on the next re-save of its source (GG5-4, 3/3), a retyped line loses its wording (GG5-5), a normal YAML alias edit leaves a permanent conflict (GG5-20, hit in every harness run), a comment-stripping editor duplicates the page (GG5-21) |
| Sensitive items | **Broken** | Health and finance files are not sensitive without a triage model (GG5-2); nothing filters the vault or graph (GG5-3); the model copies sensitive values into the vault (GG5-1). Card/IBAN detection and the MCP filter for flagged items work |
| Manual merge and split | Works with caveats | `knowledge.merge` redirects old ids, stubs old pages and survives re-extraction and restart; `knowledge.split` survives too, but the old id keeps pointing at the wrong person (GG5-12) and there is no rename (GG5-9) |
| Knowledge pages in the app (entity view, Why?, reject/undo, merge) | Works with caveats | Run for real in headless Chromium against a real daemon: Why?, Open, Open in vault, Reject/Undo round-trip exactly. Restore on superseded facts (GG5-14), recency from write time (GG5-22), indistinguishable duplicates (GG5-11) |
| Graph view | Works with caveats | IPC overview 45/45 nodes and all 59 current edges, nothing rejected/superseded/dangling; depth 1-3 neighbourhoods match an independent BFS; 45 nodes draw fine. Kind filter and `limit` (GG5-24); health nodes unmarked (GG5-3) |

## Findings

Severity is user impact. "Verified" means a second, fresh agent reproduced it from the repro text
alone (`qa/GG5/verify/<area>.md`); real-model findings were re-checked by a fresh agent against the raw run artifacts, and reproduced by a
second run of the workflow where noted.
Each finding names its detailed write-up (`qa/GG5/findings/<area>.md`, same title), which has the full
quotes and the code walk; ids differ between runs, names do not.

Common repro setup (deterministic, any Linux box): `CARGO_INCREMENTAL=0 cargo build -p nuggetd`, then
`python3 qa/GG5/harness/nugget_qa.py --nuggetd target/debug/nuggetd --home <h> --out <o> --no-models`
(phase1, phase2, edits, reextract, restart; about 3 minutes), `python3 qa/GG5/harness/grade.py --gold
qa/GG5/corpus/gold.json --run <o>`. The area scripts each do this themselves with a fresh `--home`.
Real models: dispatch `.github/workflows/qa-GG5.yml` (`models`, optional `product_ref`).

### GG5-1 · Blocker · The model writes a card number, an account number, balances, a salary and a diagnosis into plain-text vault pages
- Verified: REPRODUCED from the raw artifacts (`qa/GG5/verify/real-model.md`). Area: real-model.md, first finding.
- Repro: real-model runs; `python3 -I qa/GG5/scenarios/real-model/sensitive_claims.py qa/GG5/corpus/gold.json qa/GG5/runs/41117293540-qwen3-4b reextract`.
- Expected: health and finance details do not end up in generated text, least of all a card number from a mail Nugget's own PII
  scan flags as sensitive; the vault is plain text the guide says you may sync. Deterministic-only, none of them reach the vault.
- Actual (4B, integration): `Commitment/replacement Northshore Credit Union debit card (…).md`:
  `- replacement Northshore Credit Union debit card has title 4000 0566 5566 5556. [^2]` (and `has title 05/30`, the expiry);
  `Person/Mortgage account (…).md` (kind person): `- Mortgage account has title NCU-4471-0093-218.`;
  `Person/Gross salary (monthly (…).md` `has title £7,450.00`; `Person/Balance at 30 June 2026 (…).md` `has title £210,437.55`.
  EE1 branch: the card number on `Organization/Northshore Credit Union`, the account number and balances in Mara's own history
  (`The user has title NCU-4471-0093-218.`), `Lena Brandt dislikes [[Preference/iron-deficiency anaemia]]`, a person
  "Prescription" `has title ferrous sulfate 200 mg`, and "iron-deficiency anaemia" in the vault README. 8B: model claims from the payslip, but no figures, identifiers or diagnosis.
  The card number is in the 4B and EE1 vaults from phase1 to restart.
- Frequency: 4B integration and EE1 4B (2 of 3 runs); 8B 0.
- Cause: local extraction runs on sensitive items by design (`crates/nugget-knowledge/src/llm.rs:188-214`, sensitivity only
  excludes cloud providers); the gate never checks what a `has_title` object is (`llm.rs:172-186`, `1697`, `1751`), an unknown
  subject becomes a person (`llm.rs:157-170`), the quote only has to contain the subject or the object (`llm.rs:1823`); and the vault
  renders whatever is in the graph, with no sensitivity filter (GG5-3).
- Owner: crates/nugget-knowledge `llm.rs` (EE1's area; its brief does not target this, and the EE1 run is worse), crates/nugget-vault.
- Fix direction: never render claims or entities whose only evidence is a sensitive item into the vault (or render them
  redacted); for items flagged sensitive (and every Health/Finance-like item, GG5-2) skip or restrict the LLM step; reject
  `has_title` objects that are numbers, amounts, dates, emails or identifiers, and subjects that are field labels.

### GG5-2 · High · Health and finance files are not sensitive without a triage model, so a default MCP client reads the diagnosis, prescription, account number and salary
- Verified: REPRODUCED (`qa/GG5/verify/truth-sensitive.md`). Area: truth-sensitive.md, first finding.
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s1_sensitive.py <dir>` (creates an MCP client with the default scopes,
  `include_sensitive: false`, and calls `search`/`get_entity` over `nuggetd mcp --client`).
- Expected: privacy.md, "Items whose status is unknown are treated as sensitive"; mcp.md, sensitive items "excluded unless you allow them".
- Actual: only the card-number mail is withheld. `search "ferrous sulfate anaemia"` returns the GP note ("…iron-deficiency anaemia…
  Prescription: ferrous sulfate 200 mg"), `search "Mortgage account NCU"` returns "Mortgage account: NCU-4471-0093-218 …",
  the payslip ("Gross salary (monthly) | £7,450.00"), and `get_entity` Jonas Albers (counsellor) with the session.
- Frequency: 1/1 author, 1/1 verifier (plus an independent 3-file corpus).
- Cause: `crates/nugget-ingest/src/pipeline.rs:519` marks an item sensitive only on a PII hit (cards, US SSNs, IBANs, API keys,
  `pii.rs`) or a triage verdict, and `pipeline.rs:579` writes `sensitive: false` even when triage never ran or failed, so no
  item is ever "unknown". The same flag gates cloud embeddings, cloud extraction (`cloud_sources`) and cloud Ask/Double Tap context.
- Owner: crates/nugget-ingest (not EE1).
- Fix direction: write "unknown" when triage did not run and treat it as sensitive, as documented; add model-free signals
  (folder and tag names such as `Health/`, `#finance`, labelled account numbers, payslip tables).

### GG5-3 · High · The vault, the graph and the entity view ignore sensitivity: even an item Nugget flagged sensitive becomes plain-text pages
- Verified: REPRODUCED (`verify/truth-sensitive.md`, `verify/app.md`). Areas: truth-sensitive.md second finding; app.md "Health entities".
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s2_flagged_item_in_vault.py <dir>` (adds an IBAN to the counselling invite so
  the PII scan flags it).
- Expected: an item hidden from MCP is not copied into a plain-text folder that may be synced; health entities are at least marked.
- Actual: MCP returns 0 results for it, but the vault has `Person/Jonas Albers (…).md`, `Event/Counselling session (…).md`, the
  clinic address as a place, footnote quotes from the invite, README entries, and on Mara's own page "Mara Okafor attended
  Counselling session". The graph overview and Mara's neighbourhood show the therapist and the session like any other node;
  `GraphNodeView`/`EntityDetail` carry no sensitivity field, so the app cannot mark them.
- Frequency: 1/1 author, 1/1 verifier; deterministic and all real-model runs (grade.json `sensitive_hits`).
- Cause: no sensitivity check in `crates/nugget-vault` rendering or `crates/nuggetd/src/graphview.rs` (graphview computes
  `sensitive` per evidence item but only the MCP path uses it).
- Owner: crates/nugget-vault (EE1 owns vault rendering of entities and claims; not in its brief), crates/nuggetd graphview, desktop.
- Fix direction: do not render entities, claims or footnotes supported only by sensitive items (or an opt-in), and expose
  `sensitive` on graph nodes and claims so the app can hide or badge them.

### GG5-4 · High · Re-extraction undoes the user's vault edit: the old generated line comes back next to the user's version
- Verified: REPRODUCED (`verify/vault-edits.md`, also on an independent 5-note corpus). Area: vault-edits.md, first finding.
- Repro: `python3 qa/GG5/scenarios/vault-edits/run_main.py <out>`: on Harbor's page change
  `- Harbor is part of [[…|Brightwell Analytics]]. [^4] <!-- c:36 -->` to `- Harbor is a Brightwell Analytics project (fixed by me) [^4] <!-- c:36 -->`,
  then re-save the source note (append a line).
- Expected (guide): the old fact becomes `superseded`, the user's text a `user_asserted` fact.
- Actual: right after the edit, c:36 `superseded` and the page shows only the user's line. After re-saving the source, c:36 is
  `active` again and the page shows both lines; for Dana's single-valued `works_at`, two current employers. It stays that way after
  a restart. Not when the edited line keeps the original `[[link]]`.
- Frequency: 3/3 edited lines over 2 runs, 3/3 for the verifier.
- Cause: the edit records plain `Superseded` (`crates/nugget-vault/src/lib.rs:722-723`); re-extraction retracts evidence-less claims
  (`crates/nugget-store/src/graph.rs:567-572`) and then revives the same claim id (`add_evidence_tx`, `graph.rs:910-915`;
  single-valued: freshness of an evidence-less claim is its `updated_at`, the retraction time, `crates/nugget-knowledge/src/session.rs:702-704`,
  so it beats the user's claim; only `Active` rivals are superseded, `session.rs:1328`).
- Owner: crates/nugget-knowledge session.rs, nugget-store graph.rs, nugget-vault (EE1-adjacent: EE1 changes supersession).
- Fix direction: make "replaced by the user's edit" a sticky user decision like `rejected`, and while a `user_asserted` value
  exists for a single-valued predicate, supersede extracted values instead of reviving them.

### GG5-5 · High · Retyping a fact line without its invisible marker (keeping the link) silently drops the user's wording
- Verified: REPRODUCED (`verify/vault-edits.md`). Area: vault-edits.md, second finding.
- Repro: `run_main.py` scenario 8: replace a generated line by a retyped one with the same `[[link]]` and no `<!-- c:N -->`.
- Expected: the user's sentence is kept as their fact.
- Actual: the old claim is rejected, the new user claim has the same triple and is folded into it (c:55): the page shows the
  original generated sentence with a `(you)` tag; the user's text is in neither the page nor the store. The verifier also saw the
  survivor turn `active` again after re-extraction.
- Frequency: 1/1 author, 2/2 verifier.
- Cause: duplicate-triple folding keeps the older claim's text (`crates/nugget-store/src/graph.rs`, `fold_duplicate_claims`).
- Owner: crates/nugget-store, crates/nugget-vault `edits.rs`.
- Fix direction: when a user claim folds into an existing one, keep the user's text and status.

### GG5-6 · High · Notes and calendar files are dated by file modification time: a copied old note overrides newer mail and flips a current fact
- Verified: REPRODUCED (`verify/truth-sensitive.md`). Area: truth-sensitive.md, third finding.
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s3_copied_note_dates.py <dir>`: the same corpus twice, once with each
  note's mtime set to its own date, once with a March note copied in today (mtime = now).
- Expected: a note dated 2026-03 (frontmatter `date:`) is older than Dana's August mail; Corvid Labs stays her current employer.
- Actual: copied variant: `Dana Whitfield works at Brightwell` active, Corvid Labs superseded; "Why?" shows the copy time
  (2026-10-30T19:56:57Z) as the note's date. A user who copies or restores an old notes folder rewrites current facts.
- Frequency: 2/2 author, 1/1 verifier.
- Cause: `item_date` (`crates/nugget-knowledge/src/extract/mod.rs:96`) reads only the mail `Date` header; frontmatter `date:`
  and `DTSTART` are ignored; the files connector's item date is the mtime.
- Owner: crates/nugget-knowledge (EE1's brief targets this: "claims carry a validity interval from the source date").
- Fix direction: date a note by frontmatter `date:`/`created:` (then a date in the file name) and a calendar item by its event
  date for currentness; keep mtime only as a fallback.

### GG5-7 · High · "Nothing pending" while new files are still unextracted: a backlog of runs swallows new-item signals
- Verified: REPRODUCED (`verify/real-model.md`; coverage counts match exactly). Area: real-model.md, second finding.
- Repro: `python3 -I qa/GG5/scenarios/real-model/coverage.py qa/GG5/runs/41117293540-qwen3-4b` (files that reached the graph per
  snapshot): 4B phase2 24/42, 8B phase1 7/23 and 0 of the 19 phase-2 files at phase2, while `knowledge.status` said `pending_items: 0` for 20 s.
- Expected: new mail is extracted within a run or two, and `pending_items` counts work not yet done.
- Actual: in the 4B run 18 of the 19 phase-2 files (Dana's new-job mail, the launch move, the breakup note, both `Work/` documents)
  were not extracted when status said nothing was pending; they waited for the 10-minute periodic run or a later commit.
- Frequency: all 3 real-model runs (EE1's faster CPU less). Never without a model (runs finish in milliseconds).
- Cause: with a CPU model a 120 s run handles 1-4 files, so the plan is carried over (`crates/nugget-knowledge/src/knowledge.rs:214-220`, `292-298`);
  ingest commits that arrive meanwhile are consumed by the worker (`crates/nuggetd/src/knowledge.rs:227-231`) but never planned;
  `pending_items` is only `last_run.remaining` (`crates/nuggetd/src/handler_m2.rs:341`) and a running run is not reported.
- Owner: crates/nuggetd knowledge worker and status; crates/nugget-knowledge carry (not in EE1's brief; the EE1 run shows it).
- Fix direction: re-plan incrementally when a carried chain ends if commits arrived; report pending as changes after the cursor
  plus the carry, and add `running`.
- Note for this report: phase1/phase2/edits snapshots of the 4B and 8B runs are incomplete because of this; numbers below use `reextract`/`restart`.

### GG5-8 · High · A knowledge run without the model replaces the model's facts: at every restart before the chat model is up, and at shutdown
- Verified: REPRODUCED with corrected counts (`verify/real-model.md`). Area: real-model.md, third finding.
- Repro: 4B daemon log `qa/GG5/runs/41117293540-qwen3-4b/nuggetd.2026-10-30.log`: `21:28:32.804 knowledge run processed 19 … took_ms 111`,
  then `21:28:36.574 local runtime ready role chat`; `python3 -I qa/GG5/scenarios/real-model/restart_diff.py qa/GG5/runs/<run>`.
- Expected: restarting the app does not change the knowledge graph.
- Actual: 4B 152 → 120 claims, 116 → 86 entities, 30 vault pages turned into "no longer in your knowledge graph" stubs (20 claims gone,
  12 active and 5 superseded → retracted); 8B 14 files in 77 ms, 7 claims retracted; EE1: the run kept going after SIGTERM once llama-server was
  stopped (`background tasks did not stop in time`, then `LLM extraction failed … error decoding response body`), and claims were still written 24 ms after the daemon logged "stopped"; there, junk came back
  as current (`Mara has_title neighbour of the year`). After restart `knowledge.status.extractor_provider` stayed null 30 s after the chat model was ready.
  RESTART_LATER
- Frequency: 4B and 8B at startup, EE1 at shutdown.
- Cause: `extract_and_apply` retracts the file's old evidence first (`crates/nugget-knowledge/src/session.rs:776`), the LLM step returns
  `LLM_PENDING` when no provider is ready (`session.rs:837`), and only the deterministic part is applied (`session.rs:799`). The files
  were pending because of GG5-7. Shutdown: `crates/nuggetd/src/daemon.rs:654-683` stops the runtime while the run continues.
  Nothing wakes the worker when the model becomes ready; files that already failed 3 times never get the model again.
- Owner: crates/nugget-knowledge session.rs (EE1's area, not in its brief), crates/nuggetd knowledge.rs/daemon.rs.
- Fix direction: while a configured model is not ready yet, defer changed files (or apply deterministic facts without retracting
  model facts); wake the worker on "local runtime ready"; stop runs before stopping the runtime.

### GG5-9 · High · A person is named after whichever display name was processed first ("R. Chen", "Sam"), and the user cannot rename them
- Verified: REPRODUCED (`verify/resolution.md`, `verify/app.md`, `verify/vault-edits.md`). Areas: resolution.md first; app.md "Hard-case names"; vault-edits.md last.
- Repro: `python3 qa/GG5/scenarios/resolution/run_fixes.py <out>` (base step) and `run_order.py <out>` (same corpus with
  `Calendar/` renamed `zz-Calendar/`).
- Expected: Robert Chen (manager, full name in 12 files) is "Robert Chen"; Sam Okonkwo is "Sam Okonkwo".
- Actual: `{"name": "R. Chen", "aliases": ["Robert Chen"]}` (from one invite's `CN="R. Chen"`), `{"name": "Sam"}` (from
  `ATTENDEE;CN="Sam"`), on the page title, every link and the app's list; with only the folder renamed, "Robert Chen" and "Sam
  Okonkwo". After phase 2, re-extraction and restart it stays "R. Chen". The user cannot fix it: no rename IPC, an alias edit only
  adds an alias, editing the `# R. Chen` heading is silently ignored (no conflict), deleting a page is ignored until the next render.
  Real models add more names for the same person (`Bob`, `Rob Chen`).
- Frequency: deterministic 3/3 + order flip 1/1; all real-model runs (EE1 too: `R. Chen` + `Rob Chen`).
- Cause: the first occurrence names the entity (`extract/calendar.rs:191`, `extract/mail.rs:129`); later names become aliases
  (`resolve.rs:691`); files are processed in path order.
- Owner: crates/nugget-knowledge resolve.rs (EE1 targets order-independence and initials; not a user rename), nugget-vault (heading edits).
- Fix direction: pick the display name from all known names (fullest multi-word, then most frequent; never an initial or lone first
  name over a full name), re-evaluated on attach; add `knowledge.rename` or honour a heading edit.

### GG5-10 · Medium · Fact sentences are frozen at extraction, so after a merge, split or better name they name the wrong entity
- Verified: REPRODUCED (`verify/resolution.md`, `verify/app.md`). Areas: resolution.md second; app.md "Merging Tessellate into Lumenfold".
- Repro: `run_fixes.py`; restart snapshot pages of Mara Okafor and Gus Ferreira.
- Actual: Mara's page `- Mara Okafor corresponded with [[Person/Sam (…)|Sam]] Patel by email.` linking Sam Okonkwo and citing only his mail;
  after merging Tessellate into Lumenfold, Gus `works at [[Organization/Lumenfold (…)|Tessellate]]`; with no user action
  `Dana Whitfield works at [[…Corvid Labs…|Corvidlabs]]`.
- Frequency: 2/2, verifier 1/1.
- Cause: claim `text` is written once; merge folds duplicates into the oldest claim and keeps its text (`crates/nugget-store/src/graph.rs:1792`,
  `fold_duplicate_claims` ~`:1936-1973`); the vault renders `c.text` (`crates/nugget-vault/src/render.rs:327`).
- Owner: nugget-store, nugget-vault rendering (EE1 owns vault rendering of claims; not in its brief).
- Fix direction: render deterministic claims from (subject, predicate, object) with current names; regenerate text when an end is merged, split or renamed.

### GG5-11 · Medium · A person whose address changed, and a renamed company, stay two entities
- Verified: REPRODUCED (`verify/resolution.md`; "even with a model" is from code reading plus the integration real-model runs). Areas: resolution.md third; app.md "Hard-case names".
- Actual: two `Gus Ferreira` (old `tessellate.example`, new `lumenfold.example` address), two identical rows in the app list, the merge dialog
  and graph labels with nothing telling them apart; `Tessellate` and `Lumenfold` two organisations, although the rename mail says so.
- Frequency: deterministic 3/3; integration 4B and 8B. **EE1's branch merges Gus** (real-model run); Tessellate/Lumenfold stay split there too.
- Cause: any candidate with a different strong identifier is refused (`resolve.rs:491`, also gating embeddings at `:568`).
- Owner: crates/nugget-knowledge resolve.rs (EE1: "corroborated merges", partly fixed on its branch).
- Fix direction: a different email is weak counter-evidence for a person, not a veto, when the full name and an org/thread corroborate;
  record the reason. Show a distinguishing hint (email domain) for same-named entities in the app.

### GG5-12 · Medium · Splitting off a wrongly merged person leaves the old id and page pointing at the wrong person
- Verified: REPRODUCED (`verify/resolution.md`). Area: resolution.md fourth.
- Repro: `run_fixes.py` steps `wrongmerge`/`split`: `knowledge.merge {into: Sam (Okonkwo), from: Sam Patel}`, then
  `knowledge.split {entity, claims, name: "Sam Patel", identifiers: [["email","sam.patel@brightwell.example"]], aliases: ["Sam Patel"]}`.
- Actual: `knowledge.entity` on Sam Patel's old id still returns Okonkwo after restart; the old page is a stub redirecting to Okonkwo; a claim
  the merge folded across both Sams cannot be split (whole claims only).
- Cause: `split_tx` never re-points earlier `merged_into` rows (`crates/nugget-store/src/graph.rs:1100` vs `:1874`).
- Owner: nugget-store / nugget-graph split; nuggetd `knowledge_split` (EE1's merge reasons are adjacent, not this).
- Fix direction: an "unmerge" of a recorded merge, or re-point `merged_into` of ids whose identifiers move; split folded claims by evidence.

### GG5-13 · Medium · A newer source and the user's edited value both stay current for a single-valued fact
- Verified: REPRODUCED (`verify/vault-edits.md`). Area: vault-edits.md fourth.
- Repro: `python3 qa/GG5/scenarios/vault-edits/run_s10_s11.py <out>`: edit Dana's `works_at`, then add a newer note saying she joined Kestrel Robotics.
- Actual: Dana has `user_asserted` (the user's employer) and `active` (Kestrel) side by side; the page lists two current employers with nothing marking the clash.
- Cause: only `Active` rivals are superseded (`crates/nugget-knowledge/src/session.rs:1328`).
- Owner: crates/nugget-knowledge (EE1's brief targets supersession of single-valued claims).
- Fix direction: decide explicitly (newer source supersedes the user's value, or the user's value wins and the new one is shown as a proposal) and show it.

### GG5-14 · Medium · "Restore" on a superseded fact makes the old job current again, with no Undo
- Verified: REPRODUCED (`verify/app.md`). Area: app.md first.
- Repro: app setup in app.md (vite build, `e2e/bridge.mjs` standalone against the persona daemon), Knowledge → Dana Whitfield → the
  struck "works at Brightwell" fact (badge Superseded) → **Restore** (styled red).
- Actual: claim becomes `active` next to Corvid Labs (two employers in the app and the vault); no Undo; only "Reject" afterwards (sticky
  `rejected`, not back to `superseded`); "Facts (6)" counts the superseded fact while the list row says 5.
- Cause: `apps/desktop/src/knowledge.ts:232-239` (`factAction`: everything not current is restored, `undo: null`), `components/EntityDetail.tsx:364`, `:472`.
- Owner: desktop app (not EE1). Fix direction: no Restore on superseded facts, or a confirmed "Make current again" with Undo; count current facts only.

### GG5-15 · Medium · The model makes wrong or invented facts current
- Verified: REPRODUCED (`verify/real-model.md`). Area: real-model.md, sixth finding.
- Repro: `python3 -I qa/GG5/scenarios/real-model/claims_of.py qa/GG5/runs/<run> reextract -i Work/` (also `-n Dana`, `-p due_on`, `-i fwd-data-weekly`).
- Actual: EE1 `R. Chen leads Harbor` from "I asked her in March to lead Harbor"; EE1 `Mara Okafor has title Principal Engineer` current from
  "I support her case for promotion to Principal Engineer"; 4B/EE1 an organisation "VP Engineering" (`R. Chen works at VP Engineering`);
  4B three junk current titles for Mara from an evidence list (next to the correct `Staff Engineer, Platform`); 4B Dana both at Brightwell and Corvid Labs from one sentence; 8B Dana still
  "Design Lead" from "Before that she was Design Lead"; 8B three conflicting current Harbor launch dates. The injection mail's claims were
  stored (4B `Mara works_at Corvid Labs`, 8B `has_title Head of Platform`) but superseded, never current. Junk: at `reextract` 63 of 4B's 67
  model `has_title` claims are not titles (8B 11/12, EE1 13/16).
- Not seen: no run read Robert's "I" as Mara or made Theo her partner after the breakup (but `first_person_is_me` treats every Markdown file as
  the user's, `llm.rs:1226-1235`, and ignores `author:` frontmatter: a latent risk).
- Owner: crates/nugget-knowledge llm.rs normalisation and gate (EE1).
- Fix direction: validate `has_title` objects (no dates, amounts, ids, sentences), treat modal/past-tense quotes ("case for", "before that")
  as non-current, drop claims from text marked as an injection rather than storing them, honour `author:`.

### GG5-16 · Medium · The deterministic extractors assert things the source does not say
- Verified: REPRODUCED with a wording fix (`verify/truth-sensitive.md`). Area: truth-sensitive.md fifth.
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s4_calendar_claims.py <dir>`.
- Actual: "Mara Okafor attended Harbor launch day on 2026-12-03" (a future event; accepting counts as attending, `extract/calendar.rs:274`);
  weekly events collapse to their first date; "Why?" for "R. Chen attended" quotes the Organizer line; everyone named in a 1:1 note
  "works on Harbor" (`extract/markdown.rs:373`, a vendor too); "corresponded with" from a Cc alone.
- Owner: crates/nugget-knowledge extract/ (EE1's area; not in its brief).
- Fix direction: `invited_to`/`accepted` for future events and attended only after the event; recurrence-aware dates; members only from
  frontmatter, not from every person mentioned; corresponded_with from From/To only.

### GG5-17 · Medium · Email addresses reach the vault through footnote quotes, although the guide says they are kept out
- Verified: REPRODUCED (`verify/truth-sensitive.md`). Area: truth-sensitive.md fourth.
- Actual: 13 distinct addresses on 20 pages (deterministic), including the counsellor's; one calendar footnote lists every invitee's address.
- Cause: `include_identifiers` applies to frontmatter only (`crates/nugget-vault/src/render.rs:200`), not footnote quotes (`render.rs:386`).
- Owner: nugget-vault rendering (EE1). Fix direction: redact addresses and phone numbers in quotes unless `include_identifiers`.

### GG5-18 · Medium · Local model calls time out at 120 s on a 4-core CPU, and each timeout uses up a whole run
- Verified: REPRODUCED (`verify/real-model.md`; per-file counts match). Area: real-model.md fourth.
- Repro: `python3 -I qa/GG5/scenarios/real-model/llm_log.py qa/GG5/runs/<run>`.
- Actual: 4B 10 timeouts on 5 files (GP note and counselling invite gave up after 3), 8B 13 on 8; 70-93 s per extracted file on EPYC 7763
  (EE1's run on a 9V45: 0 timeouts, 29-54 s). The partial answer is thrown away and a timeout ends the run (a late one can stretch it to 226 s). Deterministic facts survive.
- Cause: `max_output_tokens` 1024 at the estimated ~8-10 tok/s (not measured in the artifacts) does not fit `timeout_ms = 120000`; `chat_json` wraps the whole stream in a timeout (`llm.rs:257-261`).
- Owner: crates/nugget-knowledge llm.rs/config.rs. Fix direction: derive the timeout from output cap and measured speed, or keep what
  streamed by the deadline; a smaller cap for calendar items; don't let one timeout consume the run budget.

### GG5-19 · Medium · Re-extracting an unchanged file gives different model facts, and first names stay split
- Verified: PARTIALLY REPRODUCED (`verify/real-model.md`): counting only model claims on `.eml`/`.ics` files whose text did not change,
  15 of 20 files changed (the author's 19/34 also counted deterministic and edited-note changes); the nonce cause is a hypothesis. Area: real-model.md fifth.
- Repro: `python3 -I qa/GG5/scenarios/real-model/item_claims_diff.py qa/GG5/runs/41117587773-qwen3-4b-ee1 edits reextract`.
- Actual: 19 of 34 files changed their claims when only re-saved (e.g. `(Mara Okafor, works_at, Tessellate Systems)` →
  `(Gus Ferreira, has_title, Tessellate Systems)`). At `reextract`: 4B Theo ×4, Dana ×3, Ife ×3; 8B Theo ×5, Dana ×4.
- Likely cause (not proven): a fresh random nonce in every prompt (`llm.rs:465`, `619`) at temperature 0; graph-dependent post-processing; the graph fold skips
  names the graph already has (`llm.rs:2069-2073`).
- Owner: crates/nugget-knowledge (EE1 targets folding; its run improves it: 13/20 gold entities 1:1, Sam Patel and Gus right, no bare Bob;
  Theo, Ife and Robert still split).
- Fix direction: deterministic nonce (hash of item key and version); fold a bare first name into the one full-name person it co-occurs with, and re-settle later.

### GG5-20 · Medium · An alias written with normal YAML spacing (`["A", "B"]`) causes a conflict that never clears
- Verified: REPRODUCED (`verify/vault-edits.md`, `verify/resolution.md`). Area: vault-edits.md third.
- Repro: add `, "Bobby"` to a page's `aliases: ["R. Chen"]` list as a person would type it. (Also hit by this harness's own edit in every run:
  `vault.conflicts: ["Person/R. Chen (…).md"]`.)
- Actual: a `.nugget/conflicts/` sidecar that differs from the page only by `", "` vs `","`; it persists through every stage, and aliases Nugget learns later are not written.
- Cause: the frontmatter three-way merge compares raw lines (`crates/nugget-vault/src/merge.rs:175-185`).
- Owner: nugget-vault. Fix direction: merge parsed frontmatter values, not text.

### GG5-21 · Medium · An editor that strips HTML comments makes the edit a no-op and duplicates the page
- Verified: REPRODUCED (`verify/vault-edits.md`). Area: vault-edits.md fifth.
- Repro: `python3 qa/GG5/scenarios/vault-edits/run_strip.py <out>`.
- Actual: the reworded line never reaches the graph; fresh generated regions are written above a stale copy of the user's page; after
  re-extraction the copy's footnotes point to the wrong sources.
- Owner: nugget-vault edits/merge. Fix direction: detect a page without markers and treat it as a whole-page user edit (or a conflict), never a duplicate.

### GG5-22 · Medium · Fact recency in the app uses the store's write time
- Verified: REPRODUCED (`verify/app.md`). Area: app.md second.
- Actual: every fact of a freshly indexed corpus shows under TODAY as "2 minutes ago"; a rejected or superseded fact jumps to the top as "just now".
- Cause: `apps/desktop/src/recency.ts:33-44` uses `updated_at`. Fix direction: group by the newest evidence's `observed_at` (source date).

### GG5-23 · Low · One thing becomes several entities: a "harbor" topic next to the Harbor project, a draft note as its own project, file-name names
- Verified: REPRODUCED (`verify/resolution.md`). Area: resolution.md fifth.
- Actual: project `Harbor`, topic `harbor` (from `#harbor`), project `Harbor-launch-plan` (file stem), alias `Dana-Whitfield`, fact
  `Dana-Whitfield is about friends.`; `[[Tessellate Systems]]` never resolves, so the org stays "Tessellate" (its domain).
- Cause: tags always mint topics (`extract/markdown.rs:227`); a typed note takes the file stem (`markdown.rs:151-157`); wikilinks match exact names only (`session.rs:1060`).
- Owner: nugget-knowledge (EE1). Fix direction: link a tag to a same-named project; prefer a note's H1; resolve a wikilink to the org whose domain spells it.

### GG5-24 · Low · The graph's kind filter leaves floating nodes, and `limit` is applied before the filter
- Verified: REPRODUCED (`verify/app.md`). Area: app.md sixth.
- Actual: Organizations filter on Mara's graph: "5 entities · 0 links"; `knowledge.graph {center: Mara, limit: 5, kinds: ["organization"]}` returns only Mara with `truncated: true`.
- Cause: `crates/nuggetd/src/graphview.rs:354-412`. Fix direction: filter during traversal (keep connecting nodes greyed) and apply `limit` after the filter.

### GG5-25 · Low · "Why?" quotes for calendar and frontmatter facts are Nugget's reconstruction, not text in the file
- Verified: REPRODUCED (`verify/truth-sensitive.md`: 35 = 30 .ics + 5 .md, plus re-quoted mail header lines). Area: truth-sensitive.md sixth.
- Actual: quotes such as `members: Dana Whitfield` where the file has a YAML list; `.ics` quotes from Nugget's text rendering (`ics.rs:412-427`); `To: Sam Patel <…>` per recipient.
  Faithful decodings, not invented text, but the code comment says "exactly as stored".
- Fix direction: quote the original lines (or label the quote as derived).

### GG5-26 · Low · Single-word aliases added in the vault never resolve new items (undocumented)
- Verified: REPRODUCED (`verify/vault-edits.md`). Area: vault-edits.md sixth.
- Actual: after adding `Bobby`, `Gustavo`, `Ifey`, a new note naming them creates three new people; `Bobby Chen` resolves. By design (person names
  need two words) but the guide's alias row does not say so. Fix direction: use user-added aliases for resolution (they are trusted), or document it.

## What works well

Keep these working; each was checked against the running daemon (and the second, verifying agent).

- **The verbatim-quote gate.** No model claim in any run has a quote that is not in its source; prompt-injection text never produced a
  current fact (the 4B and 8B runs stored the injected claims as superseded; EE1 extracted nothing from the injection).
- **No wrong merges.** Sam Patel and Sam Okonkwo stay apart in every run and snapshot; Ife and Mara are never merged; documents written by
  others (`Work/`) never make their author the user, and no run read Robert's "I" as Mara or kept Theo as her partner after the breakup.
- **Evidence everywhere.** Every active claim is cited; footnotes resolve to the right item (`nugget://item/…` via MCP `resources/read`).
- **Mail dating and supersession.** Mail is dated by its `Date` header; Dana's job change supersedes correctly when dates are right
  (deterministic; EE1 with the model). Cancelled events are skipped; automated senders (the bank) create no person.
- **Sensitive basics.** Card numbers and IBANs are detected; MCP hides a flagged item completely (search, entities, resources); the
  deterministic extractors put no account number, card number, salary or diagnosis into the vault.
- **Vault edits that do work:** deleting a fact line makes it `rejected` and it stays rejected through re-extraction and restart; bullets under a
  heading take its predicate, others become `note`; `## Notes` text survives every stage; rewording a line that keeps its `[[link]]` works;
  edits made while the daemon is stopped are applied at start-up; editors that save atomically are picked up; a two-word alias resolves new items.
- **Merge and split.** Old ids redirect, old pages become redirect stubs, no page links to an old id, duplicate claims fold with all their citations,
  and merges and splits survive re-extraction and restart. Dana's two addresses merge through her typed person note; "Corvidlabs" is renamed
  "Corvid Labs" once the real name is seen.
- **The user's own entity** (`is_me`, from `[knowledge.me]`) absorbs nothing wrong: 22 deterministic claims, all true.
- **Graph IPC** is exact: overview and centred views match the current claims, `truncated` is exact at the boundary, `kinds`/`source`
  filters work, bad input gets proper errors. The app draws 45+ nodes, list mode and depth selector work.
- **App:** Why? shows the quote with a working Open link; Open in vault opens the right page; Reject/Undo round-trips through IPC, graph
  and vault; the merge preview is clear.
- **Robustness:** a timed-out model call keeps the file's deterministic facts; retries stop after 3 attempts per version; thinking is off
  and temperature 0 for extraction; vault pages always validate.
- **EE1's branch** (not merged; one run, faster CPU, so not a clean comparison) already fixes or improves: Gus Ferreira's two addresses are one
  person (GG5-11), Sam Patel never fragments, no bare "Bob", Dana's employer settles correctly, far fewer junk titles (16 vs 67),
  gold entities 1:1 13/20 vs 10/20.

## Notes on the harness and the grader

- The grader's gold-fact recall is strict: it needs the exact predicate from the extractor's
  vocabulary and the gold object, so deterministic `attended`/`corresponded_with` facts and LLM facts
  with a near-synonym predicate do not count. Read the recall numbers as a floor; the findings are
  each checked by hand.
- `quote_not_in_source` counts quotes not found verbatim in the raw file. Most are Nugget's faithful
  decoding of `.ics` and mail headers (GG5-25); they are not invented text.
- Files from git have no meaningful modification time, and Nugget dates notes and calendar files by
  it (GG5-6), so the harness sets each file's mtime from the date in its name or frontmatter, as a
  user's own files would have.
- The harness's edit E1 rewrites a Dana fact line naming Brightwell into "Dana is Head of Product at Corvid
  Labs (fixed by me)". In the 4B and 8B runs it hit an already correct line, so Dana's employer after `edits` in
  those runs reflects the edit, not the model (the EE1 run skipped it). The harness also waits on
  `pending_items`, which GG5-7 shows is not reliable with a CPU model: compare runs at `reextract`/`restart`.
- Rerun: push to `qa/knowledge` is no longer a trigger; dispatch `QA GG5` with `models`
  (comma-separated manifest ids) and optionally `product_ref` (build the product from another
  branch, harness and corpus unchanged).
