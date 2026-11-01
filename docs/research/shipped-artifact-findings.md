# Sentient OS 2.7.1: shipped-artifact findings

A read-only inspection of the shipped release (DMG parsed locally with `plistlib`, 7-Zip and `strings`;
nothing executed). Labels: **OBSERVED**, **INFERRED**, **UNKNOWN**.

## Artifact
- `SentientOS-2.7.1.dmg`, 40,989,414 bytes, SHA-256 `76ef7ec8…dd730f6`, matching the Homebrew cask (OBSERVED).
- Bundle `jesai.Sentient-OS-macOS`, build 17, minimum macOS 15.4; 93 MB unpacked (OBSERVED).
- Contains the main Swift binary (also the root wake helper), the LiteRT-LM runtime (64 MB), Sparkle
  (feed checked every 3 h, auto-installs), Sentry and TelemetryDeck SDKs, a **root LaunchDaemon** plist
  for overnight wake, and a pinned manifest for a downloaded agent runtime (OBSERVED).

## Computer use is mostly OpenAI's
The manifest pins the **OpenAI Codex CLI** (130 MB tarball) and a closed **`SkyComputerUseService`**
helper (36 MB) with per-app instruction files; the binary also references the open-source `cua-driver`
(OBSERVED). INFERRED: Sidekick orchestrates third-party runtimes (≥ 170 MB at first use) rather than
its own agent.

## Network endpoints (OBSERVED via `strings`)
- Models: OpenAI Responses and OAuth, OpenRouter, local Ollama and LM Studio ports, a Hugging Face model URL.
- Connectors are hosted MCP servers for Gmail, Calendar, Drive, Notion, Slack, Granola and Microsoft 365.
- Own cloud: a Supabase project, Cloudflare Workers for the Double Tap relay and downloads, and an
  "MCP mirror" at `mcp.sentient-os.ai/u_<user>/p_<password>/mcp`: a **password in the URL path**.

## Not in the public repo
The Double Tap relay Worker and the MCP mirror server (INFERRED from a bounded search); the Codex CLI
and computer-use helper are closed binaries (OBSERVED). The Supabase repo has migrations only.

## Design consequences for Nugget

Own the agent runtime and native accessibility drivers; direct-API connectors with no vendor-account
gating; any relay or mirror self-hostable, end-to-end encrypted and bearer-authenticated; no root
daemon; telemetry off with a local egress ledger.
