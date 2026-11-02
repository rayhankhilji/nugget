# Working on these docs

The book lives in `docs/book/` and is built with [mdBook](https://rust-lang.github.io/mdBook/)
(MPL-2.0, a build tool only, never linked into Heed). CI builds and checks it; it is not hosted yet.

Install with `cargo install mdbook --version 0.4.52 --locked`, preview with `mdbook serve docs/book`,
and run the drift check with `python3 docs/book/check-refs.py`.

## The drift check

- **Marked tables.** A table preceded by `<!-- check-refs: GROUP -->` is compared both ways with the
  names the script extracts for GROUP (config structs, CLI, `NUGGET_*` variables, IPC methods,
  notifications and error codes, MCP tools and scopes, connector kinds and settings, workspace crates).
  Several tables may share a marker. Numeric and boolean defaults are compared with `impl Default`. The
  IPC **Daemon** column must be `yes` (served), `not yet` (not served) or `in progress` (being wired
  on another branch; may not exist yet).
- **Whole-book scans.** Backticked dotted names that look like IPC methods or config keys must exist;
  `nuggetd` commands and flags in shell examples must exist (or be in `PLANNED`); `NUGGET_*` variables
  must appear in the source; ```` ```toml ```` blocks must parse against the config structs (so do not
  write feature sections such as `[draft]` as toml blocks); relative links and `#anchors` must resolve.

`check-refs.py` fails CI when the book and code disagree; its docstring has the details. It parses
Rust with regular expressions, so update its `SOURCES` table if a struct moves.

## Style

Plain, direct sentences; say what is not done yet on the page itself. Numbers only from `nugget-bench`
reports, with conditions. Facts about Sentient OS only from `docs/research/`.
