# GG5 findings: vault-edits

Question: does editing a vault page merge back and survive re-extraction and restarts, as
`docs/book/src/guide/knowledge.md` "Editing the vault" promises?

Commit `e7299584da9ca18c83793107da6d97ceec7c8f37`, debug `target/debug/nuggetd`, `--no-models`
(deterministic extractors only). The vault merge and claim-status logic below runs the same with a
model; only which facts get extracted would differ.

Scripts (each starts a fresh daemon with its own `--home` under the given out dir, indexes
`qa/GG5/corpus/phase1` then `phase2`, makes the edits, and stops the daemon):
- `python3 qa/GG5/scenarios/vault-edits/run_main.py <out>`: scenarios 1-9 on separate pages, checked
  after the edit, after re-extraction, after a newer source, after a restart and after an offline
  edit. Results are in `<out>/checks.json` (claim statuses via `knowledge.entity` plus page lines),
  and there is a vault copy per stage.
- `python3 qa/GG5/scenarios/vault-edits/run_s10_s11.py <out>`: a newer source after a user edit,
  an edit that keeps the `[[link]]`, and a two-word alias. Results are in `<out>/results.json`.
- `python3 qa/GG5/scenarios/vault-edits/run_strip.py <out>`: an editor that drops HTML comments,
  and a deleted page file. Results are in `<out>/results.json`.

Runs used for this report are under
`/tmp/claude-0/-home-user-nugget/3826b9b2-0317-5073-9bf7-b47d75df8cb7/scratchpad/vault-edits/{main,s10,strip}`.

### Re-extraction brings back the fact the user edited: the old generated line returns next to the user's version
- Severity: High (an edit is silently undone: no conflict and no notice)
- Repro: `run_main.py <out>`. On the Harbor page, the line
  `- Harbor is part of [[Organization/Brightwell Analytics (…)|Brightwell Analytics]]. [^4] <!-- c:36 -->`
  becomes `- Harbor is a Brightwell Analytics project (fixed by me) [^4] <!-- c:36 -->` (the marker is
  kept, the link is removed). On Dana Whitfield's page, the `works at … Corvidlabs` line (c:51)
  becomes `Dana Whitfield is Head of Product at Corvid Labs (fixed by me)`. Then the sources are
  re-saved (`Notes/Projects/Harbor.md` and `Notes/People/Dana-Whitfield.md` get a line appended, and
  `.eml`/`.ics` files get their mtime touched). `run_s10_s11.py` repeats the Dana case.
- Expected (guide): "The old fact becomes `superseded`; your text becomes a new fact with status
  `user_asserted`". Only `rejected` is described as sticky, but a user edit that comes back on the
  next re-save of a source amounts to the edit being reverted.
- Actual (`main/checks.json`):
  - After the edit: c:36 is `superseded` and c:62 `user_asserted` ("…(fixed by me)"). The page
    shows only the user line. Correct so far.
  - After re-extraction: c:36 is `active` again. The page now shows both lines:
    ```
    - Harbor is part of [[Organization/Brightwell Analytics (ent_mauqemdcj5qjwlmx)|Brightwell Analytics]]. [^3] <!-- c:36 -->
    - Harbor is a Brightwell Analytics project (fixed by me) (you) <!-- c:62 -->
    ```
  - Dana (single-valued `works_at`): c:51 goes from `superseded` to `active`, so the page has two
    current employers:
    ```
    - Dana Whitfield works at [[Organization/Corvid Labs (…)|Corvidlabs]] (has a corvidlabs.example address). [^3][^4][^5] <!-- c:51 -->
    - Dana Whitfield is Head of Product at Corvid Labs (fixed by me) (you) <!-- c:65 -->
    ```
  - The state stays like this after the newer source, after the restart and after the offline
    start.
- Reproduces: 3/3 edited lines across 2 runs (multi-valued `part_of`, and single-valued `works_at`
  twice). It does not happen when the edited text keeps the original `[[link]]` (S11 in
  `run_s10_s11.py`): the user claim then has the same triple, takes the evidence, and keeps the
  user's text.
