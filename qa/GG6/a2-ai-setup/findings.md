# A2 AI setup: QA findings (qa/first-week @ 62b538f, debug build 2026-10-31)

Scripts (run from repo root; need `target/debug/nuggetd{,-e2e,-fake-llama-server}`, root for tmpfs; JSON goes to `qa/GG6/a2-ai-setup/results/`):
`node qa/GG6/a2-ai-setup/recommend.mjs` · `node qa/GG6/a2-ai-setup/downloads.mjs [case,...]` · `node qa/GG6/a2-ai-setup/byok.mjs` · `node qa/GG6/a2-ai-setup/runtime-later.mjs`. All traffic is localhost: a fault-injecting mock download server, the real manifest's entries re-signed with a throwaway key, a mock OpenAI/Anthropic API, and wrapper scripts around the fake llama-server.

## 1. Results (71 scenarios; all ran on the new build)
| Area | N | Outcome (time) |
|---|---|---|
| Recommendation 5 RAM x 3 GPU (IPC) + UI at 4/8/16 GB | 15+3 | ran, 12 s. GPU only changes the reason text, never the pick |
| Downloads: baseline, drop once, stall, server down, daemon restart, cancel, SHA mismatch, 404, 500, offline, captive-portal HTML, disk preflight, disk full | 13 | baseline 4.4 s/8 MiB. Self-healed: drop once (5.0 s, Range resume), stall (64.8 s), restart (resumed from a checkpoint). Failed as expected: 404 (46 ms), SHA (4.4 s, staging deleted), preflight (3 ms). Too slow to fail: offline 108 s, 500 106 s, HTML 103 s, server down 107 s. Disk full failed in 1.6 s and installed after space was freed |
| BYOK `ai.key.test` (OpenAI-compatible, Anthropic, OpenAI) | 18 | all give the right `reason`. The slow server answers `no_network` at 10.0 s |
| BYOK save, rotate, bad name, restart, keychain on, leak scan | 6 | the key never reaches config.toml, logs or disk. Rotating the key and keychain-on both fail (F1, F2) |
| BYOK through the onboarding UI | 7 | each answers in about 0.28 s (slow: 10.25 s). Messages lose detail (F5) |
| Runtime: exits 1 / crashes after 300 ms / takes 120 s to load | 3 | crash: recovers. Slow load: the first answer takes 120 s. Exit 1: raw error dump (F7) |
| Decide later: Chat, Ask, draft (IPC + UI), Models, Double Tap, Run setup again | 6 | see F10 |

What changed after the merge: the onboarding model list was reworked. The old build badged every model "Recommended", including the embedding model (whose card said "answers come more slowly"), and showed all 3 cards on a 16 GB machine. The new build groups models into chat and search and badges one chat model. Daemon download and BYOK results are byte-for-byte the same in the old and new runs.

## 2. Findings
**F1 High: the user cannot change or remove a saved API key.** `byok.mjs`. Saving again under the same name returns `reason: exists` ("A provider with this name is already set up. Choose another name."; 1/1). No UI or IPC method edits or deletes a provider. The Models page has no BYOK form, so a revoked key means editing config.toml and the keychain by hand. Code: `crates/nuggetd/src/features/ai_keys.rs:231,553`, `apps/desktop/src/pages/Models.tsx`. Fix: add `ai.key.update`/`ai.key.remove` (overwrite the keychain entry, drop the provider table) and add the form to Models.

**F2 High: BYOK cannot be saved on Linux without a Secret Service, and nothing else is offered.** `byok.mjs` (keychain-on case, 1/1): `ai.key.save failed: keychain: Platform secure storage failure: no secret service provider or dbus session found (code -32004)`. The form shows this raw text. Code: `ai_keys.rs` `write_provider` → `secrets.set`. Fix: catch this error, explain it, and offer `env:VAR` (or an encrypted file) as the fallback.

**F3 Medium: with `NUGGET_KEYCHAIN=off`, a saved key is lost on restart, and the save message asks for a restart.** `byok.mjs`. Before the restart, `providers.list` → `[]`. After the restart, `custom` has the error `keychain dev.nugget.Nugget/provider:custom: Platform secure storage failure…` and `chat` returns `no provider configured for tier Light` (1/1). Save writes to `MemorySecrets` (`crates/nuggetd/src/daemon.rs:417`), but providers read the OS keychain directly (`crates/nugget-infer/src/config.rs:149`). So the e2e byok scenario proves the save, never that the key can be used. Fix: resolve `keychain:` through the daemon's `SecretStore`, or keep the key-file store when the keychain is off.

