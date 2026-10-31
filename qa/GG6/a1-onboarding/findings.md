# GG6 area A1: onboarding and the first search

Build: `1b7a0a5` (includes merge 62b538f and EE6's "a step counts as done only when its goal is met"); baseline run on
`5f1998e`. Real `nuggetd` (run through `setpriv` without CAP_DAC_OVERRIDE, so chmod 000 blocks it even as root) and the
real `dist/` in headless Chromium through the e2e bridge. How to run: `README.md`.

## Results

| Suite | Scenarios | Checks | 5f1998e | 1b7a0a5 |
|---|---|---|---|---|
| `flows.mjs`: continue, skip-each, skip-all, back, resume (quit on each of 6 steps), setup-again | 6 | 33 | 27 pass / 6 fail | 27 / 6 |
| `sources.mjs`: 13 folder mistakes | 13 | 13 | 9 / 4 | 9 / 4 |
| 10 day-one questions on 141 synthetic files (md, txt, eml, ics, pdf, docx, csv) | 10 | hit@3 | 10/10 | 10/10 |

Timings on 1b7a0a5, ms after app start, clicking as fast as the UI allows: daemon ready 112, Welcome 277, AI step 581,
source step 940, folder added 1374, "141 items indexed. Search works now." 2408 (1.0 s from adding to searchable),
Done 3090, Search open 3370; each search about 175 ms. A synthetic home folder of about 3,800 files reached
"1,501 items indexed" in 4.5 s (node_modules, dot-folders and Trash skipped). Hit@3: rank 1 for 8/10 questions;
rank 3 for "what did Priya say about the budget" (a filler note ranks above her email) and "when does my car
insurance renew" (answer in a PDF).

## Findings

**A1-F1 (High). Folders with nothing indexable spin on "getting ready to index…" forever** (empty, unsupported-only,
chmod 000, a single file over 50 MB). 4/4 on both builds.
- Repro: `node qa/GG6/a1-onboarding/sources.mjs empty,unsupported,chmod000,huge200mb`
- Expected: the step says nothing was indexed and why (no supported files / permission denied / file over 50 MB skipped), with a next step.
- Actual: the row stays at `empty: getting ready to index…` (still after 26 s; after 91 s for the 200 MB file). The daemon reports `state "watching", items 0, last_error null, health {code "ok", reason "Up to date."}`, even for the chmod 000 folder (`shots/20-src-chmod000.jpg`). The step still counts as done; the Done step lists the folder under "Sources: Added".
- Owning code: `apps/desktop/src/onboarding/steps.tsx:80` (`indexPhase` treats 0 items as "waiting"); `crates/nugget-connector-files/src/scan.rs:328` (unreadable) and `:354` (over `max_file_bytes`) only log; nothing reaches `last_error`/`health`.
- Fix: the connector reports scan counts (seen / unsupported / too big / denied) and sets `health` when a root can't be read; the UI treats "watching, 0 items after a finished scan" as settled with its own message.

**A1-F2 (Medium). Adding the same folder twice creates a duplicate source and duplicate results.** 1/1 on both builds.
- Repro: `node qa/GG6/a1-onboarding/flows.mjs resume` (adds a folder, quits, reopens, adds it again, as a user does when the step looks empty, A1-F3).
- Actual: `files-1` and `files-2`, both "resume-src", 40 items each; Status shows 80 items; search "heron resume test 7" returns `n7, n7, …`. No warning.
- Owning code: `crates/nuggetd/src/handler.rs:618` `sources_add` doesn't compare with other sources; `crates/nugget-connector-files/src/config.rs:450` `check_roots` only checks within one source.
- Fix: refuse or say "already added as X" when a folder equals or overlaps an existing source's.

**A1-F3 (Medium). The source step forgets the added folder after Back, quit-and-reopen, or Run setup again.** 3/3 on both builds.
- Repro: `node qa/GG6/a1-onboarding/flows.mjs back,resume,setup-again`
- Actual: before Back the step shows `back-small: 1 item indexed. Search works now.`; afterwards no rows and "Add a folder" again. On 1b7a0a5 the step silently counts as done (`sourcesFound`) but shows nothing, which invites A1-F2.
- Owning code: `apps/desktop/src/onboarding/steps.tsx:89` (added folders are component state only).
- Fix: list existing sources with live progress; the button becomes "Add another source".

**A1-F4 (Medium). "Ask with this context" with no AI shows a raw daemon error and no next step.** 1/1 on both builds.
- Repro: `node qa/GG6/a1-onboarding/flows.mjs continue`
- Actual: alert `no provider is available for the synthesis tier`, only "Ask again". The Chat page in the same state says "No models are set up yet. Add one on the Models page…" with "Go to Models".
- Owning code: `crates/nuggetd/src/handler.rs:409`; `apps/desktop/src/pages/Search.tsx:291` shows `errorMessage(err)` verbatim.
- Fix: reuse Chat's copy and button, or disable Ask (keep Find context) with that hint.

**A1-F5 (Low). Folder-form errors are daemon jargon.** 5/5.
- Repro: `node qa/GG6/a1-onboarding/sources.mjs tilde,tildeDocs,overlap,file,nonexistent`
- Actual: "root \`~\` must be an absolute path" (the form's `isAbsolutePath` accepts `~`, so it fails only after Submit); "roots \`…/overlap\` and \`…/overlap/sub\` overlap".
- Owning code: `apps/desktop/src/components/AddSourceWizard.tsx:234,167`; `crates/nugget-connector-files/src/config.rs:459,468,551`.
- Fix: expand `~`; map daemon errors to codes with friendly copy.

**A1-F6 (Low, new in 1b7a0a5). Choosing "Decide later" is reported as skipping the step.** 1/1.
- Repro: `node qa/GG6/a1-onboarding/flows.mjs continue`
- Actual: with "Decide later" selected the footer says "Nothing set up here yet: Continue skips this step."; Done says "You skipped one step. Go back to: Choose your AI" (`shots/07-done.jpg`), contradicting the option's own text.
- Owning code: `apps/desktop/src/onboarding/machine.ts:76` `stepGoalMet`.
- Fix: count `ai === "later"` as a decision; keep "AI model: Not chosen yet" in the summary.

**A1-F7 (Low). No AI option is preselected** although one carries a "Recommended" badge (`AiStep.tsx`).

## Friction log

| Moment | Who | Evidence |
|---|---|---|
| A folder with nothing indexable spins forever, then setup says Done (F1) | some | `shots/20-src-chmod000.jpg` |
| Ask on Search shows a red internal error instead of "set up a model" (F4) | most users who skip AI | `results-flows.json` |
| Back at the source step it looks like nothing was added; they add it again and get duplicates (F3, F2) | some | `results-flows.json` (resume) |
| "Decide later" is called a skipped step (F6) | most users without AI | `shots/07-done.jpg` |
| `~/Documents` passes the form, then fails on Submit with jargon (F5) | few | `results-sources.json` (tilde) |

## What works well

- Resume: quitting on each of the 6 steps and reopening lands on the same step (6/6). Skip-all persists; Run setup again restarts at Welcome and keeps the AI choice.
- The first useful search is fast: about 2.4 s of software time for 141 files, with live progress; Done's "Search your sources" goes straight to Search.
- Retrieval without any model: 10/10 hit@3 across PDF, DOCX, .eml and .ics; snippets contain the answers.
- A whole home folder: node_modules, dot-folders and Trash skipped; 1,501 items in 4.5 s. Unicode, emoji and space paths work.
- A folder with one unreadable file among readable ones indexes the rest; a relative path gets a clear hint before Submit.
- The Chat page with no model explains itself and links to Models. No uncaught page errors in any run.

Harness notes: the daemon is started with `TMPDIR=/tmp` (a deep scratch path makes the socket path exceed `sun_path`).
