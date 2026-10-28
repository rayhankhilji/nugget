# Sentient OS v2.7: full teardown

A synthesis of a staged REA-method investigation of [Sentient-OS-Labs/sentient-os](https://github.com/Sentient-OS-Labs/sentient-os)
(AGPL-3.0, v2.7 source, v2.7.1 release). The source was read by five parallel workers, each with its own subsystem.
The shipped DMG was inspected separately (see [shipped-artifact-findings.md](shipped-artifact-findings.md)).

> **Clean-room note.** This document describes behaviour and architecture in our own words. No AGPL code or prompt text
> is reproduced. Nugget is Apache-2.0: implementers may read this document, but must not copy code, prompts or
> distinctive expression from the upstream repository.

Labels: **O** = observed in code, docs or artifact. **I** = inferred. **U** = unknown. File references are relative to
`Sentient OS macOS/` in the upstream repo.

---

## 1. One-paragraph summary

Sentient OS is a macOS-only (Apple Silicon, 15.4+) SwiftUI menu-bar app of about 98k lines of Swift.
1. **Triage, on device.** Overnight it uses an on-device Gemma 4 E4B (LiteRT-LM, Metal) to triage local data (files, Notes, iMessage, WhatsApp, Apple Mail, Calendar) into roughly 30-word summaries.
2. **Knowledge base, in the cloud.** It hands those summaries to a frontier model (ChatGPT via a bundled OpenAI Codex CLI, Claude via the Claude Code CLI, or a custom Responses endpoint) that writes an 80–120 note Markdown "vault".
3. **Proactive cards.** The same frontier model generates up to 5 prepared actions each morning.
4. **Sidekick.** Computer use runs on **OpenAI's own signed computer-use helper, driven through `codex exec` for every backend**.
5. **Double Tap.** Drafts are one streamed vision call (screenshot plus the whole vault), sent by default through Sentient's closed Cloudflare relay and pasted with a clipboard Cmd-V.

Development happens in a private tree that is mirrored to GitHub in bulk with each release.

---

## 2. Subsystem findings

### 2.1 On-device engine
- **Model and runtime (O).** Gemma 4 E4B `.litertlm` (3.66 GB) on LiteRT-LM v0.13.1 with the Metal GPU backend and speculative decoding (MTP). Temperature is 0.15, output is capped at 1,024 tokens, and the KV cache is usually 4k (16k for chats). It holds about 3 GB resident.
- **What it does (O).** It is used **only for per-item triage**: keep, junk or sensitive, plus a title and summary as JSON. A regex backstop catches SSNs, Luhn-valid card numbers and passport numbers. The local model never acts as the "frontier" model.
- **GPU wedge (O).** A Dawn/WebGPU executor hang is worked around by reloading the engine every 40 items and after 3 consecutive failures. Each reload costs about 10 s.
- **Model download (O).** Eight resumable range parts, pinned SHA-256, atomic move into place. Two problems: it fetches from HF `resolve/main` (a moving ref), and **once installed the model is never re-verified or updated**.
- **No hardware gating (O).** There is no RAM-tier selection, and Intel Macs are stripped out.

### 2.2 Providers
- **Backends (O).** Three of them: `chatgpt` (the default), `claude` and `custom`. Custom has OpenRouter, LM Studio and generic presets.
- **Dispatch (O).** One switch, with no provider abstraction.
- **Codex is the harness for ChatGPT and custom (O).** Its behaviour is steered with undocumented `-c` config keys. A loopback `ResponsesTranslator` proxy rewrites Codex's tool dialect for weaker servers, and **silently falls back to a tool-less direct connection** if it can't bind.
- **Claude (O).** Structured work runs through `claude -p`. Computer use still runs in Codex, with Claude piped in through a loopback "subscription bridge" provider.
- **Plan detection and OAuth (O).** The ChatGPT plan is read from an **unverified JWT claim**. The app refreshes Codex OAuth tokens itself using Codex's public client id. Model ids (`gpt-6-sol`, `gpt-6.1-terra`, and so on) and usage-limit error strings are hard-coded.
- **Custom endpoints (O).** They must pass a vision probe: read back a random 4-digit code from a rendered image. This is a good idea worth matching.
- **Distribution (O).** The Codex CLI and OpenAI's "Codex Computer Use.app" helper, which is extracted from OpenAI's desktop installer, are re-hosted on Sentient's own R2 bucket. Whether they are allowed to redistribute these is **U**.

### 2.3 Overnight pipeline and scheduling
- **Wake mechanism (O).** A 3 AM default wake, implemented by a **root LaunchDaemon** (the app binary run in a helper mode) that runs `pmset schedule wake` and `pmset -a disablesleep 1` so the Mac stays awake with the lid closed. Safety timers: a 30 min deadman and a 6 h ceiling.
- **Stage order (O).** Local on-device pass, then hosted Gmail, then hosted Calendar, then MCP connectors, then the proactive cycle (vault build or merge, mirror push, judge, research, wipe summaries).
- **Checkpoints (O).** Per-bucket high-water mark plus a first-run floor, committed atomically with each summary. The run is strictly serial: one engine, one item at a time.
- **Power and thermal rules (O).** Only the critical thermal state is acted on. Battery floors are 50%, 30% and 20%. Low Power Mode is ignored.
- **Crash risk (I).** If the helper crashes mid-run, sleep can stay disabled.

### 2.4 Ingestion and sources
| Source | Mechanism (O) | Weakness |
|---|---|---|
| Files and screenshots | Directory walk. PDFs: first 3 pages. Text capped at 8k chars. Images go to the VLM at 768 px; **no OCR** | Keyed on date added, so **edits are never re-read** |
| iMessage | Copy of `chat.db`, **heuristic** `attributedBody` decoder | Breaks when the OS changes |
| WhatsApp | Copy of the Core Data SQLite; 12 KB windows, 90-day lookback | Private schema |
| Apple Notes | gzip plus a **schema-less protobuf walk** | Keyed on creation date, so edits are missed |
| Apple Mail | SQLite backup of the Envelope Index, at most 600 messages per pass | Private schema |
| Apple Calendar | EventKit, -7 to +30 days, full snapshot each run | Attendees are omitted |
| Gmail, GCal, Slack, Outlook, Drive | **Read and summarized by the cloud CLI** through the vendor's hosted MCP connectors | Gmail and Calendar require a ChatGPT or Claude plan; non-deterministic; costs money |
| Notion, Granola | Their own PKCE OAuth and direct MCP calls | Picks only 3 to 6 candidates per run |
| SiYuan | JSON block trees rendered to Markdown | (community-contributed) |

**Writing style (O)** is not a model, just raw samples: 5 sent messages for each of the 6 most recent contacts, plus 15 sent emails.

### 2.5 Knowledge base
- **How it's written (O).** The frontier LLM writes free-form Markdown into a staging directory, which is then swapped in atomically. Constraints exist only in the prompt: at most 10 domains, 80–120 notes, `[[wikilinks]]`, no frontmatter. **Nothing validates them.**
- **No structured layer (O).** There is no entity store, no provenance (summaries are deleted after each cycle), no embeddings, no FTS and no graph DB.
- **Graph view (O).** The graph is re-derived from wikilinks every time it loads. Links resolve on the file-name stem alone, so notes with the same name collide.
- **Retrieval (O).** Double Tap sends **the entire vault, up to 600 KB, with silent truncation**. Sidekick gets the README plus 400 paths and runs `grep` itself.
- **Encryption (O).** None at rest locally. The optional mirror encrypts the vault zip, but the key is derived from a password embedded **in the share URL**, and the server decrypts on every request.

### 2.6 Sidekick (computer use)
- **Runtime (O).** Every backend runs `codex exec --dangerously-bypass-approvals-and-sandbox` with a loopback MCP broker. The broker proxies OpenAI's closed `SkyComputerUseService` helper through a second `codex app-server` process.
- **Perception and actions (O).** The AX tree plus a screenshot on each read. A fixed set of 10 tools, targeting element ids rather than coordinates. The browser is driven as native windows; CDP was rejected because of Chrome 136+ consent prompts.
- **Good ideas worth matching (O).** Mutations are refused unless that app's state was read successfully first. Failed RPCs are never replayed. A cheap router model decides between a connector and computer use.
- **Missing safeguards (O).** No step cap, no overall deadline, **no app allow/deny list, and no confirmation for irreversible actions**. The hotkey does **not** stop a running task, so there's no global kill switch. "Done" means the agent *claimed* it was done; outcomes aren't verified.
- **Their own cursor (O).** The legacy cua-driver drew an overlay "agent cursor". What the current helper draws is **U** (closed binary).

### 2.7 Double Tap
- **Trigger and context (O).** Right Cmd pressed twice within 0.35 s. One screenshot of the display under the cursor, at most 2000 px, plus the whole vault and the writing samples. **The focused field's text is not read through AX.**
- **Request (O).** The OpenAI Responses API with prompt caching and `reasoning: none`, sent by default through Sentient's closed `workers.dev` relay. The relay's model, retention and budgets are **U**.
- **Insertion (O).** Clipboard, synthetic Cmd-V, then the clipboard is restored after 500 ms. Non-text clipboard formats are lost, and delivery is not verified. The model decides whether the field is a reply box by returning a sentinel.

### 2.8 Proactive cards
- **Judge (O).** Reads the last 7 days of summaries plus calendar and returns up to 8 items. Research then turns these into at most 5 `PreparedAction`s (gmail, calendar, computer or research). **Ranking is purely the LLM's ordering.**
- **Execution (O).** Happens only when the user taps a card. Cards are stored as JSON **in UserDefaults**, which puts personal content in a plist. Production connector selection is gated by `dbg.*` debug flags.

### 2.9 Cloud, business and trust
- **Supabase (O).** Only anonymous-auth RPCs for invites, contact emails, YC-company welcomes and support reports. There are no edge functions. RLS is well done, but the YC lookup is callable anonymously, which lets anyone check whether a domain is in the list.
- **Closed services (O).** The Double Tap relay Worker, the MCP mirror server (in a separate repo) and the website/appcast. "Supporting infrastructure is open source" overstates it.
- **Telemetry (O).** **On by default.** An always-on core tier sends an **LLM-written free-text workflow phrase**, plus install and uninstall pings that are sent even after opt-out.
- **Updates (O).** Sparkle, **mandatory and silent**, checked every 3 h. The signing key is held by a single person.
- **"Free for life" (O).** Gates nothing; it's a growth mechanic. The invite-cap UI says 5, but the server is uncapped.
- **Engineering process (O).** No XCTest target and no build CI; the only workflow is a CLA bot. Testing relies on a 13k-line DEBUG-only in-app "Self Tests - Temp" harness plus 97 `swiftc` fixture scripts. `HomeView.swift` is a 2,041-line god object. Only 1 external PR has been merged in the visible history.
- **Issue themes (O).** False negatives in permission and readiness checks, connector verification failures, LiteRT hangs, wake-helper breakage, Codex quota burn, and requests for Linux, localisation, an action log and default-deny data access.

---

## 3. Where Sentient is structurally weak (our wedge)

1. **Their core capabilities are someone else's.** Computer use is OpenAI's helper, the agent loop is the Codex CLI, and connectors are vendor-hosted MCP servers. They can't fix what they don't own, and one upstream change can break them.
2. **They run on undocumented ground.** Undocumented CLI config keys, impersonating an OAuth client, re-hosted vendor binaries, and reverse-engineered private SQLite and protobuf formats.
3. **The memory layer is shallow.** Free-text LLM Markdown with no provenance, search, entities, validation or incremental updates, and retrieval that dumps the whole vault.
4. **It runs nightly, in batches, one item at a time.** Edits and deletions are never picked up, and it needs a root daemon and a closed-lid wake.
5. **Their trust story is weaker than their marketing.** Telemetry is on by default, the relay is closed and on by default, there's a secret in the URL, updates are forced, nothing is encrypted at rest, and there's no action log.
6. **macOS on Apple Silicon only, and English only.**
7. **Quality is unmeasured.** There's no computer-use benchmark, no drafting eval and no CI.

See [../PLAN.md](../PLAN.md) for how Nugget answers each of these.
