# Microsoft 365

Nugget talks to Microsoft Graph directly from your computer, with no Nugget server in between.
Release builds sign in with Nugget's own Entra app; you can
[use your own](#advanced-use-your-own-oauth-client) instead. It works with work or school accounts (Microsoft 365, Exchange Online) and personal accounts
(Outlook.com, OneDrive personal).

| Source | Permission | What is indexed | Sync |
|---|---|---|---|
| `outlook` | `Mail.Read` | Inbox and Sent Items (or folders you choose), last 365 days | Every 60 s |
| `outlook-calendar` | `Calendars.Read` | Your default calendar, 365 days back to 365 days ahead | Every 5 min |
| `onedrive` | `Files.Read` | Your OneDrive, or folders you choose | Every 5 min |

Each source also asks for `offline_access` (to stay signed in) and `User.Read` (to name the account).
These are read-only and cover only your own mailbox, calendar and files, not ones shared with you.

## Connect

1. On **Sources**, choose **Add source → Outlook mail** (or **Outlook calendar**, **OneDrive**).
2. Optionally set the tenant and the email to pre-select.
3. Choose **Connect** and approve in the browser. The redirect reaches a one-time listener on
   `127.0.0.1` (PKCE, with a state check) and syncing starts.

Some work or school organisations block user consent to outside apps; an administrator must then
approve Nugget for the organisation, or you can use your own registration (below). Tokens stay in the
keychain and are refreshed automatically. If access is revoked, the source shows **Needs sign-in**
(within about an hour); choose **Reconnect**.

**Disconnect** deletes the tokens on this computer. Microsoft offers desktop apps no way to revoke a
grant, so to remove Nugget's access entirely, also remove it at
[myapps.microsoft.com](https://myapps.microsoft.com) (work or school) or
[account.live.com/consent/Manage](https://account.live.com/consent/Manage) (personal). Removing the
source disconnects it too.

## Advanced: use your own OAuth client

Use this if you prefer your own registration, or if your build has none built in (builds from source
usually don't; they then require `client_id`). Your client always wins when set.

1. In the [Microsoft Entra admin center](https://entra.microsoft.com/), signed in with the account to
   index, go to **Identity → Applications → App registrations → New registration**. Any name.
2. **Supported account types**:
   - any organisation and personal accounts: the default tenant, `common`;
   - only your organisation: set `tenant` to your tenant id or domain;
   - only personal accounts: set `tenant` to `consumers`.
3. **Redirect URI**: platform **Public client/native (mobile & desktop)**, value `http://localhost`.
4. **Register**, and copy the **Application (client) ID**.

No client secret is needed (Nugget uses PKCE). If Microsoft reports a redirect URI mismatch
(`AADSTS50011`), add `http://127.0.0.1` as a redirect URI of type `InstalledClient` in the app's
**Manifest**. If your organisation blocks user consent, an administrator must **Grant admin
consent** for the permissions above under **API permissions**.

In Nugget, open **Advanced: use your own OAuth client** when adding the source, paste the client ID
and choose **Connect**. One registration serves all three sources.

## What gets indexed

- **Outlook mail**: each message, parsed for headers, body, quoted text and signatures. Drafts and
  messages over 35 MiB are skipped; attachments are listed by name only.
  Moves between synced folders and deletions are followed.
- **Outlook calendar**: each occurrence as text (title, time with time zone, location, organiser, up
  to 200 attendees, meeting link, description up to 20,000 characters). Cancelled events are removed.
  Only the default calendar is synced.
- **OneDrive**: Markdown, text, HTML, CSV, TSV, JSON, `.eml`, `.ics`, PDF, `.docx`, `.xlsx` and
  `.pptx` up to `max_file_mb` (25 MB). Other and larger files are skipped without a placeholder.
  Downloads use Microsoft's pre-authenticated links, fetched without your access token.

All settings: [Connector settings → `outlook`](../reference/connectors.md#outlook).
