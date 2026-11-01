# QA findings and where they went

Wave GG tests the product end to end with real models and reports findings in `docs/qa/<id>.md`,
with harnesses and evidence under `qa/<id>/`. Each verified finding goes to the team that owns
the code (or to a follow-up package when that team is done). This table records where each went
and its state; it is updated as fixes merge.

Large raw captures (over 1 MB) stay on the QA branch and are not merged: GG8's full onboarding
packet log is `qa/GG8/real/results/s60_onboarding/flows.txt` on branch `qa/privacy`.

## GG8: privacy and egress ([report](GG8.md))

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG8-1 Ask follow-up history to the cloud unscanned | High | EE3 follow-up | **fixed** (merged, regression tests with GG8's s53 history) |
| GG8-2 Cloud embeddings carry sensitive notes | High | DD4 follow-up | **fixed** (merged, regression test with GG8's canaries) |
| GG8-3 Double Tap sent texts become a hidden source read by MCP and cloud Ask | High | EE2 (`ee/draft-recall`) | in progress |
| GG8-4 Card number in the Double Tap thread to the cloud unscanned | Medium | EE2 | in progress |
| GG8-5 Update check on github.com by default, unledgered | Medium | EE5 follow-up | in progress |
| GG8-6 Ledger rows without items | Medium | DD4, EE3, EE4 | **fixed** (embed, Ask, proactive judge; regression tests) |
| GG8-7 Debug logs hold MCP arguments and results | Low | EE4 | **fixed** |
| GG8-8 Audit log stores MCP queries verbatim | Low | EE4 | **fixed** (keyed hash + length) |
| GG8-9 Ask sends absolute file paths | Low | EE3 | **fixed** (grounded prompt; regression test) |
| GG8-10 Read-ahead/prewarm report a local provider when only cloud is usable | Low | EE4 | **fixed** |
| GG8-11 IMAP STARTTLS not ledgered, host carries :993 | Low | EE4 | **fixed** |
| GG8-12 `extractor_provider` null during cloud extraction | Low | EE4 | **fixed** |
| First chat after onboarding: "no provider for tier Synthesis" (not privacy) | High | EE3 follow-up | **fixed** (routes while the model loads; regression test) |

Raw model outputs over 300 KB from GG1–GG3 (and GG3's copy of the desktop app under
`verify/v2/desk`) stay on their QA branches (`qa/doubletap`, `qa/ask`, `qa/agent`).

## GG1: Double Tap end to end ([report](GG1.md))

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG1-1 Instruction planted in a non-sensitive vault note is obeyed (attacker payment link in 62 drafts) | **Blocker** | EE2 follow-up (nugget-draft facts/post) | routed |
| GG1-2 Password fields labelled in other languages are drafted into, password echoed | High | EE2 follow-up (classify) | routed |
| GG1-3 VS Code integrated terminal not refused | High | EE2 follow-up (classify) | routed |
| GG1-4 Each thread message cut from the front: newest question lost | High | EE2 follow-up (prompt) | routed |
| GG1-5 Draft glued to typed text ("Sounds good,i'll") | High | EE2 follow-up (post) | routed |
| GG1-6 Date phrases in the thread filter retrieval and drop the needed fact | High | DD4 follow-up (retrieval) | fixed in c03b984 (drafts ignore date phrases) |
| GG1-7 Form fields get no facts and no field name | High | EE2 follow-up | routed |
| GG1-8 Japanese length counted in whitespace words | High | EE2 follow-up | routed |
| GG1-9 Japanese facts never retrieved | High | DD4 follow-up (retrieval) | fixed in c03b984 (CJK/Thai bigram search, schema v11) |
| GG1-10 One-time-code and card fields not refused, value echoed | High | EE2 follow-up (classify) | routed |
| GG1-11 Default grounding mode checks no values: invented figure | High | EE2 (in progress, recall work) | routed |
| GG1-12 Latency without read-ahead on CPU (15.9 s p50) | High | known (PLAN M3 local: partly met) | open |
| GG1-13 and later | Medium/Low | see report; EE2 follow-up unless retrieval | routed |

## GG2: Ask and Chat ([report](GG2.md)), tested at 5f1998e, before EE3's grounded prompt

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG2-1 Planted instructions obeyed (passport to attacker, French spread) | **Blocker** | EE3: re-test on the grounded prompt, fix what remains | fixed in bd1b67a3 (EE3): 0 planted instructions obeyed on the 4B and 8B (GG2 harness, 4 runners); hidden text no longer indexed |
| GG2-2 Model not told today's date; mail passages lack Date/From | High | EE3 (grounded prompt adds today and dates: re-test) | fixed in bd1b67a3 (EE3: today's date and weekday; mail passages start with From/Date) |
| GG2-3 Date filters and Citation.date use file mtime, not mail Date or DTSTART | High | DD4 follow-up (retrieval) | fixed in c03b984 (copied-folder time questions 76.9% → 92.3%) |
| GG2-4 Default config's Ollama entry tried first: follow-ups never rewritten, ~2 s failover | High | DD4 follow-up (rewrite, routing) | fixed in c03b984 (example commented out; unprobed on-device providers after the runtime slot; rewrite falls back to the heuristic) |
| GG2-5 Sources appear only with the first token (35–95 s) | High | EE3 | fixed in bd1b67a3 (EE3): sources in 0.03–0.05 s, was 33–88 s |
| GG2-6 One triage LLM call per item: 315 files take 41 min, Ask 3× slower meanwhile | High | HH2 (ingest triage) | fixed in 08a0cce (all 315 searchable in 28 s, was 1,436 s; background batched triage yields to Ask) |
| GG2-7 4B triage leaves real items unsearchable | High | HH2 (ingest triage) | fixed in 08a0cce (junk is a label only; items without chunks 98 → 0; legacy installs re-indexed) |
| GG2-8 No relevance floor: near-misses become answers | High | DD4 follow-up (retrieval) | partly fixed in c03b984 (near misses marked "partial match" in the Ask prompt; dropping them emptied 58 answerable contexts); real-model effect not yet re-measured |
| GG2-9 to GG2-18 | Medium/Low | retrieval items DD4, prompt items EE3 | retrieval items (9–13) fixed in c03b984; prompt items with EE3 |

## GG3: computer-use agent ([report](GG3.md)), tested at 0d81796, before DD3

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG3-1 Never asks on ambiguous targets (trashed the whole Documents folder) | High | DD3 follow-up | fixed in 1e783b0 (DD3): unseen objects sent back, then asked; real 4B ask rate 0.59 → 0.71 (target 0.9 missed) |
| GG3-2 Unrequested Empty Trash | High | DD3 follow-up | fixed in 1e783b0 (DD3): unrequested Empty Trash 5 → 0 |
| GG3-3 Harm triage off in the daemon | High | **decision for the user** (DD3 builds "Do it anyway"; default unchanged) | open |
| GG3-4 judge() reports success after a declined send | High | DD3 follow-up | fixed in 1e783b0 (DD3) |
| GG3-5 Send confirmation shows no recipient or body; editable after confirming | High | HH1 | fixed in 7f3c956f (HH1): the confirmation shows the fields; the approval is frozen to their digest |
| GG3-6 press_key on risky buttons skips confirmation (non-English labels) | High | HH1 | fixed in 7f3c956f (HH1) |
| GG3-7 Risk judged from a cached screen; changed label not re-checked | High | DD3 follow-up | fixed in 1e783b0 (DD3: fresh observe and re-check before acting) |
| GG3-8, 13, 14, 19, 20 | Medium | DD3 follow-up | fixed in 1e783b0 (DD3) |
| GG3-10, 11, 12, 15–18, 21–25 | Medium/Low | HH1 | fixed in 7f3c956f (HH1): GG3 probes 49 → 5 findings; kill-switch scenarios 57/57 |

## GG4: proactive cards and morning brief ([report](GG4.md)), simulated two weeks, real models

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG4-1 .ics attendees/organizer/status read from the wrong place: no meeting prep, cancelled events stay | High | BB2 follow-up | routed |
| GG4-2 No UID/SEQUENCE dedup: moved meetings shown twice | High | BB2 follow-up | routed |
| GG4-3 RRULE never expanded: recurring meetings missing | High | BB2 follow-up | routed |
| GG4-4 TZID ignored: cross-zone times off by hours | High | BB2 follow-up | routed |
| GG4-5 One shared learning model: one dismissal moves unrelated senders, never decays | High | BB2 follow-up | routed |
| GG4-6 Items never marked sensitive at ingest (therapy, clinic, salary) | High | HH2 | fixed in 08a0cce (deterministic sensitivity rules; English-heavy, a few de/fr/es words) |
| GG4-7 Confidential HR salary letter becomes a quoted "Reply to" card | High | BB2 follow-up | routed |
| GG4-8 Scoped learning resets leave the shared part | High | BB2 follow-up | routed |
| GG4-9 to GG4-19 | Medium | BB2 follow-up | routed |
| GG4-20 to GG4-33 | Low | see report | open |

## GG5: knowledge graph and vault ([report](GG5.md)), real models

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG5-1 4B model writes card/account numbers, salary, diagnosis into vault pages as title facts | **Blocker** | EE1 follow-up | fixed in 202ed641 (EE1): 0 card, account, amount, salary or diagnosis hits in 3 real-model runs (before 40/84/60) |
| GG5-2 Health/finance items not sensitive without triage; unknown written as not sensitive | High | HH2 (with GG4-6) | fixed in 08a0cce (unknown counts as sensitive; `ingest.assume_unflagged_safe` opts out) |
| GG5-3 Vault and graph ignore sensitivity | High | HH3 | fixed in 3fc731f0 (HH3): confirmed-sensitive items hidden from the vault and graph |
| GG5-4 Re-saving a source revives a fact the user edited | High | HH3 | fixed in 3fc731f0 (HH3) |
| GG5-5 Retyped fact line loses the user's wording | High | HH3 | fixed in 3fc731f0 (HH3) |
| GG5-6 Notes and .ics dated by mtime: wrong current employer | High | EE1 follow-up (DD4 fixing retrieval dates) | fixed in 202ed641 (EE1: one item-date rule) |
| GG5-7 New files never planned during a carried run; pending count wrong | High | HH3 | fixed in 3fc731f0 (HH3): 24/42 → 42/42 files planned |
| GG5-8 Restart re-extracts before the model is ready and replaces model facts | High | HH3 | fixed in 3fc731f0 (HH3): restart 0 lost facts |
| GG5-9 People named by the first display name seen, never renamed | High | EE1 follow-up | fixed in 202ed641 (EE1) |
| GG5-10 and later | Medium/Low | knowledge side EE1, vault/worker side HH3 | mostly fixed (EE1 202ed641, HH3 3fc731f0); see each report for what remains |

## GG6: a new user's first week ([report](GG6.md)), installers from ec35d39 (before HH2) on three OSes, real 4B model

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG6-1 With a local model, indexing runs at LLM speed (24 files: 474–589 s in onboarding) | **Blocker** | HH2 | fixed by HH2's background triage (08a0cce): GG6's own repro indexes 1,000 items in 6 s with a frozen model, was 8 items in 240 s (container); real-OS confirmation pending |
| GG6-2 Folders with nothing indexable spin on "getting ready to index…" forever; setup says Done | High | KK2 | fixed in 1ee96782 (KK2): settles in about 0.5 s and says why |
| GG6-3 A hung local model server blocks Ask for minutes while status says Ready | High | KK1 | fixed in a006b384 (KK1): a hung server ends the chat in 9–10 s and restarts |
| GG6-4 Default config enabled an Ollama provider nobody set up | High | KK1 (migration) | fixed for new installs by DD4 (c03b984); configs written by earlier builds need a migration |
| GG6-5 A model server that won't start: raw repeated dump, crash loop blocks 32 s, relaunch storm | High | KK1 | fixed in a006b384 (KK1) |
| GG6-6 Failed model downloads sit at "Downloading" ~100 s, then a raw error | High | KK1 | fixed in a006b384 (KK1): failures detected in seconds; cancel and resume |
| GG6-7 A saved API key can't be changed or removed | High | KK2 | fixed in 1ee96782 (KK2): replace and remove without a restart |
| GG6-8 Quitting leaves the daemon and the model server (3.3 GB on Windows) running | High | EE5 follow-up | fixed in 8efab875 (EE5): Linux and macOS proven in a dry run; Windows graceful stop unproven (GitHub Actions down) |
| GG6-9 Uninstall silently leaves models, the store, the keychain entry and `~/Nugget Vault` | High | EE5 follow-up | fixed in 8efab875 (EE5): the NSIS page lists what stays; removal opt-in; Windows uninstall test not yet run |
| GG6-10 After an update on macOS/Linux the old daemon serves the new UI | High | EE5 follow-up | fixed in 8efab875 (EE5): an older owned daemon is replaced; macOS not covered by the rehearsal |
| GG6-11 to GG6-16, GG6-21, GG6-22, GG6-26, GG6-31 (keys without Secret Service, onboarding memory, duplicate sources, Ask with no AI, restart after a key, download estimate, wording, skipped files) | Medium/Low | KK2 | fixed in 1ee96782 (KK2) |
| GG6-17, GG6-23 to GG6-25, GG6-29, GG6-30 (crash mid-answer, warm-up state, WAL warning, full disk, sleep timers, download cancel/resume) | Medium/Low | KK1 | fixed in a006b384 (KK1); the 23 s source-error lag (GG6-25) remains |
| GG6-18 Update checks contact GitHub by default, outside the ledger | Medium | EE5 | fixed in 44b9e11 (no request before a channel is chosen; every update request ledgered) |
| GG6-19 macOS: no recovery once Accessibility is denied | Medium | KK2 | fixed in 1ee96782 (KK2) |
| GG6-20, GG6-27, GG6-32 (GNOME without tray, macos-14 jobs cancelled, no shell log) | Medium/Low | EE5 follow-up | fixed in 8efab875 (EE5): Apple Silicon build on macos-15 |
| GG6-28 Windows daemon killed by CTRL_BREAK in the NSIS smoke | Low | EE5 | fixed in 44b9e11 |

## JJ1: wave II features and HH2's triage as a real user ([report](JJ1.md)), tested at efebc0c, repros rerun at 44b9e11

| Finding | Severity | Owner | State |
|---|---|---|---|
| JJ1-1 Sensitivity rules miss de/es/fr items; the 4B triage clears health, money and legal mail for cloud use (9/22 stay sensitive) | High | HH2 | rules fixed in 245d8283 (HH2): 45/45 GG7 situations, 0/79 false positives, JJ1 corpus 22/22; model side: batches of 8 (normal junk 0/100, model-sensitive 2/100); JJ1 re-run pending |
| JJ1-2 Overlay stuck after the target regains focus; later double taps ignored | High | II3 | fixed in c1b0893f |
| JJ1-3 Draft inserted into whichever field has focus | High | II3 | fixed in c1b0893f (field checked before insert; otherwise copied) |
| JJ1-4 Complete one-line replies become an empty draft | High | EE2 (nugget-draft) | waiting: EE2 is paused until the user says continue |
| JJ1-5 WhatsApp date lines in fr/ar import nothing while the source says up to date | High | II1 | fixed in d8d83b17 (II1) |
| JJ1-6 Day-first 12-hour WhatsApp exports get future dates; re-exports duplicate | High | II1 | fixed in d8d83b17 (II1) |
| JJ1-7 A German chat is drafted and refined in English | High | EE2 (nugget-draft) | waiting: EE2 is paused |
| JJ1-8 Refinements unchanged or unreliable with the 4B | Medium | II3 | fixed in c1b0893f (explicit target, one retry); real-model re-measure pending |
| JJ1-9 Recent drafts keep IBAN, SSN and API keys | Medium | II3 | fixed in c1b0893f (draft.check_sensitive) |
| JJ1-10 Events with TZID on the wrong day | Medium | JJ2 | fixed in 32a60e12 for Search; Ask's date phrases and the stored event start still read it as UTC (extraction, BB2) |
| JJ1-11 Date filter wrong across DST | Medium | JJ2 | fixed in 32a60e12 |
| JJ1-12 to JJ1-15 (Telegram zone, phone-only people, one bad iMessage row, Telegram symlinks) and the connector Lows | Medium/Low | II1 | fixed in d8d83b17 (II1) |

## GG7: AI features in all 9 UI languages ([report](GG7.md)), real 4B and 8B

| Finding | Severity | Owner | State |
|---|---|---|---|
| GG7-1 Double Tap writes the correspondent's mail outside English (14/30 on the 4B) | High | EE2 (nugget-draft prompt) | waiting: EE2 is paused until the user says continue |
| GG7-2 English question about non-English mail not found | High | DD4 | fixed in e9daa0ce pending signing: nomic-embed-text v2 (multilingual) takes cross-language hits from 0.50 to 1.00 at 10 on GG7's corpus; the store records its embedding model so existing installs keep v1.5 until re-embedded |
| GG7-3 Sensitivity rules miss 37/40 non-English items; CJK never matches | High | HH2 | fixed in 245d8283: 45/45, 0 false positives, CJK and Arabic matching |
| GG7-4 "Needs a reply" English-only | High | BB2 (nugget-proactive) | waiting: BB2 is paused until the user answers it |
| GG7-5 Dates, promises and renewals English-only (proactive, knowledge) | High | BB2 | waiting |
| GG7-6 Card, brief and action text English in every locale | High | BB2 | waiting |
| GG7 Mediums: search variants (width, Arabic clitics and ة, ß and compounds) | Medium | DD4 | fixed in e9daa0ce |
| GG7 Mediums/Lows: RTL of Latin-led Arabic, ISO dates in titles, localised quote headers, ؟ and CJK sentence ends | Medium/Low | LL2 | fixed on ll/polish (merging) |
