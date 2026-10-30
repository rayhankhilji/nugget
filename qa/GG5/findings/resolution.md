# GG5 findings: entity resolution (merge / split)

Area: are people and organisations merged and split correctly by the deterministic pipeline on the
Mara Okafor persona corpus, and do the user's manual tools (`knowledge.merge`, `knowledge.split`
over IPC) fix what it gets wrong, durably?

Commit tested: `1d6bda785a7bfc1889bc4b6c26fa7805e4b57dc0` (qa/knowledge), debug `nuggetd`, `--no-models`
mode (deterministic extractors only; this container cannot run llama-server). Each finding says
where a real-model run may differ.

Scripts (all under `qa/GG5/scenarios/resolution/`; each starts its own daemon with a fresh `--home`
and stops it):
- `run_fixes.py <out>`: indexes phase1+phase2, then merges the two Gus Ferreira entities and
  Tessellate into Lumenfold, merges Sam Patel into "Sam" (Okonkwo) by mistake and splits him back
  out, edits the `# R. Chen` heading, re-saves every source naming Gus/Sam/Tessellate/Lumenfold/Chen,
  restarts. Result: `<out>/result.json` (copy in `out/A-result.json`), snapshots per step.
- `run_order.py <out>`: same corpus with `Calendar/` renamed `zz-Calendar/` so it sorts after
  `Mail/` (copy of the result in `out/B-order-result.json`).
