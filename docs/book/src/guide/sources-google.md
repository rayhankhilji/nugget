# Gmail, Google Calendar and Google Drive

Nugget talks to Google directly, with **your own OAuth client**. No Nugget server sits in between, and
no Nugget-owned Google app sees your data. The trade-off is a one-time setup in the Google Cloud
console, and a weekly re-sign-in while your app stays in Google's "Testing" mode (explained
[below](#the-7-day-sign-in-limit-in-testing-mode)).

Nugget asks only for read-only scopes:

| Source | Scope | Sync |
|---|---|---|
| `gmail` | `https://www.googleapis.com/auth/gmail.readonly` | Backfills the last 365 days (`window_days`), newest first, then polls for changes every 60 s |
| `gcal` | `https://www.googleapis.com/auth/calendar.readonly` | Events from 365 days back to 365 days ahead (`past_days`, `future_days`), then polls every 5 min |
| `gdrive` | `https://www.googleapis.com/auth/drive.readonly` | Lists your files once, then follows Drive's change feed, polled every 5 min |

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

## Google Drive

> **Not yet tried against a live Google account.** The Drive connector has been tested only against a
> mock Drive server that imitates Google's API. It may still trip over details of the real service.
> If you try it, please check the list [below](#checking-drive-with-a-real-account) and report what
> you find.

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
these):

1. Sign-in with `drive.readonly` works with a Desktop-app OAuth client in Testing mode, and the consent
   screen shows only read-only Drive access.
2. The first sync finishes for a Drive with a few hundred files, and the item count matches what you
   expect (owned files only, with both options off).
3. A Google Doc's Markdown export reads well (headings, lists, tables), and a multi-sheet spreadsheet
   gives one CSV per sheet; with the Sheets API disabled, the first sheet is still indexed.
4. A Slides deck is indexed slide by slide with speaker notes; with the Slides API disabled, the plain
   export still works.
5. A PDF, a `.docx` and an `.xlsx` are searchable; an image and a file above `max_file_mb` appear with
   their "Not indexed" note.
6. Within about 5 minutes: an edit shows up, a rename and a move update the item, trashing removes it,
   restoring from Trash brings it back, and unsharing a shared file removes it.
7. Turning on **Include files shared with me** and **Include shared drives** adds those files;
   turning them off removes them.
8. Restarting Nugget in the middle of the first sync resumes it rather than starting over.
9. Each item's link opens the right file in Drive.
10. Revoking access at <https://myaccount.google.com/permissions> puts the source in **Needs sign-in**,
    and **Reconnect** recovers it.
11. The egress ledger shows the traffic under `sync:gdrive`, and nothing else.
