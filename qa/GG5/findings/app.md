# GG5 findings: desktop app, Knowledge pages and graph (`app`)

Commit `e7299584da9ca18c83793107da6d97ceec7c8f37`, deterministic extractors only (`--no-models`
config: no chat model). With a real model the set of facts differs (more facts, maybe better
names), but every UI behaviour below follows the data it is given, so it applies there as well.

**The UI was run for real.** `pnpm install` + `vite build` in `apps/desktop`, the real `dist/` served by
`apps/desktop/e2e/bridge.mjs` (standalone mode, Tauri shim inlined) against a real `nuggetd` loaded
with the persona corpus (phase1 then phase2), driven in headless Chromium by Playwright. Every UI
finding is observed behaviour (screenshots in `qa/GG5/scenarios/app/shots/`), not only code review.
The only stub: the bridge cannot open links, so the scripts catch `open_url` and record the URL
instead. Each recorded URL was then checked to exist on disk.

Shared setup used by every repro below (about 1 minute):
```sh
cd /home/user/nugget
S=/tmp/claude-0/-home-user-nugget/3826b9b2-0317-5073-9bf7-b47d75df8cb7/scratchpad/app
(cd apps/desktop && pnpm install --frozen-lockfile && pnpm exec vite build)
python3 qa/GG5/scenarios/app/populate.py $S/home qa/GG5/scenarios/app/run   # prints READY <pid> <socket>; daemon stays up
SOCK=$(python3 -c "import json;print(json.load(open('qa/GG5/scenarios/app/run/daemon.json'))['socket'])")
(cd apps/desktop && node e2e/bridge.mjs $SOCK 8765 &)
node qa/GG5/scenarios/app/ui_explore.cjs            # read-only pass -> ui_explore.log, shots/0x-1x
python3 qa/GG5/scenarios/app/ipc_graph_checks.py $SOCK qa/GG5/scenarios/app/run   # -> ipc_graph_checks.before.txt
VAULT=$S/home/data/vault node qa/GG5/scenarios/app/ui_mutate.cjs $SOCK            # reject/undo/restore/merge -> ui_mutate.log, shots/2x
node qa/GG5/scenarios/app/ui_graph_kinds.cjs        # -> ui_graph_kinds.log, shots/30-*
kill <bridge pid>; kill -TERM <daemon pid>
```
Entity ids below are from my run and will differ in a fresh one. Names are stable.

---

### "Restore" on a superseded fact makes the old job current again, with no Undo
- Severity: Medium
- Repro: setup above, then `ui_mutate.cjs` (section "Restore on a superseded fact"). Knowledge →
  search "Dana Whitfield" → the struck-grey fact "Dana Whitfield works at Brightwell (has a
  brightwell.example address)." with badge **Superseded**. Its only action button is
  **Restore**, styled red like the reject button (`class: btn btn-small btn-danger-ghost`). Click it.