**F4 Medium: a saved key does nothing until "Restart Nugget".** `byok.mjs`: `providers.list` is `[]` right after a successful save. The new user isn't told how to restart the background service, and until they do, Chat keeps saying "No models are set up yet". Fix: hot-add the provider (rebuild the router) and drop `restart_required`.

**F5 Medium: the BYOK form shows a generic message instead of the daemon's specific one.** `byok.mjs` UI cases, 5/5. `ByokForm.tsx:150-156` maps `reason` to fixed strings and drops `message`:
| Input | Daemon message | UI shows |
|---|---|---|
| `http://192.0.2.10/v1` | "Use an https:// address…" | "Nothing that looks like an AI provider answered at this address…" |
| key with an inner space or newline | "The key contains spaces… Copy it again." (no request sent) | "The provider rejected this key." |
| model name typo | "…not in the provider's list; check its name." | "Key works. Found 2 models." (then saves the typo) |
| 429 | "busy or out of quota" | "had a problem answering" |
| 30 s server | "did not answer in time" | "Could not reach the provider" |

Fix: show `message`, or add more reason codes (`https_required`, `malformed_key`, `unknown_model`, `rate_limited`, `timeout`).

**F6 High: offline from the start, HTTP 500, a captive portal or the server going down mid-download all sit on "Downloading" for 103 to 109 s, then fail with raw text.** `downloads.mjs offline,http500,html,server-down` (4/4). The errors read `network error: download failed: error sending request for url (http://127.0.0.1:43127/<rev>/qa-offline.gguf)` and `…connection closed after 53 of 8388608 bytes` (a 53-byte HTML login page). The failure event also reports `downloaded: 0` when 50% is staged. Code: `crates/nugget-models/src/download.rs:63-65` (8 retries, 30 s cap) and `:621`. Fix: fail or warn after about 15 s with no bytes, show "Retrying (n/8)…", treat a `text/html` 200 as "your network needs a sign-in", and map errors to plain words.

**F7 High: when the local runtime fails, Chat shows a raw dump.** `runtime-later.mjs` (exit-1, 1/1). The red note repeats "llama-server exited during start-up (exit status: 1); last output: starting qa-chat on /tmp/nugget-run-…/chat.sock (ctx 1×8192, ngl 0, accel cpu)…" five times. Ask and draft fail the same way. There is no Retry or next step, and the assistant bubble stays empty (screenshot `shots/runtime-exit1-chat.jpg`). Code: `crates/nugget-runtime/src/health.rs:161`, `apps/desktop/src/pages/Chat.tsx:612`. Fix: say "The local model couldn't start" with Retry and Details, and de-duplicate the tail.

**F8 High: a 4 GB machine is offered no chat model and isn't told so.** `recommend.mjs`: `4 GB *: nomic-embed-text-v1.5-q8` only. "On this device" still carries "Recommended" and lists only the "Search model" card. Nothing says chat needs 8 GB or points to BYOK. Code: `AiStep.tsx` (`onboarding.ai.localBadge` is unconditional; the `:157` note fires only when nothing at all is recommended). Fix: when no chat model fits, drop the badge, say so, and preselect BYOK.

**F9 Medium: no download time is shown, and Welcome promises "Setup takes about two minutes".** `recommend.mjs` UI text has no time estimate (3/3). The 8 GB pick (qwen3-4b, 2.5 GB) takes 33 min at 10 Mbit/s (table below). Fix: show "about N min on a typical connection" and a live ETA in the progress text, and qualify the two-minute claim.

**F10 Low: after "Decide later", the errors are raw and BYOK is hard to find.** `runtime-later.mjs`. Sending a message shows `no provider is available for the synthesis tier` with an empty bubble. The banner's "Go to Models" leads to local models only, and BYOK is reachable only through "Run setup again" (sidebar). The `laterDetail` copy says "Models page, or add a provider to your config file". Fix: add BYOK to Models (see F1) and give a friendly no-AI chat state.

