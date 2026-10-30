# GG5 verification: desktop app, Knowledge pages and graph (`app`)

Verifier rerun on 2026-10-30. HEAD `e3b3f8e`. The findings cite `e7299584`, but
`git diff e7299584 HEAD -- apps crates` is empty, so the code is the same. Used the prebuilt
`target/debug/nuggetd` and the existing `apps/desktop/dist`, with no rebuild.

## What I ran

- A fresh home at `…/scratchpad/verify-app/home`, filled by `populate.py` with phase1 then phase2.
  It settled at 45 entities and 59 claims for phase2 (`knowledge.status`). The daemon was pid 5849.
- `e2e/bridge.mjs` on port **8791**, not 8765, because other agents run their own bridges.
- Copies of the author's scripts are in `qa/GG5/verify/app/`. In the copies, output goes to my
  scratchpad (`OUTDIR`), not into `qa/GG5/scenarios/app/`.
- Before the scripts, I made my own IPC observations with `qa/GG5/verify/app/look.py`. I checked
  the UI output against those and against `ipc_graph_checks.py` (run before any mutation).
- Logs are in `qa/GG5/verify/app/ui_explore.log`, `ui_mutate.log` and
  `ipc_graph_checks.before.txt`. Three screenshots are in `qa/GG5/verify/app/shots/`.
- I stopped my bridge and daemon at the end.

### Problems with the shared setup (they apply to every finding)

1. **`ui_mutate.cjs` cannot run on a fresh home as written.** It hardcodes the author's entity ids
   `ent_sumwxdjkqphaj23w` (R. Chen) and `ent_7tcfsniepzr64q2o` (Dana). It uses them for
   `knowledge.entity` RPCs and for the vault file names `R. Chen (ent_…).md` and
   `Dana Whitfield (ent_…).md`. The findings say "Entity ids … will differ in a fresh run". On a
   fresh run the script throws at the first `claimStatus(chen, …)`. My copy looks the ids up by name
   (`byName("R. Chen")`, `byName("Dana Whitfield")`), and with that change it ran to completion.
2. Every script writes its logs and `shots/` into `qa/GG5/scenarios/app/` (`__dirname`). So a
   rerun as written overwrites the author's captures. The port is fixed at 8765 unless you pass a
   base URL.
3. The setup runs `pnpm install` and `vite build`. I skipped them as instructed, which made no
   difference here.

With those changes, the rest of the setup worked as written.

---

### 1. "Restore" on a superseded fact makes the old job current again, with no Undo

**VERDICT: REPRODUCED**

Ran `ui_mutate.cjs` (with ids looked up by name), section Dana. Observed:
```
dana before: …"-superseded|Dana Whitfield works at Brightwell (…).|Confidence 75% Superseded … Restore"
superseded fact action button: Restore | aria: Restore this fact: … | class: btn btn-small btn-danger-ghost
dana after restore, live: Restored "Dana Whitfield works at Brightwell (…)".      (no Undo button; reject shows "Undo")
dana after restore, ipc works_at: [[19,"active","…Brightwell…"],[51,"active","…Corvidlabs…"]]
vault: - Dana Whitfield works at [[Organization/Brightwell Analytics (…)|Brightwell]] … <!-- c:19 -->
       - Dana Whitfield works at [[Organization/Corvid Labs (…)|Corvidlabs]] … <!-- c:51 -->
```
- The claim ids (19, 51) match the author's.
- Facts heading: in my screenshot (`verify/app/shots/dana-facts-heading.png`), Dana's heading reads
  "Facts (6)", taken before the restore, when one of the six was superseded. The list row for Dana
  reads "5 facts". So the list and the detail disagree, which strengthens the finding.

Cited code checks out:
- `knowledge.ts:232-239`: `factAction` returns `restore` with `undo: null` for any status other
  than user_asserted or active.
- `EntityDetail.tsx:364`: the count filter is `!isDroppedStatus`. `knowledge.test.ts:208`
  confirms superseded is not dropped.
- `EntityDetail.tsx:472`: the button class is `btn-danger-ghost` unless the fact is retracted.

I did not exercise "reject after restore gives `rejected`, not `superseded`" in the UI. It follows
from `factAction`, where an active fact maps to reject with decision `rejected`.

Wording: the guide line is quoted short. `docs/book/src/guide/knowledge.md:118` reads "…a changed
job **title**, or by your edit". Severity Medium is fair.

### 2. Fact recency uses the store's write time: every fact is "Today", and rejected facts jump to the top

