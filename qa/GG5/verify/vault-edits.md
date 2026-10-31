# GG5 verify: vault-edits

Verifies `qa/GG5/findings/vault-edits.md`. Same binary (`target/debug/nuggetd`, not rebuilt), `--no-models`.

What I ran (fresh homes under
`/tmp/claude-0/-home-user-nugget/3826b9b2-0317-5073-9bf7-b47d75df8cb7/scratchpad/verify-vault/`):
- The author's three scripts, unchanged: `run_main.py main`, `run_s10_s11.py s10_s11`, `run_strip.py strip`.
  All ran to completion, with no tracebacks. I read each script first; they edit what the findings say
  they edit, and the checks read claim status from `knowledge.entity` and lines from the actual page file.
- My own minimal repro, `qa/GG5/verify/vault-edits/mini.py mini`, on a synthetic corpus of five typed person
  notes (`type: person`, `email:`, `org:`), with nothing from the GG5 corpus. Zoe: edit the works_at line and
  drop the link (F1). Yuri: retype the line without a marker but keep the link (F2). Xena: spaced alias list
  `["Xena A", "Xena Alias"]` (F3); control Walt: compact `["Walt N","Walt Nw"]`. Walt: edit the line, then
  re-save the note with a new org and a newer date (F4). Vera: strip all comments and reword (F5). After
  that, every note is re-saved and the daemon is restarted.
  (A first version without `email:` gave hyphenated one-word person names that re-resolved to new entities
  on every re-save. I discarded that run; it says nothing about the findings.)
- I read every cited file:line.

No daemons of mine are left running.

---

### 1. Re-extraction brings back the fact the user edited
**VERDICT: REPRODUCED** (multi-valued and single-valued, in the author's scripts and independently)

- `main/checks.json`. Harbor c:36: `superseded` after the edit, `active` after re-extraction. The page shows
  both `- Harbor is part of [[…|Brightwell Analytics]]. [^3] <!-- c:36 -->` and
  `- Harbor is a Brightwell Analytics project (fixed by me) (you) <!-- c:62 -->`. Dana c:51: `superseded`
  goes to `active` next to user claim c:65. The state persists through the newer source, the restart and the
  offline start.
- `s10_s11/results.json`: Dana c:51 is `superseded` and then `active` again after re-extraction (same as above).
- mini (independent): Zoe c:5 is `superseded` after the edit, and `active` after the note is re-saved.
  The page has `- Zoe-Fixer works at [[…|Acme Widgets]]. [^1] <!-- c:5 -->` plus the `(you)` line. It
  survives the restart. Walt c:2 shows the same.
- Cited code checked: vault lib.rs:722-723 (`Superseded`), graph.rs:567-572 (retract unless
  user_asserted/rejected), graph.rs:910-915 (`retracted` goes to `active`), session.rs:702-704, 1229-1231,
  1313-1316, 1328. All are accurate.
- Wording correction (minor): the sentence "The user-asserted claim is never compared" is too strong for the
  single-valued case. `others` filters on `is_active()`, which includes `UserAsserted` (nugget-graph
  types.rs:193-194), so the user claim does take part in the "historical" test. It loses because the
  revived claim's freshness is its retraction time (`updated_at`), which is later than the edit. Only the
  supersede step (session.rs:1328) is limited to `Active`. The conclusion is unchanged. Severity High is fair.

### 2. Retyping a fact line without its marker (keeping the link) silently drops the user's text
**VERDICT: REPRODUCED**

- main: Priyanka c:55 is `user_asserted` with the original text "Priyanka Rao works at Brightwell (has a
  brightwell.example address).". No claim contains "retyped". The page line is
  `… (has a brightwell.example address). [^1] (you) <!-- c:55 -->`. This holds through every stage.
- mini: Yuri c:4 becomes `user_asserted` "Yuri-Retyper works at Beta Systems." and "(retyped by me)" is gone.
  Note: in mini, after re-extraction, the claim became c:11 `active` (no `(you)`), still without the user's
  text. It is the same data loss, but the status does not always stay `user_asserted`.
