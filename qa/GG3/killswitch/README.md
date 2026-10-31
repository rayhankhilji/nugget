# GG3 kill switch and confirmation probes

Question: can the computer-use agent act after Stop (the kill switch) or act past a pending
confirmation? Probed at commit `0d81796` (branch `qa/agent`) through the real `nuggetd` over its IPC
socket, the real desktop frontend (headless Chromium through `apps/desktop/e2e/bridge.mjs`), and an
in-process Rust probe of the agent loop with the real computer-use tools.

Short answer: **no action was ever seen to start after `policy.kill` returned** (0 across every
run below). The weak spots are at the edges: an action already handed to the OS or the browser
finishes after Stop while the log says "interrupted"; `agent.confirm` is accepted (and logged as
"approved") for up to ~50 ms after the kill; a task waiting on `ask_user` survives Stop and acts after
Resume; the browser extension and the daemon disagree about whether Nugget is stopped. See the
findings in the lead's report.

## Why a mock model

`nuggetd-fake-llama-server` (`crates/nugget-runtime/src/bin/fake-llama-server.rs`) cannot emit tool
calls: `/v1/chat/completions` only ever answers "Hello from fake" or a cited quote (line ~577), and
the word "tool" appears nowhere in its handler. With it the agent can never reach a confirmation. So
`lib/mockmodel.mjs` is a ~100-line scripted OpenAI-compatible server (SSE streaming with
`delta.tool_calls`, as `crates/nugget-infer/src/adapters/openai_compatible.rs` parses), configured
the way a BYOK/LAN provider is: a `[[providers]]` entry, `kind = "openai_compatible"`,
`tiers = ["agentic"]` (`lib/setup.mjs`). Each request's turn = number of assistant messages so far;
a turn can be delayed (slow model).

## Why a fake browser extension

On Linux the computer-use (accessibility) tools have no desktop to act on here (no AT-SPI bus), so to
see whether an action *executed* the probes use the browser tools, which the agent gets in the same
registry. `lib/fakeext.mjs` attaches to the daemon exactly like `nugget-browser-host`
(`browser.host.attach`, `browser.host.request` notifications, `browser.host.deliver` replies) and
mirrors `extensions/browser/src/router.ts` (after a `halt` request it refuses until the popup's
Resume). It serves a synthetic compose form on `https://mail.example.test`. Every `act` it receives
is timestamped: that is the ground truth for "the action ran". It can hold an `act` (slow action).

`rust/` (ks-probe) covers the computer-use tools themselves: the real `Agent`, `PolicyEngine`,
`KillSwitch` and `nugget_computer::tools_with` over a fake `ComputerUse` whose `key_input` / `act`
take 1.5 s, like typing a long text.

## Rerun

```sh
cd /home/user/nugget
CARGO_INCREMENTAL=0 cargo build --locked -p nuggetd --features fake-runtime --bins   # gives target/debug/nuggetd
cd qa/GG3/killswitch

node smoke.mjs                                  # mock model -> browser tools -> a confirmation -> approve
KS_REPEAT=3 node scenarios.mjs                  # every IPC scenario, 3 reps (about 4 min)
KS_REPEAT=3 node scenarios.mjs s1d_approve_right_after_kill_returned s10_extension_left_halted   # pick some

# Desktop UI (scenario 6): build the frontend somewhere outside the repo, then point KS_DIST at it
D=$(mktemp -d); cp -r ../../../apps/desktop/{e2e,index.html,overlay.html,package.json,pnpm-lock.yaml,src,tsconfig.json,vite.config.ts} $D/
(cd $D && pnpm install --frozen-lockfile && npx vite build)
KS_DIST=$D/dist KS_REPEAT=3 node ui.mjs

# Computer-use tools in process (scenarios 3 and 1 for the accessibility toolset)
(cd rust && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=../../../../target cargo build --offline)
KS_REPEAT=3 ../../../target/debug/ks-probe
```

`NUGGETD=<path>` uses another daemon binary; `KS_KEEP=1` keeps the daemon's temp home. Every
scenario writes `logs/<name>.json` (all reps: replies with timings, agent events, the agent's log
entries via `agent.get`, every request the fake extension saw). `logs/scenarios-run.txt`,
`logs/ui-run.txt`, `logs/ks-probe.txt` are the console output of the runs reported here.

## Scenarios

