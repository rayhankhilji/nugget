# Security model

Nugget indexes mail, notes and files, holds credentials, and can act on your computer. To report a
vulnerability, follow [`SECURITY.md`](https://github.com/rayhankhilji/nugget/blob/main/SECURITY.md)
(privately, never in a public issue).

## Threat model

Nugget protects your content, secrets, its authority to act for you, and the integrity of its logs,
models and app against: ingested content (including **prompt injection**); a malicious provider, MCP
client, browser peer or plugin; other local users; network attackers; disk theft; and a compromised
dependency or release pipeline. **Out of scope:** malware running as you, root attackers, and content
you deliberately send to a cloud provider you configured (recorded, not prevented).

## Controls

| Area | Control | Known gaps |
|---|---|---|
| IPC | Unix socket in a private `0700` directory or a Windows named pipe admitting only you and SYSTEM (clients verify the server's user); no TCP; one daemon per data directory; 16 MiB message cap; secrets write-only. Any process running as you can call any method, by design | No peer-credential check; Windows pipe checks not exercised on a physical Windows machine |
| At rest | One SQLCipher (AES-256) store; 256-bit key in the OS keychain, or a `0600` `store.key` without one (protects against other users, not disk theft). Credentials only in the keychain; config rejects plaintext keys | Vault and logs are plain text (no secrets in logs, no content at `info`); Windows directories inherit ACLs; no key rotation |
| Egress | Every non-local request records host, feature, bytes and items. Locality from the pinned address; can be raised, never lowered. No plain `http` to internet hosts; rustls with the OS trust store; no cross-host redirects. Sensitive items never go to a cloud model. No telemetry | A crash mid-request can leave no row; a local server that forwards to the cloud looks on-device unless marked |
| Local models | `llama-server` on `127.0.0.1` only, random per-start API key passed in the environment, scrubbed environment, own process group or job object, orphans reaped. Binaries from the signed manifest, unpacked as hostile input | Port ownership verified on Linux only |
| Model supply chain | ed25519-signed manifest, pinned revision, size and SHA-256, verified before atomic install; rollback protection | Release signing key not yet generated |
| Plugins | WASM components with only host `http`, `kv`, `secrets`, `log`, `clock` and `sink` imports (WASI traps); capabilities approved at install and checked on every call; fuel, deadline and memory limits; ed25519-signed packages | |
| MCP | Local only, per-client scoped tokens stored as keyed hashes, Host/Origin checks, rate limits, audit without content ([MCP](../reference/mcp.md)) | PM-4: an HTTP client can send its token to a port squatter while `nuggetd` is down (mitigated, not fixed) |
| Supply chain | `cargo deny` (no GPL, AGPL, LGPL), Dependabot, pinned CI actions | Releases unsigned until keys exist |

## Actions: the policy engine

Every agent, browser and proactive action carries a risk class and passes the policy gate (Allow,
Confirm or Deny) first.

Risk classes: **Observe** (read, navigate), **Edit** (undoable, nothing leaves the device), **Send**,
**Destroy**, **Spend** (purchases, bookings, payments) and **Security** (passwords, permissions,
installs, settings).

- Classification can only raise risk; deny rules always win; an Allow rule covers Send and above only
  if it names the class; with no rule, Send and above need confirmation.
- A confirmation timeout or the kill switch counts as denial; per-task approvals and per-class rate
  limits bound a task.
- The action log is SHA-256 hash-chained and exportable as JSONL; secure-field text is redacted.
- Success is reported only when every mutating action passed its after-action check.
- Double Tap and accessibility never read secure fields, refuse excluded apps, and verify insertion by
  reading back; the hotkey listener drops all other key codes. Browser control acts only on sites you
  allowed.

## Prompt injection

Ingested content is treated as data: prompts say so, MCP fences it in `<untrusted-content>` blocks,
LLM facts need a verbatim quote, and no model output acts without the policy gate. This reduces the
risk without removing it. **Open Blocker:** Double Tap obeys an instruction planted in a vault note.
The audit log of config changes, sign-ins and installs is hash-chained but unkeyed, so someone with the
store key could rewrite it.
