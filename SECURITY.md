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

- **Daemon IPC** (user-private socket or named pipe) and anything letting another local user in.
- **MCP server** (`nuggetd mcp --client <id>` and the `127.0.0.1` listener): scoped tokens,
  revocation, Host/Origin checks, rate limits, out-of-scope or sensitive leaks.
- **Connectors and OAuth**: PKCE, loopback redirect, `state`, token storage, hostile content.
- **Store and secrets**: the SQLCipher key, keychain and key-file fallback, the audit chain, secrets
  leaking into config, logs, IPC responses or the audit log.
- **Egress**: requests without a ledger entry, locality misclassification, cross-host redirects,
  sensitive items reaching a cloud model.
- **Models**: the signed manifest, verified downloads and unpacking, loopback `llama-server`.
- **Agent and computer use**: any way to make Nugget take a Send, Destroy, Spend or Security action
  without the required confirmation, including by prompt injection, is high severity.
- **Browser control** (`nugget-browser-host`, `extensions/browser`, per-site consent), **plugins**
  (capability checks, resource limits, signatures, sandbox escapes), **Double Tap** (secure fields,
  excluded apps; the trigger listener must never record other keystrokes), and the **desktop app
  and updates** (Tauri commands, webview permissions, signed payloads and feeds, installers).

**Out of scope**: attacks needing root/administrator or an unlocked session; malware already
running as the same user; bugs in a provider's own service unless Nugget worsens them; content a
user deliberately sends to a cloud provider they configured (recorded in the ledger, not a leak).

## Threat model

A full threat model is planned before v1.0 ([docs/PLAN.md](docs/PLAN.md)). Assets: user content,
secrets (API keys, OAuth tokens, store key), authority to act for the user, and the integrity of
the audit log, egress ledger, model files and app. Adversaries: ingested content (including prompt
injection), malicious plugins, MCP servers or model providers, other local users, network
attackers, disk theft, and supply-chain compromise. The controls are described in the book's
[security architecture](docs/book/src/architecture/security.md); open release supply-chain gaps
are in [docs/release.md §8](docs/release.md#8-known-gaps).
