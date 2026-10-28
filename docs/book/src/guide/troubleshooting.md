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
