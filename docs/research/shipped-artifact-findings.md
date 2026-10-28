# Sentient OS 2.7.1: shipped-artifact findings (REA pass)

Method: the REA "reverse-engineer-anything" workflow, applied to the **shipped release** rather than the source.
The REA MCP/CLI could not be run in this environment (third-party package execution was blocked), so the
inspection used only local, read-only parsing: a hand-written UDIF/zlib DMG reader, 7-Zip for HFS+,
`plistlib`, and `strings`. Nothing from the target was executed.

Labels: **OBSERVED** = seen directly in the artifact. **INFERRED** = reasoned from observations.
**UNKNOWN** = not established.

## Artifact identity
- `SentientOS-2.7.1.dmg`, 40,989,414 bytes, SHA-256 `76ef7ec8…dd730f6`. This matches the `sha256` in
  `Sentient-OS-Labs/homebrew-tap` `Casks/sentient-os.rb` (OBSERVED).
- UDIF image (zlib, block type `0x80000005`) holding a GPT disk with one HFS+ volume. The app is 93 MB unpacked (OBSERVED).
- Bundle id `jesai.Sentient-OS-macOS`, build 17. Built with Xcode 27A266a against the macOS 27.0 SDK; minimum macOS 15.4 (OBSERVED).

## What is inside the bundle
| Component | Size | Notes |
|---|---|---|
| `MacOS/Sentient OS` | 16.5 MB | Main Swift binary; also serves as the root wake helper (`--wake-helper`), per the cask comments |
| `Frameworks/libCLiteRTLM_mac.dylib` | 63.8 MB | Prebuilt LiteRT-LM runtime (Google, Apache-2.0) for the on-device model |
| `Sparkle.framework` | about 2 MB | Auto-update. Feed `https://sentient-os.ai/appcast.xml`, checked every 3 h, installs automatically |
| `Sentry.framework` | small | Crash reporting (US Sentry ingest DSN embedded) |
| `TelemetryDeck_TelemetryDeck.bundle` | small | Product analytics (`nom.telemetrydeck.com`) |
| `Library/LaunchDaemons/…WakeHelper.Managed.plist` | | **Root** LaunchDaemon that wakes the Mac for the overnight run |
| `Resources/BundledCodexRelease.json` | 57 KB | Pinned manifest for a downloaded agent runtime (see below) |
| `connect-chatgpt-*.mp4`, `connect-claude-*.mp4` | about 7 MB | Onboarding videos for linking ChatGPT and Claude accounts |

Telemetry and crash SDKs are declared in the public source (`Package.resolved`, `OnboardingTelemetryConsent.swift`).
They are not hidden, but they are in the shipped app (OBSERVED).

## The computer-use stack is mostly OpenAI's (key finding)
`BundledCodexRelease.json` pins two downloads (OBSERVED):
1. **OpenAI Codex CLI 0.160.0** (`codex` is 241 MB, plus `codex-code-mode-host`, a voice host built on GStreamer, `rg`, `zsh`).
   The tarball is 130 MB.
2. A **`computer-use-26.929.1001365-arm64.zip`** helper (36 MB) whose app is `SkyComputerUseService`, with per-app
   instruction files (`AppInstructions/Slack.md`, `Notion.md`, `Spotify.md`, `Numbers.md`, …).

The binary also references `github.com/trycua/cua/releases/download/cua-driver-rs-v…`. The source registers that driver
as an MCP server, `cua-driver` (OBSERVED in `ConnectorLab.swift`, `DevToolsView.swift`).

INFERRED: Sidekick is an orchestration and UX layer over the OpenAI Codex agent, its computer-use helper and the
open-source cua driver. It is not an agent they built themselves. Reliability and capability depend on third-party
runtimes they pin and download (about 170 MB or more at first use).

## Network endpoints compiled into the binary (OBSERVED via `strings`)
- **Models:** `api.openai.com/v1/responses`, `auth.openai.com/oauth/token` (ChatGPT-plan OAuth),
  `openrouter.ai/api/v1/responses`, local `127.0.0.1:11434/v1` (Ollama) and `:1234/v1` (LM Studio),
  and the Hugging Face model URL `litert-community/gemma-4-E4B-it-litert-lm/…/gemma-4-E4B-it.litertlm`.
- **Connectors are hosted MCP servers:** `gmailmcp.googleapis.com/mcp/v1`, `calendarmcp.googleapis.com/mcp/v1`,
  `drivemcp.googleapis.com/mcp/v1`, `mcp.notion.com/mcp`, `mcp.slack.com/mcp`, `mcp.granola.ai/mcp`,
  and `microsoft365.mcp.claude.com/mcp`.
- **Their own cloud:**
  - Supabase project `hjqedlalhfoxwehxhton.supabase.co`
  - Cloudflare Workers `sentient-doubletap-relay.…workers.dev` (cloud Double Tap) and `sentient-downloads.…workers.dev/releases/`
  - `mcp.sentient-os.ai/u_<user>/p_<password>/mcp` (the "MCP mirror" of your knowledge)

## What is not in the public repo
- The Double Tap relay Worker source: only `RuntimeDistribution/wrangler.toml`, for downloads, is present (INFERRED from a bounded grep for `workers.dev` and `wrangler`).
- The MCP mirror server behind `mcp.sentient-os.ai`: the client is present, the server is not (INFERRED, same search).
- The Supabase repo has migrations and tests only, no edge functions. The migrations cover the invite program, the YC-company
  welcome catalog, connected email-account snapshots, Gmail sent metadata, and computer-use support reports (OBSERVED).
- The Codex CLI and SkyComputerUseService are closed third-party binaries (OBSERVED).

## Security and trust notes
- A root LaunchDaemon is installed for wake-from-sleep (OBSERVED, cask plus plist).
- The MCP mirror puts a **password in the URL path** (OBSERVED in `MirrorClient.swift:10`). URLs leak through logs, history and
  copy/paste, so this is weaker than a bearer token or OAuth (INFERRED).
- Auto-updates install silently every 3 h through Sparkle, with an EdDSA public key pinned (OBSERVED).

## What this means for our design
1. Own the agent runtime, or make it a swappable open protocol. Don't wrap and pin a 400 MB third-party CLI.
2. Use native accessibility drivers (AX, UIA, AT-SPI) behind one Rust trait; cua-driver (MIT) is a reference, not a dependency.
3. Connectors: local-first direct APIs (IMAP, CalDAV, Gmail and Graph REST) plus *optional* remote MCP. No vendor-account gating.
4. A self-hostable mirror and relay, with OAuth/bearer auth, end-to-end encryption and published server source.
5. No root daemon: use OS schedulers (launchd user agents, Windows Task Scheduler wake timers, systemd timers) and incremental indexing so there's no need to wake the machine.
6. Telemetry off by default and fully self-hostable (OpenTelemetry), with a local "what left my device" ledger.
