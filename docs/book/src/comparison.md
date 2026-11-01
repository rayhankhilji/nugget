# Nugget and Sentient OS

Nugget started as an answer to [Sentient OS](https://github.com/Sentient-OS-Labs/sentient-os), an
AGPL-3.0, macOS-only personal AI. Claims about Sentient come from Nugget's clean-room research on
v2.7/v2.7.1 ([`docs/research/`](https://github.com/rayhankhilji/nugget/tree/main/docs/research)) and
may be out of date. The sourced version is
[`docs/compare/sentient-os.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/compare/sentient-os.md).

| Area | Sentient OS (observed) | Nugget |
|---|---|---|
| Platforms | macOS 15.4+, Apple Silicon | macOS, Windows, Linux; **no signed release yet** |
| Licence | AGPL-3.0, CLA | Apache-2.0, DCO |
| Indexing | Nightly batch via a root wake daemon; edits never re-read | Continuous, per-user; change searchable in 0.6 s (p95); edits and deletes propagate |
| Search | No index; Double Tap sends the whole vault (600 KB cap, silently truncated) | Hybrid full-text and vector search, budgeted context with `[n]` citations |
| Knowledge | LLM-written Markdown, no provenance or validation | Typed graph, every fact with a verbatim quote, rendered vault with your edits merged back |
| Mail and calendar | Cloud CLI via vendor-hosted MCP; needs a ChatGPT or Claude plan | Direct APIs with your own OAuth client, plus IMAP and CalDAV |
| At rest | Not encrypted | SQLCipher, key in the OS keychain |
| What leaves | Telemetry on by default; Double Tap via a closed relay | No telemetry; every off-device request in an egress ledger |
| Sharing with AIs | Hosted "MCP mirror", password in the URL | Local MCP server, scoped per-client tokens |
| Computer use | OpenAI's closed helper; no step cap, confirmations or kill switch | Own agent and policy engine: risk classes, confirmations, kill switch, hash-chained log |
| Engineering | No unit-test target, no build CI | CI on three OSes, tests, public benchmarks |

**Where Sentient is ahead:** a packaged, signed macOS app; Apple Notes and Apple Mail sources; easier
hosted connectors. No head-to-head benchmark exists.
