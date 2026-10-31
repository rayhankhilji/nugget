# GG5 verification: entity resolution findings (`qa/GG5/findings/resolution.md`)

Verifier: fresh run, own daemons, `--no-models` (no models configured), binary
`target/debug/nuggetd` as built (not rebuilt). HEAD `5d89fb8` on qa/knowledge (the finding names
commit `1d6bda7`; the binary was not rebuilt, so this is the same build the other agents use).

What I ran (outputs in my scratchpad, not in the repo):
- `python3 qa/GG5/scenarios/resolution/run_fixes.py <scratch>/verify-resolution/A`: exit 0, about 70 s.
- `python3 qa/GG5/scenarios/resolution/run_order.py <scratch>/verify-resolution/B`: exit 0, run at the same time.
- `python3 qa/GG5/verify/resolution/alias_check.py <scratch>/verify-resolution/A`: my script. It restarts
  the daemon on run A's home, adds `"Bob Chen"` to the frontmatter `aliases:` of the `R. Chen` page,
  and reads the entity back. I wrote it because the finding states this and no script tests it.
- Read every cited `file:line`.
- Compared with the author's `out/A-result.json` and `out/B-order-result.json` only after I had my
  own results. Entity ids differ between runs; the claim ids and outcomes match.

`<scratch>` = `/tmp/claude-0/-home-user-nugget/3826b9b2-0317-5073-9bf7-b47d75df8cb7/scratchpad`.
All my daemons were stopped with SIGTERM by the harness (`daemon.stopped` logged; no
`verify-resolution` nuggetd process is left).

General repro defect: findings 1, 4 and 5 cite a "reference run `o1/phase2/entities.json`" in an
unnamed scratchpad. A fresh tester cannot find it. The `run_fixes.py` step `base` is enough on its
own, and that is what I used.

---

### 1. A person's name is whatever display name was processed first ("R. Chen", "Sam"), and the user has no way to rename it
**VERDICT: REPRODUCED**

- Run A `base`, `header`, `reextract` and `restart`: person `R. Chen`, aliases `["Robert Chen"]`,
  identifier `robert.chen@brightwell.example`. Claims read `Mara Okafor corresponded_with R. Chen #34`.
  The page is `Person/R. Chen (ent_flcyd6gag63cezl2).md`. Twelve links elsewhere read `|R. Chen]]`
  and two read `|Robert Chen]]`. Okonkwo is `Sam`, aliases `["Sam Okonkwo"]`.
- Run B (`Calendar/` renamed `zz-Calendar/`): `{"name": "Robert Chen", "aliases": ["R. Chen"]}` and
  `{"name": "Sam Okonkwo", "aliases": ["Sam"]}`. The result depends on processing order, as the
  finding says.
- Heading edit (step `header`): the file keeps `# Robert Chen`. `knowledge.entity` still returns
  `"name": "R. Chen"`, the file name is unchanged and `vault.conflicts: []`. Confirmed.
- Frontmatter alias (my `alias_check.py`): the aliases become `["Robert Chen","Bob Chen"]`. The name
  stays `R. Chen` and the page name is unchanged. This confirms "only adds an alias".
  - Side observation, not part of this finding: after this alias edit, `vault.conflicts` lists the
    page. `.nugget/conflicts/Person/R. Chen (…).md` says "these blocks conflicted: frontmatter". The
    two frontmatters differ only in spacing: the user wrote `["Robert Chen", "Bob Chen"]` and the
    generated version is `["Robert Chen","Bob Chen"]`. So a plain alias edit can raise a spurious
    conflict. This is worth a separate check by the vault-edits owner.
- No rename IPC: the strings in the code are `knowledge.changed/claim.status/entities/entity/graph/merge/split/status`. Confirmed.
- Cited code: `calendar.rs:191` and `mail.rs:129-131` take the display name when present. At
  `resolve.rs:684-692`, only an address-like name or a domain-made org name is replaced; any other
  name is added as an alias. Accurate. (The mail cite should be `mail.rs:129`, the `let (name, weak)`
  match, rather than `:131`. This is trivial.)
- Wording correction: "Robert Chen ... in 9 mails" should say **8 mail files** (6 Inbox, 2 Sent).
  There are 13 corpus files outside `gold.json` and the README: those 8 mails, `Harbor.md`, the
  1:1 meeting note, the go/no-go `.ics` and a `Work/` review. One `.ics` (`2026-06-03-1on1-bob.ics`)
  has `CN="R. Chen"` as both ORGANIZER and ATTENDEE.
- Severity High: agree. The user's manager is misnamed on every page, and the user cannot correct it.

### 2. Claim sentences are frozen at extraction: after a merge, split or better name they name the wrong entity
**VERDICT: REPRODUCED**

- Run A `restart/vault/Person/Mara Okafor (…).md`:
  `- Mara Okafor corresponded with [[Person/Sam (ent_i67v5g6asjkcpydo)|Sam]] Patel by email. [^15][^16] <!-- c:35 -->`.
  The two citations are both `Sam Okonkwo <sokonkwo@mailbox.example>`. The sentence is wrong, as
  the finding says.
