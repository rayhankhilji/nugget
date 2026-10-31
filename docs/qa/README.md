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
| GG8-1 Ask follow-up history to the cloud unscanned | High | EE3 follow-up (`ee/ask-quality`) | in progress |
| GG8-2 Cloud embeddings carry sensitive notes | High | DD4 follow-up | **fixed** (merged, regression test with GG8's canaries) |
| GG8-3 Double Tap sent texts become a hidden source read by MCP and cloud Ask | High | EE2 (`ee/draft-recall`) | in progress |
| GG8-4 Card number in the Double Tap thread to the cloud unscanned | Medium | EE2 | in progress |
| GG8-5 Update check on github.com by default, unledgered | Medium | EE5 follow-up | in progress |
| GG8-6 Ledger rows without items | Medium | embed: DD4 (**fixed**); Ask: EE3; proactive judge: EE4 follow-up | partly fixed |
| GG8-7 Debug logs hold MCP arguments and results | Low | EE4 follow-up (`ee/security-review-2`) | in progress |
| GG8-8 Audit log stores MCP queries verbatim | Low | EE4 follow-up | in progress |
| GG8-9 Ask sends absolute file paths | Low | EE3 follow-up | in progress |
| GG8-10 Read-ahead/prewarm report a local provider when only cloud is usable | Low | EE4 follow-up | in progress |
| GG8-11 IMAP STARTTLS not ledgered, host carries :993 | Low | EE4 follow-up | in progress |
| GG8-12 `extractor_provider` null during cloud extraction | Low | EE4 follow-up | in progress |
| First chat after onboarding: "no provider for tier Synthesis" (not privacy) | High | EE3 follow-up | in progress |