- Reference run: scratchpad `o1/phase2/entities.json` (the lead's harness run).

Run: `python3 qa/GG5/scenarios/resolution/run_fixes.py /tmp/.../scratchpad/resolution/A` (about 90 s).

---

### A person's name is whatever display name was processed first ("R. Chen", "Sam"), and the user has no way to rename it
- Severity: High
- Repro: `run_fixes.py` (step `base`) or the reference run `o1/phase2/entities.json`; then
  `run_order.py` for the order dependence.
- Expected vs actual: Robert Chen (the user's manager) is "Robert Chen" in 9 mails, the project
  note, the meeting note and a calendar invite; only one calendar invite has organiser
  `CN="R. Chen"`. Actual entity:
  `{"name": "R. Chen", "aliases": ["Robert Chen"], "identifiers": [["email","robert.chen@brightwell.example"]]}`,
  page `Person/R. Chen (ent_…).md`, heading `# R. Chen`, every link on other pages reads `R. Chen`,
  and `Mara Okafor corresponded_with R. Chen`. Sam Okonkwo likewise is `"name": "Sam"` (from
  `ATTENDEE;CN="Sam"` in `Calendar/2026-04-16-climbing.ics`) although his own mails say "Sam
  Okonkwo". Later, better names never replace it: after phase2, re-extraction and restart it is
  still "R. Chen". It is purely processing order: with only the calendar folder renamed so it sorts
  after `Mail/` (`run_order.py`), the same corpus gives
  `{"name": "Robert Chen", "aliases": ["R. Chen"]}` and `{"name": "Sam Okonkwo", "aliases": ["Sam"]}`.
  The user cannot fix it: there is no rename IPC (the knowledge methods are entities/entity/graph/
  merge/split/claim.status/status), adding an alias in the frontmatter only adds an alias, and
  editing the page heading to `# Robert Chen` is silently ignored: the file keeps the user's
  heading but `knowledge.entity` still returns `"name": "R. Chen"`, the file name and all links stay
  `R. Chen`, no conflict is reported (`vault.conflicts: []`).
- Root cause: the first occurrence creates the entity with its display name
  (`crates/nugget-knowledge/src/extract/calendar.rs:191`, `extract/mail.rs:131`); every later
  occurrence matched by email only adds its name as an alias
  (`crates/nugget-knowledge/src/resolve.rs:691`). Only an address or a domain-made organisation
  name ever gives way to a better one (`resolve.rs:684`). Items are processed in feed order, which
  for the files connector is path order (`Calendar/` < `Mail/`), not date or frequency. The vault
  does not interpret header-region edits (`crates/nugget-vault/src/edits.rs` handles facts and
  aliases only).
- Reproduces: 3/3 (reference run, `run_fixes.py` twice); order flip 1/1.
- Owning area: crates/nugget-knowledge `resolve.rs` (EE1). EE1's brief targets this in part:
  "deterministic and order-independent" alias handling and "initials" would cover choosing
  "Robert Chen" over "R. Chen". It does not ask for a user rename (IPC or vault heading edit). A
  model run would not change it: the LLM path resolves through the same `attach`.
- Fix direction: choose the display name deterministically from all known names (prefer the
  fullest multi-word name, then the most frequent; never an initial or a lone first name over a
  full name), re-evaluated on attach. Add a `knowledge.rename` (or honour a heading edit) that pins
  a user-chosen name.

### Claim sentences are frozen at extraction: after a merge, split or better name they name the wrong entity
- Severity: Medium
- Repro: `run_fixes.py`; look at `<out>/restart/vault/Person/Mara Okafor (*.md`,
  `Gus Ferreira (*.md`, and at `base/vault/Person/Dana Whitfield (*.md`.
- Expected vs actual: a fact line should describe the claim as it now is. Actual (after the user
  merged Sam Patel into Sam by mistake, split him back out, re-extracted, restarted), on the
  user's own page:
  `- Mara Okafor corresponded with [[Person/Sam (ent_252ytloyxwwab52s)|Sam]] Patel by email. [^15][^16] <!-- c:35 -->`
  Claim 35 now has subject/object Mara/Sam Okonkwo and only Okonkwo's evidence
  (`From: Sam Okonkwo <sokonkwo@mailbox.example>`), but its text still reads "Sam Patel", and the
  renderer links the word "Sam" to Okonkwo. After merging Tessellate into Lumenfold, Gus's current
  employer reads `- Gus Ferreira works at [[Organization/Lumenfold (…)|Tessellate]] (has a
  tessellate.example address).` Without any user action: `Dana Whitfield works at [[…Corvid Labs…|Corvidlabs]]`
  (the org's name was later improved), `Dana-Whitfield is about friends.`, and on the `R. Chen`
  page `Robert Chen works at …` next to `R. Chen attended …`.
- Root cause: `NewClaim.text` is written once by the extractor; the store's merge folds duplicate
  triples into the oldest claim and keeps its text (`crates/nugget-store/src/graph.rs:1792`
  `merge_tx`, `fold_duplicate_claims` at `:1936`); nothing rewrites text on merge, split or
  rename; the vault renders `c.text` (`crates/nugget-vault/src/render.rs:327`).
- Reproduces: 2/2.
- Owning area: crates/nugget-store graph merge + crates/nugget-vault rendering of claims (EE1 owns
  "vault rendering of entities and claims"). EE1's brief does not name this. With a model the
  LLM's own sentences are also frozen, so the same happens.
- Fix direction: render deterministic claims from (subject, predicate, object) at render time
  using current names (keep `text` only for user-asserted / free-form claims), or regenerate
  `text` when an end of the claim is merged, split or renamed.

### One person with an old and a new address stays two people (Gus Ferreira), even with a model
- Severity: Medium
- Repro: reference run `o1/phase2/entities.json`, or `run_fixes.py` step `base`.
- Expected vs actual: gold has one Gus Ferreira (`gus.ferreira@tessellate.example`, later
  `gus@lumenfold.example`; the rename mail says "My new address is gus@lumenfold.example").
  Actual: two persons named "Gus Ferreira" with one address each, two vault pages with the same
  title (`Person/Gus Ferreira (ent_2mi42…).md`, `Person/Gus Ferreira (ent_cw3bb…).md`), one
  "works at Tessellate", the other "works at Lumenfold"; a `[[Gus Ferreira]]` wikilink cannot
  resolve (two candidates). Same for the organisations: `Tessellate` and `Lumenfold`
  (`domain` identifiers `tessellate.example` / `lumenfold.example`).
- Root cause: a candidate with a different value of a strong identifier is never usable
  (`resolve.rs:491` `usable` → `identifiers_conflict`), whatever the name, so a full-name match is
  refused; only a trusted record listing both addresses merges them (Dana's typed person note
  did, which is why Dana is one entity). `usable` also gates the embedding/adjudication path
  (`resolve.rs:568`), so a local model cannot merge Gus either. The org rename is only stated in
  mail body text, which the deterministic extractors do not read.
- Reproduces: 3/3.
- Owning area: crates/nugget-knowledge `resolve.rs` (EE1). EE1's brief targets it in part
  ("merge only on corroborating evidence (shared email, same thread participants, same org)"),
  but its examples are about nicknames; a person changing address (the same job-change case the
  brief names for claims) is not called out, and the blanket "never across conflicting
  identifiers" rule would still block it.
- Fix direction: treat a different email as weak counter-evidence for persons, not a veto: same
  full name plus a corroborator (same org after org merge, same thread, a body line "my new
  address is X") may merge, with the reason recorded. The user's merge (below) does fix it.

### Splitting off a wrongly merged person leaves the old id and its vault page pointing at the wrong person
- Severity: Medium
- Repro: `run_fixes.py` steps `wrongmerge` and `split`: `knowledge.merge {into: Sam(Okonkwo), from: Sam Patel}`,
  then `knowledge.split {entity: Sam, claims: [4,21,32,59], name: "Sam Patel", identifiers: [["email","sam.patel@brightwell.example"]]}`.
- Expected vs actual: undoing the mistaken merge should send Sam Patel's old id to the new Sam
  Patel. Actual: `knowledge.entity {id: <old Sam Patel id>}` returns
  `{"id": "ent_252ytloyxwwab52s", "name": "Sam", "aliases": ["Sam Okonkwo"], …}` (Okonkwo) through
  restart, and the old page `Person/Sam Patel (ent_vv5aomiyakczmky2).md` stays a stub saying
  "This page moved: the entity was merged into [[Person/Sam (ent_252ytloyxwwab52s)|Sam]]", so the
  user's own links to Sam Patel's page now lead to the climbing partner. Also, the claim
  "Mara corresponded with Sam" that the merge had folded (evidence from both Sams) cannot be
  split: it stays on Okonkwo with 8 citations of `sam.patel@brightwell.example` mails until those
  source items happen to be re-extracted (here forced by re-saving them; then Patel gets a new
  claim #62), and its sentence still says "Sam Patel" (previous finding).
- Root cause: `split_tx` (`crates/nugget-store/src/graph.rs:1100`) creates a new entity and never
  re-points `merged_into` rows of ids previously merged into the split entity (only `merge_tx`
  writes `merged_into`, `graph.rs:1872`); split moves whole claims only, not evidence.
- Reproduces: 2/2.
- Owning area: crates/nugget-store / crates/nugget-graph split, nuggetd `knowledge_split`
  (`crates/nuggetd/src/handler_m2.rs:266`). EE1's brief is adjacent ("record why two mentions were
  merged so … the user can split them") but does not target redirect repair or evidence-level split.
- Fix direction: offer an "unmerge" of a recorded merge (restore the merged-away id with the
  identifiers, aliases and evidence it brought), or at least re-point `merged_into` of a merged id
  whose identifiers move to the split-off entity, and split folded claims by evidence item.

### Same thing, several entities: "harbor" topic vs "Harbor" project, a draft becomes a project, file-slug names
- Severity: Low
- Repro: reference run `o1/phase2/entities.json`.
- Expected vs actual: one Harbor. Actual: project `Harbor` (from `Notes/Projects/Harbor.md`),
  topic `harbor` (identifier `tag:harbor`, from `#harbor`), and project `Harbor-launch-plan` (the
  draft plan note, `type: project`, `project: Harbor`) with `Mara Okafor leads Harbor-launch-plan`;
  its name is the file stem, not its heading "Harbor launch plan". File stems also leak as aliases
  (`Dana Whitfield` aliases `["Dana-Whitfield"]`, fact `Dana-Whitfield is about friends.`). The
  wikilink `[[Tessellate Systems]]` in Harbor.md never resolves (no entity has that name;
  `link_targets` does exact names only, `crates/nugget-knowledge/src/session.rs:1060`), so the
  organisation never learns its real name and stays "Tessellate" (made from the domain).
- Root cause: tags always become topic entities (`extract/markdown.rs:227`); a typed note's entity
  takes the item title, which the files connector sets to the file stem
  (`extract/markdown.rs:151-157`); wikilink targets are not matched against domain-named orgs.
- Reproduces: 2/2.
- Owning area: crates/nugget-knowledge `extract/markdown.rs`, `session.rs` (EE1); not named in
  EE1's brief. A model run may add a "Tessellate Systems" org that `absorb` folds by domain.
- Fix direction: link a tag to a same-named project instead of minting a topic; prefer the note's
  H1 over the file stem; let a wikilink resolve to the one org whose domain spells it
  (`domain_matches_name`).

---

## Works well
- The two Sams stay apart: `Sam` (Okonkwo, `sokonkwo@mailbox.example`) and `Sam Patel`
  (`sam.patel@brightwell.example`) are separate in every run; no first-name-only merge.
- Dana Whitfield's two addresses are merged into one entity by her typed person note (trusted
  identifiers); the duplicate became a redirect stub `merged_into: "ent_vetia…"`.
- Corvid Labs is created from the domain as "Corvidlabs" and renamed to "Corvid Labs" once the
  real name is seen (old name kept as alias) (`resolve.rs:681-690`).
- Mara's `is_me` entity absorbs nothing wrong: 22 claims, all organiser/attendee/leads/
  corresponded_with facts she really has; Sent mail from both addresses attaches to her.
- Documents written by others about Mara (`Work/`) do not make the author the user: with
  deterministic extractors they yield only tag topics (`hr`, `promotion`, `performance-review`);
  the `author:` frontmatter is ignored. (The LLM path's first-person handling is EE4's KN-5 and
  untested here.)
- `knowledge.merge` works and holds: the old id resolves to the survivor through
  `knowledge.entity` (Gus old id → `ent_vw6c…` with both emails; Tessellate id → Lumenfold with
  alias "Tessellate" and both domains); the old pages become redirect stubs with `merged_into`
  frontmatter; no page outside `.nugget/` links to an old id; duplicate claims fold (one
  `works_at Lumenfold` with all four citations). Re-saving every Gus/Tessellate/Lumenfold source
  and restarting the daemon did not re-create either duplicate (43 entities before and after).
- `knowledge.split` works and holds: the new Sam Patel keeps his email and facts; re-extraction
  routes his mails to the split-off entity (new `Mara corresponded_with Sam Patel` claim) and the
  split survives restart.
