# Security model

Nugget indexes mail, notes and files, holds credentials for your accounts, and (later) will act on your
computer. This page describes what it protects, against whom, how, and where the current gaps are. To
report a vulnerability, follow
[`SECURITY.md`](https://github.com/rayhankhilji/nugget/blob/main/SECURITY.md): privately, never in a
public issue.

## Threat model

**Assets**

- **Your content**: items, chunks, embeddings, the knowledge graph, drafts.
- **Secrets**: provider API keys, connector OAuth tokens and passwords, OAuth client secrets, the store
  key, MCP client tokens.
- **Authority**: the ability to send, change or delete things on your behalf (planned features).
- **Integrity**: the audit log, the egress ledger, model files and the app itself.

**Adversaries**

1. Someone who controls **content Nugget ingests**: an email, a shared document, a calendar invite.
   This includes **prompt injection**.
2. A malicious or compromised **model provider**, **MCP client**, or (later) **plugin**.
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

**Plugins** (planned, M6) will run as **WASM components** under wasmtime, with no ambient file,
network or process access: only the capabilities they declare and you approve, with network access
mediated (and recorded) by the host.

## MCP

The MCP server (built, not yet wired in) is local only: stdio through the daemon, or HTTP on
`127.0.0.1` with Host and Origin checks, closed CORS and bearer tokens that never appear in URLs. Tokens
are stored as keyed HMACs, scoped per client (sources, tools, sensitive items), revocable, rate-limited
and audited per call. See [MCP tools and scopes](../reference/mcp.md).

## Prompt injection

Ingested content is **data, not instructions**:

- Ask's system prompt tells the model to treat passages as data and ignore instructions in them.
- MCP results fence source content in `<untrusted-content>` blocks that the content cannot close.
- The LLM fact extractor keeps a fact only if its quote is found verbatim in the source.
- Model output cannot widen permissions: today no model output triggers any action. When actions arrive
  (M4), every one passes through the policy engine (`nugget-policy`) with inline confirmation for risky
  actions, step and time budgets and a kill switch, and red-team tests target zero unconfirmed risky
  actions.

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
