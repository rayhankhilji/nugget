# Connector settings

The `settings` object of each source kind, as sent in `sources.add` and `sources.update` and returned in
`SourceInfo`. Unknown keys are rejected. Settings never contain secrets: passwords, API tokens and
OAuth client secrets go through `sources.secret.set` and live in the OS keychain.

Each connector also publishes its settings as a JSON Schema (draft 2020-12), which the desktop app
uses to build its forms.

<!-- check-refs: connector.kinds -->
| Kind | Display name | Sign-in | Keychain accounts |
|---|---|---|---|
| `files` | Folders | none | none |
| `obsidian` | Obsidian vault | none | none |
| `gmail` | Gmail | OAuth | `oauth:<source id>` (tokens), `oauth_client_secret:<source id>` |
| `gcal` | Google Calendar | OAuth | `oauth:<source id>` (tokens), `oauth_client_secret:<source id>` |
| `imap` | IMAP mail | password | `password:<source id>` |
| `caldav` | CalDAV calendar | password | `password:<source id>` |

All accounts are under the keychain service `dev.nugget.Nugget`, and are deleted when the source is
removed.

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

Item keys are `<root index>:<relative path>` with `/` separators on every OS, for example
`0:Projects/plan.md`. Item versions are the modification time and size, and item URLs are `file://`
URLs. `.gitignore` and `.nuggetignore` files are honoured.

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

Item URLs are `obsidian://open?vault=…&file=…` links; the `file://` URL is in the item's
`metadata.file_url`.

## `gmail`

Scope `gmail.readonly`. See [Gmail and Google Calendar](../guide/sources-google.md) for creating the
OAuth client.

<!-- check-refs: connector.gmail -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | Your OAuth client id (type "Desktop app") |
| `client_secret` | string | none | **Refused** by the daemon in settings; set it with `sources.secret.set`. The daemon injects it when it builds the connector |
| `account_hint` | string | none | Email address to pre-select on Google's consent screen |
| `window_days` | integer, 1 to 3650 | `365` | How far back to index mail on first sync (and on a resync) |
| `include_spam_trash` | bool | `false` | Also index Spam and Trash |
| `allow_modify` | bool | `false` | Let approved proactive actions archive threads (remove the `INBOX` label). Asks for the `gmail.modify` scope, so reconnect the account after turning it on |

Sync: newest-first backfill, then `history.list` deltas every 60 s. An expired history id triggers a
bounded resync.

## `gcal`

Scope `calendar.readonly`.

<!-- check-refs: connector.gcal -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | As for `gmail` |
| `client_secret` | string | none | As for `gmail`: set it with `sources.secret.set` |
| `account_hint` | string | none | As for `gmail` |
| `calendars` | array of calendar ids | every calendar selected in Google Calendar | Calendar ids to sync, for example `["primary"]` |
| `past_days` | integer, 0 to 3650 | `365` | Days of past events |
| `future_days` | integer, 0 to 3650 | `365` | Days of future events |

Sync: `events.list` with recurring events expanded, then `syncToken` deltas every 5 minutes. A
`410 Gone` triggers a full resync. Cancelled events become deletes.

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

Sync: per mailbox, `UIDVALIDITY` and UIDs, with CONDSTORE and QRESYNC when the server supports them.
Mailboxes are opened read-only (`EXAMINE`, `BODY.PEEK`). A changed `UIDVALIDITY` re-indexes that
mailbox. Item keys are `<mailbox>/<uidvalidity>/<uid>`.

## `caldav`

<!-- check-refs: connector.caldav -->
| Key | Type | Default | Description |
|---|---|---|---|
| `url` | URL | required | Server root, principal, calendar home or one calendar collection. `https://` only; `http://` is accepted for loopback hosts. Credentials in the URL are refused |
| `username` | string | required | Login name |
| `calendars` | array of strings | every calendar holding events or tasks | Calendars to sync, by display name, href, or the last path segment |

Sync: discovery through `current-user-principal` and `calendar-home-set`, then RFC 6578
`sync-collection` per calendar, or a `getctag` check plus an ETag listing when the server has no sync
support. Polled every 5 minutes. Item keys are the event's absolute href path.

## Microsoft 365, SaaS and plugin kinds

> The Microsoft 365 (`outlook`, `outlook-calendar`, `onedrive`) and SaaS (`slack`, `notion`,
> `github`, `linear`) kinds are registered in `nuggetd` and offered by the Sources wizard. Plugin
> kinds (`plugin:<id>`) are still being wired in (wave W). This table is compared with the connector
> crates.

<!-- check-refs: connector.kinds -->
| Kind | Display name | Sign-in | Keychain accounts |
|---|---|---|---|
| `outlook` | Outlook mail | OAuth | `oauth:<source id>` (tokens), `oauth_client_secret:<source id>` (optional) |
| `outlook-calendar` | Outlook calendar | OAuth | as for `outlook` |
| `onedrive` | OneDrive | OAuth | as for `outlook` |
| `slack` | Slack | password (pasted token) | `password:<source id>` |
| `notion` | Notion | password (pasted token) | `password:<source id>` |
| `github` | GitHub | password (pasted token), or the device flow with `client_id` | `password:<source id>`, `oauth:<source id>` (device-flow tokens) |
| `linear` | Linear | password (pasted API key), or OAuth with `client_id` | `password:<source id>`, `oauth:<source id>`, `oauth_client_secret:<source id>` (optional) |

