# Microsoft 365

Nugget talks to Microsoft Graph directly, with **your own app registration** in Microsoft Entra ID (the
new name for Azure Active Directory). No Nugget server sits in between and no Nugget-owned Microsoft app
sees your data. It works with work or school accounts (Microsoft 365, Exchange Online) and personal
Microsoft accounts (Outlook.com, OneDrive personal).

Nugget asks only for read-only delegated permissions. Each source requests its own scope plus
`offline_access` (for a refresh token) and `User.Read` (so the sign-in screen can name the account):

| Source | Graph permission | What is indexed | Sync |
|---|---|---|---|
| `outlook` | `Mail.Read` | Messages in Inbox and Sent Items (or the folders you choose), last 365 days | Per-folder delta queries, polled every 60 s |
| `outlook-calendar` | `Calendars.Read` | Events in your default calendar, 365 days back to 365 days ahead | Calendar-view delta queries, polled every 5 min |
| `onedrive` | `Files.Read` | Documents in your OneDrive (all of it, or the folders you choose) | Drive delta queries, polled every 5 min |

**Scopes.** These are the narrowest delegated permissions Graph offers for each job: `Mail.Read`,
`Calendars.Read` and `Files.Read` read only your own mailbox, calendars and OneDrive (not mail or
files shared with you, which need the `.Shared` or `.All` variants), and cannot write anything.
`offline_access` only lets the sign-in last beyond an hour, and `User.Read` only reads your basic
profile.

## Step 1: register an app

1. Sign in to the [Microsoft Entra admin center](https://entra.microsoft.com/) (or the Azure portal)
   with the account you want to index. A personal Microsoft account can register apps too.
2. Go to **Identity → Applications → App registrations → New registration**.
3. Name: anything, for example "Nugget".
4. **Supported account types**: pick the one that matches your account:
   - work or school and personal accounts: "Accounts in any organizational directory and personal
     Microsoft accounts" (Nugget's default tenant, `common`);
   - only your organisation: "Accounts in this organizational directory only" (then set `tenant` to
     your tenant id or domain);
   - only personal accounts: "Personal Microsoft accounts only" (then set `tenant` to `consumers`).
5. **Redirect URI**: platform **Public client/native (mobile & desktop)**, value `http://localhost`.
6. Choose **Register**, and copy the **Application (client) ID** from the overview page.

You do not need a client secret. Nugget signs in as a public client with PKCE, the flow Microsoft
recommends for desktop apps. If you registered under the **Web** platform instead and created a
secret, Nugget can use it: it goes to the OS keychain, never into `sources.toml`.

During sign-in Nugget listens on `127.0.0.1` on a random port for the redirect. If Microsoft reports
a redirect URI mismatch (error `AADSTS50011`), open the app's **Manifest** and add `http://127.0.0.1`
as a redirect URI of type `InstalledClient`: the portal form does not accept that address directly.

## Step 2: API permissions (optional)

Under **API permissions → Add a permission → Microsoft Graph → Delegated permissions**, you can add
`offline_access`, `User.Read`, `Mail.Read`, `Calendars.Read` and `Files.Read`. This is optional: the
permissions are requested at sign-in either way. Some organisations block user consent; there an
administrator has to **Grant admin consent** for these permissions before you can sign in.

## Step 3: connect in Nugget

1. On **Sources**, choose **Add source → Outlook mail** (or **Outlook calendar**, **OneDrive**).
2. Paste the application (client) ID. Optionally set the tenant and the email address to pre-select.
3. Choose **Connect**. Your browser opens Microsoft's sign-in page, with the account picker shown so
   you can connect a second account.
4. Approve the read-only access. The redirect reaches a one-time listener on `127.0.0.1` (open for at
   most 5 minutes), and the source switches to syncing.

One app registration can serve all three sources; each source signs in separately and asks only for
its own permission. Tokens are stored in your OS keychain under `oauth:<source id>` and refreshed
automatically. If a refresh is refused (you revoked access, or an administrator did), the source goes
to **Needs sign-in** (`needs_auth`); choose **Reconnect**.

Over IPC this is `sources.add` with `{"client_id": "…"}`, then `sources.auth.begin`, which returns the
URL to open.

## What gets indexed

- **Outlook mail**: each message as raw MIME (`message/rfc822`), parsed for headers, the text body,
  quoted text and signatures. Drafts are skipped, and so are messages over 35 MiB. A message moved
  between two synced folders is a delete under one and a new item under the other; a deleted message
  is removed from Nugget. Read flags and categories do not cause a re-download.
- **Outlook calendar**: each event instance as readable text (title, time, location, organiser,
  attendees, online-meeting link, description). Recurring series are expanded into their occurrences.
  Cancelled events are removed. Outlook's own sensitivity label (private, confidential) is kept in the
  item's metadata. The window slides: once it has moved more than 7 days, the next pass lists the new
  window.
- **OneDrive**: files of these types up to `max_file_mb` (25 MB by default): Markdown, plain text, HTML,
  CSV and TSV, JSON, `.eml`, `.ics`, PDF, `.docx`, `.xlsx` and `.pptx`. Other files are skipped. File
  contents come from Microsoft's pre-authenticated download links, which Nugget fetches **without**
  your access token and only over HTTPS from Microsoft's download hosts (`sharepoint.com`,
  `onedrive.com`, `1drv.com` and similar).

