# Plugins

> **Status: being wired in (wave W).** The plugin host (`nugget-plugin`: WebAssembly components,
> capabilities, limits, signed packages, the plugin store) and its guest SDK are built and tested.
> The daemon methods (`plugins.list`, `plugins.inspect`, `plugins.install`, `plugins.approve`,
> `plugins.remove`, `plugins.enable`), plugin sources in the Sources wizard, plugin tools for the
> agent and the **Plugins** page are being connected in this wave. This page describes the
> behaviour of the built host.

Plugins add **connectors** (new kinds of source, such as an RSS or Atom feed) and **read-only tools**
for the [agent](agent.md). They are WebAssembly components that run in a sandbox inside the daemon,
with only the capabilities you approve.

To write one, see the
[plugin author guide](https://github.com/rayhankhilji/nugget/blob/main/docs/plugins.md).

## Installing a plugin

On the **Plugins** page:

1. Choose a plugin package (a `.nuggetplugin` file on your computer).
2. Nugget checks its **signature** before reading anything else in it, and shows the plugin's name,
   version, publisher key and the **capabilities** it asks for.
3. Approve the capabilities, or cancel.
4. Enable the plugin. A connector plugin then appears as a new kind in the Sources wizard
   (`plugin:<id>`); a tool plugin's tools become available to the agent.

You can disable or remove a plugin at any time. Installs, approvals and removals are in the audit
log.

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

There is no filesystem, no sockets, no environment variables and no access to other plugins, your
store or your other sources. Tools are read-only: during a tool call a plugin may only make `GET` and
`HEAD` requests and read its store and secrets.

Blocking declared hosts that resolve to a private or loopback address is being added in this wave.

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
