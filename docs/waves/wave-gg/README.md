# Wave GG: quality assurance

Eight independent QA packages that test the AI and agent features end to end with real models and report findings with repros. QA teams add new files only (`qa/<id>/`, `docs/qa/<id>.md`, one dispatch-only workflow); fixes go to the owning build teams through the orchestrator.

| WP | Branch | Package |
|---|---|---|
| GG1 | `qa/doubletap` | GG1: Double Tap, end to end, with real models. |
| GG2 | `qa/ask` | GG2: Ask and Chat, end to end, with real models. |
| GG3 | `qa/agent` | GG3: the computer-use agent, end to end, with real models. |
| GG4 | `qa/proactive` | GG4: proactive cards and the morning brief, with real models. |
| GG5 | `qa/knowledge` | GG5: knowledge graph and Markdown vault, with real models. |
| GG6 | `qa/first-week` | GG6: the first week of a new user, on all three OSes. |
| GG7 | `qa/multilingual` | GG7: AI features in other languages. |
| GG8 | `qa/privacy` | GG8: privacy and egress, verified from the outside. |
