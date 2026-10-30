# GG5 findings: truth-sensitive

Area: are the vault's facts true, current and properly cited, and does sensitive material stay
where it belongs. All runs used the deterministic daemon (`target/debug/nuggetd`, no models;
`--no-models` harness config). Commit at time of run: `1d6bda785a7bfc1889bc4b6c26fa7805e4b57dc0`
(the reference run `o1/` was on `e7299584`). Scripts and captured outputs are in
`qa/GG5/scenarios/truth-sensitive/` (`s1_sensitive.py` -> `s1.json`, `s2_flagged_item_in_vault.py`
-> `s2.json`, `s3_copied_note_dates.py` -> `s3.json`, `s4_calendar_claims.py` -> `s4.json`). Each
script takes one argument, a fresh work dir, e.g.
`python3 qa/GG5/scenarios/truth-sensitive/s1_sensitive.py /tmp/.../scratchpad/truth-sensitive/s1`.
The scripts start and stop their own daemon (SIGTERM).

### Health and finance items are not sensitive unless they hold a card/SSN/IBAN/API key, so a default MCP client gets the diagnosis, prescription, therapy appointment, mortgage account number and salary
- Severity: High
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s1_sensitive.py <dir>` (phase1+phase2 corpus, dated as
  the harness does; creates an MCP client with the default scopes `{"include_sensitive": false,
  "sources": null, "tools": []}`, which is what "Connect an app" gives, and calls tools over the
  `nuggetd mcp --client` stdio bridge). Output: `s1.json` keys `mcp_default` and `mcp_include_sensitive`.
- Expected vs actual: gold.json lists all five Health/Finance files as `sensitive_items`; PERSONA says
  they "must not leak into generated text or to the cloud"; mcp.md promises "Sensitive items: excluded
  unless you allow them". With the default client only the debit-card mail (Luhn-valid card number)
  is withheld. Everything else comes back:
  - `search "ferrous sulfate anaemia"` -> `0:Health/2026-05-12-gp-visit.md`: "…Brandt says it's iron-deficiency anaemia, probably diet plus heavy periods… Prescription: ferrous sulfate 200 mg…"
  - `search "Mortgage account NCU"` -> "Mortgage account: NCU-4471-0093-218 / Property: 12 Saltmarsh Row, Halden Bay / Balance at 1 January 2026: £214,880.12"
  - `search "salary payslip net pay"` -> "Gross salary (monthly) | £7,450.00…"
  - `get_entity` Jonas Albers -> "email: jonas@albers-counselling.example … Jonas Albers organized Counselling session on 2026-06-17T17:00:00Z."
  - `search "replacement debit card number"` -> the card mail is absent (correctly) for the default client, present for `include_sensitive`.
  The same `sensitive: false` flag makes these items eligible for cloud embeddings
  (`allow_cloud_embeddings`) and cloud knowledge extraction (`[knowledge.llm] cloud_sources`), and
  for cloud Ask/Double Tap context.
- Root cause: `crates/nugget-ingest/src/pipeline.rs:519`
  `let sensitive = !ex.pii.is_empty() || triage.as_ref().is_some_and(|t| t.sensitive);` and the PII scan
  (`crates/nugget-ingest/src/pii.rs:1-9`) only knows cards, US SSNs, IBANs and API keys. Medical and
  financial content is only caught by a local triage model (`ai.rs:95`), which is optional; without one
  (the deterministic case here; also any user whose triage model is not installed or failing) nothing
  else marks it. A non-IBAN account number like `NCU-4471-0093-218` is never caught. privacy.md
  ("Sensitive items") states the scanned categories accurately, but a user reading "hidden from MCP
  clients" will reasonably expect their GP note and payslip to be covered. With a triage model the
  item flags may differ (real-model runs on GitHub will show it).
- Reproduces: 1/1 (S1); consistent with the reference run `o1/` (sensitive flags only on the card mail).
- Owning area: crates/nugget-ingest (pii.rs, pipeline.rs). Not EE1; EE1's brief does not target this.
- Fix direction: add deterministic signals that do not need a model (path/folder and tag hints such as
  `Health/`, `#health`, `#finance`, `.ics` with counselling/clinic terms, labelled account numbers,
  salary tables), and/or default MCP/cloud to "unknown until triaged" when no triage model ran; at
  minimum say plainly in privacy.md and mcp.md that health/finance text is not detected without a
  triage model.

