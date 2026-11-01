# Plugins

Plugins add **connectors** (new kinds of source, such as RSS feeds) and **read-only tools** for the
[agent](agent.md). They are WebAssembly components that run sandboxed inside the daemon with only the
capabilities you approve. To write one, see the
[plugin author guide](https://github.com/rayhankhilji/nugget/blob/main/docs/plugins.md).

**Current gaps:** a connector plugin becomes a source kind only after Nugget restarts, and the **Add
source** wizard doesn't list plugin kinds yet, so add a plugin source through
[`sources.add`](../reference/ipc.md).

## Installing a plugin

On the **Plugins** page, choose **Install** (or **Update**) next to an entry under **Available
plugins**, or **Install from file** and pick a `.nuggetplugin` file. Nugget checks the signature
first, then shows the plugin's name, version, signer and what it asks to access. Choose **Allow and
install** or **Cancel**, then enable it. Tools are available at once; a connector adds a source kind
`plugin:<id>` after a restart. You can disable or remove a plugin at any time.

**Available plugins** lists packages in the catalog folders: `plugin-catalog` in the
[data directory](../reference/data-dir.md), or instead the folders you set in `config.toml`, plus any
in the daemon's `NUGGET_PLUGIN_CATALOG` variable (separated like `PATH`):

```toml
[plugins]
catalog_dirs = ["/opt/nugget/plugins", "/home/me/plugins"]  # absolute paths
```

Only files directly in a folder are listed (at most 64). Unreadable, unsigned or untrusted packages
are shown with the reason and no install button.

## What a plugin can ask for

| On the approval screen | What it allows |
|---|---|
| **Connect to *host*** | HTTP requests to that host only (`https` unless plain `http` is spelled out). Hosts on loopback or private addresses are refused |
| **Read a secret you set, named *name*** | That one secret from your OS keychain (`plugin:<id>:<name>`) |
| **Store up to *size* of its own data** | A private key-value store |
| **Read files in the folders you choose** | Read-only access to the folder or file you pick in the source settings, nothing else |
| **This plugin asks for no access** | It can only compute on what Nugget gives it |

Nothing else: no other files, sockets, environment, other plugins or your store. Tools may only make
`GET` and `HEAD` requests. On an update, new access is marked **New**; an update that asks for
nothing new installs without asking.

Each call runs in a fresh instance limited to 64 MiB of memory, 15 s per tool call, 30 minutes per
sync, 32 HTTP requests per tool call and 16 MiB per response.

## Signatures and trust

- Packages are signed with **ed25519**; only keys you trust (in `[plugins]` in `config.toml`) are
  accepted. Unsigned plugins need an explicit developer mode.
- Updates must have a higher version and the same signing key.
- Files are re-hashed and the signature re-checked on every load, so a plugin changed on disk is
  refused.

## First-party plugins

These connectors are built in Nugget's repository (`plugins/`) and appear under **Available plugins**
when your copy ships them.

- **RSS and Atom feeds** (`plugin:dev.nugget.rss`): settings **Feed URLs** (`feeds`, 1 to 100) and
  **Entries per feed** (`max_entries`). Also gives the agent a latest-headlines tool. It can only
  reach the feed platforms it declares (Substack, WordPress, Blogspot, GitHub Pages, Medium,
  FeedBurner, hnrss, arXiv, GitHub and GitLab releases, YouTube, `mastodon.social` and similar);
  other hosts are refused unless the plugin is rebuilt and re-signed.
- **Obsidian vault** (`plugin:dev.nugget.obsidian`): **Vault folder** (`vault`) and **Folders to
  skip** (`exclude`). Reads only that folder. Nugget also has a built-in
  [Obsidian source](sources-obsidian.md).
- **Read-later exports** (`plugin:dev.nugget.read-later`): **Export file or folder** (`export`) and
  **Skip archived articles** (`skip_archived`). Use Pocket's export (`ril_export.html` or CSV),
  Instapaper's CSV export, or a Netscape `bookmarks.html` (Pinboard, Raindrop, browsers). Export
  again to the same place to pick up new articles.

## Privacy

Every plugin request goes through Nugget's network layer and is in the [egress ledger](privacy.md)
(`sync:plugin:<id>` or `tool:plugin:<id>`). Content a plugin brings in is treated as untrusted when it
reaches a model. Installs, approvals and removals are in the audit log.
