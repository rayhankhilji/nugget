# What works today

Nugget is pre-release. **No release has been cut**: [build from source](guide/install.md#build-from-source)
or use the unsigned installers from a release dry run. This page summarises
[`docs/PLAN.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/PLAN.md), which lists every
open issue. Numbers come from [`bench/README.md`](https://github.com/rayhankhilji/nugget/blob/main/bench/README.md);
"local model" means a 4B-class model on CI runners unless stated.

| Area | State |
|---|---|
| Memory and search | Works. Change searchable in 0.6 s (p95, target 60 s); recall@10 1.00 vs 0.905 text-only. First cited result 3.6 s after a fresh install |
| Knowledge graph and MCP | Works; every vault fact cited. Local-model entity F1 0.67–0.77 (target 0.8). MCP for Claude Desktop, Claude Code, Cursor and others |
| Double Tap | Works in any app. **Cloud latency never measured.** Local: 0.1–0.6 s after read-ahead (needs 6–21 s of focus), 10–30 s without. Fact recall 0.62 (target 0.8). **Open Blocker:** obeys instructions planted in a vault note |
| Agent and computer use | Works behind the safety policy. Bench success 0.92 with the 4B; 0 unconfirmed risky actions. Does not reliably ask when a target is ambiguous; the harm check is off pending a decision |
| Proactive cards | Works. precision@5 0.883, recall 0.997, nothing runs without approval. `.ics` meeting prep is unreliable |
| Platforms | Installers build on all three OSes; AppImage, Apple-silicon dmg and NSIS pass a launch test; MSI, deb, rpm build only. Not tested on physical macOS or Windows machines |
| Privacy and security | Encrypted store, no telemetry, egress ledger. Open: two High privacy findings with cloud providers, one mitigated MCP port issue |
| Updates | Built and tested; nothing to update to until a release exists |

## Known gaps

- **Unsigned:** release keys and signing certificates do not exist, so Gatekeeper and SmartScreen warn
  ([how to get past that](guide/install.md#unsigned-builds)); signing is untested with real
  certificates. Linux packages are not GPG-signed.
- **Re-verified on the real 4B only:** Ask ignores planted instructions, card numbers stay out of the
  vault, and onboarding indexes in seconds (Linux only). Not yet run: the 8B, macOS and Windows
  onboarding, a real-model computer-use run.
- **Privacy with a cloud provider:** an Ask follow-up can forward earlier answers unscanned; Double Tap
  sent texts are readable by MCP clients and cloud Ask.
- **MCP:** an HTTP client can send its token to a program that took Nugget's port while `nuggetd` was
  down (mitigated, not fixed); the stdio bridge avoids it.
- **Knowledge:** vault edits can be overwritten by re-extraction.
- **Wayland:** the Double Tap shortcut is verified via the portal on KDE Plasma 6 and GNOME 50 only.
- **Not measured:** battery cost in joules. Drive, Microsoft 365, Slack, Notion, GitHub and Linear
  are tested against mock servers only.

## Not built yet

The end-to-end-encrypted relay; connectors for Apple Notes, Apple Mail, macOS Calendar and Contacts,
Signal, Thunderbird, Logseq, Maildir and CardDAV; MCP servers as sources; a community plugin registry.