**VERDICT: REPRODUCED**

`ui_explore.cjs` shows every checked entity in a single "TODAY" group, with "Last seen 1–2 minutes
ago": R. Chen, Dana, Mara, Jonas, Counselling session and Theo. Dana's superseded Brightwell fact
sits third, between current facts. After a reject, Undo, a second reject and a reload
(`ui_mutate.cjs`), the rejected fact moves to the top:
```
rejected after reload, ui: ["-retracted|Robert Chen works on Harbor.|Confidence 60% Rejected Last seen just now Harbor → Why? 1 source Restore",
                            "|Robert Chen works at Brightwell (…)|Confidence 99% Last seen 3 minutes ago …", …]
```
The wire data, from my own IPC read:
```
Dana 19 superseded … upd 2026-10-30T19:54:39.06Z obs ['2026-03-04T09:00:00Z', '2026-03-20T09:00:00Z']
```
- `recency.ts:33-44` (`claimTime` takes the maximum of `updated_at` and evidence `observed_at`) and
  `recency.ts:84-100` (sorts by time first, then by status rank) say what the finding says.
- Minor: the li class is `fact-retracted` for a rejected fact. The author's quote shows this too.

Severity Medium is fair.

### 3. Hard-case names: "R. Chen" and "Sam" as display names, two identical "Gus Ferreira" rows, Tessellate/Lumenfold split

**VERDICT: REPRODUCED**

- **R. Chen:** heading "R. Chen Person", "Also known as Robert Chen". The list row is
  "R. Chen Person · 6 facts · 11 mentions". Mara's fact reads "Mara Okafor corresponded with Robert
  Chen by email … R. Chen →". His facts mix "Robert Chen works on Harbor." with "R. Chen organized
  1:1 Mara / Bob". The graph list shows "R. Chen".
- **Sam:** search "Sam" returns "Climbing with Sam", "Sam Person · 3 facts · 4 mentions" and
  "Sam Patel Person · 5 facts · 11 mentions". Sam's alias is Sam Okonkwo.
- **Gus Ferreira:** two rows, "Gus Ferreira Person · 3 facts · 3 mentions" and "… 2 facts · 2
  mentions". The graph centre picker shows "Gus Ferreira / Person" twice. The merge option shows
  only "Gus Ferreira Person · 2 facts · 2 mentions". The Mara d2 graph list has two identical
  "Gus Ferreira | Person | 1" rows.
- **Organizations:** Tessellate and Lumenfold are separate. Dana's fact reads "works at
  Corvidlabs". The Harbor project and the harbor topic are told apart by kind.

On the cause (`resolve.rs:681-693`, inside `attach` at line 678): the code matches. A name is
replaced only when it looks like an address or was made from a mail domain. The corpus agrees
with the first-seen explanation:
- "R. Chen" comes from `phase1/Calendar/2026-06-03-1on1-bob.ics` (`ORGANIZER;CN="R. Chen"`).
- "Sam" comes from `phase1/Calendar/2026-04-16-climbing.ics` (`ATTENDEE;CN="Sam"`).
- Calendar sorts before Mail, so these files are presumably indexed first. I inferred that from the
  path order; I did not trace the indexing order.

Corrections:
- "named 'Robert Chen' in 10 of the 11 mentions" was not verifiable from the repro. The entity has
  11 mentions. At file level, "R. Chen" appears in 2 corpus files (the 1:1 .ics and
  `phase2/Work/2026-09-18-promotion-case-note.md`) and "Robert Chen" in 12. The direction holds,
  but the exact figure is unsupported.
- This entry bundles four separate issues (display-name choice, missing disambiguator, Gus not
  merged, org rename). They should be split: the UI disambiguator and the resolver naming have
  different owners.

Severity Medium is fair.

### 4. Merging Tessellate into Lumenfold leaves the only employer fact reading "works at Tessellate"

**VERDICT: REPRODUCED**

`ui_mutate.cjs`, merge sections. After merging the two Gus entities, both `works_at` facts are
current: "works at Tessellate … Tessellate →" and "works at Lumenfold … Lumenfold →". After
merging Tessellate into Lumenfold:
```
after org merge, detail facts: ["|Gus Ferreira works at Tessellate (has a tessellate.example address).|… Why? 4 sources Reject this fact"]
ipc: 23 active Gus Ferreira works at Tessellate (…). 4 evidence, object {'id': 'ent_uxjvhc3o6fdilunq', 'name': 'Lumenfold'}
vault Person/Gus Ferreira: - Gus Ferreira works at [[Organization/Lumenfold (ent_uxjvhc3o6fdilunq)|Tessellate]] (…). [^2][^3][^4][^5] <!-- c:23 -->
graph after merges: 34 entities · 52 links
```
- The claim id (23) matches the author's.
- `graph.rs:999` `fold_tx` keeps the oldest id, and its UPDATE sets only status, confidence and
  updated_at, so the text stays as stored.
