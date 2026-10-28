# Security policy

Nugget indexes your mail, messages, notes and files, holds credentials for your accounts, and can
operate your computer. We treat security bugs as the most serious class of bug.

## Reporting a vulnerability

**Do not open a public issue for a security problem.**

Report it privately through GitHub's
[private vulnerability reporting](https://github.com/rayhankhilji/nugget/security/advisories/new)
(the **Security → Report a vulnerability** tab of the repository). Please include:

- the affected component and version or commit
- your OS and how Nugget is configured (which providers, connectors and plugins)
- the steps to reproduce, or a proof of concept
- the impact as you understand it

What to expect:

- We acknowledge reports within **3 working days**.
- We aim to send a first assessment within **10 working days**.
- We agree a disclosure date with you, normally no later than **90 days** after the report.
- We credit reporters in the advisory unless you ask us not to.

Please give us a reasonable chance to fix the problem before you disclose it. Only test against
your own installation and data.

## Supported versions

Nugget is pre-1.0. Only the latest release and the `main` branch receive security fixes.

## Threat model (outline)

This outline will become a full threat-model document before v1.0 (see
[docs/PLAN.md §7](docs/PLAN.md#7-engineering-practices)).

### Assets

- **User content**: indexed items, chunks, embeddings, the knowledge graph and drafts.
- **Secrets**: provider API keys, OAuth tokens for connectors, the store encryption key.
- **Authority**: the ability to send messages, change files or operate apps on the user's behalf.
- **Integrity**: the audit log, the egress ledger, the model files, and the app itself.

### Adversaries

1. A remote attacker who controls content Nugget ingests: an email, web page, shared document or
   calendar invite. This includes **prompt injection**.
2. A malicious or compromised **plugin**, **MCP server** or **model provider**.
3. **Other local users** on a shared machine, and unprivileged local processes running as other
   users.
4. A network attacker between Nugget and a provider or model mirror.
5. Someone who gets the **disk at rest**: a stolen laptop, a backup, a cloud-synced folder.
6. A compromised dependency or release pipeline (**supply chain**).

Malware already running as the same user, with full access to that user's session, is out of
scope. We still try to limit the damage it can do, for example through keychain-held keys.

### Trust boundaries and controls

#### Local daemon IPC (`nuggetd` ⇄ desktop app and clients)

- JSON-RPC 2.0 over a Unix domain socket in a user-private `0700` directory, or a Windows named
  pipe scoped to the current user. There is no TCP listener by default.
- Messages are capped at 16 MiB. The server checks parameters and returns typed errors; malformed
  input must never crash the daemon.
- Risks: other users connecting to the socket; confused-deputy calls from local apps; resource
  exhaustion.
- Planned: per-client tokens for the local MCP server, plus scoped capabilities per client.

#### Encrypted store

- SQLCipher (AES-256) encrypts the whole database at rest, including full-text and vector indexes.
- The key lives in the OS keychain (macOS Keychain, Windows Credential Manager, Secret Service).
  Where no keychain is usable (headless Linux, CI), it falls back to a `0600` key file under the
  data directory and logs a warning. That fallback protects against other users, not against
  someone who has the disk.
- The audit log is a SHA-256 hash chain, so editing or deleting earlier rows is detectable with
  `verify_audit_chain`.
- Deleting an item cascades to its chunks and vectors.

#### Egress ledger

- Every request to a non-on-device destination (`LocalNetwork` or `Cloud`) records an
  `EgressEvent`: the host, the feature that caused it, bytes sent and received, and which items
  were included. The Privacy page shows the ledger.
- Provider locality is derived from the base URL host: loopback is on-device, private ranges are
  local network, everything else is cloud.
- Telemetry is off by default. Nugget runs no first-party servers in the data path.
- Limits: the ledger records what Nugget's own network layer sends. Code that bypasses that layer
  is a bug. Plugins (below) have no direct network access for this reason.

#### Models and providers

- The model manifest is signed (ed25519, detached signature) and verified before use. Entries pin
  an exact revision, size and SHA-256. Downloads are hash-checked before an atomic install, and
  disk preflight fails closed.
- API keys are read from the keychain or the environment, never from the repository or logs.
- Model output is untrusted input. It cannot widen permissions by itself.

#### Plugins and MCP

- Third-party connectors and tools run as **WASM components** (wasmtime). They have no ambient
  filesystem, network or process access. They get only the capabilities they declare (network
  hosts, scopes), and the user approves those capabilities.
- Plugin network access goes through the host, so it is recorded in the egress ledger.
- MCP servers used as sources are treated like untrusted plugins. The local MCP server Nugget
  exposes uses per-client scoped tokens with revocation and never puts secrets in URLs.

#### Computer use and actions

- A policy engine sits between the agent and the OS:
  - per-app allow and deny lists
  - a risk classifier for actions such as send, pay, delete, share and install
  - **inline confirmation** before any risky action
- Agent loops have step and time budgets. A global **kill-switch** hotkey stops all agent activity.
- Actions have idempotent ids and are verified after they run. The tamper-evident audit log keeps
  before and after evidence.
- Ingested content (adversary 1) is data, not instructions. Prompt-injection resistance is tested
  in red-team suites; the M4 target is 0 unconfirmed risky actions.

#### Updates and supply chain

- Dependencies are checked with `cargo deny` (advisories, licenses, sources), and Dependabot keeps
  them current. Third-party CI actions are pinned to a major version or a commit.
- Planned: signed releases with threshold keys in CI, a user-chosen update channel, and opt-in
  auto-update.

### Out of scope

- Attacks that need root or administrator access, or physical access to an unlocked session.
- Vulnerabilities in a provider's own service, unless Nugget's use of it makes them worse.
- Content a user deliberately sends to a cloud provider they configured. The egress ledger records
  it, but it is not a leak.
