# Connector settings

The `settings` object of each source kind (`sources.add`, `sources.update`, `SourceInfo`). Unknown
keys are rejected. Gmail and Outlook poll every 60 s, most other cloud kinds every 5–10 minutes;
folders and chat exports are watched. Secrets never go in settings: use `sources.secret.set` (OS keychain, service
`app.heed.Heed`; deleted with the source). `.gitignore` and `.nuggetignore` are honoured by
folder kinds.

<!-- check-refs: connector.kinds -->
| Kind | Display name | Sign-in | Keychain accounts |
|---|---|---|---|
| `files` | Folders | none | none |
| `obsidian` | Obsidian vault | none | none |
| `gmail` | Gmail | OAuth, `gmail.readonly` | `oauth:<source id>` (tokens), `oauth_client_secret:<source id>` |
| `gcal` | Google Calendar | OAuth, `calendar.readonly` | `oauth:<source id>` (tokens), `oauth_client_secret:<source id>` |
| `gdrive` | Google Drive | OAuth, `drive.readonly` | `oauth:<source id>` (tokens), `oauth_client_secret:<source id>` |
| `imap` | IMAP mail | password | `password:<source id>` |
| `caldav` | CalDAV calendar | password | `password:<source id>` |
| `outlook` | Outlook mail | OAuth, `offline_access User.Read Mail.Read` | `oauth:<source id>` (tokens), `oauth_client_secret:<source id>` (optional) |
| `outlook-calendar` | Outlook calendar | OAuth, `… Calendars.Read` | as for `outlook` |
| `onedrive` | OneDrive | OAuth, `… Files.Read` | as for `outlook` |
| `slack` | Slack | password (pasted token) | `password:<source id>` |
| `notion` | Notion | password (pasted token) | `password:<source id>` |
| `github` | GitHub | password (pasted token), or the device flow with `client_id` | `password:<source id>`, `oauth:<source id>` (device-flow tokens) |
| `linear` | Linear | password (pasted API key), or OAuth with `client_id` | `password:<source id>`, `oauth:<source id>`, `oauth_client_secret:<source id>` (optional) |
| `imessage` | iMessage | none | none |
| `whatsapp` | WhatsApp exports | none | none |
| `telegram` | Telegram export | none | none |

