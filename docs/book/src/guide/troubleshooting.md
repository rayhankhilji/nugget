# Troubleshooting

## Where to look first

1. `nuggetd status` shows whether the daemon is running, its socket, the store, the audit chain and
   every provider (with the reason one is unavailable).
2. The daemon log: JSON lines in the `logs` folder of the [data directory](../reference/data-dir.md),
   one file per day (`nuggetd.<date>.log`), 14 kept. Local model servers log to
   `llama-server-chat.log` and `llama-server-embedding.log` in the same folder.
3. More detail: start the daemon with `NUGGET_LOG=debug nuggetd`. `NUGGET_LOG` takes the usual
   `tracing` filter syntax, for example `NUGGET_LOG=info,nugget_ingest=debug`.

Logs never contain secrets or the content of your items at `info` level or above.

## The app says "Start Nugget" or "Not running"

- The app looks for `nuggetd` next to its own binary, then on your `PATH`. Build both into the same
  `target/` directory, or put `nuggetd` on your `PATH`.
- If `nuggetd` exits at once, run it in a terminal to see why. A config error names the file and the
  key; fix it, or move `config.toml` aside to get a fresh default.
- `nuggetd is already running` means another daemon holds the lock for this data directory. Only one
  daemon runs per data directory.

## Config errors

`config.toml` is strict: an unknown key or section, a plaintext `api_key`, a plaintext `http://` URL to
a public IP address, or a duplicate provider id stops the daemon with an error naming the problem. (A
`http://` URL to a public *host name* is caught when the host is resolved: that provider is then
listed as unavailable instead.) One
exception: unknown keys inside `[ingest]` and `[retrieve]` are currently ignored, so check spelling
there against [the reference](../reference/config.md). Restart `nuggetd` after any change; there is no
hot reload.

## A provider shows "unavailable"

- `api_key env:X: … not set`: the environment variable is not set **in the daemon's environment**. A
  service started at login does not see variables from your shell profile. Use `keychain:NAME`
  instead, or set the variable for the service.
- `keychain:NAME: no OS keychain`: on headless Linux there is no Secret Service. Use `env:VAR`.
- A local server refuses connections: check it is running (`ollama serve`) and that `base_url` ends in
  `/v1` for OpenAI-compatible servers.
- "not probed": the provider only serves plain chat until it is probed. Probe it on the **Status**
  page.

## Search is text-only

No embedding provider is available. Install the embedding model on **Models** (and check that
`[runtime] enabled = true`), or configure a local `openai_compatible` provider with
`tiers = ["embedding"]`. Items already indexed are embedded in the background once one is available.
A cloud-only embedding tier is ignored unless you opt in (see [Privacy](privacy.md)).

## A source is stuck

