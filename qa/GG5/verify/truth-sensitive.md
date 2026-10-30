# GG5 verification: truth-sensitive

Verifier: fresh rerun of every repro on `qa/knowledge` with the prebuilt `target/debug/nuggetd`
(no rebuild, deterministic, no models). All homes are under
`/tmp/claude-0/-home-user-nugget/3826b9b2-0317-5073-9bf7-b47d75df8cb7/scratchpad/verify-truth/`
(`s1`..`s4` = the author's scripts rerun, `v1` = my own minimal scenario). Every daemon was stopped
afterwards (none left running).

Before trusting the scripts I read them: S1 makes the default client with `scopes: {}` and the server
returns `{"include_sensitive": false, "sources": null, "tools": []}`, then calls tools over the real
`nuggetd mcp --client` stdio bridge. S2 adds a checksum-valid IBAN so the item is flagged by the
deterministic scan. S3 runs the same corpus twice, changing only the note's mtime. S4 writes its own
.ics files. The scripts test what the findings say they test. I added one independent script,
`qa/GG5/verify/truth-sensitive/v1_minimal.py`. It writes a 3-file corpus that shares nothing with
the GG5 corpus: a mail where Dana is only in Cc, an untyped meeting note with a vendor in `people:`
and `project: Harbor`, and a GP note with a diagnosis but no card, SSN, IBAN or key.

---

## 1. Health and finance items are not sensitive unless they hold a card/SSN/IBAN/API key, so a default MCP client gets them
**VERDICT: REPRODUCED.** Severity High stands. The doc reference needs strengthening (see below).

- Ran: `s1_sensitive.py <V>/s1`, plus `v1_minimal.py <V>/v1` as an independent check.
- Observed with the default client (`include_sensitive: false`):
  - `search "ferrous sulfate anaemia"` returns `Health/2026-05-12-gp-visit.md`: "…Brandt says it's iron-deficiency anaemia, probably diet plus heavy periods… Prescription: ferrous sulfate 200 mg…"
  - `search "Mortgage account NCU"` returns "Mortgage account: NCU-4471-0093-218 / Property: 12 Saltmarsh Row…".
  - `search "salary payslip net pay"` returns "Gross salary (monthly) | £7,450.00…".
  - `get_entity` Jonas Albers returns `email: jonas@albers-counselling.example` and "Jonas Albers organized Counselling session on 2026-06-17T17:00:00Z."
  - `search "replacement debit card number"`: the card mail is absent for the default client (3 hits, none from Finance/). The `include_sensitive` client gets it as hit [1] with "Card number: 4000 0566…".
  - v1 (independent): a 2-line GP note with "iron-deficiency anaemia… ferrous sulfate" is returned in full to a default client.
- Citations checked:
  - `pipeline.rs:519` is exactly `let sensitive = !ex.pii.is_empty() || triage.as_ref().is_some_and(|t| t.sensitive);`.
  - `pii.rs:1-9` lists cards, SSNs, IBANs and API keys.
  - `ai.rs:95` is the triage system prompt with "sensitive (contains credentials, financial, medical, legal or identity details)".
- **Correction to the doc reading.** The finding says privacy.md "states the scanned categories accurately". privacy.md also says: "Items whose status is unknown are treated as sensitive" (privacy.md:83-84). knowledge.md:40 repeats it for cloud extraction ("items whose sensitivity is unknown"). In code, "unknown" only means a missing `sensitive` key (`nugget-retrieve` `is_sensitive`: `metadata.get("sensitive") != Some(&Value::Bool(false))`). But `pipeline.rs:579` always writes `meta.insert("sensitive", Value::Bool(sensitive))`. So an item that was never triaged (no model, or triage failed: `triage_error`) is stored as known non-sensitive `false`, never as unknown.
  - That contradicts the documented promise directly. It is a stronger basis for the finding than "a user will reasonably expect", and it is the clearest fix point: write `sensitive: false` only when triage actually ran.
- The claimed cloud consequences (cloud embeddings, `cloud_sources` extraction, cloud Ask/Double Tap context) follow from the same flag, because all of those gates use `is_sensitive`. I did not exercise any cloud path, so treat that part as code-inferred.

## 2. The vault, the graph view and knowledge.entity ignore sensitivity: even an item Nugget itself flagged sensitive becomes plain-text vault pages
**VERDICT: REPRODUCED** for the vault. The IPC/graph part is accurate but needs rewording (see below). Severity High stands for the vault.

- Ran: `s2_flagged_item_in_vault.py <V>/s2`.
- Observed:
  - The default MCP client sees nothing: `search "counselling sleep diary"` gives `0 result(s)`, `find_entities "Jonas"` gives `0 entit(ies)`, and `find_entities ""` gives `0 entit(ies)`. So the item really is flagged.
  - The vault still gets `Event/Counselling session (…).md`, `Person/Jonas Albers (…).md`, `Person/Mara Okafor (…).md` and `Place/Albers Counselling, 3 Quay Lane, Halden Bay (…).md`.
  - Jonas's page reads "Jonas Albers organized [[Event/Counselling session…]] on 2026-06-17T17:00:00Z. [^1]", with footnote `"Organizer: Jonas Albers <jonas@albers-counselling.example>"`.
  - Mara's page reads "Mara Okafor attended [[Event/Counselling session…]]", with footnote `"Attendees: Jonas Albers <jonas@…> (accepted), Mara Okafor <mara.okafor@mailbox.example> (accepted)"`.
  - The IBAN itself is not in the vault (grep for `GB82` finds nothing).
  - `knowledge.graph` overview nodes: `["Counselling session", "Jonas Albers", "Albers Counselling, 3 Quay Lane, Halden Bay", "Mara Okafor"]`.
  - In my S1 full-corpus run the vault has 32 hits: `Counselling session` 10, `Jonas Albers` 8, `Albers Counselling` 6, `Lena Brandt` 4, `health` 4. No account number, card number, salary or diagnosis appears.
- Code checked:
  - `grep sensitive crates/nugget-vault/src` finds nothing.
  - In nugget-knowledge the only use is the cloud-LLM gate (llm.rs, config.rs). Extraction and resolution do not filter.
  - `graphview.rs:31-67` computes `sensitive` per evidence item (`ItemInfo`). The finding's "per origin" refers to the origin roll-up at `graphview.rs:111-142`. Only MCP (`scope.rs:6-9`, `allows`) acts on it.
- Corrections:
  - **IPC and graph:** `knowledge.entity` and `knowledge.graph` serve the owner's own desktop app over the local socket. Showing the owner their own sensitive data there is not a leak by itself, and no doc promises otherwise. The real exposure is the plain-text vault, which knowledge.md:92-93 says "you may sync elsewhere". Drop the IPC/graph part from the title or downgrade it to context.
  - **Docs:** privacy.md's "Sensitive items" list covers cloud, cloud Ask and MCP only. It does not say sensitive items are kept out of the vault. So this is an undocumented gap rather than a broken promise. It is still High, because the vault is designed to be synced.

## 3. A note's own date is ignored: a copied old note (mtime = today) overrides a newer mail and flips the current employer
**VERDICT: REPRODUCED.** Severity High stands. The scenario needs a stale note copied in alongside newer conflicting mail; the result is a wrong "current" fact with no warning.

- Ran: `s3_copied_note_dates.py <V>/s3`. The note's mtime was checked with `stat`: `dated` is `2026-03-01 09:00:00`, `copied` is `2026-10-30 19:56:57`.
- Observed:
  - `dated`: "Dana Whitfield works at Brightwell…" is **superseded** (the note's evidence has `observed_at 2026-03-01T09:00:00Z`), and "…works at Corvidlabs…" is **active**.
  - `copied`: "Dana Whitfield works at Brightwell (has a brightwell.example address)." is **active**, with evidence `['0:Notes/People/Dana (Brightwell).md', '2026-10-30T19:56:57.372582912Z', 'org: Brightwell Analytics']`. "…works at Corvidlabs…" is **superseded**, even though the 2026-08-14 mail and the 2026-08-17 note support it.
- Citations checked:
  - `extract/mod.rs:96-104` `item_date` takes `extracted.date`, then `source_modified_at`, then `updated_at`.
  - `nugget-extract/src/mail.rs:99` is the only place `date` is set. `apply_frontmatter` (`nugget-extract/src/markdown.rs:143-172`) handles only title, tags and aliases, so frontmatter `date:` is never used.
  - `EntityDetail.tsx:485` is `const at = evidenceTime(ev);`.
  - The corpus README (lines 14-15) does say "Markdown/.ics items are dated by file mtime; re-apply mtimes after checkout".
- Docs: knowledge.md only says `superseded` means "Replaced by a newer fact" (line 118). There is no explicit promise about source dates, so the finding is about correctness rather than a doc contradiction. The wording is fine.

## 4. Email addresses are written into the vault through footnote quotes, although the docs promise they are kept out
**VERDICT: REPRODUCED.** Severity Medium stands.

- Ran: S1 (`vault_emails`), plus my own grep of `<V>/s1/home/data/vault`.
- Observed: 13 distinct addresses on 20 pages, the same numbers the finding gives for the reference run. They include Mara's private `mara.okafor@mailbox.example`, `jonas@albers-counselling.example` and `sokonkwo@mailbox.example`.
  - Gus Ferreira's page: `[^1]: [Harbor go/no-go review…](nugget://item/…): "Attendees: Mara Okafor <mara.okafor@brightwell.example> (accepted), Robert Chen <robert.chen@brightwell.example> (accepted), Sam Patel <sam.patel@brightwell.example> (accepted), Gus Ferreira <gus@lumenfold.example> (tentative)"`.
  - No page frontmatter contains an `email:` key, so `include_identifiers` is honoured there.
- Citations checked:
  - knowledge.md:91-93 promises "Email addresses and phone numbers are kept out of the pages by default".
  - `nugget-vault/src/config.rs:9-11` holds `include_identifiers` and its doc comment.
  - `render.rs:200` is the frontmatter gate.
  - `render.rs:386-410` `footnote()` appends `inline(q)` truncated to 300 characters, without masking.
  - `extract/mail.rs:127` is `let quote = format!("{}: {}", p.role, p.raw);`.
  - `calendar.rs:247-290` builds the `Organizer: {raw}` and `Attendees` evidence.

## 5. Deterministic extractors assert things the source does not say
**VERDICT: REPRODUCED**, with a wording correction to the `corresponded_with` example. Severity Medium stands.

- Ran: `s4_calendar_claims.py <V>/s4`, the S1 snapshot (`<V>/s1/out/s1/entities.json`), and `v1_minimal.py <V>/v1`.
- **Future event stated as past.**
  - S4: `15 active "Mara Okafor attended Harbor launch day on 2026-12-03T09:00:00Z."` and `16 active "Robert Chen attended Harbor launch day on 2026-12-03T09:00:00Z."` (today is 2026-10-30).
  - The cancelled offsite produces `offsite_page: None`.
  - The code is at `calendar.rs:275-280`, the `match status { "accepted" => ("attended", …), "" if organizer => ("attended", …) }`. There is no time check.
- **Recurrence collapsed.**
  - The stand-up (`RRULE:FREQ=WEEKLY;BYDAY=TU,WE,TH`) gives only "…attended Harbor stand-up on 2026-03-10T09:30:00Z".
  - Counselling (`FREQ=WEEKLY;COUNT=8`) gives only "…on 2026-06-17T17:00:00Z", although `get_entity` shows `"rrule":"FREQ=WEEKLY;COUNT=8"` in the attributes.
- **Wrong line quoted for "attended".** `11 R. Chen attended 1:1 Mara / Bob` and `29 Jonas Albers attended Counselling session` both quote `Organizer: …`. The same happens for every organiser-attendee: claims 3, 7, 15 and 19 (Mara) all quote the Organizer line. `text::locate` (`text.rs:52`) returns the first chunk line that contains the needle, as the finding says.
- **Meeting-note people become project members.**
  - S1: `49 active "Robert Chen works on Harbor."` quotes `people: Robert Chen` from `Notes/Meetings/2026-06-03-1on1-with-Bob.md` (untyped, `project: Harbor`).
  - v1 (independent): a sales-call note with `people: [Quinn Vendor]`, `project: Harbor` gives `active "Quinn Vendor works on Harbor."`, quoting `people: Quinn Vendor`. The vendor gets a vault page and project membership.
  - The code is at `markdown.rs:373-379`, the `_ => works_on` branch for untyped notes.
- **`corresponded_with` correction.**
  - In the GG5 corpus the claim "Mara Okafor corresponded with Dana Whitfield by email" is *true*. Its evidence is three mails, and two of them are Dana writing directly to Mara (`From: Dana Whitfield…` on 2026-03-20 and 2026-08-14). The Cc mail is only one of the three citations.
  - The finding's sentence "from a mail Robert sent with Dana in Cc; Mara and Dana exchanged nothing in it" reads as if the claim were false. It should say instead: a Cc on a mail to the user is enough on its own to create the claim, and one of the cited mails is such a Cc.
  - I confirmed that it is enough on its own: v1, whose only mail is Robert to Mara with Dana in Cc, produces `active "Mara Okafor corresponded with Dana Whitfield by email."` with quote `Cc: Dana Whitfield <…>`.
  - Code: `extract/mail.rs:165-194`. When the user is on the message, every non-crowd participant gets the claim at 0.9.

## 6. Why? quotes for 35 claims are reconstructed text, not text from the file
**VERDICT: REPRODUCED.** The total matches and the breakdown differs slightly; one category is missing from the finding. Severity Low stands.

- Ran: I could not use the grader on the author's `o1/` reference run, because its path is not given and I could not find it on disk. Instead I ran the same per-claim check as `grade.py:207-222` against my S1 snapshot: an active claim is bad if none of its evidence quotes occurs, whitespace-normalised and case-insensitive, in the source file.
- Observed:
  - 59 active claims, of which **35** have no verbatim quote. That is **30 .ics + 5 .md**; the finding says 29 + 6.
  - The md quotes are `members: Dana Whitfield`, `members: Sam Patel`, `people: Robert Chen`, `tags: data-platform` and `tags: launch`, all with `chunk_id: None`. These are the 5 strings the finding lists, which supports 5 rather than 6. The difference is likely the single-pass corpus versus the harness's phase1-then-phase2 run.
- Citations checked:
  - `ics.rs:412-427` renders `Location:`, `Organizer:` and `Attendees:` lines.
  - `markdown.rs:89` is `quote: format!("{key}: {raw}")`.
  - `markdown.rs:10` is the module doc "quotes the `key: value` line it came from, exactly as stored".
  - The test helper `field_lines` / `assert_quotes_are_source_text` at `markdown.rs:514-537` compares against its own `key: value` rendering, as the finding says.
- **Addition:** mail evidence is synthesized too. `extract/mail.rs:127` writes `"{role}: {raw}"` per recipient, so a header `To: Mara Okafor <…>, Sam Patel <…>` is cited as `To: Sam Patel <sam.patel@brightwell.example>`, a line that is not in the file. I found 5 such evidence quotes, in `2026-09-03-harbor-launch-date.eml` and `2026-10-06-harbor-launch-moved.eml`. The per-claim grader does not flag them, because each of those claims also has another verbatim quote. A claim backed only by a multi-recipient To or Cc line would be flagged.
- The finding's reading of the ics class is fair: it is a faithful decoding that sits in the chunk text, and the grader's check is too strict for it.

## Spot checks of "Works well"
- Confirmed from S1:
  - The card mail is hidden from the default client and shown to the `include_sensitive` client.
  - No card number, account number, salary or diagnosis string is in the vault.
- Confirmed from S4:
  - The cancelled offsite has no page.
  - MCP `resources/read` of `nugget://item/files-1/0%3ACalendar%2F…` returns the item text.
- No deep-link scheme is configured in `apps/desktop/src-tauri/tauri.conf.json` or its Cargo.toml.