### The vault, the graph view and knowledge.entity ignore sensitivity entirely: even an item Nugget itself flagged sensitive becomes plain-text vault pages (therapist's name, appointment title, clinic address)
- Severity: High
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s2_flagged_item_in_vault.py <dir>`. Input is only the
  counselling invite with an IBAN added to its DESCRIPTION, so the deterministic PII scan marks it
  `sensitive: true` (no model needed). Output `s2.json`.
- Expected vs actual: the item is sensitive, and the MCP layer treats it so: default-client
  `find_entities ""` -> `0 entit(ies)`, `search "counselling sleep diary"` -> `0 result(s)`. But the
  same daemon writes it all into the plain-text vault (which knowledge.md says "you may sync
  elsewhere") and returns it on IPC/graph:
  - vault pages: `Event/Counselling session (…).md`, `Person/Jonas Albers (…).md`,
    `Place/Albers Counselling, 3 Quay Lane, Halden Bay (…).md`, `Person/Mara Okafor (…).md`
  - `Person/Jonas Albers (…).md`: `- Jonas Albers organized [[Event/Counselling session (…)|Counselling session]] on 2026-06-17T17:00:00Z. [^1]` and
    `[^1]: [Counselling session](nugget://item/files-1/0%3AHealth%2F2026-06-17-counselling.ics): "Organizer: Jonas Albers <jonas@albers-counselling.example>"`
  - `knowledge.graph` overview nodes: `["Counselling session", "Jonas Albers", "Albers Counselling, 3 Quay Lane, Halden Bay", "Mara Okafor"]`
  With the real corpus (S1 and reference run `o1/phase2`, grade.json `sensitive_hits`: 25 hits) the
  vault README lists `Jonas Albers`, `Dr. Lena Brandt` (the GP, from the GP note's `people:`
  frontmatter), `Counselling session` and the clinic address; Mara's own page reads "Mara Okafor
  attended Counselling session on 2026-06-17T17:00:00Z", so anyone reading the synced vault sees she
  is in weekly therapy, with whom and where. Copies also sit in `<vault>/.nugget/base/` (S1
  `vault_hits`). No account number, card number, salary or diagnosis string reached the vault in the
  deterministic run (an LLM extractor run may differ: it could quote them).
- Root cause: no sensitivity check anywhere in `crates/nugget-vault/src` (render.rs renders every
  entity and active claim) or in nugget-knowledge; `crates/nuggetd/src/graphview.rs:31-67` computes
  `sensitive` per origin but only the MCP scope filter (`crates/nugget-mcp/src/scope.rs:6-9`) uses it.
- Reproduces: 2/2 (S2 run twice), plus S1 and the reference run.
- Owning area: crates/nugget-vault render.rs (EE1 owns "vault rendering of entities and claims", but its
  brief does not target sensitivity), crates/nuggetd graphview.rs.
- Fix direction: do not render facts (or footnote quotes) whose only evidence is a sensitive item, and
  do not create pages for entities known only from sensitive items, unless the user opts in (e.g.
  `[vault] include_sensitive = false` by default); document the rule in privacy.md and knowledge.md.

### A note's own date is ignored: a copied old note (mtime = today) overrides a newer mail and flips the current employer
- Severity: High
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s3_copied_note_dates.py <dir>`. Full corpus plus one
  typed note `Notes/People/Dana (Brightwell).md` (`type: person`, `date: 2026-03-01`,
  `email: dana.whitfield@brightwell.example`, `org: Brightwell Analytics`). Variant `dated` sets its
  mtime to 2026-03-01; variant `copied` leaves mtime = now, as after `cp`, `git clone`, a sync client
  or restoring a backup. Output `s3.json`.
- Expected vs actual: the note says it was written 2026-03-01, before Dana's 2026-08-14 "New job" mail,
  so Corvid Labs should stay current in both variants.
  - dated: `Dana Whitfield works at Brightwell …` **superseded**; `… works at Corvidlabs …` **active**.
  - copied: `Dana Whitfield works at Brightwell (has a brightwell.example address).` **active** with
    evidence `["0:Notes/People/Dana (Brightwell).md", "2026-10-30T19:48:35.693348407Z", "org: Brightwell Analytics"]`;
    `… works at Corvidlabs …` **superseded**.
  The Why? panel shows the copy time as the note's date (`evidenceTime(ev)` in
  `apps/desktop/src/components/EntityDetail.tsx:485`). A user who copies an old notes folder in gets
  every note dated "today", so any note-derived fact beats any mail-derived fact.
- Root cause: `crates/nugget-knowledge/src/extract/mod.rs:96-104` `item_date` uses
  `metadata.extracted.date` (only set for mail, `crates/nugget-extract/src/mail.rs:99`), else
  `source_modified_at` (mtime), else `updated_at`. Frontmatter `date:`, a date in the file name and an
  .ics `DTSTAMP`/`DTSTART` are never consulted. The corpus README already has to work around this ("re-apply mtimes after checkout").
- Reproduces: 2/2.
- Owning area: crates/nugget-knowledge extract/mod.rs (and nugget-extract for the markdown `date`).
  EE1's brief targets this ("Claims carry a validity interval from the source date").
- Fix direction: have the markdown extractor emit `extracted.date` from frontmatter `date`/`created`
  (and a leading `YYYY-MM-DD` in the file name), and .ics items use DTSTAMP/LAST-MODIFIED; use mtime
  only as the last resort, and prefer the earlier of mtime and ctime-like signals when a file was
  evidently copied.

### Email addresses are written into the vault through footnote quotes, although the docs promise they are kept out
- Severity: Medium
- Repro: S1 (`s1.json` key `vault_emails`) or `grep -rhoE '[A-Za-z0-9._-]+@[A-Za-z0-9.-]+example' <o1>/phase2/vault --include=*.md | sort | uniq -c`.
- Expected vs actual: knowledge.md: "Email addresses and phone numbers are kept out of the pages by
  default, because the vault is plain text you may sync elsewhere" (`[vault] include_identifiers =
  false`, `crates/nugget-vault/src/config.rs:9-11`). Actual: 13 distinct addresses on 20 pages in
  the reference vault, e.g. `Person/Jonas Albers (…).md`:
  `[^1]: [Counselling session](nugget://item/…): "Organizer: Jonas Albers <jonas@albers-counselling.example>"`,
  `Person/Gus Ferreira (…).md`: `"Attendees: Mara Okafor <mara.okafor@brightwell.example> (accepted), Robert Chen <robert.chen@brightwell.example> (accepted), Sam Patel <sam.patel@brightwell.example> (accepted), Gus Ferreira <gus@lumenfold.example> (tentative)"`.
  One calendar footnote exposes every invitee's address on every invitee's page.
- Root cause: the frontmatter honours `include_identifiers` (`crates/nugget-vault/src/render.rs:200`), but
  `footnote()` (`render.rs:386-410`) prints the evidence quote verbatim, and the mail/calendar
  extractors quote header lines (`extract/mail.rs:127`, `extract/calendar.rs:247-290`).
- Reproduces: 2/2 (S1, reference run).
- Owning area: crates/nugget-vault render.rs (EE1 owns vault rendering; not in its brief).
- Fix direction: when `include_identifiers` is false, mask `<addr>` / bare addresses and phone numbers in
  footnote quotes (e.g. `Organizer: Jonas Albers`), or quote only the name part for header-derived
  evidence.

### Deterministic extractors assert things the source does not say: "attended" a future event, "works on" a project for anyone named in a meeting note, "corresponded with" someone only Cc'd
- Severity: Medium
- Repro: `python3 qa/GG5/scenarios/truth-sensitive/s4_calendar_claims.py <dir>` (adds a 2026-12-03 "Harbor
  launch day" invite with both people PARTSTAT=ACCEPTED, today being 2026-10-30; and a CANCELLED offsite).
  Output `s4.json`. The other claims are in the reference run `o1/phase2/entities.json`.
- Expected vs actual:
  - Future event stated as past: `15 active "Mara Okafor attended Harbor launch day on 2026-12-03T09:00:00Z."` and
    `16 active "Robert Chen attended Harbor launch day on 2026-12-03T09:00:00Z."` Acceptance is not
    attendance; for a future event it is plainly false. `crates/nugget-knowledge/src/extract/calendar.rs:274-279`
    maps `accepted` (and an organiser with no PARTSTAT) to `attended` without looking at the start time.
  - Recurring events collapse to the first occurrence: the weekly counselling invite (`RRULE:FREQ=WEEKLY;COUNT=8`)
    yields only "Mara Okafor attended Counselling session on 2026-06-17T17:00:00Z" (same for the weekly Harbor stand-up).
  - "Attended" cited with the wrong line: `R. Chen attended 1:1 Mara / Bob` and `Jonas Albers attended Counselling session`
    show Why? quote `Organizer: R. Chen <robert.chen@brightwell.example>` / `Organizer: Jonas Albers <…>`.
    The attendee loop locates the attendee's address and `text::locate` (`text.rs:52`) returns the first
    line containing it, which is the Organizer line, so the quote does not show attendance.
  - Meeting note people become project members: `Notes/Meetings/2026-06-03-1on1-with-Bob.md`
    (`people: [Robert Chen]`, `project: Harbor`) yields `29 active "Robert Chen works on Harbor."` quoted
    `people: Robert Chen`. Any untyped note with `project:` turns everyone in `people:`/`attendees:` into a
    member (`extract/markdown.rs:373-379`), e.g. a vendor in a meeting note.
  - `22 active "Mara Okafor corresponded with Dana Whitfield by email."` from a mail Robert sent with Dana in Cc
    (`Cc: Dana Whitfield <…>`); Mara and Dana exchanged nothing in it.
- Reproduces: 1/1 (S4) for the future event; 2/2 (reference run + S1) for the others.
- Owning area: crates/nugget-knowledge extract/calendar.rs, markdown.rs, mail.rs (EE1; its brief's
  "time-aware claims" covers the future/past distinction only loosely).
- Fix direction: use `invited_to`/"is going to" for events that have not started, and keep
  `attended` for past, non-cancelled events; mention recurrence ("weekly from …"); locate the
  `Attendees:` line for attendee evidence; use `related_to`/`mentioned_in` rather than `works_on` for
  people in an untyped note; limit `corresponded_with` to From/To with the user on the other side.

### Why? quotes for 35 claims are reconstructed text, not text from the file (what they are, and which mislead)
- Severity: Low
- Repro: `python3 qa/GG5/harness/grade.py --gold qa/GG5/corpus/gold.json --run <o1>`; grade.json
  `snapshots.phase2.quote_not_in_source` (35 entries).
- What they are: (a) 29 .ics claims quote `Location: …`, `Organizer: Name <addr>` and
  `Attendees: A <a> (accepted), …`. These are lines of Nugget's own plain-text rendering of the
  invite (`crates/nugget-extract/src/ics.rs:412-427`) stored as the item's chunk text, so they are in
  the indexed item (chunk_id is set, MCP `resources/read` of the footnote URL shows them) but not in
  the .ics file, which has `LOCATION:` / `ORGANIZER;CN="R. Chen":mailto:…`. That is a faithful decoding;
  the grader's verbatim-in-file check is too strict for these. (b) 6 frontmatter claims quote
  `members: Dana Whitfield`, `members: Sam Patel`, `people: Robert Chen`, `tags: data-platform`,
  `tags: launch`: the files hold YAML lists (`members:\n  - Dana Whitfield`), so the quote is synthesized
  (`extract/markdown.rs:89`, `chunk_id: null`), although the module doc says "quotes the `key: value`
  line it came from, exactly as stored" (`markdown.rs:10`); the unit test only compares with its own
  `key: value` rendering of the parsed YAML (`markdown.rs:512-537`), so it cannot catch this.
  Misleading cases are the ones in the previous finding (Organizer line for "attended", `people:` for
  "works on"); the rest show the right thing in a slightly different form.
- Reproduces: 2/2 (reference run, S1).
- Owning area: crates/nugget-knowledge extract/markdown.rs (EE1).
- Fix direction: quote the source's own lines (`members:` plus the `- Dana Whitfield` line), or mark
  synthesized quotes as such in Why?; let the grader accept ics-rendered lines as source text.

## Works well
- The PII scan flags the Luhn-valid card number; the debit-card mail is hidden from a default MCP
  client and appears for an `include_sensitive` client (S1). An IBAN in an invite also flags it, and
  then MCP hides the entities, facts and search hits completely (S2 `find_entities ""` -> 0).
- The automated Northshore sender produced no person, organisation or claim; no account number, card
  number, salary or diagnosis string reached the vault in deterministic mode (S1 `vault_hits`).
- Mail is dated by its `Date` header; with correct dates Dana's Brightwell job is superseded by Corvid
  Labs and the history keeps all evidence (S3 `dated`).
- A `STATUS:CANCELLED` invite creates no event, no attendance and no vault page (S4 `offsite_page: null`).
- Vault footnote URLs `nugget://item/files-1/0%3A…` resolve to the right item through MCP
  `resources/read` (S4 `resources_read`); every active claim in the reference run has evidence
  (grade.json `uncited: []`). Note there is no OS handler for `nugget://` (no deep-link plugin in
  `apps/desktop/src-tauri`), so clicking a footnote in Obsidian opens nothing.
