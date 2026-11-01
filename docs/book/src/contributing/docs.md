# Working on these docs

The book lives in `docs/book/` and is built with [mdBook](https://rust-lang.github.io/mdBook/)
(MPL-2.0, a build tool only, never linked into Nugget). CI builds and checks it; it is not hosted yet.

```sh
cargo install mdbook --version 0.4.52 --locked
mdbook serve docs/book            # preview on http://localhost:3000
python3 docs/book/check-refs.py   # the drift check
```

## The drift check

`docs/book/check-refs.py` (Python 3.11+, standard library) fails CI when the book and the code disagree.
Its docstring and `SOURCES` table say exactly what it reads.

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

The Rust is parsed with regular expressions; if a struct moves, update `SOURCES`.

## Style

Plain, direct sentences, second person in the user guide. Say what is not done yet on the page itself.
Numbers only from `nugget-bench` reports, with their conditions. Facts about Sentient OS only from
`docs/research/`.
