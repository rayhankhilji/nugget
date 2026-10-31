# GG3 verification v1: C1–C11 (fresh verifier, product at 0d81796)

Saved by the lead from the verifier's final message. Raw outputs in this folder (per-probe `*.txt`,
`process-run-{1,2,3}.txt` byte-identical, `recovery-refused-*.json`, `harness-modal-shop-*.json`,
and `policycheck/`: the verifier's own checker calling the real PolicyEngine and risk functions).

| Id | Verdict | Severity |
|---|---|---|
| C1 | CONFIRMED-BENCH-ONLY: the bench app id `settings` misses the policy's settings-app rule (classify.rs:270-276, SETTINGS_APPS classify.rs:2003) and deny-security-panes (rules.rs:394-397); `com.apple.systempreferences` "Automatic updates" → Confirm, "Lock screen…" → Deny. Residual: settings apps not on the list (e.g. xfce4-settings-manager) and update/lock toggles in third-party apps | Low |
| C2 | CONFIRMED (Firewall part bench-only). Wider than claimed: every risky word only the policy's lists know (non-English, and English revoke/admin/checkout) can be pressed with `press_key space` at Edit/Allow; classify.rs:201-205 excludes key verbs from label checks, risk.rs:510-513 falls back to English-only click_risk | High |
| C3 | CONFIRMED: type_text without element targets the last-observed window's cache (state.rs:72-82, tools.rs:226-231, 985-993); after-snapshot re-reads only that window (observe.rs:163-167) | Medium |
| C4 | CONFIRMED: gate uses cache (agent.rs:777, :831; state.rs:66-70); fresh snapshot at agent.rs:982 is never re-gated before act (agent.rs:1000) | High |
| C5 | CONFIRMED: click gets no text_preview (nugget-computer tools.rs:1000-1022), browser submit none (nugget-browser action.rs:290-294); dialog shows only app/label/preview (Agent.tsx ~489-525) | High |
| C6 | CONFIRMED: injection.rs:103-500 English-only, no decoding; per-unit scoring (injection.rs:800-870, scanned agent.rs:1066) misses split phrases | Medium |
| C7 | CONFIRMED: render.rs:53-60/91-96 turns zero-width into spaces before the scan (agent.rs:1064-1066); regression of docs/reviews/security-m3-m6.md PA-3 on the computer-use path | Medium |
| C8 | CONFIRMED (intended per agent.rs:266-267, weak design): agent.rs:916-918 clears suspicion on any approval ≥ Edit | Medium |
| C9 | PARTIAL: log/event/observe leaks are bench-backend artifacts (nugget_env.rs:498, 740-741; real fields are secure → value=hidden, tools refuse). Real: label-only secret detection is English-only (risk.rs:78-95, 221-231), "one-time/sign-in/6-digit code", "account key" missed → value in preview (tools.rs:995-1003) and logged args (tools.rs:1056-1070) | Low |
| C10 | CONFIRMED-BENCH-ONLY: bench perform() moves focus before checking refusal (nugget_env.rs:458-463) → verify_change Unverifiable (observe.rs:295-300); desktop treats unverified as a warning (agent.ts:268-275). Related, code-read only: the real a11y backend counts any visible change after a press as success (nugget-a11y backend.rs:608-623) — a refusal that shows an error banner would verify Passed | Low |
| C11 | CONFIRMED: judge() (agent.rs:1543-1558) clears a Failed action when any later action of the same tool name passed: `click:Send:approved:FAILED(... not on screen)`, `click:Edit:allowed:passed`, finish → `outcome=succeeded`, sent=0 | High |

Notes: C3 and C4 share one root cause (risk from cached snapshots; the fresh pre-act snapshot is never re-gated). C1 and the Firewall half of C2 come from the bench's settings app id.
