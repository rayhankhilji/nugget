# Troubleshooting

## Where to look first

1. `nuggetd status`: the daemon, store, audit chain and every provider's state.
2. The logs, in the `logs` folder of the [data directory](../reference/data-dir.md) (shown under
   **Technical details** on Status):

   | System | Log folder |
   |---|---|
   | Linux | `~/.local/share/nugget/logs` |
   | macOS | `~/Library/Application Support/dev.Nugget.Nugget/logs` |
   | Windows | `%LOCALAPPDATA%\Nugget\Nugget\data\logs` |
   | With `NUGGET_HOME=DIR` | `DIR/data/logs` |

   `nuggetd.<date>.log` is the daemon, `nugget-desktop.<date>.log` the app, `llama-server-*.log`
   the local model servers. Logs at `info` and above never contain secrets or item content.
3. For more detail, start with `NUGGET_LOG=debug nuggetd` (or `nugget-desktop`); it takes `tracing`
   filters such as `NUGGET_LOG=info,nugget_ingest=debug`.

## The app says "Start Nugget" or "Not running"

- The app looks for `nuggetd` next to its own binary, then on your `PATH`.
- If `nuggetd` exits at once, run it in a terminal; a config error names the file and key.
- `nuggetd is already running`: another daemon holds this data directory.
- "exited before it was ready" or "did not answer": see the end of `nuggetd.<date>.log`.

## Nugget is still running after I quit

Quitting follows your answer to "also stop background indexing and the local AI?" (change it under
**Status → Updates → Desktop app**). A login service or a `nuggetd` you started yourself is never
stopped by the app. Stop it with `systemctl --user stop nuggetd` (Linux),
`launchctl bootout gui/$(id -u)/dev.nugget.nuggetd` (macOS) or `schtasks /End /TN "Nugget Daemon"`
(Windows), or turn off the login option.

## The Status page says the background service is another version

- **Started outside the app**: stop that `nuggetd`, then click **Check again**.
- **Restarting the login service didn't work**: log out and in, toggle **Start the Nugget background
  service at login**, or run `nuggetd restart-service`.
- **Nugget couldn't restart it**: quit and reopen Nugget; the app log says what failed.
- **Newer than this app**: an older copy of Nugget is open. Update it or open the newer copy.

## Config errors

`config.toml` is strict: unknown keys, a plaintext `api_key`, plain `http://` to a public IP, or a
duplicate provider id stops the daemon with an error. Unknown keys inside `[ingest]` and `[retrieve]`
are currently ignored, so check spelling against [the reference](../reference/config.md). Restart
`nuggetd` after changes; there is no hot reload.

## A provider shows "unavailable"

- `api_key env:X: … not set`: the variable isn't in the **daemon's** environment (a login service
  doesn't see your shell profile). Use `keychain:NAME` instead.
- `keychain:NAME: no OS keychain` (headless Linux): use `env:VAR`.
- A local server refuses connections: check it is running and that `base_url` ends in `/v1`.
- The `local` Ollama example disappeared: Nugget comments out that block if it is the untouched
  default. Uncomment it if you use Ollama.
- "not probed": probe it on the **Status** page.

## The on-device model is warming up, stopped, or won't start

- **Starting** ("Warming up: 12s of about 40s"): the model is loading; questions wait for it.
- **Failed**, "stopped responding" or "stopped unexpectedly": Nugget restarts it automatically. Ask
  again once **Ready**.
- **Failed**, "couldn't start" or "keeps stopping": free memory or fix the cause, then press
  **Retry** (on Status, or under a failed Chat answer). **Details** shows the last log line.

## Search is text-only

No embedding provider is available. Install the embedding model on **Models** (with
`[runtime] enabled = true`), or configure a local provider with `tiers = ["embedding"]`. A cloud-only
embedding tier needs an opt-in (see [Privacy](privacy.md)).

## A source is stuck

| State or error | What to do |
|---|---|
| `needs_auth` on Gmail, Google Calendar or Google Drive | Reconnect. If it happens weekly, see [the 7-day limit](sources-google.md#connect). |
| `needs_auth` on IMAP or CalDAV | Set a new app password. |
| TLS or certificate error on IMAP | Your OS doesn't trust the server's certificate. For Proton Mail Bridge, see [the certificate steps](sources-imap-caldav.md#proton-mail-bridge-the-certificate). |
| `refusing plain http` on CalDAV | Use `https://` (plain http only for `localhost`). |
| Some files in a folder show errors | Unreadable files are skipped; the error list names them. |
| Folder changes are slow to appear | Network drives may not send file events; lower `rescan_interval_secs` for that source. |
| Indexing pauses on a laptop | Backfills pause on battery below 20 % (`low_battery_threshold`). |

## The Knowledge page is empty or thin

- Extraction runs every 10 minutes; the **Knowledge** page shows what is waiting.
- Without a local `synthesis` model only deterministic extractors run. Cloud extraction is used only
  for sources in `cloud_sources` under `[knowledge.llm]`.
- If facts about you land on a separate person, set `[knowledge.me]`.
- Vault edit conflicts are written under `.nugget/conflicts/` in the vault. A wrong fact that keeps
  returning: reject it (see [Fact statuses](knowledge.md#fact-statuses)).

## An AI app cannot connect over MCP

See [Errors an app may show](mcp.md#errors-an-app-may-show). The daemon must be running. Over HTTP,
check `port` and `http = true` under `[mcp]`.

## Double Tap, the agent and plugins

| Problem | What to do |
|---|---|
| The Double Tap trigger does nothing on macOS | Allow Nugget under System Settings → Privacy & Security → Accessibility, then restart the app |
| The trigger does nothing on Linux Wayland | Use a chord (default `Ctrl+Alt+Space`); see [Wayland](double-tap.md#wayland) |
| A chord cannot be registered | Another app holds it. Choose another |
| "drafting is refused…" or "drafting is disabled in …" | The field is secure or the app is excluded (edit the list on the **Double Tap** page) |
| Escape ×3 does not stop the agent | Needs Accessibility on macOS; unsupported on Wayland. Use **Stop all agents** or the **Safety** page |
| Every agent action is denied | Resume the kill switch on **Safety**, or check deny rules and rate limits |
| A confirmation disappeared | It timed out after 2 minutes (a denial) |
| A plugin will not install | It is unsigned or signed by an untrusted key, or an update has a lower version or different key |
| A plugin request is refused | The host wasn't approved, or a tool tried to do more than read |

## Running out of disk space

Model installs need room for the model plus 10 % plus 1 GiB. **Status** warns below 1 GiB free.
When the disk is full, indexing stops without losing data and resumes once you free space.

## Starting over

Stop the daemon and delete the data, config and cache directories (see
[Data directory](../reference/data-dir.md)). To only rebuild the index, delete `nugget.db` and its
`-wal` and `-shm` files.

## Reporting a bug

Open an issue with your OS, `nuggetd status --json`, and relevant lines from `nuggetd.<date>.log`
and `nugget-desktop.<date>.log` (check them for anything private). Report security problems
privately as described in
[`SECURITY.md`](https://github.com/rayhankhilji/nugget/blob/main/SECURITY.md).
