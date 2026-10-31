# Gmail, Google Calendar and Google Drive

Nugget talks to Google directly, with **your own OAuth client**. No Nugget server sits in between, and
no Nugget-owned Google app sees your data. The trade-off is a one-time setup in the Google Cloud
console, and a weekly re-sign-in while your app stays in Google's "Testing" mode (explained
[below](#the-7-day-sign-in-limit-in-testing-mode)).

Nugget asks only for read-only scopes (the one exception is opt-in: see **Allow archiving** below):

| Source | Scope | Sync |
|---|---|---|
| `gmail` | `https://www.googleapis.com/auth/gmail.readonly` | Backfills the last 365 days (`window_days`), newest first, then polls for changes every 60 s |
| `gcal` | `https://www.googleapis.com/auth/calendar.readonly` | Events from 365 days back to 365 days ahead (`past_days`, `future_days`), then polls every 5 min |
| `gdrive` | `https://www.googleapis.com/auth/drive.readonly` | Lists your files once, then follows Drive's change feed, polled every 5 min |

**Scopes.** Each source asks for one scope, for its own product only. `gmail.readonly` and
`calendar.readonly` are the read-only scopes of those APIs: they cannot send, change or delete mail or
events. The one exception is Gmail's **Allow archiving** setting (`allow_modify`, off by default): it
asks for `gmail.modify` instead, so that a suggested action can archive a thread after you confirm it.
Google grants that scope everything except permanent deletion, including sending and drafting
mail; Nugget uses it only to remove the `INBOX` label. It takes effect after you reconnect, and turning
the setting off again (then reconnecting) goes back to `gmail.readonly`.

For Drive, `drive.readonly` is the narrowest scope that can read and export your files' content:
`drive.file` only sees files Nugget itself created or opened, and `drive.metadata.readonly` cannot read
content at all. Nugget never asks for permission to change, share or delete anything in Drive.

## Step 1: create a Google Cloud project

1. Open the [Google Cloud console](https://console.cloud.google.com/) and sign in with the Google
   account you want to index.
2. Create a project (any name, for example "Nugget"), or pick one you already have.

## Step 2: enable the APIs

Under **APIs & Services → Library**, enable the APIs for the sources you want:

- **Gmail API** for `gmail`;
- **Google Calendar API** for `gcal`;
- **Google Drive API** for `gdrive` (required), plus the **Google Sheets API** and the **Google Slides
  API** (recommended). Without the Sheets API only the first sheet of each spreadsheet is indexed;
  without the Slides API presentations are indexed as one block of text instead of slide by slide.

## Step 3: configure the consent screen

Google calls this the **OAuth consent screen** (newer consoles: **Google Auth Platform → Branding** and
**Audience**).

1. Choose **External** as the user type. ("Internal" exists only for Google Workspace organisations; if
   you have one, Internal avoids the testing limit below.)
2. Fill in an app name and your email address.
3. Under **Test users**, add your own Google account.

You do not need to add scopes here; Nugget requests them when you sign in.

## Step 4: create the OAuth client

1. Under **APIs & Services → Credentials** (or **Google Auth Platform → Clients**), choose **Create
   credentials → OAuth client ID**.
2. Application type: **Desktop app**. Any name.
3. Copy the **client ID** (it ends in `.apps.googleusercontent.com`) and the **client secret**.

For desktop apps Google does not treat the client secret as confidential, but Nugget still keeps it in
your OS keychain only.

## Step 5: connect in Nugget

1. On **Sources**, choose **Add source → Gmail** (or **Google Calendar**, **Google Drive**).
2. Paste the client ID and client secret, and optionally name the source.
3. Choose **Connect**. Your browser opens Google's sign-in page.
4. Google warns that the app is not verified: it is your own app, so continue. Approve the read-only
   access.
5. Google redirects to a one-time listener on `127.0.0.1` (a random port, open for at most 5 minutes)
   on your machine. The source switches to syncing.

Behind the scenes this is the OAuth 2.0 authorization-code flow with PKCE for installed apps (RFC 8252).
The tokens are stored in your OS keychain and refreshed automatically. To add another Google source,
repeat step 5 with the same client: one OAuth client can serve Gmail, Calendar and Drive. Each source
signs in separately and asks only for its own scope.

## The 7-day sign-in limit in Testing mode

While your OAuth app's publishing status is **Testing**, Google expires its refresh tokens after
**7 days**. After that, the source changes to **Needs sign-in** (`needs_auth`) and stops syncing.
Nothing is lost: choose **Reconnect** on the Sources page and sign in again; syncing resumes where it
stopped.

To avoid the weekly sign-in, you can set the app's publishing status to **In production**. For an app
only you use, Google shows the "unverified app" warning at sign-in but does not expire the tokens after
7 days. Unverified apps that request restricted scopes such as `gmail.readonly` are capped at a small
number of users; check Google's current OAuth policy before sharing your client with anyone.

A shared, Google-verified Nugget client is planned so this setup becomes optional; it needs Google's
verification of the restricted Gmail scope first.

## What gets indexed

- **Gmail**: each message as raw MIME (`message/rfc822`), parsed for headers, the text body, quoted
  text and signatures. Spam and Trash are skipped unless you set `include_spam_trash`. A message deleted
  in Gmail is deleted from Nugget.
- **Calendar**: each event as readable text (title, time, location, attendees, description). Cancelled
  events are removed. With `calendars` unset, every calendar selected in Google Calendar is synced.

- **Drive**: see [Google Drive](#google-drive) below.

If Gmail's history id expires (after a long time offline), the connector re-syncs the window instead of
skipping changes.

All settings: [Connector settings → `gmail`, `gcal`, `gdrive`](../reference/connectors.md#gmail).

## Checking Gmail with a real account

Gmail and Calendar are tested against mock Google servers; nobody has run this checklist against a
live account yet. It takes about 10 minutes. Set **Backfill window** to 30 days so the first sync is
short, and leave **Allow archiving** off. If a step fails, see
[Reporting a failed check](sources.md#reporting-a-failed-check).

1. **Connect.** Add a Gmail source and choose **Connect**. Expected: after the "unverified app"
   warning, Google asks for exactly one data permission, read-only mail ("View your email messages and
   settings", the `gmail.readonly` scope). Nothing about sending, deleting or other Google products.
   The source switches to `syncing`.
2. **First sync.** Wait for the source to reach `idle`. Expected: the item count is close to the
   number of messages (not conversations) Gmail finds for `newer_than:30d -in:drafts`. Spam, Trash
   and drafts are not counted. Messages stay unread in Gmail.
3. **Open a result.** Search for a word from a recent message and open the hit. Expected: it opens
   the message in Gmail. The link uses `mail.google.com/mail/u/0/`, so it opens in the first Google
   account signed in to your browser; note it if that is not the synced account.
4. **New mail and changes.** Send yourself a message, then add a label to an older one, and choose
   **Sync now**. Expected: the new message is searchable within a minute, and the labelled message is
   re-read (the `labels` in its details include the new label's id, such as `Label_12`). Marking a
   message read or unread alone causes no re-read.
5. **Move.** Archive a message, then move another to Spam, and choose **Sync now**. Expected: the
   archived one stays (it is still in All Mail); the one in Spam disappears.
6. **Delete.** Move a message to Trash and choose **Sync now**. Expected: it disappears from search.
   Restore it from Trash: it comes back on the next pass.
7. **Unicode.** Send yourself a message with the subject `Grüße 🍰 日本語 test`. Expected: the title
   shows exactly that, and searching for `Grüße` finds it.
8. **Large attachment.** Send yourself a message with a 20 MB attachment. Expected: the message is
   indexed and its body is searchable. The attachment is listed by name in the item's details; its
   content is not indexed (Nugget never extracts mail attachments). Gmail's 25 MB limit keeps mail
   below Nugget's 64 MiB download cap; a message above it would be skipped with a log warning.
9. **Revoke.** At <https://myaccount.google.com/permissions>, remove your app's access, then choose
   **Sync now**. Expected: within a few minutes the source shows **Needs sign-in** (`needs_auth`, health
   `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused."
   **Sign in** recovers it, and syncing resumes without downloading everything again.
10. **Egress.** Expected: the **Privacy** page lists this traffic under `sync:gmail`, to
    `gmail.googleapis.com` and `oauth2.googleapis.com` only.

## Checking Google Calendar with a real account

Nobody has run this checklist against a live account yet. Set **past_days** and **future_days** to 30
for a short first sync, and use a test calendar if you have one. If a step fails, see
[Reporting a failed check](sources.md#reporting-a-failed-check).

1. **Connect.** Add a Google Calendar source and choose **Connect**. Expected: Google asks for one
   permission, read-only calendars ("See and download any calendar you can access using your Google
   Calendar", the `calendar.readonly` scope), and nothing that lets Nugget change events.
2. **First sync.** Wait for `idle`. Expected: the item count matches the events in the window across
   the calendars ticked in Google Calendar's sidebar. Each occurrence of a repeating event is its own
   item, so a weekly meeting adds about nine items to a 60-day window.
3. **Edit.** Change an event's title and description, then choose **Sync now**. Expected: the item
   shows the new text.
4. **Move.** Drag an event to another day, and move a second one to another synced calendar. Choose
   **Sync now**. Expected: the first item's "When:" line shows the new day; the second is gone from the
   old calendar and appears under the new one.
5. **Delete.** Delete an event and choose **Sync now**. Expected: it disappears from search.
6. **Unicode.** Create an event titled `Café ☕ 会議`. Expected: the title shows exactly that and is
   searchable.
7. **Large event.** Paste a very long description (over 20,000 characters) and attach a Drive file.
   Expected: the event is indexed with its description cut at 20,000 characters; the attachment's
   content is not indexed. At most 200 attendees are listed.
8. **Recurring event.** Create a weekly event, then change the time of one occurrence and delete
   another. Expected: every occurrence in the window is an item; only the changed one shows the new
   time, and the deleted one disappears.
9. **Another time zone.** Create an event with its own time zone (in the event's details, **Time
   zone**), different from your calendar's. Expected: the item's "When:" line ends with a zone name and
   a UTC offset, like `(America/New_York, UTC-04:00)`, and the time and offset give the same moment
   Google Calendar shows. If the zone name is the event's but the time and offset are your calendar's,
   report it.
10. **Revoke.** Remove the app's access at <https://myaccount.google.com/permissions> and choose
    **Sync now**. Expected: **Needs sign-in**, health `reconnect`, with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused." **Sign in** recovers it.
11. **Egress.** Expected: traffic under `sync:gcal`, to `www.googleapis.com` and
    `oauth2.googleapis.com` only.

## Google Drive

> **Not yet tried against a live Google account.** The Drive connector has been tested only against a
> mock Drive server that imitates Google's API. It may still trip over details of the real service.
> If you try it, please work through the checklist [below](#checking-drive-with-a-real-account) and
> report what you find (see [Reporting a failed check](sources.md#reporting-a-failed-check)).

### What gets indexed

The first sync lists every file in scope (it remembers its place, so a restart resumes the listing
rather than starting over). After that the connector follows Drive's change feed from where it left
off, every 5 minutes. Renaming or moving a file updates it in Nugget; trashing or deleting it, or
losing access to it because it was unshared, removes it.

| In Drive | In Nugget |
|---|---|
| Google Docs | Exported as Markdown (plain text if that export fails) |
| Google Sheets | Each sheet as CSV, through the Google Sheets API, up to `max_file_mb` in total. Without the Sheets API, only the first sheet (Drive's own CSV export) |
| Google Slides | Text slide by slide, with speaker notes, through the Google Slides API. Without it, Drive's plain-text export |
| PDF, Word (`.docx`), Excel (`.xlsx`), PowerPoint (`.pptx`), Markdown, plain text, CSV, HTML and similar text files | Downloaded and extracted like files in a [folder source](sources-files.md) |
| Everything else: images, video, audio, archives, legacy Office files (`.doc`, `.xls`, `.ppt`), other Google file types such as Forms and Drawings, and any file over `max_file_mb` (25 MB by default) | Listed by name only, with a note "Not indexed: *reason*", so you can still find it and see why its content is missing |

Folders and shortcuts are not items of their own. Every item links back to the file in Drive, so a
search hit or a citation opens it in your browser.

### Your files, and shared content on request

By default a `gdrive` source indexes only files you own in **My Drive**. Two options, both off by
default, widen that:

- **Include files shared with me** (`include_shared_with_me`): files other people shared with you that
  are not in a shared drive.
- **Include shared drives** (`include_shared_drives`): files in shared drives (formerly Team Drives)
  you are a member of.

Raising **Largest file (MB)**
(`max_file_mb`, 1 to 200) re-fetches files that were skipped for size.

### Limits, quota and sign-in

- Google throttles busy clients. Rate-limit answers (`429`, and `403` "rate limit exceeded") and server
  errors (`5xx`) are retried with backoff, honouring Google's `Retry-After`.
- If your project's **daily** Drive quota is used up, the source pauses for an hour and the Sources page
  shows the quota error; it resumes from its last checkpoint by itself.
- If you revoke Nugget's access in your Google account, or the 7-day Testing limit
  [above](#the-7-day-sign-in-limit-in-testing-mode) is reached, the source shows **Needs sign-in**;
  choose **Reconnect**.

Every request, including token refreshes, exports and downloads, is in the egress ledger under
`sync:gdrive`. Requests go only to Google: the Drive, Sheets and Slides APIs (`www.googleapis.com`,
`sheets.googleapis.com`, `slides.googleapis.com`) and Google's token endpoint.

### Checking Drive with a real account

What a person should verify the first time with a real Google account (the mock server cannot prove
these). Most steps take a minute; the full list takes about 10 minutes on a Drive with a few hundred
files. Changes arrive through Drive's change feed, so choose **Sync now** after each change rather
than waiting 5 minutes. If a step fails, see [Reporting a failed check](sources.md#reporting-a-failed-check).

1. **Connect.** Sign in with a Desktop-app OAuth client in Testing mode. Expected: the consent screen
   asks for one permission, read-only Drive ("See and download all your Google Drive files", the
   `drive.readonly` scope), and nothing that lets Nugget change, share or delete files.
2. **First sync.** Expected: it finishes for a Drive with a few hundred files, and the item count
   matches what you expect: files you own in My Drive (folders and shortcuts are not items), with both
   sharing options off.
3. **Docs and Sheets.** Expected: a Google Doc's Markdown export reads well (headings, lists, tables),
   and a multi-sheet spreadsheet gives one CSV per sheet. With the Sheets API disabled, the first sheet
   is still indexed.
4. **Slides.** Expected: a deck is indexed slide by slide with speaker notes. With the Slides API
   disabled, the plain export still works.
5. **Other files.** Expected: a PDF, a `.docx` and an `.xlsx` are searchable. An image appears by name
   with the note "Not indexed: Images are not indexed.", and a file above `max_file_mb` with "Not
   indexed: Larger than the 25 MB limit." (or your limit). Raising **Largest file (MB)** fetches it.
6. **Edit, rename, move, delete.** Edit a Doc, rename one file, move another to a different folder,
   trash a third, then choose **Sync now**. Expected: the edit shows up, the rename and the move update
   their items, and the trashed file disappears. Restoring it from Trash brings it back; unsharing a
   file someone shared with you (with that option on) removes it.
7. **Unicode.** Name a Doc `Notizen über Kuchen 🍰 日本語`. Expected: the title shows exactly that and
   is searchable.
8. **Shared content.** Turn on **Include files shared with me** and **Include shared drives**.
   Expected: those files are added; turning the options off removes them.
9. **Restart.** Restart Nugget in the middle of the first sync. Expected: it resumes rather than
   starting over.
10. **Links.** Expected: each item's link opens the right file in Drive.
11. **Revoke.** Revoke access at <https://myaccount.google.com/permissions> and choose **Sync now**.
    Expected: the source shows **Needs sign-in** (health `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused.", and **Sign in** recovers it.
12. **Egress.** Expected: the **Privacy** page shows the traffic under `sync:gdrive`, to
    `www.googleapis.com`, `sheets.googleapis.com`, `slides.googleapis.com` and `oauth2.googleapis.com`,
    and nothing else.
