# GG5 corpus: Mara Okafor (synthetic)

All people, organisations, addresses, account and card numbers here are invented (`.example`
domains, test card number). See `../PERSONA.md` for the persona; `gold.json` is the grader's
ground truth (every fact carries a verbatim quote from its source file).

Layout: `phase1/` items dated 2026-03-01 to 2026-08-10, `phase2/` items 2026-08-11 to
2026-10-25. Index phase1, check, then add phase2. Inside each: `Mail/Inbox`, `Mail/Sent`
(.eml), `Calendar` (.ics), `Notes` (.md), plus `Health`, `Finance` (sensitive) and `Work`
(documents others wrote about Mara). 42 files: 21 .eml, 7 .ics, 14 .md.

Harness notes: the files connector sets no `source.folder`, so sent-folder inference of the
user does not fire; configure `[knowledge.me] emails` with both of Mara's addresses and add
`mailbox.example` to `extra_freemail_domains`. Markdown/.ics items are dated by file mtime;
re-apply mtimes after checkout (filename date prefix, else frontmatter `date:`).

| Item | Hard case |
|---|---|
| phase1/Mail/Inbox/2026-03-04-harbor-kickoff.eml | Mara leads Harbor; Robert signs "Bob"; Dana as Design Lead |
| phase1/Mail/Sent/2026-03-05-re-harbor-kickoff.eml | user recognisable as sender (work address); "Bob" |
| phase1/Mail/Inbox/2026-03-12-tessellate-intro.eml | Gus at Tessellate (pre-rename domain) |
| phase1/Mail/Inbox/2026-03-20-dashboard-design-review.eml | Dana Design Lead at Brightwell (later superseded) |
| phase1/Mail/Inbox/2026-04-02-pipeline-green.eml | Sam Patel signs "Sam"; "Rob Chen", "Rob" |
| phase1/Mail/Inbox/2026-04-09-thursday-climbing.eml | Sam Okonkwo signs "Sam"; neighbour, climbing club |
| phase1/Mail/Inbox/2026-04-18-ife-may-visit.eml | "Ife", "Mar"; sister; Port Ellery; Theo still partner |
| phase1/Mail/Sent/2026-04-19-re-may-visit.eml | user from personal address; Theo cc'd; "climbing with Sam" |
| phase1/Mail/Inbox/2026-06-10-tessellate-renewal.eml | vendor relationship |
| phase1/Mail/Sent/2026-06-24-dinner-tonight.eml | partner context (phase1 only) |
| phase1/Calendar/2026-03-10-harbor-standup.ics | Harbor stand-up with Sam Patel (recurring) |
| phase1/Calendar/2026-04-16-climbing.ics | attendee CN is just "Sam" (Okonkwo's address): ambiguous name |
| phase1/Calendar/2026-06-03-1on1-bob.ics | organiser CN "R. Chen"; summary says "Bob" |
| phase1/Notes/Journal/2026-03-15.md | Theo as partner; "Bob"; both Sams; lives in Halden Bay |
| phase1/Notes/Journal/2026-07-05.md | relationship faltering (not yet ended) |
| phase1/Notes/Projects/Harbor.md | typed project note: owner/members/org frontmatter, wikilink to Tessellate Systems |
| phase1/Notes/People/Ife.md | typed person note; formal name "Ifeoma Okafor" via aliases; email ident |
| phase1/Notes/Meetings/2026-06-03-1on1-with-Bob.md | "1:1 with Bob"; frontmatter people: Robert Chen |
| phase1/Health/2026-05-12-gp-visit.md | sensitive: diagnosis, prescription, GP |
| phase1/Health/2026-06-17-counselling.ics | sensitive: therapy appointment |
| phase1/Finance/2026-04-30-payslip-april.md | sensitive: salary |
| phase1/Finance/2026-07-01-mortgage-statement.md | sensitive: account number |
| phase1/Finance/2026-05-21-new-debit-card.eml | sensitive: card number; automated sender (no person) |
| phase2/Mail/Inbox/2026-08-14-dana-new-role.eml | job change: Dana Head of Product at Corvid Labs, new address |
| phase2/Mail/Sent/2026-08-15-re-new-role.eml | user as sender; "climbing with Sam" |
| phase2/Mail/Inbox/2026-09-01-tessellate-is-now-lumenfold.eml | org rename; Gus's new address (merge, not new person/job) |
| phase2/Mail/Inbox/2026-09-03-harbor-launch-date.eml | launch 2026-11-12 (later superseded); "Bob" |
| phase2/Mail/Sent/2026-09-04-re-harbor-launch-date.eml | "Sam said the pipeline is green" (Sam Patel, from context) |
| phase2/Mail/Inbox/2026-09-15-fwd-data-weekly.eml | prompt injection in forwarded newsletter |
| phase2/Mail/Inbox/2026-09-22-lumenfold-region-move.eml | Gus at Lumenfold |
| phase2/Mail/Inbox/2026-10-06-harbor-launch-moved.eml | final launch date 2026-12-03 supersedes earlier dates |
| phase2/Mail/Inbox/2026-10-12-theo-books.eml | ex-partner, now friendly acquaintance |
| phase2/Mail/Sent/2026-10-20-re-sunday-crag.eml | user to Sam Okonkwo; Ife visiting |
| phase2/Calendar/2026-08-21-dana-farewell.ics | Dana with new address |
| phase2/Calendar/2026-10-14-harbor-go-no-go.ics | Gus at lumenfold.example; launch 3 Dec |
| phase2/Calendar/2026-10-24-ife-visit.ics | Ife and Sam Okonkwo together (personal context) |
| phase2/Notes/Journal/2026-08-16.md | breakup (July) written in August: Theo no longer partner |
| phase2/Notes/Journal/2026-09-24.md | both Sams in one note; "1:1 with Bob" |
| phase2/Notes/Projects/Harbor-launch-plan.md | stale draft (date 2026-08-20) saying 2026-11-19 |
| phase2/Notes/People/Dana-Whitfield.md | typed person note: org Corvid Labs, both emails |
| phase2/Work/2026-08-28-mid-year-review-Mara-Okafor.md | written by Robert about Mara ("I" is Robert) |
| phase2/Work/2026-09-18-promotion-case-note.md | written by Priyanka about Mara; "R. Chen", "Rob" |