- Cause: a user edit records the old claim as plain `Superseded` (crates/nugget-vault/src/lib.rs:722-723).
  When its item is re-extracted, `retract_tx` turns any evidence-less claim that is not
  `user_asserted`/`rejected` into `retracted` (crates/nugget-store/src/graph.rs:567-572). The
  re-extraction then reuses that claim id:
  - Multi-valued predicates: `add_evidence_op` (crates/nugget-knowledge/src/session.rs:1229-1231),
    and `add_evidence_tx` flips `retracted` to `active` (crates/nugget-store/src/graph.rs:910-915).
  - Single-valued predicates: `add_single_valued` with `revive`. `freshness()` of a claim with no
    evidence is its `updated_at` (session.rs:702-704), which is the moment of the retraction, so it
    is newer than the user's claim. It is therefore not "historical" and gets set back to `Active`
    (session.rs:1313-1316).
  The user-asserted claim is never compared, because only `Active` rivals are superseded
  (session.rs:1328).
- Owning area: crates/nugget-knowledge (session.rs claim lifecycle), crates/nugget-store graph.rs,
  and crates/nugget-vault lib.rs (`apply_plan`). This is EE1-adjacent: EE1's brief covers
  supersession of claims, but not user edits.
- Fix direction: record a user edit as a sticky user decision (for example a `superseded_by_user`
  flag or link, or treat it like `rejected` for revival) so that retraction and re-extraction never
  revive it, and supersede extracted values of the same subject and predicate while a
  `user_asserted` value exists.

### Retyping a fact line without its marker (but keeping the link) silently drops the user's text
- Severity: High (silent data loss: the user's wording disappears and the original sentence is
  shown with a `(you)` tag)
- Repro: `run_main.py <out>`, step "Priyanka: S8b". On the Priyanka Rao page the line
  `- Priyanka Rao works at [[Organization/Brightwell Analytics (…)|Brightwell]] (has a brightwell.example address). [^1] <!-- c:55 -->`
  is replaced with
  `- Priyanka Rao works at [[Organization/Brightwell Analytics (…)|Brightwell]] (has a brightwell.example address) (retyped by me, no marker).`
  This is what a user produces by retyping or pasting a line: no footnote and no `<!-- c: -->`.
- Expected: the guide says to leave the comments in place, but it promises that a bullet without a
  marker under a heading becomes a new `user_asserted` fact. The user's text should therefore
  survive, either as a new fact or with the old one rejected.
- Actual: after the edit, `knowledge.entity` shows c:55 with status `user_asserted` and the
  original text `Priyanka Rao works at Brightwell (has a brightwell.example address).`. No claim
  contains "retyped". The page line becomes
  `- Priyanka Rao works at [[…|Brightwell]] (has a brightwell.example address). [^1] (you) <!-- c:55 -->`,
  so the "(retyped by me, no marker)" text is gone from both the store and the page. This holds
  after re-extraction, after the restart and after the offline start, with no conflict.
- Reproduces: 1/1. The code path is deterministic.
- Cause: `plan_edits` sees c:55's marker missing, so it rejects c:55, and it adds the unmarked
  bullet as a new fact (crates/nugget-vault/src/edits.rs:111-129). `user_claim` takes the object
  from the first wikilink (lib.rs:667-686), so the new claim has the same
  `(subject, works_at, Brightwell)` triple as the just-rejected c:55. The graph's "no duplicate
  live triples" fold then merges it into c:55 (the older id survives with its own text, and the
  status becomes `user_asserted`), and the user's text is dropped (crates/nugget-graph/src/lib.rs:14-25).
- Owning area: crates/nugget-vault (edits.rs, lib.rs `apply_plan`/`user_claim`), together with
  the nugget-graph fold rule.
- Fix direction: when a user claim folds into an existing claim, keep the user's text (update the
  survivor's `text`). Alternatively, match an unmarked line to the removed claim with the same
  triple and treat it as an edit (supersede) instead of reject plus add.

### Adding an alias the way people write YAML (`["A", "B"]`) causes a permanent false conflict on the page
- Severity: Medium (alarming `.nugget/conflicts/` sidecar that never clears. The edit itself is
  applied. Generated alias updates stop reaching the page.)
- Repro: `run_main.py <out>` edits the R. Chen page from `aliases: ["Robert Chen"]` to
  `aliases: ["Robert Chen", "Bobby"]` (with a space after the comma). Compare the Gus page edit
  `aliases: ["Gustavo"]` and the Ife page edit `aliases: ["Ife","Ifeoma Okafor","Ifey"]`
  (compact, no space).
- Expected: "Add an alias in the frontmatter: the alias is added to the entity", with no
  conflict, because Nugget did not change that block.