If Microsoft expires a delta token (after a long time offline), the connector re-lists the window
instead of skipping changes. Throttling (`429`, `503` with `Retry-After`) is retried up to 4 times in
a pass; after that the source backs off and resumes from its last checkpoint.

Whether an item is **sensitive** is decided at indexing time, as for every source (see
[Privacy](privacy.md)); Microsoft sources have no special default.

Every request, including token refreshes and file downloads, is in the egress ledger under
`sync:outlook`, `sync:outlook-calendar` or `sync:onedrive`.

All settings: [Connector settings → `outlook`](../reference/connectors.md#outlook).

## Checking Microsoft 365 with a real account

The three connectors are tested against a mock Graph server; nobody has run these checklists against
a live account yet. Each takes about 10 minutes. Changes arrive by polling, so choose **Sync now**
after each change. If a step fails, see [Reporting a failed check](sources.md#reporting-a-failed-check).

To revoke access for the last step of each list:

- personal Microsoft account: <https://account.live.com/consent/Manage>, pick your app, **Remove these
  permissions**;
- work or school account: <https://myaccount.microsoft.com> → **Sign out everywhere**, which revokes
  every refresh token (an administrator can also remove the app's consent in the Entra admin center).

Access tokens already issued keep working until they expire, so the source can take up to about an
hour to notice.

### Checking Outlook mail with a real account

Set **window_days** to 30 for a short first sync.

1. **Connect.** Add an Outlook mail source and choose **Connect**. Expected: Microsoft's consent
   screen lists "Read your mail", "Sign you in and read your profile" and "Maintain access to data you
   have given it access to" (`Mail.Read`, `User.Read`, `offline_access`), and nothing that sends or
   changes mail.
2. **First sync.** Wait for `idle`. Expected: the item count is close to the number of messages from
   the last 30 days in Inbox and Sent Items together, without drafts. Messages stay unread in Outlook.
3. **New mail.** Send yourself a message and choose **Sync now**. Expected: it is searchable within
   a minute. Flagging it or adding a category does not re-download it.
4. **Move.** Move a message to Archive (not synced by default) and choose **Sync now**. Expected: it
   disappears. Add `archive` to `folders`: it appears again, under Archive.
5. **Delete.** Delete a message and choose **Sync now**. Expected: it disappears from search (Deleted
   Items is not synced).
6. **Unicode.** Send yourself a message with the subject `Grüße 🍰 日本語 test`. Expected: the title
   shows exactly that and is searchable.
7. **Large attachments.** Send yourself one message with a 10 MB attachment, and, on a work account,
   one with a 30 MB attachment. Expected: the first is indexed, with the attachment listed by name and
   its content not indexed. The second is over Nugget's 35 MiB limit once encoded (attachments grow by
   a third in MIME), so it is skipped: it does not appear, and the log says "skipping oversized
   message".
8. **Revoke.** Revoke access as described above. Expected: once the access token expires, **Needs
   sign-in** (health `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused." **Sign in** recovers it.
9. **Egress.** Expected: the **Privacy** page shows traffic under `sync:outlook`, to
   `graph.microsoft.com` and `login.microsoftonline.com` only.

### Checking Outlook calendar with a real account

Set **past_days** and **future_days** to 30 for a short first sync.

1. **Connect.** Expected: the consent screen lists "Read your calendars", "Sign you in and read your
   profile" and "Maintain access to data you have given it access to" (`Calendars.Read`, `User.Read`,
   `offline_access`).
2. **First sync.** Expected: one item per event occurrence in your **default** calendar within the
   window. Other calendars are not synced. Each occurrence of a recurring series is its own item.
3. **Edit.** Change an event's title and choose **Sync now**. Expected: the item shows the new title.
4. **Move.** Drag an event to another day and choose **Sync now**. Expected: its "When:" line shows
   the new day.
5. **Delete.** Delete an event, and cancel a meeting you organise. Choose **Sync now**. Expected: both
   disappear from search.
6. **Unicode.** Create an event titled `Café ☕ 会議`. Expected: the title shows exactly that and is
   searchable.
7. **Large event.** Paste a very long description and attach a file. Expected: the event is indexed
   with its description cut at 20,000 characters; the attachment is not indexed. At most 200 attendees
   are listed.
8. **Recurring event.** Create a weekly series, then change one occurrence and delete another.
   Expected: one item per occurrence in the window; only the changed one shows its change, and the
   deleted one disappears.
9. **Another time zone.** Create an event with a different start time zone (Outlook's **Time zones**
   option on the event). Expected: the "When:" line shows the time in that zone, with its name and UTC
   offset, for example `2026-11-03 10:00 – 11:30 (Pacific Standard Time, UTC-08:00)`.
10. **Revoke.** Expected: **Needs sign-in** (health `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused."; **Sign in** recovers it.
11. **Egress.** Expected: traffic under `sync:outlook-calendar`, to `graph.microsoft.com` and
    `login.microsoftonline.com` only.

### Checking OneDrive with a real account

Use a small test folder: put its path in `folders` so the first sync is short.

1. **Connect.** Expected: the consent screen lists "Read your files", "Sign you in and read your
   profile" and "Maintain access to data you have given it access to" (`Files.Read`, `User.Read`,
   `offline_access`), and nothing that changes or shares files.
2. **First sync.** Expected: the item count matches the files of supported types (Markdown, text,
   HTML, CSV, JSON, `.eml`, `.ics`, PDF, `.docx`, `.xlsx`, `.pptx`) up to 25 MB in the folder. Folders,
   images and other types are not items.
3. **Edit.** Edit a `.docx` in Word for the web and choose **Sync now**. Expected: the new text is
   searchable.
4. **Rename and move.** Rename one file and move another to a subfolder that is still in scope.
   Choose **Sync now**. Expected: both items update (new name, new path), with no duplicates.
5. **Delete.** Delete a file and choose **Sync now**. Expected: it disappears from search. Deleting a
   whole folder removes every file in it.
6. **Unicode.** Name a file `Notizen über Kuchen 🍰 日本語.md`. Expected: the title shows exactly that
   and is searchable.
7. **Large file.** Upload a PDF over 25 MB. Expected: it does not appear at all (unlike Google Drive,
   OneDrive has no "Not indexed" placeholder). Raising **max_file_mb** above its size indexes it.
8. **Revoke.** Expected: **Needs sign-in** (health `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused."; **Sign in** recovers it.
9. **Egress.** Expected: traffic under `sync:onedrive`, to `graph.microsoft.com`,
   `login.microsoftonline.com` and Microsoft's download hosts (names ending in `sharepoint.com`,
   `1drv.com` or similar). File downloads carry no access token.
