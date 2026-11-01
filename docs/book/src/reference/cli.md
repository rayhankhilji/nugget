# Command line

`nuggetd` is the daemon and its small command-line client.

```text
nuggetd [--home DIR] [COMMAND]
```

With no command it runs the daemon in the foreground until SIGINT or SIGTERM. If another daemon holds
the data directory, it says so and exits 0.

<!-- check-refs: cli.commands -->
| Command | What it does |
|---|---|
| `status` | Asks the running daemon for its status and prints it. Exits 1 if no daemon answers |
| `install-service` | Registers `nuggetd` to start at login as a per-user service, and starts it |
| `uninstall-service` | Stops and removes that service |
| `restart-service` | Restarts that service, so it runs the `nuggetd` its definition points at (the desktop app does this after an update) |
| `mcp` | `nuggetd mcp --client <id>`: the stdio MCP bridge an AI app launches. `nuggetd mcp install --client <app>`: sets an app up |
| `backup` | `nuggetd backup [--to DIR]`: back up now, to the configured folder or `DIR`. `nuggetd backup set-passphrase`: set the backup passphrase, which seals the store key into every later backup. Goes through the daemon when it runs, works offline otherwise. See [Backup and export](../guide/backup.md) |
| `restore` | `nuggetd restore --from DIR`: restore a backup (stop the daemon first). The current data is kept aside |
| `export` | `nuggetd export --to DIR`: export everything in open formats into a new folder in `DIR`. **Not encrypted** |

<!-- check-refs: cli.flags -->
| Option | Applies to | Meaning |
|---|---|---|
| `--home DIR` | every command | Keep all data, config and cache under `DIR` (as `DIR/data`, `DIR/config`, `DIR/cache`). Overrides `NUGGET_HOME`. A relative path is made absolute |
| `--json` | `status` | Print the raw status object (the [`status`](ipc.md#status-providers-and-models) IPC result) instead of the summary |
| `--client ID` or `--client APP` | `mcp`, `mcp install` | For the bridge, the MCP client id to serve. For `mcp install`, the app to set up: `claude-desktop`, `claude-code` or `cursor` |
| `--name NAME` | `mcp install` | Name of the new client in Nugget (default: the app's name) |
| `--config PATH` | `mcp install` | The app's config file to merge into (default: its usual location) |
| `--yes`, `-y` | `mcp install` | Do not ask for confirmation. Required when standard input is not a terminal |
| `--to DIR` | `backup`, `export` | Where to write: for `backup`, instead of the configured folder; for `export`, required |
| `--from DIR` | `restore` | The backup directory (`nugget-backup-<date and time>`) |
| `--passphrase-file FILE` | `restore`, `backup set-passphrase` | Read the backup passphrase from `FILE` (else the `NUGGET_BACKUP_PASSPHRASE` environment variable, else a prompt) |
| `--version` | none | Print the version |
| `--help` | every command | Print help |

Exit status is `0` on success and `1` on any error. `nuggetd status` also reports the audit chain:
`CHAIN VERIFICATION FAILED` means audit rows were changed or removed.

## `nuggetd install-service`

Installs a launchd agent `dev.nugget.nuggetd` (`~/Library/LaunchAgents/`) on macOS, a
`systemd --user` unit `nuggetd.service` on Linux, or a least-privilege Task Scheduler logon task
`Nugget Daemon` on Windows. It runs the binary you ran it from, with any `--home` or `NUGGET_HOME`
baked in, and refuses to run as root.

## `nuggetd mcp`

`nuggetd mcp --client <id>` is the stdio bridge an AI app launches (token in `NUGGET_MCP_TOKEN`; logs
on stderr); see [MCP transports](mcp.md#transports).

**`nuggetd mcp install`** creates a client in the running daemon (all sources, all read-only tools, no
sensitive items) and sets the app up:

| `--client` | What happens | Default config file |
|---|---|---|
| `claude-desktop` | Adds an `mcpServers.nugget` entry | macOS `~/Library/Application Support/Claude/claude_desktop_config.json`, Windows `%APPDATA%\Claude\claude_desktop_config.json`, Linux `~/.config/Claude/claude_desktop_config.json` |
| `cursor` | The same, in Cursor's config | `~/.cursor/mcp.json` (Windows `%USERPROFILE%\.cursor\mcp.json`) |
| `claude-code` | Prints the `claude mcp add …` command to run | none |

The config is merged, never overwritten: the old file is kept as `<file>.nugget-backup-<time>`, the new
one is written readable only by you, and a client named by a previous `nugget` entry is revoked.
Restart the app afterwards. See [Connecting AI apps over MCP](../guide/mcp.md).
