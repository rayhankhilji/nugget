# Gmail and Google Calendar

Nugget talks to Google directly, with **your own OAuth client**. No Nugget server sits in between, and
no Nugget-owned Google app sees your data. The trade-off is a one-time setup in the Google Cloud
console, and a weekly re-sign-in while your app stays in Google's "Testing" mode (explained
[below](#the-7-day-sign-in-limit-in-testing-mode)).

Nugget asks only for read-only scopes:

| Source | Scope | Sync |
|---|---|---|
| `gmail` | `https://www.googleapis.com/auth/gmail.readonly` | Backfills the last 365 days (`window_days`), newest first, then polls for changes every 60 s |
| `gcal` | `https://www.googleapis.com/auth/calendar.readonly` | Events from 365 days back to 365 days ahead (`past_days`, `future_days`), then polls every 5 min |

## Step 1: create a Google Cloud project

1. Open the [Google Cloud console](https://console.cloud.google.com/) and sign in with the Google
   account you want to index.
2. Create a project (any name, for example "Nugget"), or pick one you already have.

## Step 2: enable the APIs

Under **APIs & Services → Library**, enable the **Gmail API**, the **Google Calendar API**, or both.

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

1. On **Sources**, choose **Add source → Gmail** (or **Google Calendar**).
2. Paste the client ID and client secret, and optionally name the source.
3. Choose **Connect**. Your browser opens Google's sign-in page.
4. Google warns that the app is not verified: it is your own app, so continue. Approve the read-only
   access.
5. Google redirects to a one-time listener on `127.0.0.1` (a random port, open for at most 5 minutes)
   on your machine. The source switches to syncing.

Behind the scenes this is the OAuth 2.0 authorization-code flow with PKCE for installed apps (RFC 8252).
The tokens are stored in your OS keychain and refreshed automatically. To use Google Calendar too,
repeat step 5 with the same client: one OAuth client can serve both sources.

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

If Gmail's history id expires (after a long time offline), the connector re-syncs the window instead of
skipping changes.

All settings: [Connector settings → `gmail`, `gcal`](../reference/connectors.md#gmail).
