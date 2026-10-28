# Working on these docs

This book lives in `docs/book/` and is built with [mdBook](https://rust-lang.github.io/mdBook/).
mdBook is licensed MPL-2.0; it is a build tool only, not a dependency of any Nugget crate, and nothing
it produces is linked into the product. The book is not hosted anywhere yet; CI only builds and checks
it.

```sh
cargo install mdbook --version 0.4.52 --locked
cargo install mdbook-linkcheck --version 0.7.7 --locked   # optional locally
mdbook serve docs/book            # preview on http://localhost:3000
mdbook build docs/book            # output in docs/book/build/ (git-ignored)
python3 docs/book/check-refs.py   # the drift check
```

## The drift check

Reference pages are only useful if they are right. `docs/book/check-refs.py` (Python 3.11+, standard
library only) compares the book with the source code and fails CI when they disagree.

**Marked tables.** A table that documents a set of names from the code is preceded by an HTML comment:

```markdown
<!-- check-refs: config.runtime -->
| Key | Type | Default | Description |
```

For each marker the script reads the first-column names (the first backticked token in each row) and
compares them with the set it extracts from the source. The comparison is **both ways**: a documented
name that does not exist fails, and so does a name in the code that is not documented. Several tables
may share one marker (the IPC method tables do); their rows are combined.

| Marker | Compared with |
|---|---|
| `config.sections`, `config.models`, `config.models.selected`, `config.runtime` | The `Config`, `ModelsConfig`, `TierSelection` and `RuntimeConfig` structs in `crates/nuggetd/src/config.rs` |
| `config.ingest`, `config.retrieve`, `config.retrieve.recency` | `IngestConfig` (`nugget-ingest`), `RetrieveConfig` and `RecencyBoost` (`nugget-retrieve`) |
| `config.providers` | `ProviderConfig` (`nugget-infer`) |
| `cli.commands`, `cli.flags` | The clap definitions in `crates/nuggetd/src/main.rs` |
| `env.daemon`, `env.mcp`, `env.dev` | String literals `"NUGGET_…"` in the source (one way: documented names must exist) |
| `ipc.methods`, `ipc.notifications` | The `method` and `notification` constants in `nugget_core::ipc` |
| `ipc.errors` | The `code` constants in `nugget-ipc`, names **and** values |
| `mcp.tools`, `mcp.scopes` | The tool constants in `nugget-mcp` and the `McpScopes` struct |
| `connector.kinds` | Each connector's kind constant and display name |
| `connector.files`, `connector.obsidian`, `connector.gmail`, `connector.gcal`, `connector.imap`, `connector.caldav` | Each connector's settings struct |
| `crates` | The workspace members' package names |

**Extra checks on those tables:**

- **Defaults.** In config tables with a Default column, a default that is a plain number or boolean is
  compared with the literal in the struct's `impl Default`.
- **Daemon support.** The IPC method tables have a Daemon column. `yes` must match a handler arm in
  `crates/nuggetd/src/handler.rs`, and `not yet` must not. When the knowledge-graph or MCP methods are
  wired in, the check fails until this book is updated, which is the point.

**Whole-book scans:**

- Any backticked token that looks like an IPC method or a dotted config key (`sources.add`,
  `ingest.allow_cloud_embeddings`) must be a real method, notification or config key.
- Any `nuggetd <command>` or `--flag` in a shell example must be a real command or flag, or be listed
  in the script's `PLANNED` table (today: `nuggetd mcp --client`). A planned entry that appears in the
  code fails the check, so the "planned" notes get removed when the feature lands.
- Every `NUGGET_…` variable mentioned must exist in the source.
- Every ```` ```toml ```` block is parsed and its sections and keys are validated against the config
  structs.
- Every relative link must point at an existing page, and its `#anchor` at an existing heading.

The script parses Rust with regular expressions, not a Rust parser. It is deliberately simple and tied
to the current code layout; if you move a struct, update the `SOURCES` table at the top of the
script.

## Style

- Plain, direct sentences; second person for the user guide.
- Say what is not done yet, where it matters, in the page itself.
- Numbers only from `nugget-bench` reports, with their conditions.
- Facts about Sentient OS only from `docs/research/`.
