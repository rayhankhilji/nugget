# Introduction

Nugget is an open-source (Apache-2.0), local-first personal AI for macOS, Windows and Linux. It indexes
your files, notes, mail, calendars and chats continuously, answers questions from them with citations,
builds a cited knowledge graph rendered as an editable Markdown vault, and shares it with other AI apps
over [MCP](guide/mcp.md). It also drafts in your voice in any text field ([Double Tap](guide/double-tap.md)),
runs tasks in your apps under a [safety policy](guide/safety.md) ([the agent](guide/agent.md)), shows
proactive cards that act only with your approval ([Today](guide/today.md)), and loads signed
WebAssembly [plugins](guide/plugins.md). Every request that leaves your device is recorded.

Nugget is two parts: **`nuggetd`**, a Rust daemon that owns the encrypted store, connectors and every
model call; and **the desktop app** (Tauri 2), which holds the OS permissions and talks to the daemon
over the [local IPC protocol](reference/ipc.md).

This book has a [user guide](guide/install.md), a [reference](reference/config.md) checked against the
code in CI ([how](contributing/docs.md)), the [architecture](architecture/overview.md),
[contributing](contributing/setup.md) notes and a [comparison with Sentient OS](comparison.md).

Nugget is **pre-release**: no release has been cut. [What works today](status.md) lists what runs and
what does not. Contributions are accepted under Apache-2.0 with a
[DCO sign-off](contributing/rules.md#developer-certificate-of-origin).
