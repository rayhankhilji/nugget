# Security model

Nugget indexes mail, notes and files, holds credentials for your accounts, and is gaining the ability to
act on your computer (being wired in now, wave W). This page describes what it protects, against whom, how, and where the current gaps are. To
report a vulnerability, follow
[`SECURITY.md`](https://github.com/rayhankhilji/nugget/blob/main/SECURITY.md): privately, never in a
public issue.

## Threat model

**Assets**

- **Your content**: items, chunks, embeddings, the knowledge graph, drafts.
- **Secrets**: provider API keys, connector OAuth tokens and passwords, OAuth client secrets, the store
  key, MCP client tokens.
- **Authority**: the ability to send, change or delete things on your behalf (the agent, browser
  control and proactive actions, being wired in now).
- **Integrity**: the audit log, the egress ledger, model files and the app itself.

**Adversaries**

1. Someone who controls **content Nugget ingests**: an email, a shared document, a calendar invite.
   This includes **prompt injection**.
2. A malicious or compromised **model provider**, **MCP client**, **browser extension peer** or
   **plugin**.
3. **Other local users** on a shared machine.
4. A **network attacker** between Nugget and a provider, a mail server or a model mirror.
5. Someone with the **disk at rest**: a stolen laptop, a backup, a synced folder.
6. A compromised **dependency or release pipeline**.

**Out of scope**: malware already running as your user (it can do what you can, though keychain-held
keys limit some damage), attackers with root or administrator rights, and content you deliberately send
to a cloud provider you configured (it is recorded, not prevented).

## IPC access control

- The daemon listens only on a **local socket**: a Unix socket inside a directory with mode `0700`
  (the daemon refuses to use one that is not private to you), or on Windows a **named pipe** whose
  ACL admits only your account and SYSTEM. Its name includes your SID, and clients verify that the pipe's
  server runs as you before sending anything, because another account could create a pipe with the
  same name first. There is **no TCP listener**.
- A **lock file** ensures one daemon per data directory, taken before the socket is touched.
- Every message is capped at **16 MiB**; parameters are validated and errors are typed; malformed input
  must never crash the daemon.
- Secrets sent over IPC (`sources.secret.set`) are write-only: never echoed in responses,
  notifications, logs or the audit log.

**Trust boundary:** any process running as your user can open the socket and call any method. That is
deliberate (it is the same boundary as your files), but it means the IPC protocol is not a defence
against same-user malware. **Gaps:** there is no peer-credential check (`SO_PEERCRED`, `getpeereid`)
in addition to the directory permissions, no per-connection limit on in-flight calls, and the Windows
pipe checks have only been compile-checked in CI, not exercised on a real Windows machine.

## Encryption at rest

- The whole store, including the full-text and vector indexes, the graph, the ledgers and the cursors,
  is one **SQLCipher** (AES-256) database.
- Its key is 256 random bits held by the **OS keychain** (macOS Keychain, Windows Credential Manager,
  Secret Service on Linux). Where no keychain is usable (headless Linux, CI, `NUGGET_KEYCHAIN=off`), it
  falls back to a `store.key` file with mode `0600` and logs a warning. That fallback protects against
  other users, not against someone who has the disk.
- Connector credentials and provider keys live only in the keychain (or are referenced from
  environment variables). Config files never hold secrets, and the config loader rejects plaintext keys.
- Deleting an item deletes its chunks and vectors.

**Not encrypted:** the **Markdown vault** (once rendered) is plain text by design, so you can read,
edit and sync it; email addresses and phone numbers are kept out of it by default. **Logs** are plain
text but never contain secrets, and never contain item content at `info` level or above. **Model
files** are public weights.

**Gaps:** on Windows the data directory and key file inherit ACLs, which is risky on non-system NTFS
volumes and FAT or exFAT drives (a protected DACL is planned). There is no key rotation yet
(`PRAGMA rekey`), and no migration of a file key into the keychain.

## Egress accounting

- Every request to a destination that is not on this device (`local_network` or `cloud`) records an
  **egress event**: when, host, feature, bytes sent and received, and the items whose content was
  included. The Privacy page shows the ledger.
- **Locality** is derived from the provider's address: loopback is on-device, private ranges are local
  network, everything else is cloud. Host names are resolved once, when the provider is built, and the
  result is pinned. A provider can be marked as further away than its address (`locality = "cloud"`),
  never closer.
- Plain `http` to an internet host is refused for providers; IMAP is TLS only; CalDAV is https only
  except to loopback. TLS uses rustls with the aws-lc-rs provider and the **OS trust store**.
- **Redirects are never followed** across hosts, so a provider or mail server cannot bounce a request,
  and its credentials, somewhere else.
- **Sensitive items** never go to a cloud model for triage, embedding, extraction or Ask.
- There is **no telemetry** and no first-party server.

**Gaps:** an event is written when an exchange ends, so a crash mid-request can leave no row (a
two-phase record is planned). Local servers that forward to the cloud (Ollama's `-cloud` models) look
on-device unless you mark them.

## Runtime isolation

**Local model servers** (`nugget-runtime`):

- `llama-server` binds **only** to `127.0.0.1`; `--host` and `--port` cannot be overridden through
  extra arguments. `--offline`, `--no-webui` and `--no-slots` are always set.
- Each server requires a **random 256-bit API key**, new on every start, passed in the environment
  (not on the command line, so not visible in `ps`) and redacted from captured logs.
- The child gets a **scrubbed environment**, runs in its own process group (Unix) or a kill-on-close
  job object (Windows), and is killed when the daemon exits. Orphans from a crash are reaped at the
  next start.
- **Who is on the port**: the port is picked by binding and releasing it, so another local user could
  race to bind it. On **Linux** the supervisor checks that every listener on that port belongs to the
  child's process group before trusting it. On **macOS and Windows** this is **not verified** yet; only
  the child's liveness is checked, and key rotation limits what a squatter could see.
- The `llama-server` binaries come from the signed manifest, pinned by release tag, size and SHA-256,
  and are unpacked as hostile input: no absolute paths or `..`, no hard links or devices, symlinks
  only to targets below their own directory, size and entry caps against decompression bombs, and no
  setuid bits.

**Model supply chain** (`nugget-models`): the manifest has a detached **ed25519** signature checked
before use; every entry pins an exact upstream commit (never a moving ref), size and SHA-256; downloads
are hash-checked before an atomic install; disk preflight fails closed. **Gap:** this build trusts only
a **development** signing key. An offline release key with rotation and revocation, and signed remote
manifest updates with anti-rollback, are planned.

**Connectors** run in-process as native Rust code. They get their credentials only through the
keychain interface, and every request they make goes through the metered HTTP client.

## Plugin sandbox

Status: being wired in (wave W). The host is built and tested in `nugget-plugin`; the daemon does not
load plugins yet.

- Plugins are **WASM components** run by wasmtime. A plugin imports only the host's `http`, `kv`,
  `secrets`, `log`, `clock` and (during a sync) `sink` interfaces. There is no filesystem, socket,
  environment or other clock: WASI imports are linked to traps.
- **Capabilities** are declared in the plugin's manifest, approved by you at install, and checked by
  the host on **every** import call, deny by default. Network access goes through the host, so it is
  recorded in the egress ledger.
- **Limits** on every call: fuel, an epoch deadline, a timeout and a cap on linear memory. Each call
  gets a fresh instance.
- **Packages** are signed with ed25519 over the SHA-256 of the manifest and the component, and are
  verified before install.

## MCP

The MCP server is wired into the daemon and local only: stdio through `nuggetd mcp --client <id>`, or,
when enabled, streamable HTTP on `127.0.0.1` with Host and Origin checks against DNS rebinding, closed
CORS and bearer tokens that never appear in URLs. Each connected app gets its **own token** (`ngt_…`),
stored only as a keyed hash, revocable, rate-limited and audited per call (an argument summary and item
references, never content). Tokens are **scoped per client**: a sources allow-list, a tools allow-list,
and sensitive items excluded unless the client is allowed them, applied down to individual fact
evidence and context passages. The tools are read-only. See [MCP tools and scopes](../reference/mcp.md).

## Actions: the policy engine

Status: being wired in (wave W). The engine is built and tested in `nugget-policy`, and the agent loop
(`nugget-agent`) and browser controller (`nugget-browser`) already ask it before every action; the
daemon does not run them yet.

Every action Nugget would take on your behalf (computer use, browser actions, proactive card actions)
is described with a **risk class** and passed to the policy gate first, which answers Allow, Confirm or
Deny.

| Risk class | Covers |
|---|---|
| Observe | Reading, navigating, scrolling, focusing |
| Edit | Typing into a field, selecting, opening a draft: undoable, nothing leaves the device |
| Send | Sending to another person or service: email, message, form submit, post |
| Destroy | Deleting, archiving or overwriting data, cancelling a subscription |
| Spend | Purchases, bookings, payments, signing |
| Security | Passwords, permissions, installs, system settings |

- **Classification can only raise risk.** The effective class is the higher of the one the tool
  declares and the one a deterministic classifier assigns.
- **Rules per app and site**, with built-in defaults. Deny always wins. An Allow rule that names no
  risk class covers only Observe and Edit; allowing Send and above needs the class named. With no
  matching rule, Send and above need confirmation.
- **Confirmations.** A Confirm decision waits for you. A timeout counts as a denial, and so does the
  kill switch. Per-task approvals and per-class rate limits bound what one task can do.
- **Kill switch.** One process-wide switch, checked before every step and action and while waiting
  for a confirmation; it can be bound to the global hotkey.
- **Action log.** Decisions and actions are appended to a log in which each entry carries the SHA-256
  of the previous one, exportable as JSONL. Secure-field text is redacted and card numbers are masked
  before anything is logged. Editing, inserting, deleting or reordering entries breaks the chain;
  truncating the tail is detectable only against a remembered head.
- **Done means verified.** The agent reports success only when every mutating action passed its
  after-action check.

## Double Tap and browser control

Status: being wired in (wave W). Both are built and tested; the daemon and app do not run them yet.

- **Accessibility access** (`nugget-a11y`) reads only the focused window of the frontmost app, never
  reads the value of a secure (password) field, and verifies an insertion by reading the field back.
  The clipboard fallback saves and restores every pasteboard type. Drafting refuses secure fields and
  apps you excluded before any context is read.
- **The global trigger** (`nugget-hotkey`) observes only the configured double tap or chord. For any
  other key it learns only that some key was pressed; key codes are dropped at once, and no key event
  is ever logged.
- **Browser control** runs in your own browser profile through the extension and the
  `nugget-browser-host` native-messaging host. Messages are size-capped in both directions. Every
  snapshot and action passes the policy gate, a site you have not allowed is neither read nor acted
  on, and each action is validated against the latest snapshot and verified afterwards.

## Prompt injection

Ingested content is **data, not instructions**:

- Ask's system prompt tells the model to treat passages as data and ignore instructions in them.
- MCP results fence source content in `<untrusted-content>` blocks that the content cannot close.
- The LLM fact extractor keeps a fact only if its quote is found verbatim in the source.
- Model output cannot widen permissions: in the product today no model output triggers any action. As
  actions are wired in (wave W), every one passes through the policy engine described above, with
  confirmation for risky actions, step and time budgets and a kill switch. The `cua` benchmark's
  adversarial tasks count unconfirmed risky actions, and the target is zero.

These measures reduce the risk; none of them eliminates it.

## Integrity: the audit log

Configuration changes, sign-ins, secret updates (account names only) and model installs are appended
to an audit log in the store. Each entry carries the SHA-256 of the previous one, and the daemon
verifies the chain at startup. **Gap:** the hash is unkeyed, so someone who holds the store key can
rewrite the chain and its head. An HMAC keyed from the store key, with the head anchored in the
keychain, is planned.

## Supply chain

`cargo deny` checks advisories, licences (permissive only; GPL, AGPL and LGPL are rejected) and
sources on every change; Dependabot keeps dependencies current; third-party CI actions are pinned.
Signed releases with threshold keys, a user-chosen update channel and opt-in auto-update are planned.
