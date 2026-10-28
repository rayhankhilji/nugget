# Connector settings

The `settings` object of each source kind, as sent in `sources.add` and `sources.update` and returned in
`SourceInfo`. Unknown keys are rejected. Settings never contain secrets: passwords and OAuth client
secrets go through `sources.secret.set` and live in the OS keychain.

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

## Planned kinds

`nugget-connector-microsoft` (Outlook mail, calendar, OneDrive) and `nugget-connector-saas` (Slack,
Notion, GitHub, Linear) are stubs; they register no kinds yet.
