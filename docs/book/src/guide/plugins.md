> **Status:** you browse, install, approve, enable and remove plugins on the **Plugins** page, and
> plugin tools are available to the [agent](agent.md) as soon as a plugin is enabled. The daemon
> serves `plugins.list`, `plugins.inspect`, `plugins.catalog`, `plugins.install`, `plugins.approve`,
> `plugins.remove` and `plugins.enable`. Two gaps: a connector plugin becomes a source kind only
> after Nugget restarts, and the **Add source** wizard does not list plugin kinds yet, so a plugin
> source is added through [`sources.add`](../reference/ipc.md).

Plugins add **connectors** (new kinds of source, such as an RSS or Atom feed) and **read-only tools**
for the [agent](agent.md). They are WebAssembly components that run in a sandbox inside the daemon,
with only the capabilities you approve.

To write one, see the
[plugin author guide](https://github.com/rayhankhilji/nugget/blob/main/docs/plugins.md).

## Available plugins

The **Available plugins** section of the **Plugins** page lists the plugin packages (`.nuggetplugin`
files) found in the **plugin catalog folders**, so you can install one without hunting for the
file. Each entry shows the plugin's name, description and version, who signed it (**Trusted
signer**, **Developer build** or **Untrusted signer**) and, under **What it asks to access**, its
capabilities in plain language. An entry shows **Installed** when that version is already
installed, and **Update available** with an **Update** button when you have an older version.

Nugget checks every package in the catalog exactly as it would when installing it (signature
first, then the manifest), but installs nothing until you choose **Install**. A package that cannot
be read, is unsigned, or is signed by a key you do not trust is listed with the reason and no
install button.

The catalog folders are:

- `plugin-catalog` in Nugget's data directory (for example `~/.local/share/nugget/plugin-catalog`
  on Linux; see [Data directory](../reference/data-dir.md) for each platform). Drop a
  `.nuggetplugin` file there to list it.
- Or, instead of that default, the folders you list in `config.toml`:

  ```toml
  [plugins]
  catalog_dirs = ["/opt/nugget/plugins", "/home/me/plugins"]  # absolute paths
  ```

- Plus the folders in the `NUGGET_PLUGIN_CATALOG` environment variable of the daemon (a list
  separated like `PATH`: `:` on macOS and Linux, `;` on Windows). A distributor can set it to
  ship plugins with Nugget.

Only files directly in a catalog folder are listed (not subfolders), at most 64 in all.

## Installing a plugin

On the **Plugins** page, either choose **Install** (or **Update**) next to an available plugin, or:

1. Choose **Install from file** and pick a plugin package (a `.nuggetplugin` file on your computer).
2. Nugget checks its **signature** before reading anything else in it, and shows the plugin's name,
   version, publisher key and the **capabilities** it asks for.