- Expected vs actual: a superseded fact is history (guide: "Replaced by a newer fact, for example
  a changed job"). The user would expect either no action, or an action that keeps it out of the
  current facts, or at least an Undo. Actual: the claim becomes `active` next to the newer one,
  and the vault page now gives Dana two current employers. No Undo is offered; the note only says
  `Restored "Dana Whitfield works at Brightwell …".`:
  ```
  ipc works_at: [[19,"active","Dana Whitfield works at Brightwell (has a brightwell.example address)."],
                 [51,"active","Dana Whitfield works at Corvidlabs (has a corvidlabs.example address)."]]
  vault: - Dana Whitfield works at [[Organization/Brightwell Analytics (…)|Brightwell]] (…). [^3][^4] <!-- c:19 -->
         - Dana Whitfield works at [[Organization/Corvid Labs (…)|Corvidlabs]] (…). [^5][^6][^7] <!-- c:51 -->
  ```
  After that, the UI offers only "Reject this fact". Rejecting gives `rejected` (sticky), not the
  original `superseded`, so the user cannot get back to where they started. The Facts heading also
  counts superseded facts as facts ("Facts (6)" for Dana, of which one is superseded;
  `EntityDetail.tsx:364` excludes only rejected/retracted). Screenshots: `shots/02-entity-Dana-0.png`,
  `shots/23-dana-superseded-restored.png`.
- Reproduces: 1/1. This is deterministic: `factAction` maps every status that is not active or
  user-asserted to restore.
- Owning area: `apps/desktop/src/knowledge.ts:232-239` (`factAction`: "anything else is restored",
  `undo: null`) and `apps/desktop/src/components/EntityDetail.tsx:472` (button class). The daemon side accepts `active` on a
  superseded claim (`knowledge.claim.status`). This is a desktop UI issue, not in EE1's brief.
- Fix direction: give superseded facts no Restore, or a neutral "Make current again…" that
  confirms and offers Undo (back to `superseded`). Count only current facts in the heading.

### Fact recency uses the store's write time: every fact reads "Today · minutes ago", and rejected or superseded facts jump to the top
- Severity: Medium
- Repro: setup, `ui_explore.cjs` (each entity) and `ui_mutate.cjs` (reject, then reload). Open any
  entity right after indexing; then reject "Robert Chen works on Harbor." on R. Chen and reload the page.
- Expected vs actual: "Last seen" and the recency groups should reflect when the sources said it.
  Dana's "works at Brightwell" was last seen in mail of 2026-03-20; the Harbor stand-up is from
  2026-03-10. Actual: every fact of every entity sits under one **TODAY** group with "2 minutes ago".
  The order is the order of extraction writes, so Dana's superseded Brightwell fact sits third,
  between current facts. After a reject, the rejected fact moves to the top of the page as
  "just now" (`shots/22-chen-rejected-reload.png`):
  ```
  -retracted|Robert Chen works on Harbor.|Confidence 60% Rejected Last seen just now Harbor → Why? 1 source Restore
  |Robert Chen works at Brightwell (…)|Confidence 99% Last seen 5 minutes ago …
  ```
  The wire data shows why:
  ```
  19 superseded updated_at 2026-10-30T19:43:58.880Z  observed_at ['2026-03-04T09:00:00Z','2026-03-20T09:00:00Z']
  ```
- Reproduces: 3/3 entities checked (Dana, R. Chen, Mara), on every run.
- Owning area: `apps/desktop/src/recency.ts:33-44` (`claimTime` = max(`updated_at`, evidence
  `observed_at`); `updated_at` is bumped by extraction, supersession and status changes) and
  `recency.ts:84-100` (sorted by that time before status). Desktop UI; not in EE1's brief.
- Fix direction: base "last seen" and the groups on evidence `observed_at` only, falling back to
  `updated_at` only when there is no evidence. Within a group, sort current facts before
  superseded and rejected ones, or put non-current facts in their own "Past" group.

### Hard-case names: "R. Chen" and "Sam" as display names, two identical "Gus Ferreira" rows, Tessellate/Lumenfold split
- Severity: Medium
- Repro: setup, `ui_explore.cjs`; screenshots `shots/01-list.png`, `02-entity-Chen-0.png`,
  `02-entity-Sam-1.png`, `02-entity-Gus-0/1.png`, `12-graph-mara-d2-canvas.png`.
- Expected vs actual (PERSONA.md):
  - Robert Chen is shown as heading **"R. Chen"**, with "Robert Chen" only as an alias. He is
    "R. Chen" in the list, the graph labels and Mara's facts ("Mara Okafor corresponded with Robert
    Chen by email. … R. Chen →"). His facts mix both spellings. He is named "Robert Chen" in 10 of
    the 11 mentions.
  - Sam Okonkwo is shown as **"Sam"** (alias "Sam Okonkwo"). The list shows "Sam" next to "Sam
    Patel", so the user's climbing partner reads like a duplicate of the colleague. They are not
    merged, which is correct.
  - Gus Ferreira is **two entities with the same name** (`gus.ferreira@tessellate.example` and
    `gus@lumenfold.example`). Nothing tells them apart in the list rows ("Gus Ferreira Person · 3
    facts · 3 mentions" and "… 2 facts · 2 mentions"), the graph "Centre on…" results ("Gus Ferreira
    Person" twice), the merge dialog options, the graph canvas (two identical labels next to Mara) or
    the graph list.
  - Tessellate and Lumenfold are separate organizations. The org is named "Tessellate" (not
    "Tessellate Systems"), and Dana's fact reads "works at Corvidlabs".
  - Harbor: project "Harbor" and topic "harbor" are told apart by kind label and dot colour. That
    is fine.
- Reproduces: 2/2 runs (same as the reference run `o1/phase2`).
- Owning area: name choice is in `crates/nugget-knowledge/src/resolve.rs:681-693` (`attach`).
  An existing entity's name only gives way when it looks like an address or was made from a mail
  domain. Otherwise the first-seen surface ("R. Chen" from the calendar organiser CN, "Sam" from the
  climbing invite) stays the name, and fuller names become aliases. Not merging Gus or the org
  rename is resolution (EE1's brief targets nicknames "Bob" vs "Robert Chen" and order-dependent
  folding). UI part: `Knowledge.tsx:256-266`, `GraphView.tsx` CenterPicker/GraphList and
  `MergeDialog.tsx` options show no disambiguator.
- Fix direction: prefer the most complete or most frequent personal name as the display name (keep
  short forms as aliases). In the UI, when two visible entities share a name, add a
  disambiguator (first identifier, for example the email domain) to rows, picker results, merge
  options and graph labels.

### Merging Tessellate into Lumenfold leaves the only employer fact reading "works at Tessellate"
- Severity: Medium
- Repro: setup, `ui_mutate.cjs` (sections "merge Gus Ferreira" then "merge Tessellate into
  Lumenfold"). In the UI: Gus Ferreira (lumenfold) → Merge… → pick the other Gus → Merge. Then
  Lumenfold → Merge… → search "Tessellate" → pick → Merge.
- Expected vs actual: after the rename merge, Gus should have one current employer fact that names
  Lumenfold. Actual: the two `works_at` claims are folded into claim 23. It keeps the older text, and
  the UI and vault show it as the only fact:
  ```
  ent_wg6so7dmm5wzl7ih Lumenfold aliases ['Tessellate']
      23 active works_at Lumenfold | Gus Ferreira works at Tessellate (has a tessellate.example address). 4
  vault Person/Gus Ferreira: - Gus Ferreira works at [[Organization/Lumenfold (ent_wg6so7dmm5wzl7ih)|Tessellate]] (has a tessellate.example address). [^2][^3][^4][^5] <!-- c:23 -->
  ```
  (`shots/28-merge-org-done.png`). Between the two merges, Gus showed "works at Tessellate" and
  "works at Lumenfold" both as current facts (`shots/26-merge-gus-done.png`). The merge itself
  works: the old ids resolve to the kept entity, the redirect stub pages are written, and the graph
  goes from 45 to 43 nodes with no dangling edges.
- Reproduces: 1/1.
- Owning area: claim folding on merge in `crates/nugget-store/src/graph.rs` (`fold_tx` and the
  "fold twins" query at lines 1955-1973, `fold_tx` at line 999: the surviving claim keeps its stored `text`) and vault
  link labels. This is EE1 scope (graph and vault rendering of claims).
- Fix direction: when folding twins after a merge, keep the newest text, or regenerate the
  sentence from the object's current name. Render the link label from the entity name, not the
  stale surface.

### Health entities (therapist, counselling session, clinic address) show like any other in the graph overview and Mara's neighbourhood
- Severity: Medium
- Repro: setup, `ui_explore.cjs`. Knowledge → Graph (overview), then centre on Mara Okafor, Jonas
  Albers and Counselling session. Screenshots `shots/10-graph-overview.png`,
  `12-graph-mara-d2-canvas.png`, `12-graph-jonas-canvas.png`, `02-entity-Counselling_session-0.png`.
- Expected vs actual: PERSONA marks the therapy appointment as sensitive. Search marks sensitive
  items with a badge (`pages/Search.tsx:482-487`). On Knowledge, "Counselling session" (attributes
  `rrule FREQ=WEEKLY;COUNT=8`, location "Albers Counselling, 3 Quay Lane, Halden Bay"), "Jonas
  Albers" and the clinic address are ordinary nodes. They appear in the default overview, which
  draws all 45 entities, and at depth 1 of Mara ("Mara Okafor attended Counselling session on
  Jun 17, 2026"). Jonas's graph lists Mara at 2 hops. Nothing marks them as health or sensitive
  data, or keeps them out of an overview that anyone glancing at the screen sees. IPC:
  `[sensitive Counselling session] in overview=True d1 neighbours=['Albers Counselling, …', 'Jonas Albers', 'Mara Okafor']`.
  `GraphNodeView` and `EntityDetail` carry no sensitivity field, so the UI could not mark them even
  if it wanted to. (Dr. Lena Brandt from the GP note has 0 facts and 1 mention, and the "health"
  topic has no edges.)
- Reproduces: 2/2.
- Owning area: knowledge layer, which has no entity-level sensitivity (it is only used for cloud
  gating, `crates/nugget-knowledge/src/llm.rs:187-195`), plus `crates/nuggetd/src/graphview.rs` and the
  desktop Knowledge and Graph views. Not in EE1's brief.
- Fix direction: carry a `sensitive` flag on entities and claims supported only by sensitive items.
  Badge them in the list, detail and graph, and leave them out of the overview by default, with a
  "show sensitive" toggle.

### Kind filter on a centred graph draws floating nodes with no links, and the IPC applies `limit` before the filter
- Severity: Low
- Repro: setup, `ui_graph_kinds.cjs` (Mara → Show in graph → chip "Organizations", then "People"),
  and `ipc_graph_checks.py` (section 3/5). Screenshot `shots/30-graph-mara-kind-Organizations.png`.
- Expected vs actual: with Organizations on, Mara's graph shows "4 entities · 0 links": Mara,
  Brightwell Analytics, Corvid Labs and Lumenfold all float unconnected. The 2-hop paths through
  Dana and Gus, which are the reason they are there, are filtered out with no hint. With People on,
  "Jonas Albers" (depth 2, reached through the counselling event) floats with no link. The filter
  is applied after a traversal over all kinds, so the depth shown is the unfiltered depth. Over IPC,
  `limit` is applied to the traversal before the kind filter:
  `knowledge.graph {center: Mara, depth: 2, limit: 5, kinds: ["organization"]}` returns only Mara,
  with `truncated: true`, although 4 organizations are within 2 hops. (The UI asks for 2001 nodes, so
  it does not hit this here.)
- Reproduces: 2/2.
- Owning area: `crates/nuggetd/src/graphview.rs:354-412` (`neighbourhood`: filter after
  `graph.neighbourhood(center, depth, limit)`) and `crates/nugget-store/src/graph.rs:1983-2040`.
- Fix direction: when kinds are filtered, add synthetic "via <entity>" edges (or keep the
  intermediate nodes greyed out). Apply `limit` to the kept nodes, not the traversed ones.

---

## Works well
- The real UI runs against the real daemon with no page or console errors. Every UI command
  reached the daemon (`knowledge_entities`, `knowledge_entity`, `knowledge_graph`,
  `knowledge_status`, `sources_list`, `open_url`).
- The list shows "1–45 of 45". Kinds show correctly, and the Harbor project and harbor topic are
  told apart by kind label and colour.
- **Why?** shows a verbatim quote, the item title, the source name and a date. Every evidence and
  mention **Open** gives a `file://` URL to the real corpus file. All 13 captured URLs exist.
  **Open in vault** opens `…/data/vault/Person/Dana Whitfield (ent_…).md`, which exists, and there
  is also an "Open in Obsidian" button.
- Reject → Undo → reject → Restore round-trips exactly over IPC (`29 rejected` → `active` →
  `rejected` → `active`). The rejected fact is struck through with a red **Rejected** badge. Its
  graph edge disappears while rejected and returns on undo, and the vault page drops the line.
- Superseded facts are styled distinctly (grey text and a **Superseded** badge) and are excluded
  from graph edges. Claim 19 is never an edge.
- `knowledge.graph` overview: 45/45 entities, all 59 current entity-to-entity claims as edges,
  none non-current, no dangling edges or duplicate nodes. Mara centred: depths 1, 2 and 3 match an
  independent BFS over current claims exactly (19, 36 and 36 nodes). `truncated` is exact at the
  limit boundary (44 → true, 45 → false; Mara d1 18 → true, 19 → false). The `kinds` filter
  returns only the requested kinds. A known source returns everything. An unknown source returns an
  empty overview, or only the centre. An unknown kind gives `-32602`; an unknown centre gives
  `-32001`.
- The graph canvas handles the whole 45-node graph. It reports "45 entities · 53 links" (parallel
  attended+organized edges merged) and has a Legend with kind counts. List mode gives an
  accessible table with hops and connections. The depth selector changes Mara's view
  (19/36/36 entities).
- Merge: the preview shows the kept name, the merged aliases and the fact and mention counts, and
  warns that ids keep working. After the merge the old ids resolve to the kept entity, the vault
  writes redirect stub pages, and the list and graph update (45 → 43 entities).
