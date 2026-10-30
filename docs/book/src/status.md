# What works today

Last updated: 2026-10-30

Nugget is pre-release. **No release has been cut.** Everything below runs in the daemon and the
desktop app on the development branch, with CI green on macOS, Windows and Linux, but there is no
published installer yet: you can [build from source](guide/install.md#build-from-source) or use the
unsigned installers from a release dry run ([Install](guide/install.md)).

This page is a plain summary of [`docs/PLAN.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/PLAN.md)
and its v1 checklist. Numbers come from the benchmark suites
([`bench/README.md`](https://github.com/rayhankhilji/nugget/blob/main/bench/README.md)); where a
number comes from a scripted or fake model rather than a real one, it says so. "Local model" means a
4B-class model running on CPU or a laptop GPU unless stated otherwise.

## At a glance

| Area | State |
|---|---|
| Memory and search | Works; targets met |
| Knowledge graph and MCP | Works; extraction quality with a local model below target |
| Double Tap | Works; cloud latency not measured, local latency met only after read-ahead |
| Agent and computer use | Works; targets met with the 4B local model, the 8B is limited by CPU speed |
| Proactive cards | Works; targets met |
| Platforms and installers | Installers build and pass smoke tests on all three OSes; unsigned; Wayland global shortcut does not fire |
| Privacy and security | Works; security review reconciled; release keys not created |
| Updates | Built and verified in tests; nothing to update to until a release exists |

## Memory and search

- **Sources:** folders, Obsidian vaults, Gmail, Google Calendar, Google Drive, IMAP, CalDAV,
  Microsoft 365 (Outlook mail and calendar, OneDrive), Slack, Notion, GitHub and Linear. Google
  Drive, Microsoft 365 and the Slack, Notion, GitHub and Linear connectors are tested against mock
  servers only, not yet against live accounts. See [Adding sources](guide/sources.md).
- **Freshness:** a change is searchable within 0.6 s (95th percentile) on the benchmark, against a
  60 s target. Edits and deletions propagate.
- **Search quality:** on the evaluation set, hybrid search finds the right item in the top 10 every
  time (recall@10 1.00, against 0.905 for text-only search). On the larger synthetic corpus it misses
  its gate by one query in 500 (0.936 against 0.938), with a placeholder embedder; a re-run with a
  real embedding model is pending. See [Benchmarks](architecture/benchmarks.md).
- **First run:** from an empty install to a cited search result on the bundled sample folder takes
  3.6 s in the desktop end-to-end test (target: under 2 minutes). With the recommended local model
  installed during onboarding, the first cited answer comes 5.2 s after the app starts; that test
  serves the model from a local file, so your download time comes on top.
- **Resources** at 100,000 items: idle CPU 0.03%, daemon startup 74 ms, first search 83 ms. Memory
  after idle was 1.4 GiB, most of it the drafting voice profile, which has since been cut from about
  980 MiB to about 106 MiB (measured on its own; the daemon has not been re-measured). Battery cost
  is **not measured**.

## Knowledge graph and MCP

- **Every vault fact cites its source items** (citation coverage 1.0, checked in CI). See
  [The knowledge graph and vault](guide/knowledge.md).
- **Extraction with a local model is below target.** Entity F1 is 0.67–0.77 on 93 test items
  (target 0.8); claim F1 is 0.66–0.67, with no unsupported claims. Nicknames and job changes are
  still often missed.
- **MCP:** Claude Desktop, Claude Code, Cursor and other apps can query Nugget over stdio or loopback
  HTTP with a scoped token per app; `nuggetd mcp install` sets an app up, and a test connects clients
  configured exactly that way. One known weakness: an HTTP client can send its token to another
  program that took Nugget's port while `nuggetd` was down; the stdio bridge avoids this. See
  [Connecting AI apps over MCP](guide/mcp.md).

## Double Tap

- **Works in any app:** press the shortcut and a draft in your voice is inserted where you type. See
  [Double Tap](guide/double-tap.md).
- **Latency with a cloud model (target: under 1.2 s to the first inserted character): not
  measured.** It needs a provider key and has not been run.
- **Latency with a local model (target: under 3 s): partly met.** Once read-ahead has finished, the
  first character appears in 0.1–0.6 s on every machine measured. Read-ahead needs the reply field
  to keep focus for 6–21 s first (4.5–11 s with the optional smaller Double Tap model). Without
  read-ahead it takes 10–30 s (3–15 s with the smaller model), which misses the target. A
  trigger pressed while read-ahead is still running has not been measured.
- **Quality:** on the drafting benchmark with a local model, drafts invent nothing (hallucination 0)
  and leak no marked-sensitive content, and match your voice better than a screenshot-only baseline
  (0.80 against 0.62). They recall fewer of the relevant facts than the baseline (0.62 against 0.81;
  target 0.8), and one test draft still mentions an attachment that does not exist.
- **Wayland:** the global shortcut does not fire through the desktop's shortcut portal yet. Start
  Double Tap from the tray or the app instead. X11, macOS and Windows are not affected.

## Agent and computer use

- **The agent uses your desktop apps** through the accessibility APIs, and your browser through the
  extension, behind the safety policy: risk classes, confirmations, budgets, a kill switch (Escape
  three times) and a hash-chained action log. See [The agent](guide/agent.md) and
  [Safety](guide/safety.md).
- **Task success** on the computer-use benchmark with real local models: 0.92 with the 4B model
  (target 0.8). The 8B model reaches 0.83 on a faster CPU but only 0.67 on the slowest runner, where
  its turns time out.
- **Safety:** 0 risky actions without confirmation and 0 cases of following injected instructions
  in every measured configuration. Needless confirmations are 0 with the 4B model (target at most
  0.10).
- **Not on yet:** the harm check that refuses goals such as fraud, impersonation or harassment runs
  in the benchmark but is off in the daemon; whether to turn it on for local models is still to be
  decided. The agent does not ask when a name is ambiguous (two people called Sam).

## Proactive cards

- **The Today page** shows cards for things that need you, each with why it is shown and a dry run
  of what it would do. Nothing runs until you approve it. See [Today](guide/today.md).
- **Measured:** precision@5 0.883 (target 0.7) and recall 0.997 (target 0.8); accepted-card
  precision 0.976; every action has a dry-run diff; 0 actions without approval. Cards learn from
  your Accept and Dismiss reasons, and you can see and reset what they learned.

## Platforms and installers

- **macOS** (Apple Silicon and Intel, 11 or later), **Windows** 10 and 11 (x64) and **Linux** (x64
  and arm64, X11 and Wayland).
- The release workflow's dry run builds every installer: `.dmg` on macOS, NSIS and MSI on Windows,
  AppImage, `.deb` and `.rpm` on Linux. The AppImage, the `.dmg` and the NSIS installer are
  installed, launched, connected to the daemon and quit in CI. See [Install](guide/install.md).
- Accessibility, Double Tap insertion and computer use have run on real macOS and Windows machines
  in CI (computer use through macOS accessibility: 16 of 16 tasks; through Windows UI Automation with a scripted model, success
  1.00). Accessibility and Double Tap insertion have also run on Wayland (sway, KWin).
- **Until release keys exist, installers are unsigned:** macOS Gatekeeper and Windows SmartScreen
  warn when you open them ([how to get past that](guide/install.md#unsigned-builds)).
- Linux packages are not GPG-signed and there is no apt or rpm repository.

## Privacy and security

- Encrypted store (SQLCipher, key in your OS keychain), no telemetry, no Nugget servers, and an
  [egress ledger](guide/privacy.md) of every request that leaves the device. Sensitive items are
  withheld from cloud models; on the drafting benchmark (with a fake model marked as cloud), no
  secrets reached the prompt or the draft.
- **Security review** of Double Tap, the agent, the browser extension, plugins, proactive cards,
  connectors and MCP: 13 findings fixed with regression tests, one mitigated (the MCP port issue above). See
  [Security model](architecture/security.md) and the
  [review](https://github.com/rayhankhilji/nugget/blob/main/docs/reviews/security-m3-m6.md).
- **Supply chain:** model downloads come from a signed, pinned manifest with rollback protection;
  release builds record SBOMs, checksums and build provenance. `nuggetd` on x86_64 Linux rebuilds
  identically in code and data; the installers are not reproducible
  ([details](https://github.com/rayhankhilji/nugget/blob/main/docs/security/reproducible-builds.md)).
- **Release keys have not been created:** the updater key, the model manifest release key, and the
  Apple and Windows signing certificates. A release built before the manifest release key exists
  would offer no model downloads.

## Updates

- The in-app updater is built: you choose the channel (Stable, Beta or Off), and by default it only
  tells you about an update; nothing downloads or installs without you. The update feed and every
  update are signature-checked before use, and it never offers an older version. See
  [Install](guide/install.md#updates).
- Nothing can update yet: there is no release, and builds made without the updater key (dry runs,
  your own builds) have no updater.
- Update checks are made by the desktop app, so they are not in the daemon's egress ledger.

## Not built yet

- The end-to-end-encrypted relay for remote access (optional in the plan).
- Connectors that read local app data (Apple Notes, iMessage, Apple Mail, Contacts, WhatsApp,
  Signal, Thunderbird), CardDAV, and MCP servers as sources.
- An OSWorld-style subset of the computer-use benchmark.

The [benchmark dashboard](benchmarks/dashboard.md) charts the nightly results over time.
