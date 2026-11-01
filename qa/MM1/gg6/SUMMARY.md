# MM1 / GG6 re-verify (container scenarios, fake runtime)

Integration head 0b1feab5 (qa/reverify 6d6e67ce); an earlier pass at bd98282e gave the same results.
Debug builds (`nuggetd`, `nuggetd-e2e`, `nuggetd-fake-llama-server` with `fake-runtime`), 3 runs each.
The original harnesses ran from a mirror (qa/ copied) so committed `results*.json` were not overwritten.
Raw outputs: `raw/`.

| Finding | Original | Now (0b1feab5, 3 runs) | Verdict |
|---|---|---|---|
| GG6-1 container (`s9-slow-index.mjs 1000 AC`) | C `{"items":8,"queue":66,…,"ms":240881}`; claim C ~6 s | A 1000 items 8045/8061/8047 ms; C (frozen server) 1000 items, queue 0, 8046/9057/8060 ms | fixed (8 s, A equally slow: debug build, busy box) |
| GG6-2 (`sources.mjs empty,unsupported,chmod000,huge200mb`) | `locked: getting ready to index…` 25.9 s; 200 MB spinning at 91 s; Done "Every step is done." | all 12 cases settle at the first poll (865–890 ms) and say why | fixed; Done step leftover = C1 |
| GG6-3 (hung server, SIGSTOP) | Ask `not done within 90000 ms`, status `ready` | Ask ends 12.96/13.10/12.94/13.11 s: "The on-device chat model stopped responding; restarting it. Try again in a moment."; status `failed`, `retry_in_ms 7997`, restarts | fixed (13 s, not the claimed 9–10 s; consistent) |
| GG6-5 start failure (`runtime-later.mjs` exit-1) | raw dump, "starting qa-chat" ×5, no Retry | fails in 161–165 ms: "The on-device chat model couldn't start: the model server quit (exit status: 1). Open Status to retry or to see its log."; once, with Retry and Status details | fixed |
| GG6-5 crash loop (`qa/MM1/gg6/crashloop.mjs`, server healthy >120 s first) | Ask blocked 32.2 s, 2,007-char error, ~53 relaunches in 9 s | Ask while backing off fails in 35–48 ms (95 chars); relaunches at 1.2/3.3/7.5/15.7/31.8 s then gives up; 10-ask burst 31–46 ms each, 0 launches; `runtime.retry` ready in 205–214 ms | fixed |
| GG6-6 (`downloads.mjs offline,http500,html,server-down`) | offline 100.5 s, HTTP 500 99.7 s, HTML 104.4 s; server-down `"downloaded":0` | offline 4.09–4.18 s; HTTP 500 4.2–4.45 s; captive portal 6–13 ms; server down at 50 %: fails in 4.7–4.9 s with `downloaded 4194304`, resume installs in 684–717 ms | fixed |
| GG6-7 (`byok.mjs` + `qa/MM1/gg6/byok-rotate.mjs`) | `reason:"exists"`, no update/remove | `ai.key.update` bad key → `bad_key`, old key kept; good key used by the next chat without restart; `ai.key.remove` drops it from `providers.list` and `config.toml`; UI Replace key / Remove work; keys in no log or config | fixed |

Harness note: the original `s3-model-crash.mjs` crashes at line 118 (`kill ESRCH`) because the daemon
now kills and restarts the hung server itself (`s3-mm1.patch` wraps it). The crash-loop counter resets
only after 120 s of healthy uptime (`supervisor.rs` `stable_after`), hence `crashloop.mjs`.

## Candidates (Low; 3/3 on 0b1feab5 and on bd98282e)
- **C1**: onboarding Done step says "You're all set" when the only source indexes nothing / can't be
  read (`sources.mjs …`: `doneText` "You're all set … Sources locked …", 12/12). `apps/desktop/src/onboarding/steps.tsx` (~433).
- **C2**: user-facing errors keep the internal "provider error:" prefix ("provider error: The on-device
  chat model couldn't start: …", Chat bubble and `chat.done`). `crates/nugget-core/src/error.rs:14`.
- **C3**: one file, two limits: "Skipped: 1 file larger than 52 MB" (source step, decimal MB of 50 MiB)
  vs "larger than the 50 MB limit" (daemon). `apps/desktop/src/sources/scan.tsx:39` / `format.ts`.

## Commands (mirror `$S/m`; A1/A2/A3_SCRATCH set)
```
NUGGETD=$PWD/target/debug/nuggetd node qa/GG6/a1-onboarding/sources.mjs empty,unsupported,chmod000,huge200mb
TMPDIR=/tmp node qa/GG6/a2-ai-setup/byok.mjs ; TMPDIR=/tmp node qa/MM1/gg6/byok-rotate.mjs
TMPDIR=/tmp node qa/GG6/a2-ai-setup/downloads.mjs offline,http500,html,server-down
TMPDIR=/tmp node qa/GG6/a2-ai-setup/runtime-later.mjs
node qa/GG6/a3-days/s9-slow-index.mjs 1000 AC
node qa/GG6/a3-days/s3-mm1.mjs     # original + qa/MM1/gg6/s3-mm1.patch
node qa/MM1/gg6/crashloop.mjs
```