- `Gus Ferreira (…).md` after the merge:
  `- Gus Ferreira works at [[Organization/Lumenfold (…)|Tessellate]] (has a tessellate.example address).`
- `base/…/Dana Whitfield (…).md`: `Dana-Whitfield is about [[…friends]]` and
  `Dana Whitfield works at [[Organization/Corvid Labs (…)|Corvidlabs]]`.
- R. Chen page: `R. Chen attended …` sits next to `Robert Chen attended …` and
  `Robert Chen works at …|Brightwell]]`. The last line shows the org's text is frozen too:
  "Brightwell", not "Brightwell Analytics".
- Cited code: `graph.rs:1794` `merge_tx`, `:1936` `fold_duplicate_claims` and `render.rs:327`
  `inline(&c.text)` all say what the finding claims.
- Minor: `Dana-Whitfield is about friends` comes from file-stem naming (finding 6), not from a later
  change of name. It fits "text ≠ current name" but not the title's "after a merge, split or better
  name".
- Severity Medium: agree.

### 3. One person with an old and a new address stays two people (Gus Ferreira), even with a model
**VERDICT: REPRODUCED** (deterministic part; the "even with a model" part is code reading only)

- Run A `base`: two `Gus Ferreira` persons, `ent_uuwwx…` and `ent_zvxoe…`, with one address each.
  The two pages have the same title. One says `works at …Tessellate`, the other
  `works at …Lumenfold`. The orgs `Tessellate` (`domain tessellate.example`) and `Lumenfold` are
  separate. The mail body says "My new address is gus@lumenfold.example."
  (`2026-09-01-tessellate-is-now-lumenfold.eml:15`).
- Dana: one entity with both emails. Confirmed.
- Cited code: `resolve.rs:489-491` `usable` returns false on `identifiers_conflict`. The candidate
  filter at `resolve.rs:568` (`.filter(|e| self.usable(spec, e))`) is on the embedding/candidate
  path. Accurate. The "even with a model" claim was not run (no model in this container). The title
  should say "(by code reading)".
- Wording: "a `[[Gus Ferreira]]` wikilink cannot resolve" is hypothetical. The corpus has no such
  link. It follows from `session.rs` `link_targets` returning nothing when there are two
  candidates. It should be marked as inferred.
- Severity Medium: agree.

### 4. Splitting off a wrongly merged person leaves the old id and its vault page pointing at the wrong person
**VERDICT: REPRODUCED**

- Run A: claims chosen `[4, 21, 32, 59]`, the same as in the finding. The new entity is `Sam Patel`
  `ent_azeb6…` with the email. `knowledge.entity {id: <old Sam Patel ent_l7dol…>}` returns
  `{"id": "ent_i67v5…", "name": "Sam", "aliases": ["Sam Okonkwo"]}` after the split, after
  re-extraction and after restart.
- Old page `Person/Sam Patel (ent_l7dol…).md`: `merged_into: "ent_i67v5…"`, "This page moved: the
  entity was merged into [[Person/Sam (…)|Sam]]". Confirmed.
- After the split, claim 35 on Mara's page has 10 citations: 8 `sam.patel@brightwell.example` mails
  and 2 Okonkwo mails. After the re-save, Patel gets new claim `#62` and claim 35 keeps only
  Okonkwo's 2. Matches.
- `SplitRequest` (`nugget-core/src/ipc.rs:722`) takes whole claim ids only; it has no evidence-level
  split. Confirmed. `split_tx` (`graph.rs:1100`) has no `merged_into` update. Its UPDATEs touch
  `entity_detail_sources`, `claims` and `mentions` only. The only `merged_into` writer is
  `merge_tx` (`graph.rs:1874-1876`; the cite says `:1872`, the comment line). Accurate.
- Repro wording: the inline IPC in the repro omits `aliases: ["Sam Patel"]`, which the script
  passes, and uses run-specific claim ids. It is followable only through the script.
- Severity Medium: agree.

### 5. Same thing, several entities: "harbor" topic vs "Harbor" project, a draft becomes a project, file-slug names
**VERDICT: REPRODUCED** (with run A `base`; the cited reference run is unavailable)

- Run A `base/entities.json`: project `Harbor`, topic `harbor` (`tag:harbor`), and project
  `Harbor-launch-plan` with `Mara Okafor leads Harbor-launch-plan`. The note's H1 is
  `# Harbor launch plan`. Dana's aliases are `['Dana-Whitfield']`. Org `Tessellate` has no alias
  `Tessellate Systems`, although `Harbor.md` has `[[Tessellate Systems]]`.
- Cited code: `markdown.rs:151-157` (title, else stem), `markdown.rs:227` (tag becomes a `Topic`
  entity) and `session.rs:1060` `link_targets` (exact name lookup) are accurate.
- Extra, same family: the project `Harbor` also has `has_topic one-on-one`.
- Severity Low: agree.

---

The "Works well" items were not verified one by one. Those I saw agree: the two Sams stay separate,
Dana is merged into one entity, the merge holds across re-extraction and restart (43 entities at
restart), and the split holds across restart.