- The "fold twins" query is at `graph.rs:1955-1973`.
- The Tessellate redirect page exists in `vault/Organization/`.

Minor wording: the finding says "the graph goes from 45 to 43 nodes". My UI showed "34 entities ·
52 links", but that is the Mara-centred view after the merges, not the overview. It is not a
contradiction.

Severity Medium is fair.

### 5. Health entities show like any other in the graph overview and in Mara's neighbourhood

**VERDICT: REPRODUCED**

From `ipc_graph_checks.py` (before mutation):
```
[sensitive Counselling session] in overview=True d1 neighbours=['Albers Counselling, 3 Quay Lane, Halden Bay', 'Jonas Albers', 'Mara Okafor'] attrs={… 'rrule': 'FREQ=WEEKLY;COUNT=8' …}
[sensitive Dr. Lena Brandt] in overview=True d1 neighbours=[]
```
- The UI overview shows "45 entities · 53 links", with Counselling session, Jonas Albers and the
  clinic address all listed.
- In the Mara d2 list, Counselling session is at hop 1, and Jonas and the clinic are at hop 2. The
  Jonas graph shows Mara at 2 hops.
- Mara's facts include "Mara Okafor attended Counselling session on Jun 17, 2026, 5:00 PM."
- The graph nodes carry only `depth, id, kind, name`. Entity detail has no sensitivity field.
- PERSONA.md:43 lists the therapy appointment as sensitive.

Cause note: `llm.rs:187-195` matches the citation, but the finding misses something. `is_sensitive`
fails closed, so every item without an explicit `metadata.sensitive == false` counts as sensitive.
A flag derived from "claims supported only by sensitive items", as the fix direction proposes,
would therefore mark everything unless sources set the flag explicitly. The fix needs real
per-item sensitivity, not this function.

Severity Medium is fair. This is a privacy and expectation issue, not a leak, since everything is
local.

### 6. Kind filter on a centred graph draws floating nodes with no links; the IPC applies `limit` before the filter

**VERDICT: REPRODUCED** (one count differs because of run order)

Ran `ui_graph_kinds.cjs` before the merges:
```
centred: 36 entities · 53 links
kinds=Organizations: 5 entities · 0 links   [Mara, Brightwell Analytics, Corvid Labs, Lumenfold, Tessellate — all depth 2 except Mara]
kinds=People: 11 entities · 9 links          (Jonas Albers, depth 2, has no drawn edge)
```
IPC: `[mara d2 limit=5 kinds=organization] nodes=[('Mara Okafor', 'person')] truncated=True`. The
unlimited d2 organization query returns 4 organizations.

The finding says "4 entities · 0 links" with Organizations on. That is because the author's
`ui_graph_kinds.log` was captured after `ui_mutate.cjs` merged Tessellate (it starts "centred: 34
entities · 52 links"). It matches the setup order, but the finding should say so: on a fresh home
the figure is 5 entities, Tessellate included. The behaviour is the same.

`graphview.rs:354-412` does call `graph.neighbourhood(&center, depth, limit)` and then filters by
kind, so the cause is correct. Severity Low is fair.

---

## Summary
| # | Finding | Verdict |
|---|---|---|
| 1 | Restore on superseded fact, no Undo | REPRODUCED |
| 2 | Recency from write time; rejected facts jump to top | REPRODUCED |
| 3 | R. Chen / Sam display names, two identical Gus rows, org split | REPRODUCED (the "10 of 11" figure is unsupported; should be split) |
| 4 | Merge leaves "works at Tessellate" text | REPRODUCED |
| 5 | Health entities unmarked in graph | REPRODUCED (`is_sensitive` fails closed, which matters for the fix) |
| 6 | Kind filter: floating nodes; limit before filter | REPRODUCED (fresh count is 5 entities, not 4) |

Repro defect affecting findings 1, 2 and 4: `ui_mutate.cjs` hardcodes the author's entity ids and
vault file names, so it fails on a fresh home. The scripts also write into
`qa/GG5/scenarios/app/` and overwrite the author's captures.