Plugin connectors register as `plugin:<plugin id>` ([below](#pluginplugin-id)). SaaS tokens are looked
up in `token_env`, then `oauth:<source id>`, then `password:<source id>`; none is `needs_auth`.

## `files`
<!-- check-refs: connector.files -->
| Key | Type | Default | Description |
|---|---|---|---|
| `roots` | array of paths | required | Folders to index. Each must exist when the source is added |
| `include` | array of extensions | the default list | File extensions to index (for example `["md", "pdf"]`). Omit for every supported type; an empty list from the desktop app also means the defaults |
| `exclude` | array of globs | `[]` | Patterns to skip, matched case-insensitively. Without `/`: a name at any depth. Leading `/`: anchored to the root. Trailing `/`: directories only |
| `max_file_bytes` | integer | `52428800` | Larger files (50 MiB) are not indexed |
| `follow_symlinks` | bool | `false` | Follow symbolic links (loops are detected) |
| `include_hidden` | bool | `false` | Index dot-files and hidden files. VCS, dependency and cache directories are skipped regardless |
| `debounce_ms` | integer, 20 to 10000 | `300` | Watcher debounce: bursts of events on one path within this window are coalesced |
| `rescan_interval_secs` | integer | `3600` (`900` on Windows) | While watching, run a full reconcile this often as a safety net; `0` never |

## `obsidian`
<!-- check-refs: connector.obsidian -->
| Key | Type | Default | Description |
|---|---|---|---|
| `vault` | path | required | The vault folder |
| `include_attachments` | bool | `true` | Also index PDF and image attachments next to the notes |
| `exclude` | array of globs | `[]` | As for `files` |
| `max_file_bytes` | integer | `52428800` | As for `files` |
| `follow_symlinks` | bool | `false` | As for `files` |
| `include_hidden` | bool | `false` | As for `files` |
| `debounce_ms` | integer, 20 to 10000 | `300` | As for `files` |
| `rescan_interval_secs` | integer | `3600` (`900` on Windows) | As for `files` |

## `gmail`

<!-- check-refs: connector.gmail -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | Your OAuth client id (type "Desktop app") |
| `client_secret` | string | none | **Refused** by the daemon in settings; set it with `sources.secret.set`. The daemon injects it when it builds the connector |
| `account_hint` | string | none | Email address to pre-select on Google's consent screen |
| `window_days` | integer, 1 to 3650 | `365` | How far back to index mail on first sync (and on a resync) |
| `include_spam_trash` | bool | `false` | Also index Spam and Trash |
| `allow_modify` | bool | `false` | Let approved proactive actions archive threads (remove the `INBOX` label). Asks for the `gmail.modify` scope, so reconnect the account after turning it on |

## `gcal`

<!-- check-refs: connector.gcal -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | As for `gmail` |
| `client_secret` | string | none | As for `gmail`: set it with `sources.secret.set` |
| `account_hint` | string | none | As for `gmail` |
| `calendars` | array of calendar ids | every calendar selected in Google Calendar | Calendar ids to sync, for example `["primary"]` |
| `past_days` | integer, 0 to 3650 | `365` | Days of past events |
| `future_days` | integer, 0 to 3650 | `365` | Days of future events |

## `gdrive`

Setup: [Google sources](../guide/sources-google.md), [Google Drive](../guide/sources-google.md#google-drive).

<!-- check-refs: connector.gdrive -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | As for `gmail` |
| `client_secret` | string | none | As for `gmail`: set it with `sources.secret.set` |
| `account_hint` | string | none | As for `gmail` |
| `include_shared_with_me` | bool | `false` | Also index files other people shared with you (outside shared drives) |
| `include_shared_drives` | bool | `false` | Also index files in shared drives you are a member of |
| `max_file_mb` | integer, 1 to 200 | `25` | Largest file downloaded or exported. Bigger files are indexed as a stub with their name and a "Not indexed" reason |

## `imap`
<!-- check-refs: connector.imap -->
| Key | Type | Default | Description |
|---|---|---|---|
| `host` | string | required | Bare host name or IP address, for example `imap.fastmail.com` |
| `port` | integer | `993`, or `143` with `starttls` | Server port |
| `tls` | `implicit` or `starttls` | `implicit` | Implicit TLS from the first byte, or `STARTTLS` before anything else (fails if the server does not upgrade). There is no plaintext mode |
| `username` | string | required | Login name |
| `folders` | array of mailbox names | `INBOX` and the Sent mailbox | Mailboxes to index. `["*"]` means every selectable mailbox except Trash, Spam, Junk and virtual views (All Mail, Starred, Important). Must not be empty when given |
| `window_days` | integer, 0 to 36500 | `365` | Index mail from the last N days; `0` for all |
| `max_message_mb` | integer, 1 to 512 | `25` | Larger messages are indexed by their headers only |

## `caldav`
<!-- check-refs: connector.caldav -->
| Key | Type | Default | Description |
|---|---|---|---|
| `url` | URL | required | Server root, principal, calendar home or one calendar collection. `https://` only; `http://` is accepted for loopback hosts. Credentials in the URL are refused |
| `username` | string | required | Login name |
| `calendars` | array of strings | every calendar holding events or tasks | Calendars to sync, by display name, href, or the last path segment |

## `outlook`

App registration: [Microsoft 365](../guide/sources-microsoft.md).

<!-- check-refs: connector.outlook -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | Application (client) ID of your own Microsoft Entra app registration (public client, redirect URI `http://localhost`) |
| `client_secret` | string | none | Optional: public clients need none. Set it with `sources.secret.set`, never in settings; the daemon injects it when it builds the connector |
| `tenant` | string | `common` | `common` (work, school and personal accounts), `organizations`, `consumers`, a tenant id or a verified domain. Anything else is refused |
| `account_hint` | string | none | Email address to pre-select on the sign-in screen |
| `folders` | array of strings, at most 100 | `["inbox", "sentitems"]` | Well-known folder names (`inbox`, `sentitems`, `archive`, ...) or folder ids |
| `window_days` | integer, 1 to 3650 | `365` | How far back to index mail on first sync (and on a resync) |

## `outlook-calendar`

<!-- check-refs: connector.outlook_calendar -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | As for `outlook` |
| `client_secret` | string | none | As for `outlook` |
| `tenant` | string | `common` | As for `outlook` |
| `account_hint` | string | none | As for `outlook` |
| `past_days` | integer, 0 to 3650 | `365` | Days of past events |
| `future_days` | integer, 0 to 3650 | `365` | Days of future events. `past_days` and `future_days` cannot both be `0` |

## `onedrive`

<!-- check-refs: connector.onedrive -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | As for `outlook` |
| `client_secret` | string | none | As for `outlook` |
| `tenant` | string | `common` | As for `outlook` |
| `account_hint` | string | none | As for `outlook` |
| `include_folders` | array of paths, at most 200 | `[]` (the whole drive) | Folder paths from the drive root, for example `/Documents/Work`. Case-insensitive |
| `exclude_folders` | array of paths, at most 200 | `[]` | Folder paths never indexed; wins over `include_folders` |
| `max_file_mb` | integer, 1 to 500 | `25` | Larger files are skipped |

## `slack`
<!-- check-refs: connector.slack -->
| Key | Type | Default | Description |
|---|---|---|---|
| `token_env` | environment variable name | none | Read the token from this variable instead of the keychain |
| `conversations` | array of strings, at most 1000 | every conversation you are in | Channel ids (`C…`, `G…`, `D…`) or names (`general`, `#general`) |
| `include_dms` | bool | `true` | With no `conversations`: include DMs and group DMs |
| `include_channels` | bool | `true` | With no `conversations`: include public and private channels you are a member of |
| `max_conversations` | integer, 1 to 1000 | `100` | Most conversations synced |
| `window_days` | integer, 1 to 3650 | `90` | How far back to index messages on first sync |
| `recheck_days` | integer, 0 to 30 | `3` | Days re-read every 6 hours to pick up edits, deletions and thread replies |

Token: a user token (`xoxp-…`) with the `*:history` and `*:read` scopes for channels, groups, IMs
and MPIMs, plus `users:read`.

## `notion`
<!-- check-refs: connector.notion -->
| Key | Type | Default | Description |
|---|---|---|---|
| `token_env` | environment variable name | none | As for `slack` |
| `include_databases` | bool | `true` | Index databases shared with the integration, and their rows |
| `max_pages` | integer, 1 to 100000 | `5000` | The most recently edited pages kept in the index |
| `max_depth` | integer, 1 to 12 | `6` | How deeply nested blocks are rendered |

Token: an internal integration secret with **Read content**.

## `github`
<!-- check-refs: connector.github -->
| Key | Type | Default | Description |
|---|---|---|---|
| `token_env` | environment variable name | none | As for `slack` |
| `client_id` | string | none | Client ID of your own OAuth App (or GitHub App) with the device flow enabled. Enables `sources.auth.begin` |
| `client_secret` | string | none | Only for refreshing expiring GitHub App user tokens. Not settable in this build |
| `repos` | array of `owner/name`, at most 100 | everything involving you | Repositories to index |
| `all_activity` | bool | `false` | With `repos`: index everything in them, not only what involves you. Requires `repos` |
| `include_discussions` | bool | `true` | Also index discussions |
| `window_days` | integer, 1 to 3650 | `365` | How far back to index on first sync |

Token: fine-grained, read-only Issues, Pull requests, Discussions and Metadata (device flow: `repo`,
`read:discussion`).

## `linear`
<!-- check-refs: connector.linear -->
| Key | Type | Default | Description |
|---|---|---|---|
| `token_env` | environment variable name | none | As for `slack` |
| `client_id` | string | none | Client ID of your own Linear OAuth application (redirect URI `http://127.0.0.1`). Enables `sources.auth.begin` (PKCE, scope `read`) |
| `client_secret` | string | none | Optional with PKCE. The connector reads it from `oauth_client_secret:<source id>`, which this build has no way to set |
| `include` | array of `assigned`, `created`, `subscribed` | all three | Which of your issues to index. Must not be empty |
| `teams` | array of team keys, at most 200 | every team | For example `["ENG"]` |
| `window_days` | integer, 1 to 3650 | `365` | How far back to index on first sync |

## `imessage`

Messaging kinds read local chat history, no network; one item per conversation per month. See
[Messaging sources](../guide/sources-messaging.md).
<!-- check-refs: connector.imessage -->
| Key | Type | Default | Description |
|---|---|---|---|
| `private` | bool | `true` | Mark every conversation sensitive (kept from cloud models) |
| `since_days` | integer, at least 1 | none (everything) | Only messages newer than this |
| `include_groups` | bool | `true` | Also index group chats |
| `db_path` | path | `~/Library/Messages/chat.db` | The Messages database. Required off macOS, where there is no default |
| `contacts` | bool | `true` | Show names from the macOS address book instead of phone numbers and addresses |
| `contacts_dir` | path | `~/Library/Application Support/AddressBook` | Advanced: folder searched for `AddressBook-v22.abcddb` files (also in its `Sources/*/`). Setting it enables contacts off macOS too |

Needs Full Disk Access on macOS; a private copy of the database is read each pass.

## `whatsapp`
<!-- check-refs: connector.whatsapp -->
| Key | Type | Default | Description |
|---|---|---|---|
| `private` | bool | `true` | As for `imessage` |
| `since_days` | integer, at least 1 | none | As for `imessage` |
| `include_groups` | bool | `true` | As for `imessage` |
| `folder` | path | required | Folder of "Export chat" files (`.zip` or `.txt`, also one subfolder level deep). Symlinks leading out of it are skipped |
| `me` | string | none | Your WhatsApp profile name as it appears in the exports, so your messages are shown as "Me" |

## `telegram`
<!-- check-refs: connector.telegram -->
| Key | Type | Default | Description |
|---|---|---|---|
| `private` | bool | `true` | As for `imessage` |
| `since_days` | integer, at least 1 | none | As for `imessage` |
| `include_groups` | bool | `true` | As for `imessage` |
| `path` | path | required | A Telegram Desktop JSON export: its `result.json`, its folder, or a parent folder holding `ChatExport_*` / `DataExport_*` folders. Inside a folder, symlinks leading out of it are skipped |

## `plugin:<plugin id>`

Settings follow the manifest's `settings_schema`; polled every `poll_interval_secs` (default 1800,
at least 60). Secrets live under `plugin:<plugin id>:<name>`. See [Plugins](../guide/plugins.md).