| State or error | What to do |
|---|---|
| `needs_auth` on Gmail or Google Calendar | Reconnect. If it happens weekly, your OAuth app is in Testing mode: see [the 7-day limit](sources-google.md#the-7-day-sign-in-limit-in-testing-mode). |
| `needs_auth` on IMAP or CalDAV | The password is missing or rejected. Create a new app password and set it again. |
| TLS or certificate error on IMAP | The server's certificate is not trusted by your OS. For Proton Mail Bridge, see [the certificate steps](sources-imap-caldav.md#proton-mail-bridge-the-certificate). |
| `refusing plain http` on CalDAV | Use the `https://` URL. Plain http is accepted only for `localhost`. |
| A folder source shows errors for some files | Unreadable files are skipped and keep any earlier indexed copy; the error list names them. |
| Changes in a folder are slow to appear | Network drives and some sync clients do not deliver file-system events. The periodic rescan (`rescan_interval_secs`) still catches changes; lower it for that source. |
| Indexing pauses on a laptop | Backfills pause on battery below 20 % (`low_battery_threshold`) and resume when charging. |

## The Knowledge page is empty or thin

- Extraction runs shortly after indexing and then every 10 minutes; check the counts and the items
  waiting on the **Knowledge** page.
- Without a local or local-network model for the `synthesis` tier, only the deterministic extractors
  run (mail headers, calendar attendees, Markdown frontmatter, tags and wikilinks, contacts). Cloud
  models are used for extraction only for sources you list in `cloud_sources` under
  `[knowledge.llm]`.
- On battery below the power threshold, runs make no LLM requests until you are charging.
- Set your name and addresses under `[knowledge.me]` if facts about you land on a separate person.

## The vault

- The vault is in `~/Nugget Vault` unless `path` under `[vault]` says otherwise (with `--home` or
  `NUGGET_HOME`, it is `vault` inside the data directory).
- If you and Nugget changed the same generated region of a page, your version is kept and Nugget's is
  written under `.nugget/conflicts/` in the vault. Compare them, keep what you want, and delete the
  conflict file.
- A wrong fact keeps coming back: reject it on the Knowledge page or delete its line in the vault.
  Either sets the sticky `rejected` status (see [Fact statuses](knowledge.md#fact-statuses)).

## An AI app cannot connect over MCP

- Run `nuggetd mcp install --client claude-desktop` (or `claude-code`, `cursor`) to write the app's
  configuration, or copy the snippet from **Connected AI apps**. Restart the app afterwards.
- The daemon must be running: `nuggetd mcp` is only a pipe to it.
- "unknown or revoked MCP client, or wrong token": the token is wrong or the client was revoked.
  Tokens are shown once; revoke the client and create a new one.
- Over HTTP, check the port (`port` under `[mcp]`, 7319 by default) and that `http = true`. Requests
  from a browser page are refused unless their origin is listed in `allowed_origins`.

## Features being wired in (wave W)

These apply once [Double Tap](double-tap.md), the [agent](agent.md), [Today](today.md) and
[plugins](plugins.md) land in the app.

| Problem | What to do |
|---|---|
| The Double Tap trigger does nothing on macOS | Allow Nugget under System Settings, Privacy & Security, Accessibility, then restart the app |
| The Double Tap trigger does nothing on Linux Wayland | Double taps cannot be observed under Wayland. Choose a chord (the default there is `Ctrl+Alt+Space`) |
| A chord cannot be registered | Another app already holds it. Choose another chord |
| "drafting is refused in password and secure fields" or "drafting is disabled in …" | Expected: the field is secure, or the app is on the excluded list. Remove an app from the list on the **Double Tap** page if you added it by mistake |
| Escape ×3 does not stop the agent | On macOS it needs Accessibility; on Wayland it is unsupported. Use **Stop all agents** in the tray, or set a chord as the kill switch |
| Every agent action is denied | The kill switch may be engaged: resume on the **Safety** page. Otherwise a deny rule matches the app or site, or the task used up its rate limit |
| A confirmation disappeared | It timed out after 2 minutes and counted as a denial |
| A plugin will not install | It is unsigned, or signed by a key that is not in your trusted keys. An update is also refused if its version is not higher or it is signed by a different key |
| A plugin request is refused | The host is not among the plugin's approved capabilities, or a tool tried to do something other than read |

## Running out of disk space

Model installs refuse to start unless there is room for the model plus 10 % plus 1 GiB, and say so.
The store grows by roughly 6 to 8 MB per 1 000 small documents with the test embedder; real embedding
models add more per chunk.

## Starting over

Stop the daemon, then delete the data, config and cache directories (see
[Data directory](../reference/data-dir.md)). To keep your sources but rebuild the index, delete only
`nugget.db` and its `-wal` and `-shm` files; the next start re-indexes everything.

## Reporting a bug

Open an issue with your OS, the output of `nuggetd status --json`, and the relevant log lines. Check
them for anything private first. Report security problems privately, as described in
[`SECURITY.md`](https://github.com/rayhankhilji/nugget/blob/main/SECURITY.md).