- Cited code checked: edits.rs:111-129 (unmarked bullet added and missing id retracted), lib.rs:667-686
  (`user_claim` object from the first wikilink), nugget-graph lib.rs:14-25 (fold, oldest id survives). All are
  accurate. Severity High is fair.

### 3. Spaced YAML alias list causes a permanent false conflict
**VERDICT: REPRODUCED**

- main: `vault.conflicts = ['Person/R. Chen (…).md']` at every stage. The sidecar diff against the page is
  only the header comment plus `aliases: ["Robert Chen", "Bobby"]` vs `aliases: ["Robert Chen","Bobby"]`.
  The aliases were applied. Ife (compact) has no conflict.
- mini: Xena (spaced) is listed as a conflict through re-extraction and the restart, with aliases applied.
  Walt (compact, two entries) has no conflict.
- Correction: the Gus edit is not a spacing control. Its list was empty before the edit, so the result
  `["Gustavo"]` has no separator. The valid controls are Ife (and my Walt).
- I did not separately test the claim that later generated aliases stop reaching the page. It is
  consistent with merge.rs:175-185, which keeps the user's line while flagged. Severity Medium is fair.

### 4. A newer source and the user's edited value both stay current (single-valued)
**VERDICT: REPRODUCED**

- s10_s11 `after_newer_source`: c:62 `user_asserted` "…Corvid Labs (fixed by me)" and c:64 `active`
  "Dana-Whitfield works at Kestrel Robotics.", both on the page. The same holds after the restart.
- mini: Walt c:7 is `user_asserted` "…Delta (fixed by me)" and c:8 is `active` "Walt-Newer works at Zeta
  Motors.". Both are on the page, there is no conflict, and this persists after the restart.
- The cause is accurate: session.rs:1326-1334 supersedes only `Status == Active` rivals.

### 5. Comment-stripping editor makes the edit a no-op and duplicates the page
**VERDICT: REPRODUCED**

- strip: Sam Patel c:18 stays `active` "Sam Patel works at Brightwell…". There is no user claim and no
  conflict. The page has the full generated regions, then a second `# Sam Patel` / `## Facts` (with
  "is a senior data engineer at") / `## Related` / footnotes. After re-extraction the generated `[^6]` is
  "Harbor launch date", while the frozen copy's `[^6]` is still "Re: Harbor: you're leading it". The
  citations in the frozen copy are therefore wrong, as stated.
- mini: Vera shows the same pattern (c:1 `active`, two `# Vera-Stripper` blocks).
- Cited code checked: edits.rs:66-69 (early return) and merge.rs:90-114 (missing regions re-inserted).
  Both are accurate.

### 6. Single-word vault aliases never resolve new items
**VERDICT: REPRODUCED**

- main `S4_alias_resolution`: new person entities `Bobby`, `Gustavo` and `Ifey` were created.
- s10_s11: `Bobby Chen` resolves to R. Chen (mention surfaces `["Bobby Chen","R. Chen","Robert Chen"]`) and
  no Bobby entity is created.
- resolve.rs:5 and 509-513 (a person needs at least two words) are accurate. Severity Low is fair.

### 7. Renaming via `# Title` and deleting a page file are silently ignored
**VERDICT: REPRODUCED**

- main: the entity name stays "Theo Lindqvist". The page keeps `# Theo L.`. README.md:23 still says
  `Theo Lindqvist`. There is no conflict.
- Jonas page: missing at every stage of main, and README.md:17 still links to it.
- strip: after deletion, `jonas_page_exists` is False; after re-extraction it is True (re-rendered).
- lib.rs:372-375 (`Ok(Some(t))` only, so a missing file is skipped) is accurate.

### "Works well" spot-check
Confirmed in main: S2 delete (c:28 `rejected` at every stage), S3 adds (c:63 `note`, c:64 `part_of`,
both `user_asserted`), S6 Ife (c:49 `rejected`, `Ifey` added, no conflict), S7 offline (c:8 `superseded`,
c:69 `user_asserted`, note kept), and S11 reword with link (c:17 `retracted`, c:63 `user_asserted`,
which gains footnotes on re-extraction).
