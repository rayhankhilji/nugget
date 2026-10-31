# RSS and Atom feeds (`dev.nugget.rss`)

A connector that indexes the entries of RSS 2.0 (and 0.9x), Atom 1.0 and RSS 1.0 (RDF) feeds,
plus a `feed_headlines` tool for the agent.

## Settings

```json
{ "feeds": ["https://example.substack.com/feed", "https://hnrss.org/frontpage"], "max_entries": 200 }
```

- `feeds`: 1 to 100 feed URLs (`https://` or `http://`, no credentials). Duplicates and fragments
  are dropped.
- `max_entries` (optional, 1 to 500, default 200): most entries taken from each feed per pass, in
  the order the feed lists them.

## What it does

- **Documents.** One per entry, keyed `<feed url>#<entry id>` (the id is the RSS `guid`, Atom
  `id`, RDF `rdf:about`, else the link, else a hash of title and content; over-long keys are
  hashed). Content is `content:encoded`, Atom `content` or the description/summary, as
  `text/html` (or `text/plain` for Atom text content). Title, link, author, categories and dates
  go into the document and its metadata (`feed_url`, `feed_title`, `feed_format`, `entry_id`,
  `author`, `categories`, `published_at`).
- **Conditional GET.** Each feed's `ETag` and `Last-Modified` are kept in the plugin's key-value
  store; an unchanged feed costs one `304 Not Modified`.
- **Incremental by entry id.** The store also keeps a hash of each entry's id and version (the
  feed's updated date, else a hash of the content). Only new or changed entries are sent.
- **Crash-safe state.** The stored state is tagged with the pass that wrote it and the cursor names
  that pass. If a pass's cursor never committed, its state does not match and the feed is fetched
  and sent in full on the next pass, so nothing is skipped.
- **Reconciling pass** (no cursor): fetches every feed unconditionally, re-sends every entry
  (unchanged versions are cheap), and deletes the documents and stored state of feeds removed
  from the settings. Entries that only dropped off the end of a feed are kept.
- **Failures.** One failing feed (a 5xx, a parse error, an undeclared host) is logged and skipped,
  and its documents stay; the pass fails only if every feed failed. Redirects are not followed by
  the host; a `3xx` is reported with its `Location` so the user can update the URL.
- **Parsing** uses `roxmltree` with DTDs disabled: a `<!DOCTYPE>` (internal subset included) is
  removed before parsing, so no entity is ever declared or expanded, and references to it stay
  literal text. Bodies over 4 MiB are refused before parsing and there is a node limit, so one
  hostile feed cannot exhaust the plugin's memory; it fails alone. Only `http(s)` links are kept
  (`javascript:` and other schemes are dropped), and dates outside years 1 to 9999 are left out. Common breakage is repaired: HTML named entities (`&nbsp;`), bare
  `&`, Latin-1 / Windows-1252 bodies, a BOM, relative links (`xml:base` too), and dates in either
  RFC 822 or ISO 8601 with the usual mistakes.

`feed_headlines` takes `{ "url": "...", "limit": 10 }` (1 to 50) and returns the feed's title,
link, format and items (`title`, `link`, `date`, `date_ms`). It only sends `GET` and stores
nothing. Because the agent picks the URL (and may have been steered by text it read), the URL may
not carry a query string or fragment: a declared host cannot be used to smuggle data out in one.
Feeds that need a query string can still be followed by the connector.

## Hosts

The host lets a plugin reach only the hosts its manifest declares (and the user approved), and a
manifest is static, signed data: a plugin cannot widen it from its settings, and the host does not
accept a bare `*`. So no manifest can cover "any feed the user types".

This plugin declares a fixed list of **feed platforms** rather than individual sites:
`*.substack.com`, `*.wordpress.com`, `*.blogspot.com`, `*.tumblr.com`, `*.ghost.io`,
`*.bearblog.dev`, `*.micro.blog`, `*.hashnode.dev`, `*.github.io`, `medium.com`, `*.medium.com`,
`dev.to`, `feeds.feedburner.com`, `hnrss.org`, `news.ycombinator.com`, `lobste.rs`,
`www.reddit.com`, `rss.arxiv.org`, `export.arxiv.org`, `github.com`, `gitlab.com`,
`www.youtube.com` and `mastodon.social`.

Why this choice:

- **Reviewable.** The approval dialog shows exactly these hosts. Each is a publishing platform or
  feed service whose feeds are public; none of them is an API holding the user's private data.
- **Least privilege.** A plugin that can fetch arbitrary URLs could be pointed (by a settings
  change, or a tool call steered by injected text) at anything on the internet. The list keeps
  the blast radius to feed hosts, and every request is metered in the egress ledger anyway.
  Private, loopback and link-local addresses are always refused by the host regardless.
- **It fails clearly.** A feed on any other host fails with `denied` ("… is not a host declared in
  the manifest"), logged per feed, without failing the other feeds.

**To follow a feed on another host**, add the host to `capabilities.hosts` in
`nugget-plugin.toml` (a plain host such as `www.example.com`, or `*.example.com` for its
subdomains), raise `version`, then build and install your copy: sign it with your own key (see
`plugins/template/README.md`), or install the package directory unsigned with `dev_mode = true`.
The daemon asks you to approve the grown host list. Keep the list to hosts you trust; at most 64.

## Building and testing

```sh
cd plugins
cargo test-native -p nugget-rss    # native tests, fixtures in tests/fixtures, no network
cargo build --release -p nugget-rss
# -> target/wasm32-wasip2/release/nugget_rss.wasm (or under $CARGO_TARGET_DIR)
```

`plugins/build.sh` builds, packages and (with `NUGGET_PLUGIN_SIGNING_KEY`) signs it.
