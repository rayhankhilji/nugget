# GG6 QA: a new user's first week, on three OSes

Wave GG, package GG6. Branch `qa/first-week`. Scope: install, first run, onboarding, AI setup (local model download and
bring-your-own-key), adding sources, the first search and answer, Double Tap setup and permissions, the tray, updates,
uninstall, and days 2–7 (restart, sleep, model-server crash, full disk, offline, a big folder, left running overnight).

**Builds tested**
- Real-OS runs: installers from release dry run [41236770656](https://github.com/rayhankhilji/nugget/actions/runs/41236770656) of `62b538f` (integration `ec35d39` plus QA files).
  - Linux and macOS: QA run [41246699570](https://github.com/rayhankhilji/nugget/actions/runs/41246699570).
  - Windows: QA run [41246825875](https://github.com/rayhankhilji/nugget/actions/runs/41246825875).
- Explorers and verifiers: the same product code (`62b538f` … `6050a7e`).
- The headline findings were re-checked on integration **`ea3f86c`** (merge `a8d9221`) and again on **`44b9e11`** (merge `33a7ee0`), in the container with debug builds.
  - **Fixed at 44b9e11:**
    - GG6-1: HH2's background triage. With a frozen model, 1,000 files index in 6 s; before, 8 items in 240 s. Not yet re-run on real OSes.
    - GG6-28: EE5 now registers the stop signals first.
    - GG6-18: EE5 says no update request goes out before a channel is chosen in onboarding and every request is in the egress ledger. Code reading only.
  - **Fixed at ea3f86c:** GG6-4, for new configs.
  - **Still reproduce at 44b9e11:** GG6-2, GG6-12, GG6-13, GG6-14, GG6-21 and GG6-31.

**Model and runners.** The real model is `qwen3-4b-q4km`, the manifest's pick for 16 GB. It was downloaded from the signed manifest through onboarding on every OS. Runners:

| OS | Runner | CPU | RAM |
|---|---|---|---|
| Linux | ubuntu-22.04 | 4 vCPU AMD EPYC 9V45 | 16 GB |
| macOS | macos-15-intel | 4 vCPU i7-8700B | 14 GB |
| Windows | windows-2022 | 4 vCPU EPYC 7763 | 16 GB |

All runs are CPU-only. Apple Silicon could not be tested: every `macos-14` job, ours and release.yml's, was cancelled within a second on 2026-10-31 (GG6-27).

## How it was tested

- **Real OS** (`.github/workflows/qa-GG6.yml`, scripts in `qa/GG6/real-os/`):
  - Each job installs the dry-run bundle the way a user would:
    - Linux: `apt install ./Nugget.deb`.
    - macOS: copy from the dmg to /Applications; Gatekeeper's view with the quarantine flag.
    - Windows: NSIS silent per-user install, and the MSI.
  - It launches the installed app, then goes through onboarding:
    - Windows: the **installed app itself**, driven through its WebView2 DevTools port (`run-win-app.mjs`).
    - macOS and Linux: the installed `nuggetd` with the real frontend in Chromium through `apps/desktop/e2e/bridge.mjs` (`run-bridge.mjs`), because WKWebView and WebKitGTK expose no DevTools port.
  - Onboarding steps: choose **On this device**, install the recommended model (a real download), add a synthetic "Documents" folder (24 files: notes, mail, calendar; `qa/GG6/real-os/corpus`), then go through Permissions, Double Tap and Done.
  - Then the first search, and 10 questions in Chat with "Use my sources" (`questions.json`, including one that should be declined).
  - Then days 2–7 (`days.mjs`):
    - restart;
    - kill the model server while idle and mid-answer;
    - SIGSTOP for 180 s with a note written meanwhile;
    - 40 idle minutes sampled every 30 s;
    - a 20,000-file folder;
    - the 10 questions again;
    - quit, then uninstall.
  - Linux also runs the model download onto a 700 MB disk.
  - Step times and screenshots are in the run artifacts. A compact copy is in `qa/GG6/real-os/results/`.
- **Explore layer**: four areas, each explored by its own subagent in the cloud container with debug builds. Scripts and results are in `qa/GG6/`:
  - A1 onboarding and first search: `a1-onboarding/`, 33 flow checks and 13 folder mistakes.
  - A2 AI setup: `a2-ai-setup/`, 71 scenarios with a signed test manifest, a mock download server, a mock OpenAI server and the fake llama-server.
  - A3 days 2–7 at daemon level: `a3-days/`, 10 scenarios.
  - A4 shell lifecycle: code reading with file:line references. The Tauri shell does not build in the container.
- **Verification layer**: two fresh subagents re-ran every High or Blocker finding from its written repro alone, on the build named above. The verdicts and quoted output are in `qa/GG6/verification.md`. In the findings below:
  - **V** = reproduced by the verifier;
  - **R** = also seen in the real-OS runs;
  - **C** = code reading only (not reproduced; kept because the evidence is precise).
  - Nothing that failed to reproduce is reported.

A harness limit to know about: the e2e bridge times out a daemon call after 60 s and then shows "lost the connection to Nugget" (`apps/desktop/e2e/bridge.mjs:229`). The real shell has no such limit. The real-OS runner stretches it, and no finding below comes from it.

## Verdict

| Capability | Verdict | Evidence |
|---|---|---|
| Install | **Works with caveats** | NSIS silent per-user install in 4.4 s; `apt install` of the .deb in 14.6 s (dependencies resolved); dmg copy in 5 s. Dry-run installers are unsigned (Authenticode `NotSigned`; macOS ad-hoc, and `spctl` says `rejected` when quarantined). That is expected for a dry run, but real releases depend on EE5's signing (`real-os/results/*/signature.txt`, `gatekeeper.txt`). |
| First run, onboarding | **Works with caveats** | The real app shows Welcome: the Windows app page was up 8.7 s after launch, with the daemon ready 3.5 s later; the Linux window took 0.5 s. A1 flows: 27 of 33 checks pass, 9 of 13 folder mistakes are handled. Resume works on every step (6/6). Broken: GG6-2, GG6-12, GG6-13, GG6-21. |
| AI setup: local model | **Works with caveats** | The real 2.5 GB download installed on 3/3 OSes (38 s / 47 s / 41 s on runner bandwidth). The 16 GB pick is right. Verification checks the SHA, and a full disk is refused before any byte is downloaded. Broken: failure paths (GG6-5, GG6-6), no chat model on 4 GB (GG6-8), no time estimate (GG6-16). |
| AI setup: bring your own key | **Works with caveats** | Key and URL checks classify correctly. The key never reaches config.toml or the logs. A saved key can't be changed or removed (GG6-7), needs a restart nobody explains (GG6-15), and fails raw on Linux without a Secret Service (GG6-11). |
| Adding sources | **Broken with a local model on the tested build; fixed at 44b9e11 (container check; the real-OS confirmation is still to do)** | Without a model, 141 files are searchable in 1.0 s and a 20k-file messy folder settles in 62 s (debug build). **With the recommended model installed, 24 files took 7.9 min (Linux), 9.8 min (macOS) and more than 10 min (Windows, 22 of 24), and a 20k-file folder indexes 4 to 9 items per minute** (GG6-1). Folders with nothing indexable spin forever (GG6-2). |
| First search and answer | **Works** | Search found the dentist note in the top 3 on 3/3 OSes; A1's day-one questions scored 10/10 hit@3 with no model. Chat with the real model: **60/60 correct answers** (10 questions × 2 days × 3 OSes), including declining the unanswerable question. The expected source was among the citations in 6 of 9 per OS. Median answer time 11 s on Linux, 20 s on macOS Intel and 71 s on Windows; the slowest took 175 s. |
| Double Tap setup and permissions | **Works with caveats** (Double Tap itself is GG1's) | Every permission is explained before the OS asks. macOS Accessibility can't be recovered in the app once denied (GG6-19, C). |
| Tray | **Not exercised** (C) | The installer smoke tests run with `NUGGET_TRAY=off`. On GNOME, closing the window quits the app while docs point to the tray (GG6-20, C). |
| Updates | **Not exercisable on dry runs** (C) | Unkeyed builds say they can't update. After an update on macOS or Linux the old daemon keeps serving the new UI (GG6-10, C). Update checks contact GitHub by default, outside the egress ledger (GG6-18, C). |
| Uninstall | **Works with caveats** | Removing the app leaves data behind: `~/Nugget Vault` on Linux and macOS, plus `~/.config/dev.nugget.desktop` and `~/.local/share/dev.nugget.desktop` on Linux (R). Gigabytes of models and the store are never offered for removal (GG6-9). Windows uninstall and the MSI could not be determined: both Windows jobs lost their runner in the MSI step and the logs return 404. |
| Days 2–7 | **Works with caveats** | Restart: ready in 0.4 / 0.6 / 4.3 s with the index intact, but the first answer then takes 21 s / 41 s / **169 s** to start (cold model load on CPU). Model-server crash while idle: recovered on 3/3. SIGSTOP 180 s: a note written meanwhile was searchable 9 s (Linux) and 33 s (macOS) after resume. Idle for 40 min: nuggetd 16–50 MB RSS and 0.03–0.08 % CPU, and the model server is freed after the 30 min idle stop. Broken: a hung model server (GG6-3), crash loops (GG6-5), quit leaves everything running (GG6-8), the big folder (GG6-1). |

## Findings

Most severe first. Each finding gives its source area id (A1–A4 are the explorer areas). Owners are crate or file, plus
the wave package that seems closest today; the orchestrator should confirm the routing.

### GG6-1 · Blocker · With a local model installed, indexing runs at LLM speed (one serialized triage call per file) · V, R 3/3 · **fixed at 44b9e11** (HH2, container re-check)
- **Repro (container)**: `node qa/GG6/a3-days/s9-slow-index.mjs 1000 AC`.
  - A: no model.
  - C: a local model whose server is frozen.
- **Repro (real)**: `qa-GG6.yml`. Onboarding with "On this device", then add `qa/GG6/real-os/corpus` (24 files).
- **Expected**: adding a folder makes it searchable in seconds; model work happens in the background.
- **Actual (container)**: A `{"items":1000,…,"ms":5022}`; C `{"items":8,"queue":66,"states":"files-1:syncing:8","ms":240881}`. Exactly 1 item per 30 s, and nothing logged at warn.
- **Actual (real OS, qwen3-4b)**: the onboarding source step went from "indexing…" to "Search works now" in **474 s (Linux) and 589 s (macOS)**. On Windows it **timed out at 600 s with `Jordan: indexing, 22 items so far…`**.
  - A 20,000-file folder then indexed at **9.2 / 3.9 / 3.8 items per minute** (Linux / macOS / Windows), with `queue_depth` stuck at 66. Extrapolated, that is 1.5 to 3.6 days.
  - The same 24 files take under 1 s when "Decide later" is chosen.
- **Cause**: ingest stage 4 calls the Triage-tier chat model inline before each item is stored, one at a time, with a 30 s timeout and up to 6,000 characters per prompt.
  - `crates/nugget-ingest/src/pipeline.rs:489-516`
  - `crates/nugget-ingest/src/config.rs:87-89`
  - failures are logged at debug only: `pipeline.rs:512`
- **Fix**: store and index first, then triage from a background queue like knowledge extraction; or skip triage in backfills. Report pending triage in `ingest.status`, and warn on timeouts.
- **Owner**: `nugget-ingest`.
- **Status at 44b9e11**: HH2 (`08a0cce`) moved triage to the background (`crates/nugget-ingest/src/triage.rs`). `s9-slow-index.mjs 1000 AC` now gives A `{"items":1000,…,"ms":7050}` and C (frozen model) `{"items":1000,…,"queue":0,…,"ms":6029}`.
- **Still to do**: confirm with the real model by dispatching `qa-GG6.yml` on a dry run of the new head. The onboarding "First source" step and the 20k-folder rate are the numbers to compare.

### GG6-2 · High · Folders with nothing indexable spin on "getting ready to index…" forever, and setup says Done · V 4/4, still on ea3f86c
- **Repro**: `A1_SCRATCH=/tmp/a1 node qa/GG6/a1-onboarding/sources.mjs empty,unsupported,chmod000,huge200mb`
- **Expected**: the step settles and says why nothing was indexed (no supported files / permission denied / a file over 50 MB skipped), with a next step.
- **Actual**: `waiting after 25877ms: locked: getting ready to index…`, and still spinning after 91 s for the 200 MB file.
  - The source reads `{"state":"watching","items":0,"last_error":null,"health":{"code":"ok","reason":"Up to date."}}`, even when the folder is unreadable (`qa/GG6/a1-onboarding/shots/20-src-chmod000.jpg`).
  - The Done step says "Every step is done."
- **Cause**:
  - `apps/desktop/src/onboarding/steps.tsx:80` treats 0 items as "waiting".
  - `crates/nugget-connector-files/src/scan.rs:328` (unreadable root) and `:354` (over `max_file_bytes`) only log.
- **Fix**: the connector reports scan counts (seen, unsupported, too big, denied) and sets `health` when a root can't be read. The UI treats "watching with 0 items after a finished scan" as settled, with its own message.
- **Owner**: `nugget-connector-files` (FF2), onboarding UI (EE6).

### GG6-3 · High · A hung local model server blocks Ask/Chat for minutes while status says Ready · V
- **Repro**: `node qa/GG6/a3-days/s3-model-crash.mjs` (step "hung": SIGSTOP the llama-server).
- **Expected**: a short timeout, a restart, and "the model server stopped responding".
- **Actual**: `retrieve.ask: not done within 90000 ms; … 0 notification(s)`, while `runtime.status` reads `{"state":"ready","error":null}` in every sample. By code reading the wait is 600 s × 3 attempts. Seen 2/2.
- **Cause**:
  - The supervisor only checks for process exit: `crates/nugget-runtime/src/supervisor.rs:1253`.
  - `provider_config` (`:1071-1117`) sets no `idle_timeout_secs`, so the read timeout defaults to 600 s (`crates/nugget-infer/src/provider.rs:174`).
- **Fix**: a first-token timeout for `local-*`, a `/health` watchdog while ready, and a human message.
- **Owner**: `nugget-runtime`, `nugget-infer` (Z3 / AA1 lineage).

### GG6-4 · High (fixed for new installs at ea3f86c) · The default config enabled an Ollama provider at 127.0.0.1:11434 that users never set up · V, R
- **Repro**: start `nuggetd --home <empty dir>` (no config), then `providers.list` and `retrieve.ask`.
- **Actual at ec35d39**:
  - `config.toml` had `[[providers]] id = "local" … base_url = "http://127.0.0.1:11434/v1"`. `nuggetd ready … "providers":1,"unavailable":0`.
  - Every ask retried it twice (about 1.9 s) before falling back.
  - In the real runs, a crash mid-answer surfaced `retrieve.ask failed: local: connection failed: error sending request for url (http://127.0.0.1:11434/v1/chat/completions)` on Linux and macOS.
  - On the earlier `5f1998e` Linux run, the first answer after a restart failed the same way.
- **Status**: `ea3f86c` comments the provider out (`crates/nuggetd/src/config.rs:98-105`).
- **Remaining**: configs written by earlier builds keep it. Also, `local-chat` only becomes routable once its server is ready (`crates/nuggetd/src/local.rs:386`), so any configured on-device provider is tried first right after a restart.
- **Fix**: a config migration that comments out an untouched default Ollama block; publish `local-chat`'s capabilities at startup.
- **Owner**: `nuggetd` config / local runtime.

### GG6-5 · High · A model server that won't start shows a raw, repeated dump; a crash loop blocks 32 s and then starts a relaunch storm · V
- **Repro, start failure**: `TMPDIR=/tmp node qa/GG6/a2-ai-setup/runtime-later.mjs`
  - `chat failed: llama-server exited during start-up (exit status: 1); last output: starting qa-chat on /tmp/nugget-run-…/chat.sock (ctx 1x8192, ngl 0, accel cpu) | fake: exiting immediately with 1 (code -32003)`
  - "starting qa-chat" appears **5 times** in the Chat alert, with no Retry (`qa/GG6/a2-ai-setup/shots/runtime-exit1-chat.jpg`).
- **Repro, crash loop**: the crash-loop step of `s3-model-crash.mjs`, started from a healthy server (the verifier's targeted run).
  - Ask blocked **32.2 s**, then returned a **2,007-character** error with 5 nested `start failed:`. `status.providers[].error` is 1,988 characters.
  - After give-up, every request starts a new launch with no backoff: about 53 launches in 9 s during a knowledge run.
- **Cause**:
  - `crates/nugget-runtime/src/health.rs:159-161` puts the log tail, which includes the supervisor's own notes (`supervisor.rs:1204`), into the error.
  - `wait_ready_state` (`:684-724`) waits out the backoff.
- **Fix**: keep the server's stderr separate; a short message with "Retry" and "See log"; fail fast while backing off; back off after give-up too.
- **Owner**: `nugget-runtime`; Chat error UI (EE6).

### GG6-6 · High · Model downloads that fail sit at "Downloading" for about 100 s, then show a raw error · V 4/4
- **Repro**: `TMPDIR=/tmp node qa/GG6/a2-ai-setup/downloads.mjs offline,http500,html,server-down`
- **Actual**:
  - offline: 100.5 s, then `network error: download failed: error sending request for url (http://127.0.0.1:41195/<rev>/qa-offline.gguf)`
  - HTTP 500: 99.7 s
  - captive-portal HTML: 104.4 s, then `connection closed after 53 of 8388608 bytes`
  - server dies at 50 %: the failure event reports `"downloaded":0`
- A3 saw the same offline against Hugging Face: 0 bytes for 97.9 s, then a raw URL dump.
- **Fix**: classify DNS and connect failures as "You're offline", fail fast, and keep the staged byte count in the failure.
- **Owner**: `crates/nugget-models/src/download.rs:563-580`; onboarding model card (EE6).

### GG6-7 · High · A saved API key can't be changed or removed · V
- **Repro**: `TMPDIR=/tmp node qa/GG6/a2-ai-setup/byok.mjs` (rotate-key case).
- **Actual**: saving again gives `{"ok":false,"reason":"exists","message":"A provider named \"custom\" is already set up. Pick another name."}`.
  - The daemon only has `ai.key.save`, `ai.key.saved` and `ai.key.test`.
  - The Models page has no key form.
- **Expected**: replace the key, or remove the provider, from the app.
- **Fix**: `ai.key.update` / `ai.key.remove`, and a "Your providers" list with Replace key / Remove.
- **Owner**: `nuggetd` handler (ai.key), `ByokForm.tsx`, Models page.

### GG6-8 · High · Quitting Nugget leaves the daemon and the model server running · R 3/3, C
- **Repro**: `qa-GG6.yml`.
  - macOS: Quit, then `pgrep`.
  - Linux: close the window, then `ps`.
  - Windows: close the window, then `Get-Process`.
- **Actual**:
  - macOS: `/Applications/Nugget.app/Contents/MacOS/nuggetd` still running after Quit.
  - Linux: `nuggetd` still running after the window closed.
  - Windows: `llama-server (3298 MB), nuggetd (17 MB), nugget-desktop (20 MB) keep running` (`real-os/results/*/after-*.txt`, `win/summary.json`).
- **Cause**:
  - "Quit Nugget" is `app.exit(0)` (`apps/desktop/src-tauri/src/tray.rs:87`).
  - The daemon is spawned detached (`daemon.rs:351-371`).
  - There is no shutdown IPC.
- **Fix**: `nuggetd stop` / a shutdown method used by Quit, or an explicit "Keep Nugget running in the background" choice shown once.
- **Owner**: desktop shell (W11 / EE6); nuggetd.

### GG6-9 · High · Uninstall silently leaves gigabytes of models, the store, the keychain entry and `~/Nugget Vault` · R (partial), C
- **Evidence**:
  - `apt remove` and `apt purge` left `~/.config/dev.nugget.desktop`, `~/.local/share/dev.nugget.desktop` and `~/Nugget Vault`.
  - macOS drag-to-Trash left `~/Nugget Vault` (`real-os/results/*/uninstall.txt`).
  - By code reading: NSIS hooks only remove the service and kill nuggetd (`packaging/windows/installer-hooks.nsh:23-31`); deb/rpm have no maintainer scripts; MSI has no hooks.
  - The NSIS install dir `%LOCALAPPDATA%\Nugget` contains the data dir `%LOCALAPPDATA%\Nugget\Nugget\data`.
- **Fix**: an uninstaller option to remove models and data, and a per-OS "Remove Nugget completely" doc page that names every path.
- **Owner**: packaging (EE5), docs.

### GG6-10 · High (C) · After an update on macOS or Linux the old daemon keeps serving the new UI until reboot
- **Cause**:
  - `Daemon::start` accepts whatever daemon already answers (`apps/desktop/src-tauri/src/daemon.rs:227-231`).
  - Only `PROTOCOL_VERSION` is compared (`src/pages/Status.tsx:72,80`).
  - The update path never restarts nuggetd (`updater.rs:317-329`).
- **Fix**: compare app and daemon versions on connect, and restart the daemon around updates.
- **Owner**: desktop shell / updater (EE5).
- **Check**: not reproducible on dry runs (no updater key). Needs an N→N+1 rehearsal.

### GG6-11 · Medium · BYOK save on Linux without a Secret Service fails with a raw error, and there is no `env:` fallback · V (partly)
- **Actual**: `ai.key.save failed: keychain: Platform secure storage failure: no secret service provider or dbus session found (code -32004)`.
- The verifier reproduced this over IPC. The form renders the error unchanged (`ByokForm.tsx:138-139,252`).
- **Fix**: explain the problem and offer an environment-variable or file fallback.
- **Owner**: `nuggetd` ai.key, `ByokForm.tsx`.

### GG6-12 · Medium · The source step forgets the added folder after Back, quit-and-reopen, or "Run setup again" · V 3/3, still on ea3f86c
- **Repro**: `A1_SCRATCH=/tmp/a1f node qa/GG6/a1-onboarding/flows.mjs back,resume,setup-again`
- **Actual**: before Back the step shows `back-small: 1 item indexed. Search works now.`; after Back it shows no rows and "Add a folder" again.
- **Cause**: `apps/desktop/src/onboarding/steps.tsx:89` (component state only).
- **Fix**: list existing sources with live progress.
- **Owner**: onboarding (EE6).

### GG6-13 · Medium · Adding the same folder again creates a duplicate source and duplicate results · V, still on ea3f86c
- **Repro**: `flows.mjs resume`
- **Actual**: `files-1` and `files-2`, both "resume-src" with 40 items each; Status says 80 items; search returns `n7, n7, …`. No warning is shown.
- **Cause**:
  - `crates/nuggetd/src/handler.rs:618` doesn't compare against other sources.
  - `crates/nugget-connector-files/src/config.rs:450` only checks within one source.
- **Fix**: refuse, or say "already added as X", for equal or overlapping roots.
- **Owner**: nuggetd sources (FF2).

### GG6-14 · Medium · Ask on Search with no AI shows a red internal error and no next step · V, still on ea3f86c
- **Actual**: `no provider is available for the synthesis tier`, with only "Ask again". On a real first run the text is `local model qwen3-4b-q4km is not installed` instead.
- Chat handles the same state well ("No models are set up yet… Go to Models").
- **Cause**: `apps/desktop/src/pages/Search.tsx:291` shows `errorMessage(err)` unchanged; the message comes from `crates/nuggetd/src/handler.rs:409`.
- **Fix**: reuse Chat's copy and button.
- **Owner**: Search page (EE6).

### GG6-15 · Medium · A saved key does nothing until "Restart Nugget", and the user isn't told how to restart · V
- **Actual**: `"Key checked and saved. Restart Nugget to start using it.","restart_required":true`; `providers.list` right after saving returns `[]`.
- Quitting does not restart the daemon (GG6-8), so the instruction doesn't work as a user would expect.
- **Fix**: hot-add the provider, or a "Restart now" button.
- **Owner**: nuggetd providers.

### GG6-16 · Medium · No download-time estimate, and Welcome promises "about two minutes" · R 3/3
- **Actual**: the model card says `2.5 GB download · License: Apache-2.0 · Needs 8 GB…` with no time.
  - Welcome says "Setup takes about two minutes."
  - Measured setup with the recommended model: 9 to 11 minutes on runners, mostly GG6-1. At 10 Mbit/s the download alone takes about 33 min.
- **Fix**: show an estimate from measured throughput, and drop the fixed claim.
- **Owner**: onboarding (EE6).

### GG6-17 · Medium · A model crash mid-answer gives a raw transport error and no retry · A3 4/4, R
- **Actual**: `retrieve.ask failed: local-chat: request failed: error sending request for url (http://localhost/v1/chat/completions): client error (SendRequest) (code -32003)`. The server is back about 1.4 s later.
- **Fix**: retry once when nothing has streamed yet; use a human message.
- **Owner**: `nugget-infer`, Ask/Chat handlers.

### GG6-18 · Medium (C) · Update checks contacted GitHub by default, outside the egress ledger · **addressed at 44b9e11** (EE5: no request before a channel is chosen in onboarding, and every request is in the ledger; code reading, not run)
- **Evidence**: Stable channel, first check 60 s after launch (`apps/desktop/src-tauri/src/updater.rs:33-39,370-387`). This contradicts `docs/book/src/privacy.md:7-8` ("Nothing is sent to the Nugget project, ever").
- **Fix**: record update checks in the ledger, list them on Welcome's "Leaves only when you choose" and in the privacy doc, or ask during onboarding.
- **Owner**: updater (EE5), docs.

### GG6-19 · Medium (C) · macOS: once Accessibility is denied, the app can't help the user recover
- **Evidence**:
  - The button only re-calls the AX prompt (`src-tauri/src/doubletap/commands.rs:100-118`).
  - `x-apple.systempreferences:` is refused by the URL allowlist (`open.rs:311`).
  - Input Monitoring always shows "unknown" (`permissions.rs:159-165`).
  - `doubletap.hint.macos_permissions` is never used.
- **Fix**: an allowlisted deep link to the Privacy panes, and a specific Double Tap error.
- **Owner**: Double Tap shell (II3 / W1).

### GG6-20 · Medium (C) · On stock GNOME (no tray) closing the window quits the app while the daemon keeps running, yet docs send users to the tray
- **Evidence**: `tray.rs:177-201`, `main.rs:160,201-204`, `docs/book/src/safety.md:4,86`, `troubleshooting.md:106`, `en.json:549,1653`.
- **Fix**: expose `has_tray` to the UI, and put "Stop all agents" in the window.
- **Owner**: desktop shell.

### Low

| ID | Finding | Evidence | Owner |
|---|---|---|---|
| GG6-21 | "Decide later" is reported as a skipped step: "You skipped one step. Go back to: Choose your AI" | V; `machine.ts:76` `stepGoalMet` | EE6 |
| GG6-22 | Folder-form errors are daemon jargon (`root \`~\` must be an absolute path`, `roots … overlap`); `~` passes the form's check | A1 5/5; `AddSourceWizard.tsx:234,167`, `connector-files/config.rs:459,468,551` | EE6, FF2 |
| GG6-23 | The first answer after a reboot starts after 21 s / 41 s / 169 s (Linux / macOS / Windows) with no "warming up" state | R; `real-os/results/*/summary.json` `days.restart` | runtime |
| GG6-24 | "Write-ahead log has an uncommitted or torn tail (a crash)" after every clean restart, and a 4.2 MB WAL is left behind | V 4/4; `nugget-store/src/integrity.rs:262-268` | FF1 |
| GG6-25 | Full disk: no `status` signal; the source error lags 23 s; the daemon log is cut mid-line. It recovers by itself and nothing is corrupted | A3 1/1 (not re-verified) | FF1 |
| GG6-26 | Progress `total` is always null, and files over 50 MB or with unsupported extensions are skipped with no trace | A3 | FF2 |
| GG6-27 | Release and QA jobs on `macos-14` were cancelled within 1 s on 2026-10-31 (release run 41236770656), so no Apple Silicon bundle was built | R 2/2 | EE5 |
| GG6-28 | Windows: nuggetd exited 0xC000013A without logging "stopped" when the NSIS smoke test sent CTRL_BREAK (release dry run 41073117904 on 5f1998e); it passed on 62b538f; fixed at 44b9e11 (EE5 registers the stop signals first) | R 1/2 | packaging / nuggetd |
| GG6-29 | Monotonic-clock timers (proactive refresh, knowledge interval, idle stop) don't count time spent in system sleep | C; `proactive.rs:870-889`, `knowledge.rs:192-196` | nuggetd |
| GG6-30 | Download cannot be cancelled. A resume after a cancel re-downloads from byte 0 (6.3 MB staged, 16.8 MB fetched again) | A2 | nugget-models |
| GG6-31 | No AI option is preselected although one is badged "Recommended" | V | EE6 |
| GG6-32 | The installed shell writes no log file; the spawn error says "Check its log" with no path | C; `main.rs:4,32-36`, `daemon.rs:254` | shell |

## Friction log (ranked by how many new users hit it)

| Who | Moment | Finding / evidence |
|---|---|---|
| **All who pick a local model** (until 44b9e11 ships) | After installing the model, the "First source" step crawls: 8 to 10 minutes for 24 files; a real Documents folder takes days | GG6-1 |
| All | Welcome says setup takes about two minutes; with a local model it takes about ten, plus the download, whose time is never shown | GG6-16 |
| All | Quit or close: nuggetd keeps running; on Windows the model server keeps 3.3 GB | GG6-8 |
| All on Windows/macOS releases | The dry-run installer is unsigned: SmartScreen and Gatekeeper ("rejected") block it | install row |
| Most who skip AI | Ask on Search shows a red internal error; "Decide later" is called a skipped step | GG6-14, GG6-21 |
| Most local-model users, every reboot | The first answer after a restart takes 20 s to nearly 3 min with nothing saying "loading the model" | GG6-23 |
| Most BYOK users | The key is "saved", but nothing works until a restart the app can't do; a wrong key can't be replaced | GG6-15, GG6-7 |
| Most uninstallers | Gigabytes of models and `~/Nugget Vault` stay behind with no prompt | GG6-9 |
| Some | Wrong folder (images, zips, unreadable): spins forever, then "Every step is done" | GG6-2 |
| Some | Back to the source step: it looks empty, so they add the folder again and get duplicate results | GG6-12, GG6-13 |
| Some | Offline or behind a captive portal: "Downloading" for about 100 s, then a URL dump | GG6-6 |
| Some (4 GB machines) | "On this device · Recommended" offers no chat model and doesn't mention a key | GG6-8 (A2) |
| Some (Linux without a keyring) | Saving an API key fails with "Platform secure storage failure" | GG6-11 |
| Few | Model server hangs (wake from sleep, swapping): Chat waits minutes while Status says Ready | GG6-3 |
| Few | Model server won't start: a wall of repeated text, no Retry | GG6-5 |

## What works well (protect these)

- **Real-model answers are right.** 60/60 correct across 3 OSes and 2 days. The unanswerable question was declined every time ("The provided passages do not contain any information…"). Citations render as `[1]` buttons with Open, in the installed Windows app (`qa/GG6/real-os/results/shots/win-12-chat_day1-first-answer.jpg`).
- **The real model install is reliable.** 3/3 real downloads of 2.5 GB in 38–47 s on runner bandwidth, with SHA-256 verification. A full disk is refused before any byte is downloaded (`insufficient disk space: need 3820750105 bytes, 733511680 available`), and the daemon stays healthy. A dropped connection resumes, a daemon restart resumes from a checkpoint, and a SHA mismatch never activates the model.
- **Search without any model is fast and good.** 141 files searchable 1.0 s after adding them; 10/10 hit@3 across PDF, DOCX, .eml and .ics. A whole home folder skips node_modules, dot-folders and Trash. A 20k-file messy folder with a symlink loop settles in 62 s with no model (debug build). Search p95 stays at 3–4 ms while indexing on all three OSes.
- **Crash safety and restarts.** SIGKILL mid-index loses nothing and adds no duplicates. Restart is ready in 0.2–4.3 s with identical results. A killed model server comes back in about 1.4 s, with exponential backoff and a give-up. The file watcher catches up 0.8 s after a 3-minute freeze.
- **Idle cost is low.** Over 40 min on each OS (release builds): nuggetd 16–50 MB RSS and 0.03–0.08 % CPU, with the store, WAL and logs flat. The model server is unloaded after the 30 min idle stop on all three OSes.
- **Onboarding resumes** on the step where the user quit (6/6). Permissions are explained before any OS prompt. The Welcome page states plainly what stays and what leaves (`qa/GG6/real-os/results/` and the macOS native screenshot in the run artifacts).
- **BYOK validation.** Bad key, wrong URL and no network are told apart; whitespace and trailing slashes are trimmed; plain http is refused except for localhost; the key never reaches config.toml or the logs.
- **Installers.** NSIS per-user install needs no admin and takes 4.4 s; the .deb installs with apt's dependency resolution; the AppImage and NSIS smoke tests pass on 62b538f.

## Files

- `.github/workflows/qa-GG6.yml`: dispatch-only. Takes a release dry-run id; runs Linux, macOS (Intel) and Windows.
- `qa/GG6/real-os/`:
  - `journey.mjs` (onboarding, search, chat)
  - `days.mjs` (days 2–7)
  - `run-bridge.mjs` (macOS/Linux)
  - `run-win-app.mjs` (the installed Windows app over CDP)
  - `corpus/`, `questions.json`
  - `results/`: per-OS summaries, native install, quit and uninstall output, one screenshot
- `qa/GG6/a1-onboarding/`, `a2-ai-setup/`, `a3-days/`: explorer scripts and compact results. Run instructions are in each script's header and in `a1-onboarding/README.md`. The A1–A4 findings texts are merged into this report; the area ids are cited above.
- `qa/GG6/verification.md`: the verification layer's verdicts with quoted output.
