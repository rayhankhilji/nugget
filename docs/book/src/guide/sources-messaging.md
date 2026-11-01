# iMessage, WhatsApp and Telegram

Nugget can index your personal message history so you can search it and ask about it: who said what,
when you agreed on something, the address a friend sent you last spring. None of these sources signs
in to anything. Each one reads data that is already on your computer, and nothing is sent anywhere
while it does.

| Kind | Where the messages come from | Platforms | How changes arrive |
|---|---|---|---|
| `imessage` | The Messages app's database on this Mac (iMessage and SMS) | macOS | Polled every 2 min |
| `whatsapp` | Chat exports you save into a folder | macOS, Windows, Linux | Folder watcher, plus a check every 5 min |
| `telegram` | A Telegram Desktop data export (JSON) | macOS, Windows, Linux | Watcher on the export, plus a check every 5 min |

Signal is not supported. [Why not](#signal) is explained at the end of this page.

Add any of them on **Sources → Add source**, in the **Messages** group.

## Private by default

Personal messages are some of the most sensitive data you have, so these three sources are set up to
stay on your device:

- **Keep these chats on this device only** is on by default (the `private` setting). Every
  conversation is then marked [sensitive](privacy.md#sensitive-items): it is never sent to a cloud
  model for embedding, triage, knowledge extraction, Ask, Double Tap or Today, and MCP clients do not
  see it unless you allow them to see sensitive items. Search and answers about your chats then use
  only on-device models. Turn it off only if you are happy for passages of these chats to reach the
  cloud models you set up.
- Even with `private` off, every conversation still goes through the local
  [scan](privacy.md#sensitive-items) for payment card numbers, IBANs, US Social Security numbers and
  API keys, and anything it finds marks that item sensitive.
- **Attachments are never opened.** Nugget records only an attachment's file name, type and size, so
  that "the photo Sam sent" can be found. The photo, voice note or document itself is not read.
- **Nothing is changed.** Nugget never writes to your Messages database or your export files, and it
  never sends a message.
- **No network.** These connectors make no network requests at all, so nothing appears for them in
  the [egress ledger](privacy.md#the-egress-ledger).

## How chats become documents

Each conversation is split into **one document per calendar month** that has messages, titled
"*conversation name* — *Month Year*" (for example "Family — May 2024"). A month is small enough that a
search result points you to the right part of a long chat, and large enough to keep the context of a
discussion together.

Each document lists the messages in order, with the sender's name and the time:

```markdown
**Alice Smith** · 2024-05-01 09:12
> Hello
> second line

**Me** · 2024-05-01 09:13
> [photo: IMG-20240501-WA0001.jpg]
```

The document also records the participants, whether it is a group chat, how many messages it has and
when the first and last were sent, so search can filter by them.

**People.** Participants with a phone number or e-mail address are passed to the
[knowledge graph](knowledge.md)'s contacts extractor, which links them to the people it already knows
from your mail and calendars. Someone you message on WhatsApp and e-mail at work becomes one person
when the phone number or address matches. Someone known only by a number (a sender not in your
contacts, or an iMessage handle with **Show names from Contacts** off) becomes a person named by that
number, which later merges with the person once the number turns up with a name.

WhatsApp exports carry names, not numbers, for people saved in your phone's contacts: those people
are matched with the rest of your data by name only. Senders you have not saved appear in the export
as their phone number (for example `+49 151 23456789`) and are matched by it.

Editing or deleting is reflected too: when a chat changes (new messages, or a message deleted before
the next export), that month's document is updated; when a chat disappears, its documents are removed.

## iMessage

iMessage reads the Messages app's own database on your Mac
(`~/Library/Messages/chat.db`). It covers everything the Messages app shows on this Mac: iMessage, and
SMS too if your iPhone forwards text messages to it. Turn on **Messages in iCloud** on your iPhone and
Mac if you want your full history on the Mac.

### Give Nugget Full Disk Access

macOS protects the Messages database. Nugget can read it only after you allow it once:

1. Open **System Settings** (Apple menu → System Settings).
2. Go to **Privacy & Security**, then **Full Disk Access**.
3. Turn on **Nugget**. If **nuggetd** (Nugget's background service) is listed too, turn it on as well.
   If Nugget is not in the list, press **+** and choose it from Applications.
4. If macOS asks, quit and reopen Nugget.
5. Back in Nugget, press **Add** (or **Sync now** on a source you already added).

If access is missing, adding the source (or its next sync) fails with "Nugget needs Full Disk Access
to read your Messages", and the **Sources** page shows the same steps under it.

Full Disk Access lets an app read any file you can read. Nugget uses it only for the Messages
database and, if you keep **Show names from Contacts** on, your Contacts database. You can turn it off
again at any time in the same place; the source then stops updating and shows the error.

### What is read

- the text of each message, its time, and who sent it;
- the conversation's name and participants (phone numbers and e-mail addresses);
- attachment file names, types and sizes (never the attachments);
- with **Show names from Contacts** on (the default), the names that go with those phone numbers and
  addresses in the Contacts app, so you see "Alice Smith" instead of "+1 555 0100".

Nugget never opens the live database. On each pass it copies the database files into a temporary
folder in its own data directory, reads the copy, and deletes the copy afterwards. Messages keeps
working normally while Nugget reads. When the database files have not changed since the last pass
(same size and modification time), the pass copies and reads nothing; Nugget still copies the
database at least once an hour, and once after each restart.

### Options

| Option | Setting | Default | Meaning |
|---|---|---|---|
| Show names from Contacts | `contacts` | on | Look up names in the macOS address book |
| Include group chats | `include_groups` | on | Index group conversations as well as one-to-one chats |
| Days of history | `since_days` | everything | Only messages newer than this many days |
| Keep these chats on this device only | `private` | on | Mark every conversation sensitive (see above) |

Advanced: `db_path` points at a different `chat.db`, for example a copy from another Mac. With
`db_path` set, the source can be added on any platform. The desktop app has no field for it: set it
through the `sources.add` or `sources.update` IPC method (see
[Without the desktop app](sources.md#without-the-desktop-app)).

iMessage is not available on Windows or Linux, because the Messages database exists only on a Mac.

## WhatsApp

WhatsApp keeps your chats on your phone and offers no way for another app to read them. What it does
offer is **Export chat**, which saves one chat as a text file (and optionally its media) in a `.zip`.
Nugget watches a folder of these exports.

### Export a chat

**iPhone**

1. Open the chat in WhatsApp.
2. Tap the contact or group name at the top.
3. Scroll down and tap **Export Chat**.
4. Choose **Without Media**. (**Attach Media** works too, but makes a much larger file, and Nugget
   only lists the media file names.)
5. In the share sheet, send the `.zip` to your computer: **AirDrop** to a Mac, **Save to Files** in
   an iCloud Drive folder you sync with your computer, or mail it to yourself.

**Android**

1. Open the chat in WhatsApp.
2. Tap **⋮** (More options) → **More** → **Export chat**.
3. Choose **Without media** (or **Include media**; Nugget only lists media file names).
4. Share the file to your computer: Google Drive, Nearby Share or Quick Share, a USB cable, or e-mail
   to yourself.

Then put the file (the `.zip` as it is, or the `.txt` inside it) in **one folder on your computer**,
for example `Documents/WhatsApp exports`. Put every chat you export in that folder.

### Add the source

On **Add source → WhatsApp**, choose that folder. Nugget reads every `.zip` and `.txt` export in it,
and in its subfolders one level deep, and picks up new files as soon as they appear.

**Your name in WhatsApp** (`me`, optional) is your WhatsApp profile name as it appears in the export.
Exports name senders by how they are saved in your phone's contacts, and name you by your profile
name; setting it lets Nugget show your own messages as "Me".

WhatsApp cannot export automatically, so a chat in Nugget is as recent as its latest export. To bring
it up to date, export the chat again into the same folder and delete the older export of that chat.
Each chat is identified by its name, so the new export updates the same documents.

Options: **Include group chats**, **Days of history** and **Keep these chats on this device only**,
as for iMessage.

## Telegram

Telegram's own app for computers, **Telegram Desktop**, can export your data in a machine-readable
format. The phone apps and the web version cannot.

### Export your data

1. Install and open [Telegram Desktop](https://desktop.telegram.org/) and sign in.
2. Go to **Settings → Advanced → Export Telegram data**. To export one chat only, open it and choose
   **⋮ → Export chat history** instead.
3. Choose what to include: **Personal chats**, **Private groups**, and the other chat types you want.
   Photos, videos and files are optional; Nugget only lists their file names.
4. At the bottom, set **Format** to **Machine-readable JSON**. (The HTML format cannot be imported.)
5. Choose where to save it and press **Export**. Telegram creates a folder such as
   `Telegram Desktop/DataExport_2024-05-01` with a `result.json` inside.

### Add the source

On **Add source → Telegram**, choose that export folder (or type the path of its `result.json`). You
can also choose the parent folder: Nugget imports every `ChatExport_*` and `DataExport_*` export it
finds there.

To bring Telegram up to date, export again into the same parent folder and delete the older export.
Nugget watches the folder and picks up the new one.

Options: **Include group chats**, **Days of history** and **Keep these chats on this device only**,
as for iMessage.

## Signal

Signal is not supported, and will not be unless Signal adds a way to export your messages:

- **Signal Desktop has no export.** It offers no way to save your message history in a file you can
  read.
- **Its local database is encrypted**, with a key that Signal protects in your operating system's
  keychain. Reading it would mean taking that key and decrypting Signal's data behind its back. That
  bypasses the encryption model Signal is built on, and we will not do that, even with your consent:
  it would also make Nugget a tool for reading other people's Signal data.
- **Its backups are encrypted too** (the on-device backups on Android and Signal's own cloud
  backups), in a format that is not documented and meant only for restoring into Signal.

If Signal ships a documented export, we will add it.

## Troubleshooting

**"Nugget needs Full Disk Access to read your Messages"** — follow
[the steps above](#give-nugget-full-disk-access), then press **Sync now**. If Nugget is already turned
on in the list, turn it off and on again: after an update, macOS sometimes keeps a stale permission for
the old version.

**"iMessage is only available on macOS"** — the Messages database exists only on a Mac. To index
messages from another Mac, copy its `chat.db` and set `db_path` (see [Options](#options)).

**iMessage shows phone numbers instead of names** — turn on **Show names from Contacts**, and check
that the person is in the Contacts app on this Mac. Contacts is read with the same Full Disk Access.

**Some iMessage history is missing** — the Mac has only what Messages on the Mac shows. Turn on
**Messages in iCloud** on your iPhone and Mac and wait for it to download, or check **Days of
history**.

**A WhatsApp chat is not found** — check that the `.zip` or `.txt` is in the chosen folder or one
level below it, not deeper, and that it is an **Export chat** file (its text file is named like
`WhatsApp Chat with Alice.txt` or `_chat.txt`).

**A WhatsApp export can't be read** — an export Nugget cannot read (for example an unrecognised date
format or a damaged `.zip`) is shown as a problem on the source on **Sources**, instead of the source
saying it is up to date. Export the chat again; if the problem stays, check that the file is an
unchanged **Export chat** file.

**My WhatsApp messages are not shown as "Me"** — set **Your name in WhatsApp** to exactly the name that
appears next to your messages in the export's text file.

**Telegram finds nothing** — check that the export used **Machine-readable JSON** (there must be a
`result.json`), and that you chose the export folder or its parent.

**A deleted message is still there** — for WhatsApp and Telegram, Nugget can only see what is in your
exports. Export the chat again and delete the older export file.

To stop indexing a source, disable or remove it on **Sources**. Removing it also deletes everything
indexed from it (see [Removing a source](sources.md#removing-a-source)).