**F11 Low: there is no way to cancel a download, and resume loses up to 16 MiB per part.** There is no cancel button and no `models.cancel` method; only dropping the connection stops it (`handler.rs:1229`). `downloads.mjs cancel`: 6.29 MB stayed staged, but the reinstall fetched all 16.78 MB from byte 0 (`persist_every` 16 MiB, `download.rs:67`). Staging is also kept forever if the user never retries.

**F12 Low: raw error strings.** Disk: `insufficient disk space: need 1082969292 bytes, 20492288 available` (`nugget-core/src/error.rs:26`). SHA: the full hex hashes. Disk full: `io error: No space left on device (os error 28)`. The 404 message has no "update Nugget" hint. Also, an Anthropic base URL ending in `/v1` becomes `/v1/v1/models` and is reported as `wrong_url` (`ai_keys.rs` `models_url`).

## 3. Friction log
| Moment | Users hit | Evidence |
|---|---|---|
| Welcome says 2 minutes; the model takes 7 to 33 min | most (slow Wi-Fi) | F9 |
| 4 GB laptop: "Recommended" option with no chat model | few | F8 |
| Offline or captive-portal Wi-Fi: about 105 s of "Downloading 0%", then a URL dump | some | F6 |
| BYOK saved, but Chat says no models until a restart nobody explains | all BYOK users | F4 |
| Pasted the key with a space, or used http, and got a misleading message | some | F5 |
| Rotating a revoked key: "choose another name" | all who rotate | F1 |
| Linux without gnome-keyring: BYOK impossible | some Linux users | F2 |
| Runtime fails: wall of red text | few | F7, `shots/runtime-exit1-chat.jpg` |

## 4. What works well
- Signed manifest, preflight and fit: the disk warning ("Not enough free disk space") appears before Install, and preflight refuses in 3 ms without touching the network.
- Downloads heal themselves: a dropped connection resumes with `Range: bytes=3145728-`, a 60 s stall recovers automatically, a daemon restart resumes from the 32 MiB checkpoint, and after a disk-full error, freeing space and retrying installs.
- A SHA mismatch deletes the staging directory (0 bytes left) and never activates the file.
- BYOK: the key never reached config.toml, logs or any file on disk (grep of the whole home). Trailing slash, surrounding whitespace and newlines are trimmed. A 30 s server is cut off at 10 s. 401/403 → bad_key, 404 → wrong_url. Plain http is refused except for loopback.
- A llama-server crash after start is restarted transparently; the next chat succeeded in 131 ms.
- New build: models are grouped into "Chat and drafting" and "Search", with a single "Best for this computer" badge and readable names.

## 5. Real manifest (`manifests/default.json`): sizes, picks and download times
| Model | Size | Min RAM | Tiers | 10 Mbit/s | 50 Mbit/s | 200 Mbit/s |
|---|---|---|---|---|---|---|
| qwen3-4b-q4km | 2.50 GB | 8 | triage, light, synthesis | 33.3 min | 6.7 min | 1.7 min |
| qwen3-1.7b-q8 | 1.83 GB | 8 | triage, light, synthesis | 24.5 min | 4.9 min | 1.2 min |
| qwen3-8b-q4km | 5.03 GB | 16 | synthesis, agentic | 67.0 min | 13.4 min | 3.4 min |
| nomic-embed-text-v1.5-q8 | 0.15 GB | 4 | embedding | 1.9 min | 0.4 min | 0.1 min |
| bge-reranker-v2-m3-q4km | 0.44 GB | 4 | rerank (opt-in) | 5.8 min | 1.2 min | 0.3 min |

Recommended (`models.list`; identical for gpu unknown, apple_silicon and nvidia):
- 4 GB: embedding model only.
- 8 GB: qwen3-4b + embedding. The UI badges qwen3-4b; the download is 2.64 GB, about 35 / 7 / 1.8 min at 10 / 50 / 200 Mbit/s.
- 16 to 64 GB: qwen3-4b, qwen3-8b and embedding. The UI badges qwen3-4b as best.

A first chat on a real install also downloads llama-server (12 to 37 MB, from github.com). The UI doesn't mention it (code reading; `nugget-runtime/src/supervisor.rs:258`). The UI shows each model's size and disk need before Install, but never a download time.
