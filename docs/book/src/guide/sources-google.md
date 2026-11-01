# Gmail, Google Calendar and Google Drive

Nugget talks to Google directly with **your own OAuth client**: no Nugget server or Nugget-owned
Google app sees your data. The cost is a one-time setup in the Google Cloud console, and a weekly
re-sign-in while your app is in Testing mode ([below](#the-7-day-sign-in-limit-in-testing-mode)).

| Source | Scope | Sync |
|---|---|---|
| `gmail` | `gmail.readonly` | Last 365 days (`window_days`), then changes every 60 s |
| `gcal` | `calendar.readonly` | 365 days back to 365 days ahead (`past_days`, `future_days`), then every 5 min |
| `gdrive` | `drive.readonly` | All files in scope, then Drive's change feed every 5 min |

Each source asks only for its own read-only scope. `drive.readonly` is the narrowest scope that can
read file content. The one exception is Gmail's **Allow archiving** (`allow_modify`, off): it asks for
`gmail.modify` so a suggested action can archive a thread after you confirm. Google grants that scope
everything except permanent deletion (including sending); Nugget uses it only to remove the `INBOX`
label. It takes effect after you reconnect.

## Set up your OAuth client

In the [Google Cloud console](https://console.cloud.google.com/), signed in with the account to index:

1. Create a project (any name) or pick one.
2. Under **APIs & Services → Library**, enable the **Gmail API**, **Google Calendar API** and/or
   **Google Drive API**. For Drive, also enable the **Google Sheets API** and **Google Slides API**;
   without them only a spreadsheet's first sheet is indexed and slides are one block of text.
3. Under **OAuth consent screen** (or **Google Auth Platform → Branding** and **Audience**), choose
   **External** (Workspace organisations can choose **Internal**, which avoids the testing limit),
   enter an app name and your email, and add your own account under **Test users**.
4. Under **Credentials** (or **Google Auth Platform → Clients**), **Create credentials → OAuth client
   ID**, type **Desktop app**. Copy the client ID and secret.

## Connect in Nugget

On **Sources**, choose **Add source → Gmail** (or **Google Calendar**, **Google Drive**), paste the
client ID and secret, and choose **Connect**. Google warns the app is unverified: it is yours, so
continue and approve. The browser redirects to a one-time listener on `127.0.0.1` and syncing starts.
Secrets and tokens stay in the OS keychain. One client can serve all three sources.

## The 7-day sign-in limit in Testing mode

While your app's publishing status is **Testing**, Google expires refresh tokens after **7 days**.
The source then shows **Needs sign-in** (`needs_auth`). Nothing is lost: choose **Reconnect** and
syncing resumes where it stopped.

To avoid this, set the publishing status to **In production**. Google still shows the "unverified
app" warning but stops expiring tokens. Unverified apps with restricted scopes such as
`gmail.readonly` are capped at a small number of users, so check Google's policy before sharing your
client. A shared, Google-verified Nugget client is planned.

## What gets indexed

- **Gmail**: each message, parsed for headers, body, quoted text and signatures. Spam and Trash are
  skipped (`include_spam_trash`). Attachments are listed by name, not indexed. Messages deleted in
  Gmail are deleted from Nugget. If Gmail's history id expires after a long time offline, the window
  is re-synced.
- **Calendar**: each event (and each occurrence of a repeating one) as text: title, time with time
  zone, location, attendees (up to 200), description (up to 20,000 characters). Cancelled events are
  removed. With `calendars` unset, every calendar selected in Google Calendar is synced.
- **Drive**: see below.

Gmail links use `mail.google.com/mail/u/0/`, so they open in the first Google account signed in to
your browser.

All settings: [Connector settings → `gmail`, `gcal`, `gdrive`](../reference/connectors.md#gmail).

## Google Drive

| In Drive | In Nugget |
|---|---|
| Google Docs | Exported as Markdown |
| Google Sheets | Each sheet as CSV (Sheets API), up to `max_file_mb` total; otherwise only the first sheet |
| Google Slides | Slide by slide with speaker notes (Slides API); otherwise plain text |
| PDF, `.docx`, `.xlsx`, `.pptx`, Markdown, text, CSV, HTML and similar | Extracted like files in a [folder source](sources-files.md) |
| Images, video, audio, archives, legacy Office, Forms, Drawings, files over `max_file_mb` (25 MB) | Listed by name with a "Not indexed: *reason*" note |

Folders and shortcuts are not items. Renames and moves update items; trashing, deleting or losing
access removes them. Every item links back to Drive. A restart during the first listing resumes it.

By default only files you own in **My Drive** are indexed. **Include files shared with me**
(`include_shared_with_me`) and **Include shared drives** (`include_shared_drives`) widen that. Raising
**Largest file (MB)** (`max_file_mb`, 1 to 200) fetches files skipped for size.

Rate limits and server errors are retried with backoff. If your project's **daily** quota runs out,
the source pauses for an hour and resumes on its own. Requests go only to `www.googleapis.com`,
`sheets.googleapis.com`, `slides.googleapis.com` and Google's token endpoint, logged as
`sync:gdrive`.
