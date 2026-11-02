# Heed and Sentient OS

Heed started as an answer to [Sentient OS](https://github.com/Sentient-OS-Labs/sentient-os), an
AGPL-3.0, macOS-only personal AI. The sourced, area-by-area comparison is
[`docs/compare/sentient-os.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/compare/sentient-os.md),
based on clean-room research of Sentient v2.7/v2.7.1 that may be out of date. In short:

- **Heed differs by design:** three OSes; continuous indexing where edits and deletes propagate
  (Sentient batches nightly and never re-reads edits); hybrid search with citations instead of sending
  the whole vault; a typed, cited knowledge graph; an encrypted store, no telemetry and an egress
  ledger; a local MCP server with scoped tokens (Sentient's mirror puts a password in the URL); its own
  agent with confirmations and a kill switch (Sentient wraps OpenAI's helper without them).
- **Sentient is ahead on:** a packaged, signed macOS release (Heed has none yet), Apple Notes and
  Apple Mail sources, and easier hosted connectors.
- No head-to-head benchmark exists. Heed's own numbers and gaps: [What works today](status.md).