3. Approve the capabilities, or cancel (see [The approval screen](#the-approval-screen)).
4. Enable the plugin. A tool plugin's tools become available to the agent at once. A connector
   plugin adds a source kind `plugin:<id>` after Nugget restarts (see the status note above).

You can disable or remove a plugin at any time. Installs, approvals and removals are in the audit
log.

## The approval screen

Before anything is installed, Nugget shows what the plugin asks for, one line per capability:

- **Connect to *host*** — it may make web requests to that site only (*any subdomain of* means
  `*.example.com`; *without encryption* means plain `http`, which a plugin must spell out).
- **Read a secret you set, named *name*** — it may read that one secret from your OS keychain. A
  plugin cannot read your other secrets or another plugin's.
- **Store up to *size* of its own data** — a private key-value store, for example to remember
  which items it already imported.
- **Read files in the folders you choose in the source settings** — a connector plugin may read
  (never write) files only inside the folder or file you pick when you add its source. It cannot
  see anything else on your computer.
- **This plugin asks for no access** — it can only compute on what Nugget hands it.

On an update, access the installed version did not have is marked **New**; an update that asks for
nothing new is installed without asking. The signer line shows the start of the publisher's key,
and a warning appears for a developer build or an untrusted signer. Choose **Allow and install** to
install, or **Cancel** to discard the package.

## First-party plugins

Three plugins are built with the plugin SDK in Nugget's repository (`plugins/`) and listed in
**Available plugins** when your copy of Nugget ships them in a catalog folder. Each is a connector:
after installing it and restarting Nugget, add a source of its kind (see the status note above).

### RSS and Atom feeds

Indexes the entries of RSS 2.0, Atom and RSS 1.0 feeds (one item per post, fetched with conditional
requests so unchanged feeds cost almost nothing), and gives the agent a tool that lists a feed's
latest headlines. After installing it and restarting Nugget, add a source of kind
`plugin:dev.nugget.rss` with these settings:

- **Feed URLs** (`feeds`, required): one to 100 feed addresses, for example
  `https://example.substack.com/feed`.
- **Entries per feed** (`max_entries`, optional): the most entries indexed from each feed per pass.

A plugin can reach only the hosts it declares, and you choose the feeds, so the approval screen
lists the feed *platforms* it covers: blog and newsletter hosts (`*.substack.com`,
`*.wordpress.com`, `*.blogspot.com`, `*.github.io`, Medium, ...), feed services (FeedBurner, hnrss,
arXiv, ...), GitHub and GitLab release feeds, YouTube channel feeds and `mastodon.social`. A feed on
any other host is refused; to follow one, its host must be added to the plugin's manifest and the
plugin rebuilt and signed again (see the plugin's `README.md`). It also asks for up to 4 MiB of
storage, to remember each feed's ETag and the entries it has already seen. Every request is in the
[egress ledger](privacy.md).

### Obsidian vault

Imports the Markdown notes of an Obsidian vault (or any folder of Markdown notes), with their front
matter, tags, aliases and `[[wikilinks]]` as metadata. Add a source of kind
`plugin:dev.nugget.obsidian` with:

- **Vault folder** (`vault`, required): the absolute path of the vault folder, the one that contains
  `.obsidian/`.
- **Folders to skip** (`exclude`, optional): vault-relative folders to leave out, such as
  `Templates` or `Archive/Private`.

It asks only to **read files in the folder you choose**: no network, no secrets, no storage. (Nugget
also has a built-in [Obsidian source](sources-obsidian.md) that opens results in Obsidian; the
plugin is a sandboxed alternative and an example of a file-reading plugin.)

### Read-later exports

Imports the articles you saved to a read-later service from the service's own export file, so no
account, password or network access is needed. First export your list:

- **Pocket**: use Pocket's export page; you get `ril_export.html` (or a CSV file such as
  `part_000000.csv` in the newer export).
- **Instapaper**: in Instapaper's settings, choose to export to CSV (`instapaper-export.csv`).
- **Pinboard, Raindrop or a browser**: export bookmarks as a Netscape bookmarks file
  (`bookmarks.html`).

Then add a source of kind `plugin:dev.nugget.read-later` with:

- **Export file or folder** (`export`, required): the absolute path of the export file, or of a
  folder holding export files.
- **Skip archived articles** (`skip_archived`, optional): leave out articles in Pocket's archive or
  Instapaper's Archive folder.

It indexes what the export holds (title, URL, tags, folder, date and any highlighted selection) and
asks only to **read the file or folder you choose**. Export again (to the same place) and the next
sync picks up the new articles.

## Signatures and trust

- A package holds a manifest, the component and a detached **ed25519** signature over the SHA-256
  of both, so neither can be swapped.
- Only packages signed by a publisher key you trust are installed. The list of trusted keys is part
  of the `[plugins]` section of `config.toml`.
- Unsigned plugins are refused unless you turn on an explicit developer mode.
- An update must have a higher version (no downgrades) and be signed by the same key as the
  installed version. You are asked again only when the update requests more than you approved.
- Every time a plugin is loaded, its files are re-hashed and its signature re-checked, so a plugin
  changed on disk is refused.

## Capabilities

A plugin can do nothing it has not declared and you have not approved. Every call it makes is
checked by the host, and anything not approved is denied.

| Capability | What it allows |
|---|---|
| Hosts | HTTP requests to the listed hosts only (`https` unless plain `http` is spelled out). Requests to other hosts are refused |
| Secrets | Reading its own named secrets, stored in your OS keychain under `plugin:<id>:<name>` |
| Key-value store | A private store of at most the approved size, under the data directory |
| File roots | A connector may read (never write) files below the folder or file you chose in the named source settings fields, during validation and sync only. `..`, absolute paths and symlinks that leave the folder are refused |

There is no other filesystem access, no sockets, no environment variables and no access to other
plugins, your store or your other sources. Tools are read-only: during a tool call a plugin may only make `GET` and
`HEAD` requests and read its store and secrets.

A declared host that is, or resolves to, a loopback, private, link-local or other non-public address
is refused, and the request connects only to the addresses that were checked.

## Limits

Each call runs in a fresh instance, so nothing leaks between calls. By default:

| Limit | Default |
|---|---|
| Memory | 64 MiB |
| Tool call time | 15 s |
| Sync time | 30 minutes |
| HTTP requests per tool call | 32 |
| HTTP response size | 16 MiB |

A CPU budget and a wall-clock deadline interrupt even a plugin stuck in a loop.

## Privacy

Plugins have no network access except through Nugget's own network layer, so every request a plugin
makes is recorded in the [egress ledger](privacy.md): feature `sync:plugin:<id>` for a connector's
syncs and `tool:plugin:<id>` for tool calls. Content a plugin brings in is indexed like any other
source and is treated as untrusted when it reaches a model.
