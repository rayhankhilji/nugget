# Security policy

Nugget indexes your mail, messages, notes and files, holds credentials for your accounts, and can
operate your computer. We treat security bugs as the most serious class of bug.

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's
[private vulnerability reporting](https://github.com/rayhankhilji/nugget/security/advisories/new)
(**Security → Report a vulnerability**), with the affected component and version or commit, your
OS and configuration (providers, connectors, plugins), steps to reproduce or a proof of concept,
and the impact as you understand it.

- We acknowledge within **3 working days** and aim to assess within **10 working days**.
- We agree a disclosure date with you, normally within **90 days**, and credit you unless you
  prefer not.
- Please give us a reasonable chance to fix it first, and test only your own installation and data.

Nugget is pre-1.0: only the latest release and `main` receive fixes.

## Scope

Everything in this repository that runs on a user's machine or ships in a release. Most wanted:

- **Daemon IPC**: the JSON-RPC socket (Unix socket in a `0700` directory, or per-user named pipe),
  message limits, parameter handling, access by other local users.
- **MCP server**: the `nuggetd mcp --client <id>` stdio bridge and the `127.0.0.1` HTTP listener;
  scoped tokens, revocation, Host/Origin checks, rate limits, out-of-scope or sensitive leaks.
- **Connectors and OAuth**: PKCE, loopback redirect, `state`, token storage and refresh, hostile
  content reaching extraction.
- **Store and secrets**: the SQLCipher key, keychain and key-file fallback, the audit chain,
  secrets leaking into config, logs, IPC responses or the audit log.
- **Egress**: requests without a ledger entry, locality misclassification, cross-host redirects,
  sensitive items reaching a cloud model.
- **Models and runtime**: the signed manifest, verified downloads and unpacking, loopback
  `llama-server`.
- **Agent and computer use**: the policy engine, kill switch and action log. Any way to make
  Nugget take a Send, Destroy, Spend or Security action without the required confirmation,
  including by prompt injection, is high severity.
- **Browser control**: the `nugget-browser-host` native-messaging host, `extensions/browser`,
  per-site consent.
- **Plugins**: the WASM host's capability checks, fuel/time/memory limits, package signatures,
  any escape to the filesystem, network or other plugins' data.
- **Double Tap**: reading the focused field and thread, insertion and clipboard fallback,
  secure-field and excluded-app handling; the trigger listener must never record other keystrokes.
- **Desktop app and updates**: Tauri commands and webview permissions, signed payloads and feeds,
  installers.

**Out of scope**: attacks needing root/administrator or an unlocked session; malware already
running as the same user (we still limit damage, e.g. keychain-held keys); bugs in a provider's
own service unless Nugget worsens them; content a user deliberately sends to a cloud provider
they configured (recorded in the ledger, not a leak).

## Threat model (outline)

A full threat model is planned before v1.0 ([docs/PLAN.md](docs/PLAN.md));
the book's [security architecture](docs/book/src/architecture/security.md) describes the controls.

- **Assets**: user content (items, chunks, embeddings, graph, drafts); secrets (API keys, OAuth
  tokens, store key); authority to act for the user; integrity of the audit log, egress ledger,
  model files and the app.
- **Adversaries**: content Nugget ingests (including prompt injection); a malicious plugin, MCP
  server or model provider; other local users; network attackers; someone with the disk at rest;
  a compromised dependency or release pipeline.

Key controls: user-private IPC with a 16 MiB message cap and no TCP listener by default;
SQLCipher (AES-256) with the key in the OS keychain (a `0600` key file where none is usable, which
protects against other users, not disk theft); a SHA-256 hash-chained audit log; an egress ledger
for every non-on-device request and no telemetry; an ed25519-signed model manifest with pinned
hashes; WASM plugins with only declared, approved capabilities; a policy engine with risk classes,
confirmations, budgets and a kill switch; `cargo deny`, Dependabot, pinned CI actions, and
release signing only in CI after a second maintainer's approval (no release signed yet) ([docs/release.md](docs/release.md),
whose §8 lists open supply-chain gaps).
