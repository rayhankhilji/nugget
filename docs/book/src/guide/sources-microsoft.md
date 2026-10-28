# Microsoft 365

> **Status: being wired in (wave W).** The `outlook`, `outlook-calendar` and `onedrive` connectors are
> implemented and tested in `nugget-connector-microsoft`, but this build of `nuggetd` does not register
> them yet, so `sources.add` refuses these kinds and the **Sources** page does not offer them. The
> daemon registration and the desktop wizard are landing in this wave. This page describes how they
> work once registered.

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
