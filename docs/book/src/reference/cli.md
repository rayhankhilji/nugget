# Command line

`nuggetd` is the daemon and its small command-line client.

```text
nuggetd [--home DIR] [COMMAND]
```

With no command, `nuggetd` runs the daemon in the foreground until it receives SIGINT or SIGTERM
(Ctrl+C, Ctrl+Break or console close on Windows). If another daemon already holds the data directory,
it prints `nuggetd is already running (<socket>)` and exits with status 0.

<!-- check-refs: cli.commands -->
| Command | What it does |
|---|---|
| `status` | Asks the running daemon for its status and prints it. Exits 1 if no daemon answers |
| `install-service` | Registers `nuggetd` to start at login as a per-user service, and starts it |
| `uninstall-service` | Stops and removes that service |
| `mcp` | `nuggetd mcp --client <id>`: the stdio MCP bridge an AI app launches. `nuggetd mcp install --client <app>`: sets an app up |

<!-- check-refs: cli.flags -->
| Option | Applies to | Meaning |
|---|---|---|
| `--home DIR` | every command | Keep all data, config and cache under `DIR` (as `DIR/data`, `DIR/config`, `DIR/cache`). Overrides `NUGGET_HOME`. A relative path is made absolute |
| `--json` | `status` | Print the raw status object (the [`status`](ipc.md#status-providers-and-models) IPC result) instead of the summary |
| `--client ID` or `--client APP` | `mcp`, `mcp install` | For the bridge, the MCP client id to serve. For `mcp install`, the app to set up: `claude-desktop`, `claude-code` or `cursor` |
| `--name NAME` | `mcp install` | Name of the new client in Nugget (default: the app's name) |
| `--config PATH` | `mcp install` | The app's config file to merge into (default: its usual location) |
| `--yes`, `-y` | `mcp install` | Do not ask for confirmation. Required when standard input is not a terminal |
| `--version` | none | Print the version |
| `--help` | every command | Print help |

## `nuggetd status`

```text
$ nuggetd status
nuggetd 0.0.1 running (protocol 1), up 1h 2m
socket:    /home/you/.local/share/nugget/ipc/nuggetd.sock
store:     1432 items, 5210 chunks
audit:     chain verified
providers:
  local-chat  openai_compatible qwen3-4b-q4km  on-device  tiers: triage, light, synthesis  probed: streaming, json_schema
  anthropic  anthropic <model>  cloud  tiers: synthesis, agentic  not probed
```

`audit` is `chain verified`, `not checked yet`, or `CHAIN VERIFICATION FAILED`, which means rows of
the audit log were changed or removed (see [Security model](../architecture/security.md)). A provider
that could not be built shows `unavailable:` and the reason.

## `nuggetd install-service`

| OS | What is installed | Where |
|---|---|---|
| macOS | A launchd agent, label `dev.nugget.nuggetd` | `~/Library/LaunchAgents/dev.nugget.nuggetd.plist` |
| Linux | A `systemd --user` unit, `nuggetd.service` | `$XDG_CONFIG_HOME/systemd/user/` (default `~/.config/systemd/user/`) |
| Windows | A Task Scheduler logon task, `Nugget Daemon`, least privilege | Task Scheduler root folder |

The service runs the binary you ran `install-service` from, so install from the binary's final
location. A `--home` or `NUGGET_HOME` set at install time is baked into the service. Running it again
replaces the previous definition. On macOS and Linux it refuses to run as root, since the service must
belong to your user.

## Exit status

`0` on success, `1` on any error (the message goes to standard error).

## `nuggetd mcp`

```text
nuggetd mcp --client <id>
nuggetd mcp install --client claude-desktop|claude-code|cursor [--name NAME] [--config PATH] [--yes]
```

See [Connecting AI apps over MCP](../guide/mcp.md) for the whole flow.

### The stdio bridge

`nuggetd mcp --client <id>` is the command an AI app launches. It reads the client's token from
`NUGGET_MCP_TOKEN` (it refuses to start without one), connects to the running daemon over the IPC
socket, and copies MCP messages between its standard input and output and the daemon. Standard output
carries MCP messages only; logs go to standard error. The daemon checks the token and enforces the
client's scopes, rate limit, revocation and audit; the bridge itself holds no data. When the daemon
runs with `--home` or `NUGGET_HOME`, the bridge needs the same data root; the setup snippets the
daemon generates set `NUGGET_HOME` for you.

### `nuggetd mcp install`

Creates an MCP client in the running daemon (with default scopes: all sources, all read-only tools,
no sensitive items) and sets the app up with it:

| `--client` | What happens | Default config file |
|---|---|---|
| `claude-desktop` | Adds an `mcpServers.nugget` entry to the app's JSON config | macOS `~/Library/Application Support/Claude/claude_desktop_config.json`, Windows `%APPDATA%\Claude\claude_desktop_config.json`, Linux `~/.config/Claude/claude_desktop_config.json` |
| `cursor` | The same, in Cursor's config | `~/.cursor/mcp.json` (Windows `%USERPROFILE%\.cursor\mcp.json`) |
| `claude-code` | Prints the `claude mcp add …` command to run; nothing is run or written for you | none (`--config` is ignored) |

The config file is merged, never overwritten: other servers and settings stay as they are, a file
that is not valid JSON is left alone (and the command fails), the previous version is copied to
`<file>.nugget-backup-<time>`, and the new file is written readable only by you (it holds the token)
and renamed into place. If a `nugget` entry was already there, the client it named is revoked. If
the merge fails, the new client is revoked again. Restart the app afterwards.

On a terminal the plan is shown and you are asked to confirm; `--yes` skips that. The daemon must be
running.