Plugin connectors register as `plugin:<plugin id>`, with the display name from the plugin's manifest,
no sign-in or a password, and their own secrets under `plugin:<plugin id>:<name>` (see
[below](#pluginplugin-id)).

For the SaaS kinds the token is looked up in this order: the `token_env` environment variable if that
setting is present, then `oauth:<source id>`, then `password:<source id>`. Nothing found is
`needs_auth`.

## `outlook`

Scopes `offline_access`, `User.Read`, `Mail.Read`. See [Microsoft 365](../guide/sources-microsoft.md)
for registering the app.

<!-- check-refs: connector.outlook -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | Application (client) ID of your own Microsoft Entra app registration (public client, redirect URI `http://localhost`) |
| `client_secret` | string | none | Optional: public clients need none. Set it with `sources.secret.set`, never in settings; the daemon injects it when it builds the connector |
| `tenant` | string | `common` | `common` (work, school and personal accounts), `organizations`, `consumers`, a tenant id or a verified domain. Anything else is refused |
| `account_hint` | string | none | Email address to pre-select on the sign-in screen |
| `folders` | array of strings, at most 100 | `["inbox", "sentitems"]` | Well-known folder names (`inbox`, `sentitems`, `archive`, ...) or folder ids |
| `window_days` | integer, 1 to 3650 | `365` | How far back to index mail on first sync (and on a resync) |

Sync: per-folder `messages/delta` over the window, then delta links every 60 s. Each message is fetched
as MIME; messages over 35 MiB and drafts are skipped. An expired delta token (`410`,
`syncStateNotFound`) triggers a resync. Item keys are `<folder>/<immutable message id>`.

## `outlook-calendar`

Scopes `offline_access`, `User.Read`, `Calendars.Read`.

<!-- check-refs: connector.outlook_calendar -->
| Key | Type | Default | Description |
|---|---|---|---|
| `client_id` | string | required | As for `outlook` |
| `client_secret` | string | none | As for `outlook` |
| `tenant` | string | `common` | As for `outlook` |
| `account_hint` | string | none | As for `outlook` |
| `past_days` | integer, 0 to 3650 | `365` | Days of past events |
| `future_days` | integer, 0 to 3650 | `365` | Days of future events. `past_days` and `future_days` cannot both be `0` |

Sync: `calendarView/delta` over the window (recurring series expanded), then delta links every
5 minutes. When the window has slid by more than 7 days, the next pass lists the current window. Item
keys are event ids.

## `onedrive`

Scopes `offline_access`, `User.Read`, `Files.Read`.

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

Sync: `drive/root/delta`, polled every 5 minutes. Supported types: `md`, `markdown`, `txt`, `text`,
`log`, `rst`, `org`, `html`, `htm`, `csv`, `tsv`, `json`, `eml`, `ics`, `pdf`, `docx`, `xlsx`, `pptx`.
Downloads use the pre-authenticated link without the bearer token, over HTTPS, from Microsoft download
hosts only. Item keys are drive item ids.

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

Token: a user token (`xoxp-…`) with `channels:history`, `groups:history`, `im:history`,
`mpim:history`, `channels:read`, `groups:read`, `im:read`, `mpim:read` and `users:read`. Sync: one
`conversations.history` probe per conversation and pass, every 5 minutes. Item keys are
`<channel>/day/<YYYY-MM-DD>` (top-level messages of one UTC day) and `<channel>/thread/<ts>`.

## `notion`

<!-- check-refs: connector.notion -->
| Key | Type | Default | Description |
|---|---|---|---|
| `token_env` | environment variable name | none | As for `slack` |
| `include_databases` | bool | `true` | Index databases shared with the integration, and their rows |
| `max_pages` | integer, 1 to 100000 | `5000` | The most recently edited pages kept in the index |
| `max_depth` | integer, 1 to 12 | `6` | How deeply nested blocks are rendered |

Token: an internal integration secret with the **Read content** capability. API version `2022-06-28`.
Sync: `search` by `last_edited_time` every 5 minutes, and a full listing every 6 hours. Item keys are
`page/<id>` and `database/<id>`.

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

Token: a fine-grained token with read-only Issues, Pull requests, Discussions and Metadata. The device
flow requests `repo` and `read:discussion`. Sync: GraphQL `search` with `updated:>=` every 10 minutes,
and an ids-only listing daily. Item keys are `issue/<owner>/<repo>/<number>` (issues and pull requests)
and `discussion/<owner>/<repo>/<number>`.

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

Sync: GraphQL `issues` with `updatedAt` filters every 5 minutes (archived issues included, so they
become deletes), and an ids-only listing every 6 hours. Item keys are `issue/<uuid>`.

## `plugin:<plugin id>`

A plugin connector's settings are whatever JSON object its manifest's `settings_schema` describes
(any object when it declares none); the plugin validates them. It is polled every
`poll_interval_secs` from the manifest (default 1800, at least 60). See
[Plugins](../guide/plugins.md).
