# Adding sources

A **source** is one configured connector: a folder, a vault, a mailbox, a calendar account. Add sources
on the desktop app's **Sources** page. Each one shows its state, item count, last sync and last error,
and has **Sync now**, **Disable** and **Remove** actions.

| Kind | What it indexes | Sign-in | How changes arrive |
|---|---|---|---|
| [`files`](sources-files.md) | Folders on this machine | None | Live file-system watcher (about 0.3 s debounce) |
| [`obsidian`](sources-obsidian.md) | An Obsidian vault's notes and attachments | None | Live file-system watcher |
| [`gmail`](sources-google.md) | Gmail, read-only | Your own Google OAuth client | Polled every 60 s (history deltas) |
| [`gcal`](sources-google.md) | Google Calendar, read-only | Your own Google OAuth client | Polled every 5 min (sync tokens) |
| [`imap`](sources-imap-caldav.md) | Any IMAP mailbox over TLS | Password or app password | IMAP IDLE on the inbox, plus polling |
| [`caldav`](sources-imap-caldav.md) | Any CalDAV calendar over https | Password or app password | Polled every 5 min (`sync-collection`) |

Microsoft 365, Slack, Notion, GitHub, Linear and the macOS-only sources (Apple Notes, Messages, Mail)
are planned and not available yet.

## What every source guarantees

- **Edits and deletes propagate.** A changed file or message is re-indexed, a deleted one disappears
  from search, and a rename is a delete plus a new item. The benchmark checks this for every
  operation.
- **Read-only.** Connectors never change your data. IMAP mailboxes are opened read-only, so messages
  are not marked as read.
- **Resumable.** Progress is saved only after the items before it are stored, so a crash or restart
  never skips anything; at worst a few items are re-checked.
- **Secrets stay in the keychain.** Passwords, OAuth tokens and OAuth client secrets go to the OS
  keychain. They never appear in `sources.toml`, logs, the audit log or IPC responses.
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

## Removing a source

Removing a source deletes everything indexed from it (by default) and the credentials stored for it.
Over IPC, `sources.remove` with `"purge": false` keeps the indexed items.

## Without the desktop app

Sources live in `sources.toml` in the config directory, but add and change them through the
[`sources.*` IPC methods](../reference/ipc.md), not by editing that file: the daemon validates
settings and stores secrets. Every kind's settings are in
[Connector settings](../reference/connectors.md).
