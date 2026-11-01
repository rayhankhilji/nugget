# iMessage, WhatsApp and Telegram

These sources let you search and ask about your personal message history. None signs in to anything:
each reads data already on your computer, and makes no network requests.

| Kind | Where the messages come from | Platforms | How changes arrive |
|---|---|---|---|
| `imessage` | The Messages app's database on this Mac (iMessage and SMS) | macOS | Every 2 min |
| `whatsapp` | Chat exports you save into a folder | All | Folder watcher, plus a check every 5 min |
| `telegram` | A Telegram Desktop data export (JSON) | All | Folder watcher, plus a check every 5 min |

Add them on **Sources → Add source**, in the **Messages** group. [Signal](#signal) is not supported.

## Private by default

- **Keep these chats on this device only** (`private`, on by default) marks every conversation
  [sensitive](privacy.md#sensitive-items): never sent to a cloud model, and hidden from MCP clients
  unless you allow sensitive items. With it off, the local sensitive-data scan still applies.
- **Attachments are never opened** (only name, type and size are recorded), nothing is written to
  your Messages database or exports, and no message is ever sent.

## How chats become documents

Each conversation becomes **one document per calendar month** (for example "Family — May 2024"),
listing messages with sender and time; changed or vanished chats are updated or removed. Participants
are linked to people the [knowledge graph](knowledge.md) knows by phone number or email (WhatsApp
exports give saved contacts by name only, so those match by name).

## iMessage

Reads `~/Library/Messages/chat.db`: everything Messages shows on this Mac, including SMS if your
iPhone forwards them. Turn on **Messages in iCloud** for your full history.

### Give Nugget Full Disk Access

In **System Settings → Privacy & Security → Full Disk Access**, turn on **Nugget** (and **nuggetd**
if listed; press **+** to add Nugget if it is missing). Reopen Nugget if macOS asks, then press
**Add** or **Sync now**.

Without access, the source fails with "Nugget needs Full Disk Access to read your Messages". If Nugget
is already on in the list, turn it off and on again: after an update macOS sometimes keeps a stale
permission. Nugget uses the access only for Messages and, with **Show names from Contacts** on, your
Contacts database. Each pass reads a temporary copy of the database, never the live file.

### Options

| Option | Setting | Default | Meaning |
|---|---|---|---|
| Show names from Contacts | `contacts` | on | Show contact names instead of numbers |
| Include group chats | `include_groups` | on | Index group conversations too |
| Days of history | `since_days` | everything | Only messages newer than this |
| Keep these chats on this device only | `private` | on | Mark every conversation sensitive |

`db_path` points at another `chat.db` (for example a copy from another Mac) and then works on any
platform. The app has no field for it; set it over IPC (see
[Without the desktop app](sources.md#without-the-desktop-app)).

## WhatsApp

WhatsApp offers no way for other apps to read chats, only **Export chat**. Nugget watches a folder of
these exports.

1. Export each chat:
   - **iPhone**: open the chat, tap its name, **Export Chat → Without Media**, and send the `.zip`
     to your computer (AirDrop, Files, or email).
   - **Android**: open the chat, **⋮ → More → Export chat → Without media**, and send it to your
     computer.
2. Put the `.zip` (or the `.txt` inside it) in one folder, for example `Documents/WhatsApp exports`.
   Subfolders one level deep are read too.
3. **Add source → WhatsApp**, choose that folder, and optionally set **Your name in WhatsApp**
   (`me`), your profile name as it appears in the export, so your messages show as "Me".

A chat is only as recent as its latest export. To update it, export it again into the folder and
delete the older export; the chat's name keeps it the same document. If an export can't be read (an
unrecognised date format or damaged `.zip`), the source shows a problem: export it again.

Options: as for iMessage, except Contacts.

## Telegram

Only **Telegram Desktop** can export machine-readable data.

1. In [Telegram Desktop](https://desktop.telegram.org/), **Settings → Advanced → Export Telegram
   data** (or **⋮ → Export chat history** in one chat).
2. Choose the chat types you want; media are optional (only file names are recorded).
3. Set **Format** to **Machine-readable JSON** (HTML can't be imported) and export. You get a folder
   such as `DataExport_2024-05-01` containing `result.json`.
4. **Add source → Telegram** and choose the export folder, its `result.json`, or a parent folder
   (every `ChatExport_*` and `DataExport_*` inside is imported).

To update, export again into the same parent folder and delete the older export. Options: as for
iMessage, except Contacts.

## Signal

Not supported. Signal has no export, and its database and backups are encrypted; reading them would
mean bypassing Signal's encryption, which Nugget will not do. A documented export would change that.