| Name | What it does | Result at 0d81796 |
|---|---|---|
| `s1_kill_then_approve` | Send confirmation pending; `policy.kill`; then `agent.confirm once` | 3/3: kill RPC 0.6-1 ms, run `halted` ~50 ms later, approve -> `-32001 no open confirmation`, 0 sends, `agent.get` finished/halted with no pending, `policy.approvals` empty. No `confirmation_resolved` event: only `finished` |
| `s1b_browser_stop_then_approve` | same, Stop from the extension (`user_stop` event) | 3/3: run halted in 1.6-2.4 ms, approve refused, daemon halted |
| `s1c_race_kill_approve` | kill and approve pipelined (both orders, and approve then kill 5 ms later) | 0 sends after the kill. Approvals sent with or right after the kill are often **accepted** and logged `approved` then `executed ... interrupted (halted)`. In the 5 ms case the send really happened (just before the kill) but 2/4 runs log it `interrupted (halted)` |
| `s1d_approve_right_after_kill_returned` | await `policy.kill` -> `{halted:true}`, then `agent.confirm` (0.02 ms later), 10x per rep | **accepted 6/10, 8/10, 6/10 (and 8/10 in a rerun)**, `confirmation_resolved: approved` broadcast, log `decision approved` + `executed interrupted`; 0 sends |
| `s2_kill_resume_approve` | kill, resume, approve the stale request (once and for-task) | 3/3 refused, no standing approval left, 0 sends |
| `s3a_kill_during_tool` | kill 200 ms into an `act` the extension holds 1.5 s; two more calls queued in the turn | run halted 42-48 ms after kill; queued calls never sent; **the held action completes ~1.3 s after the kill and is logged `interrupted (halted)`**; no `halt` sent to the extension (overlay left "acting") |
| `s3b_kill_during_model_turn` | kill 0.5 s into a 3 s model turn | 3/3 halted 42-49 ms after kill; the daemon drops the HTTP stream; no further model call or action |
| `s3c_kill_busy_loop` | kill at a random moment in a fast act loop | 3/3: 0 acts after the kill reply (0 between send and reply either); halted 0-1.2 ms after |
| `s4_run_while_killed` | `agent.run` while halted, then resume | 3/3: a task id is returned, task ends `halted` within ~1 ms, 0 model calls, not queued (nothing runs after resume) |
| `s5a_double_approve` | approve twice pipelined, then a third time | 3/3: exactly one accepted, one send |
| `s5b_wrong_task` | approve with another live task's id / an unknown id | 3/3 refused (`no open confirmation`, `unknown agent task`) |
| `s5c_approve_after_cancel` | approve after `agent.cancel` returned; and pipelined with it | after: refused 3/3. Pipelined: **accepted 3/3** but run ends `cancelled`, 0 sends |
| `s5d_approve_after_timeout` | `confirm_timeout_secs = 2`, approve after it fired | 3/3 refused, `confirmation_resolved: timed_out`, 0 sends |
| `s5e_task_approval_and_kill` | approve-for-task, kill during the first send, resume | 3/3: standing approval removed when the task ends, no second send |
| `s7a_ask_user_survives_kill` | task waiting on `ask_user`; Stop; Resume; answer | **3/3: state stays `needs_input` through Stop; after Resume the answer resumes it and it types into the page** |
| `s7b_answer_while_killed` | answer an `ask_user` while halted | 3/3: task ends `halted`, 0 model calls |
| `s8_computer_use_linux` | accessibility `click` on a never-observed id (Send, asks) with kill+approve, and with approve | 3/3: computer-use tools are offered to `agent.run` on Linux by default; confirmation shows target `computer`, label `unknown element e7`; after kill nothing runs; approved without kill: `click` fails `not found`, `observe` fails "AT-SPI: Failed to connect to address 'unix:path=/run/user/0/bus' ... Accessibility permission is not granted; ask the user to grant it." |
| `s9_resume_vs_extension_halt` | kill during a busy browser loop, Resume, new task | 0/30 extension left halted (that path does not hit the race) |
| `s10_extension_left_halted` | approve + kill pipelined, 30x per rep; then Resume in the app and run a fresh browser task | **extension left halted 3/30, 1/30, 1/30**; daemon `halted:false`, yet the next task's `browser_snapshot` returns "Refused: the kill switch is engaged; Nugget has stopped." |
| `ui.mjs` A | Agent page, Send dialog open, `policy.kill` (hotkey/tray path) | 3/3: dialog shows Risk/App/Action/On only, **no recipient or message text**; dialog closes 31-49 ms after the kill; Approve never disabled meanwhile; halted banner shown; 0 sends |
| `ui.mjs` B | keyboard user with Approve once focused presses Enter 0/20/60 ms after the kill | 0 ms: UI sends `agent_confirm`, **accepted 3/3**, logged approved; 20 ms: accepted 1/3, refused 1/3, dialog already gone 1/3; 60 ms: dialog gone 3/3. 0 sends in all 9 |
| `ks-probe` A | accessibility `type_text` (1900 chars, backend 1.5 s), kill 300 ms in | 3/3: run returns `Halted` 5-50 ms after the kill; **the backend keeps typing and finishes ~1.2 s later**; log `executed(type_text) interrupted (halted)` |
| `ks-probe` B | accessibility `click` confirmation; kill; `confirmations().answer(approve)` 0 ms later; 20x per rep | answer accepted 60/60; 0 clicks reached the backend; log sometimes `decision approved` + `executed interrupted` |

## Not exercised

- A real desktop accessibility stack on Linux (no AT-SPI bus, no GTK app here; `Xvfb` exists but no
  `at-spi-bus-launcher`), macOS or Windows backends. In-flight OS actions were shown with a fake
  backend (`rust/`); the real backends run on `spawn_blocking` too (`nugget-computer/src/state.rs`
  `blocking`), and macOS/Windows `press_key` sleep 10 ms between chords inside one call.
- The real browser extension (MV3 in Chromium) and `nugget-browser-host`; the fake mirrors
  `router.ts` semantics only.
- The global Escape x3 hotkey and the tray Stop (desktop shell / `nugget-hotkey`); `policy.kill`
  stands in for them (they call the same `KillSwitch::halt`).
- The Safety page's "Stop all agents" button while a confirmation dialog is open: the dialog is
  modal, so the button cannot be reached until "Decide later"; `keyboard.mjs` already covers the
  button itself.
