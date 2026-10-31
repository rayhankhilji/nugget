# Adding sources

A **source** is one configured connector: a folder, a vault, a mailbox, a calendar account. Add sources
on the desktop app's **Sources** page. Each one shows its state, item count, last sync and last error,
and has **Sync now**, **Disable** and **Remove** actions.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/screenshots/sources-dark.png">
  <img src="../images/screenshots/sources-light.png" alt="The Sources page with one folder source: its state, item and chunk counts, last sync, and Sync now and Remove actions" loading="lazy">
</picture>

| Kind | What it indexes | Sign-in | How changes arrive |
|---|---|---|---|
| [`files`](sources-files.md) | Folders on this machine | None | Live file-system watcher (about 0.3 s debounce) |
| [`obsidian`](sources-obsidian.md) | An Obsidian vault's notes and attachments | None | Live file-system watcher |
| [`gmail`](sources-google.md) | Gmail, read-only | Your own Google OAuth client | Polled every 60 s (history deltas) |
| [`gcal`](sources-google.md) | Google Calendar, read-only | Your own Google OAuth client | Polled every 5 min (sync tokens) |
| [`gdrive`](sources-google.md#google-drive) | Documents in Google Drive, read-only | Your own Google OAuth client | Polled every 5 min (Drive change feed) |
| [`imap`](sources-imap-caldav.md) | Any IMAP mailbox over TLS | Password or app password | IMAP IDLE on the inbox, plus polling |
| [`caldav`](sources-imap-caldav.md) | Any CalDAV calendar over https | Password or app password | Polled every 5 min (`sync-collection`) |

| Kind | What it indexes | Sign-in | How changes arrive |
|---|---|---|---|
| [`outlook`](sources-microsoft.md) | Outlook / Exchange Online mail, read-only | Your own Microsoft Entra app (client id) | Polled every 60 s (Graph delta queries) |
| [`outlook-calendar`](sources-microsoft.md) | Your default Outlook calendar, read-only | Your own Microsoft Entra app | Polled every 5 min (Graph delta queries) |
| [`onedrive`](sources-microsoft.md) | Documents in OneDrive, read-only | Your own Microsoft Entra app | Polled every 5 min (Graph delta queries) |
| [`slack`](sources-saas.md#slack) | Your DMs and channels | Pasted user token | Polled every 5 min, recent days re-read every 6 h |
| [`notion`](sources-saas.md#notion) | Pages and databases shared with your integration | Pasted integration secret | Polled every 5 min, full listing every 6 h |
| [`github`](sources-saas.md#github) | Issues, pull requests and discussions involving you | Pasted token, or the device flow | Polled every 10 min, full listing daily |
| [`linear`](sources-saas.md#linear) | Issues assigned to, created by or followed by you | Pasted API key, or OAuth with PKCE | Polled every 5 min, full listing every 6 h |

**Plugin connectors** add kinds named `plugin:<plugin id>`, for example
`plugin:dev.nugget.example.feeds` for the example feed reader. They are WebAssembly plugins you install
and approve on the [**Plugins**](plugins.md) page. A new plugin kind is available after Nugget restarts,
and the Add source wizard does not list plugin kinds yet. Each one reaches only the hosts it declared, and its
traffic is in the egress ledger under `sync:plugin:<plugin id>`. A plugin connector signs in with
nothing or with a secret you set, never with OAuth.

The macOS-only sources (Apple Notes, Messages, Mail) are planned and not available yet.

## What every source guarantees

- **Edits and deletes propagate.** A changed file or message is re-indexed, a deleted one disappears
  from search, and a rename is a delete plus a new item. The benchmark checks this for every
  operation.
- **Read-only.** Connectors never change your data. IMAP mailboxes are opened read-only, so messages
  are not marked as read.
- **Resumable.** Progress is saved only after the items before it are stored, so a crash or restart
  never skips anything; at worst a few items are re-checked.
- **Secrets stay in the keychain.** Passwords, API tokens, OAuth tokens and OAuth client secrets go
  to the OS keychain. They never appear in `sources.toml`, logs, the audit log or IPC responses.
- **Every network request is in the egress ledger**, with the feature `sync:<kind>` (for example
  `sync:gmail`).

## States

| State | Meaning |
|---|---|
| `syncing` | Running a sync pass |
| `watching` | Up to date and receiving live changes |
| `idle` | Up to date, waiting for the next poll |
| `needs_auth` | Needs you to sign in again or set a password |
| `disabled` | You turned it off |
| `error` | The last pass failed; it is retried with backoff (2 s, doubling, up to 15 min) |

## Sync health

Besides its state, each source carries a **health** line: what is wrong, in plain words, and what to
do about it. Over IPC it is the `health` field of each source in `sources.list`, with a stable `code`.

| Code | Meaning | What to do |
|---|---|---|
| `ok` | Up to date | Nothing |
| `disabled` | You turned the source off | Turn it on; it resumes where it stopped |
| `reconnect` | The service stopped accepting Nugget's sign-in (revoked access, expired refresh token, changed password, deleted token) | Sign in again, or set a new password or token |
| `rate_limited` | The service asked Nugget to slow down (rate limit or daily quota) | Nothing: Nugget waits and resumes |
| `offline` | Nugget could not reach the service | Check your connection, VPN or proxy |
| `server_error` | The service answered with a server error | Nothing: Nugget retries with backoff |
| `too_large` | A response or item was over Nugget's size limit | Nothing: very large items are skipped |
| `config` | The service rejected the source's settings | Check the account, folders or paths in the source's settings |
| `item_errors` | Syncing works, but some items could not be read and were skipped | Nothing, unless you need those items |
| `error` | Any other failure | Try **Sync now**; if it keeps failing, see [Troubleshooting](troubleshooting.md) |

On the **Sources** page a source that needs you shows the reason and a button that matches how it
signs in: **Sign in** for Google, Microsoft and Linear OAuth, the device code for GitHub's device flow,
**Set password** for IMAP and CalDAV, and **Set token** for a pasted token.

## Checking a source with a real account

The connectors are tested against mock servers that imitate each service. Nobody has yet run them
against live accounts, so each guide page ends with a short checklist you can work through with a
real account in about 10 minutes. Each one covers sign-in and the permissions you grant, the first
sync, edits, renames and moves, deletes, non-Latin and emoji titles, very large items, recurrence and
time zones for calendars, and what happens when you revoke access.

| Kind | Access it asks for | Checklist |
|---|---|---|
| `files` | None: reads the folders you pick, no network | [Checking folders with a real folder](sources-files.md#checking-folders-with-a-real-folder) |
| `obsidian` | None: reads the vault folder, no network | [Checking Obsidian with a real vault](sources-obsidian.md#checking-obsidian-with-a-real-vault) |
| `gmail` | `gmail.readonly` (`gmail.modify` only if you turn on **Allow archiving**) | [Checking Gmail with a real account](sources-google.md#checking-gmail-with-a-real-account) |
| `gcal` | `calendar.readonly` | [Checking Google Calendar with a real account](sources-google.md#checking-google-calendar-with-a-real-account) |
| `gdrive` | `drive.readonly` (also covers reading Docs, Sheets and Slides) | [Checking Drive with a real account](sources-google.md#checking-drive-with-a-real-account) |
| `imap` | Your username and an app password; mailboxes opened read-only | [Checking IMAP with a real account](sources-imap-caldav.md#checking-imap-with-a-real-account) |
| `caldav` | Your username and an app password; read requests only | [Checking CalDAV with a real account](sources-imap-caldav.md#checking-caldav-with-a-real-account) |
| `outlook` | `Mail.Read`, plus `offline_access` and `User.Read` | [Checking Outlook mail with a real account](sources-microsoft.md#checking-outlook-mail-with-a-real-account) |
| `outlook-calendar` | `Calendars.Read`, plus `offline_access` and `User.Read` | [Checking Outlook calendar with a real account](sources-microsoft.md#checking-outlook-calendar-with-a-real-account) |
| `onedrive` | `Files.Read`, plus `offline_access` and `User.Read` | [Checking OneDrive with a real account](sources-microsoft.md#checking-onedrive-with-a-real-account) |
| `slack` | A user token with nine read scopes (`*:history`, `*:read`, `users:read`) | [Checking Slack with a real account](sources-saas.md#checking-slack-with-a-real-account) |
| `notion` | An internal integration with **Read content** only | [Checking Notion with a real account](sources-saas.md#checking-notion-with-a-real-account) |
| `github` | A fine-grained token, read-only Issues, Pull requests, Discussions, Metadata (device flow: `repo`, `read:discussion`) | [Checking GitHub with a real account](sources-saas.md#checking-github-with-a-real-account) |
| `linear` | A personal API key with read access, or OAuth scope `read` | [Checking Linear with a real account](sources-saas.md#checking-linear-with-a-real-account) |

To keep a check short, give the source a small window where it has one (for example 30 days of mail),
and use a test label, folder, channel or page you can change freely.

### Reporting a failed check

If a step does not give the expected result, open an issue with:

1. the connector, the step number, what you did and what you saw;
2. the source's state and health line from the **Sources** page (or `health` from `sources.list`), and
   the output of `nuggetd status --json`;
3. the rows for `sync:<kind>` on the **Privacy** page (the [egress ledger](privacy.md#the-egress-ledger)):
   time, host and bytes, which show which requests were made, without any content;
4. the matching lines from the daemon log: `logs/nuggetd.<date>.log` in the
   [data directory](../reference/data-dir.md). For more detail, restart the daemon with
   `NUGGET_LOG=debug nuggetd`, repeat the step, and attach those lines.

Logs never contain secrets or item content at `info` level and above, but `debug` lines can name
files, folders and subjects. Read them before you share them. More in
[Troubleshooting](troubleshooting.md#reporting-a-bug).

## Removing a source

Removing a source deletes everything indexed from it (by default) and the credentials stored for it.
Over IPC, `sources.remove` with `"purge": false` keeps the indexed items.

## Without the desktop app

Sources live in `sources.toml` in the config directory, but add and change them through the
[`sources.*` IPC methods](../reference/ipc.md), not by editing that file: the daemon validates
settings and stores secrets. Every kind's settings are in
[Connector settings](../reference/connectors.md).
