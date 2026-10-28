# Introduction

Nugget is an open-source (Apache-2.0), local-first, cross-platform personal AI. It indexes your files,
notes, mail and calendars continuously, so they are searchable within seconds of a change, and it answers
questions from them with citations. It builds a knowledge graph of the people, organisations and
projects in your data, every fact citing its source, renders it as a Markdown vault you can edit, and
lets other AI apps read it over MCP. Every request that leaves your device is recorded where you can
see it.

Being wired in now (wave W), each built and tested as its own crate:

- **[Double Tap](guide/double-tap.md)**: press a hotkey in any text field and get a draft in your own
  voice, grounded in cited passages, inserted and verified.
- **[The agent](guide/agent.md)**: multi-step tasks in your apps and browser, under a
  [safety policy](guide/safety.md) with confirmations, a kill switch and a tamper-evident action log.
- **[Today](guide/today.md)**: proactive cards and a morning brief, each action previewed as a dry
  run and run only with your approval.
- **[Plugins](guide/plugins.md)**: signed WebAssembly connectors and tools with capabilities you
  approve.

Nugget has two parts:

- **`nuggetd`**, a background daemon written in Rust. It owns the encrypted store, the connectors, the
  ingest pipeline, retrieval and every model call.
- **The desktop app**, a Tauri 2 shell (React and TypeScript). It talks to `nuggetd` over a local socket
  and holds the OS permissions the daemon should not have.

Other programs talk to the same daemon over the same [local IPC protocol](reference/ipc.md), or over
[MCP](guide/mcp.md).

## How this book is organised

- **[User guide](guide/install.md)**: installing, choosing models, adding sources, searching, the
  knowledge graph, connecting other AI apps, Double Tap, the agent and safety, Today, plugins,
  privacy and troubleshooting.
- **[Reference](reference/config.md)**: every `config.toml` key, the command line, environment
  variables, the IPC methods, the MCP tools, connector settings and the data directory. A script
  checks these pages against the source code in CI, so they cannot quietly drift
  ([how](contributing/docs.md)).
- **[Architecture](architecture/crates.md)**: the crate map, data flow, security model and the
  benchmark results, including the targets we have missed.
- **[Contributing](contributing/setup.md)**: setup, the DCO and clean-room rules, testing, and how to
  write a connector.
- **[Nugget and Sentient OS](comparison.md)**: a factual comparison with the project Nugget set out to
  improve on.

## Status

Nugget is **pre-release**. No release has been cut yet, so you build it from source, and the
features of milestones M3 to M6 are still being connected to the app. [What works today](status.md) lists exactly what runs and what does not. Where a page
describes something that is not wired up yet, it says so.

## Licence

Nugget is licensed under Apache-2.0. Contributions are accepted under the same licence with a
[DCO sign-off](contributing/rules.md#developer-certificate-of-origin).