- Actual: the alias is added (`knowledge.entity` aliases `["Robert Chen","Bobby"]`), but
  `knowledge.status.vault.conflicts = ["Person/R. Chen (ent_cnciyojtoruyvjnx).md"]` and
  `.nugget/conflicts/Person/R. Chen (…).md` is written. Its header reads
  `Your version was kept; these blocks conflicted: frontmatter`. The only difference between the
  two files is:
  ```
  < aliases: ["Robert Chen","Bobby"]
  > aliases: ["Robert Chen", "Bobby"]
  ```
  The conflict is still listed after re-extraction, the restart and the offline start. The
  compact-spaced edits on Gus and Ife produce no conflict.
- Scenario 6 (the reference run's `R. Chen` conflict in `o1/edits/vault/.nugget/conflicts`): it is
  not a timing race. The harness (`nugget_qa.py` `run_edits` E4) writes `", "`, and the same
  sidecar diff appears there. A deleted line followed 0.3 s later by a compact alias edit on the
  Ife page (S6) merges cleanly: c:49 is `rejected`, the alias `Ifey` is added, and there is no
  conflict.
- Reproduces: 2/2 spaced (this run and o1) and 0/2 compact.
- Cause: `merge_fm` compares frontmatter values as raw text lines
  (crates/nugget-vault/src/merge.rs:175-185). The user's line equals neither the base nor the new
  render, because the renderer writes `,` with no space, so the merge reports a conflict. On later
  passes, `bl == nl` together with `prev_conflict` keeps it flagged forever (merge.rs:179-180),
  and while it is flagged, aliases Nugget learns later are not written to the page.
- Owning area: crates/nugget-vault merge.rs (not targeted by EE1's brief).
- Fix direction: compare `aliases`/`identifiers` as parsed lists (or normalise both sides with
  `fm_list`) before deciding a side changed, and treat "user's list == new render's list" as
  agreement.

### A newer source and the user's edited value both stay current for a single-valued fact, with no sign of the clash
- Severity: Medium (the page states two employers as current; the guide promises neither "you
  win" nor "superseded")
- Repro: `run_s10_s11.py <out>`. Dana's `works at` line is edited to
  `Dana Whitfield is Head of Product at Corvid Labs (fixed by me)`. After re-extraction, her typed
  person note `Notes/People/Dana-Whitfield.md` is re-saved now with `org: Kestrel Robotics` and
  `date: 2026-10-30`.
- Expected: one current `works_at`. Either the user's value wins and the newer source is recorded
  as history, or the newer source supersedes the user value. In both cases the page should say
  which.
- Actual (`s10/results.json`, `after_newer_source`): c:62 is `user_asserted` "…Corvid Labs (fixed
  by me)" and c:64 is `active` "Dana-Whitfield works at Kestrel Robotics.". Both are listed under
  `### Works at`. Nothing marks them as contradictory, and this survives the restart.
- Reproduces: 1/1.
- Cause: `add_single_valued` supersedes only `Active` rivals (session.rs:1326-1334). A
  `user_asserted` value is never superseded, and it never supersedes a newer extraction either.
- Owning area: crates/nugget-knowledge session.rs. EE1's brief targets this: "a newer claim about
  the same subject and predicate … supersedes the older one instead of coexisting".
- Fix direction: decide a policy (for example, the user's value stays current and newer extracted
  values are added as `superseded`, or the reverse) and flag the clash on the page or the
  Knowledge view.

### An editor or formatter that drops HTML comments makes the edit a no-op and duplicates the whole page
- Severity: Medium (the edit is silently not applied to the graph; the page grows a stale second
  copy of every section)
- Repro: `run_strip.py <out>`. On the Sam Patel page, every `<!-- … -->` is removed (as some
  Markdown formatters, web clippers and sync tools do), the works_at line is reworded to
  "is a senior data engineer at", and a note is added.
- Expected: either the edit is applied, or the user is told that the page lost its markers.
- Actual: the claims are unchanged (c:18 `active` "Sam Patel works at Brightwell…"). There is no
  `user_asserted` claim and no conflict. The written page has the fresh generated regions on top,
  followed by the user's marker-less copy as "user text": a second `# Sam Patel`, `## Facts`
  (holding the reworded line), `## Related` and footnotes. After re-extraction the generated
  footnotes were renumbered (for example `[^6]` is now "Harbor launch date"), but the frozen copy
  still has the old numbering, so its citations point to the wrong sources.
- Reproduces: 1/1.
- Cause: with no facts region, `plan_edits` returns early by design (edits.rs:66-69, "not a
  request to retract everything"). `merge` keeps everything outside regions as user text and
  re-inserts the missing regions (merge.rs:90-114).
- Owning area: crates/nugget-vault edits.rs and merge.rs.
- Fix direction: when a page that had markers comes back with none, diff it against the base by
  line content (headings and fact text) to recover the edit. Failing that, keep the user's file as
  a conflict sidecar and write a clean page, instead of concatenating both.

### Single-word aliases added in the vault are never used to resolve new items (undocumented)
- Severity: Low
- Repro: `run_main.py <out>` adds the aliases `Bobby` (R. Chen), `Gustavo` (Gus Ferreira) and
  `Ifey` (Ife Okafor), then adds `Notes/Meetings/2026-10-28-coffee-with-Bobby.md` with
  `people: [Bobby, Gustavo, Ifey]`.
- Actual: three new person entities, `Bobby`, `Gustavo` and `Ifey`, are created
  (`checks.json` `S4_alias_resolution`). A two-word alias works: `Bobby Chen` (`run_s10_s11.py`)
  resolves to R. Chen, whose mention surfaces include "Bobby Chen", and no new entity is created.
- Cause: by design, person names need at least two words (crates/nugget-knowledge/src/resolve.rs:5
  and 509-513). The guide's "the alias is added to the entity" is true, but a user who adds a
  nickname (the obvious case) expects it to help resolution.
- Owning area: docs/book/src/guide/knowledge.md, or nugget-knowledge resolve.rs. EE1 targets
  nicknames.
- Fix direction: document that single-word aliases are display-only for people, or trust
  user-added aliases (a user decision) even when they are one word.

### Renaming via `# Title` and deleting a page file are silently ignored
- Severity: Low (no promise in the guide, but no feedback either)
- Repro: `run_main.py <out>` changes `# Theo Lindqvist` to `# Theo L.` and deletes the file
  `Person/Jonas Albers (…).md`. `run_strip.py` deletes the Jonas page and then re-extracts.
- Actual:
  - Rename: the entity name stays "Theo Lindqvist" (`knowledge.entity`, README index). The page
    keeps the user's `# Theo L.` header because only the user changed the region. There is no
    conflict and no rename, and the page and the index now disagree.
  - Delete: the graph is unchanged. The page stays missing, and README.md keeps a dead link to it,
    until the entity next changes. It is then rendered again from scratch (in the `strip` run it
    reappeared after re-extraction).
- Owning area: crates/nugget-vault (lib.rs `render_page` header region, and `on_paths_changed`
  ignoring missing files at lib.rs:372-375).
- Fix direction: document both cases. Possibly map a header edit to an entity rename, and either
  re-render a deleted page right away or treat deletion as "hide this entity".

## Works well
- **Deleting a fact line** (Harbor c:28 and Ife c:49): the claim is `rejected` and the line is gone.
  It stays that way after re-extraction of every source, after a restart and after an offline
  start.
- **Adding bullets**: a bullet under `### Part of` became `user_asserted` with predicate `part_of`
  (c:64), and a bullet directly under `## Facts` became predicate `note` (c:63). Both are rendered
  with `(you)` and survive re-extraction and restarts.
- **Notes**: text under `## Notes` (Harbor, Sam Patel, Sam) is kept through every stage.
- **Rewording a line while keeping its `[[link]]` and marker** (S11, Robert's works_at): the old
  claim is `superseded` and later `retracted`. The user claim c:63 keeps the user's text, takes the
  extracted evidence on re-extraction (footnotes are added back), and stays `user_asserted`.
- **Edits while the daemon is stopped** (S7, Sam page: a fixed line, a note and the alias `Sammy O`):
  all of them are applied at start-up (c:8 `superseded`, c:69 `user_asserted`, the alias added),
  with no conflict.
- **Atomic save** (temp file, then rename over the page; Sam Patel): picked up by the watcher. The
  added bullet becomes `user_asserted` and the note is kept.
- **Two quick edits on one page** (delete, then a compact alias edit 0.3 s later): both are applied
  with no conflict.
- **A two-word alias added in the vault** (`Bobby Chen`) resolves a new note's `people:` entry to
  the right entity.
