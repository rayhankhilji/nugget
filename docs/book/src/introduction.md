# Introduction

Nugget is an open-source (Apache-2.0), local-first, cross-platform personal AI. It indexes your files,
notes, mail and calendars continuously, so they are searchable within seconds of a change, and it answers
questions from them with citations. Every request that leaves your device is recorded where you can see
it.

Nugget has two parts:

- **`nuggetd`**, a background daemon written in Rust. It owns the encrypted store, the connectors, the
  ingest pipeline, retrieval and every model call.
- **The desktop app**, a Tauri 2 shell (React and TypeScript). It talks to `nuggetd` over a local socket
  and holds the OS permissions the daemon should not have.

Other programs talk to the same daemon over the same [local IPC protocol](reference/ipc.md), or, once it
is wired into the daemon, over [MCP](guide/mcp.md).

## How this book is organised

- **[User guide](guide/install.md)**: installing, choosing models, adding sources, searching, the
  knowledge graph, connecting other AI apps, privacy and troubleshooting.
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

Nugget is **pre-release**. There are no installers yet, and parts of the plan exist only as crate
contracts. [What works today](status.md) lists exactly what runs and what does not. Where a page
describes something that is not wired up yet, it says so.

## Licence

Nugget is licensed under Apache-2.0. Contributions are accepted under the same licence with a
[DCO sign-off](contributing/rules.md#developer-certificate-of-origin).
