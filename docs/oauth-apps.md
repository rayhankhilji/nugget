# Nugget's own OAuth apps

Release builds can carry Nugget's own **public** OAuth clients, so users choose **Connect** with no
setup. This is a one-time job for a maintainer: register one app per provider, then store its values
in the repository (**Settings → Secrets and variables → Actions**). `release.yml` passes them to the
"Build and stage the nuggetd sidecar" step, and `crates/nugget-oauth` bakes them in at build time.
A value left unset keeps that provider on bring-your-own-client; nothing real is committed.

There is no Nugget server. Every client below is a public (installed-app) client: PKCE or the
device flow, tokens only in the user's OS keychain. Register each app under a Nugget-owned account,
not a personal one, and give it the name "Nugget".

## Google (Gmail, Calendar, Drive)

In the [Google Cloud console](https://console.cloud.google.com/):

1. Create a project, for example `nugget-desktop`.
2. **APIs & Services → Library**: enable **Gmail API**, **Google Calendar API**, **Google Drive
   API**, **Google Sheets API** and **Google Slides API** (the last two improve Drive extraction;
   they use the same `drive.readonly` grant).
3. **OAuth consent screen** (or **Google Auth Platform → Branding / Audience / Data access**):
   user type **External**; app name `Nugget`; user support email and developer contact; homepage and
   privacy policy links. Add the scopes Nugget asks for: `gmail.readonly`, `gmail.modify` (only
   used when a user turns on "Allow archiving"), `calendar.readonly`, `drive.readonly`.
4. **Test users**: add each person who will use the build (up to 100).
5. **Credentials → Create credentials → OAuth client ID** (or **Clients → Create client**), type
   **Desktop app**. Copy the **Client ID** and **Client secret**. Google says a desktop client's
   secret is not confidential; we still store it as a repository secret.

**Verification, honestly.** `gmail.readonly`, `gmail.modify` and `drive.readonly` are *restricted*
scopes; `calendar.readonly` is *sensitive*. Going public with restricted scopes needs Google's app
verification **plus** a third-party security assessment (CASA), renewed every year; sensitive scopes
need verification only. Until then:

- Keep the publishing status at **Testing**. It is free and limited to the named test users (100).
- In Testing, Google expires refresh tokens after **7 days**, so test users must reconnect weekly
  (the source shows **Needs sign-in**).
- Switching to **In production** without verification shows an "unverified app" screen and caps the
  app at 100 users in total for these scopes.

Paste: variable `NUGGET_OAUTH_GOOGLE_CLIENT_ID`, secret `NUGGET_OAUTH_GOOGLE_CLIENT_SECRET`.

## Microsoft (Outlook mail, Outlook calendar, OneDrive)

In the [Microsoft Entra admin center](https://entra.microsoft.com/), **App registrations → New
registration**:

1. Name `Nugget`. **Supported account types**: *Accounts in any organizational directory and
   personal Microsoft accounts* (multi-tenant plus personal; Nugget uses tenant `common`).
2. **Redirect URI**: platform **Public client/native (mobile & desktop)**, value `http://localhost`.
   Nugget redirects to `http://127.0.0.1:<random port>`. If a test sign-in fails with
   `AADSTS50011` (redirect URI mismatch), also add `http://127.0.0.1` as a redirect of type
   `InstalledClient` in the app's **Manifest**.
3. **Authentication**: **Allow public client flows** → **Yes**. Add no client secret.
4. **API permissions → Add a permission → Microsoft Graph → Delegated**: `offline_access`,
   `User.Read`, `Mail.Read`, `Calendars.Read`, `Files.Read`. Do not grant admin consent here.
5. Copy the **Application (client) ID** from **Overview**.

Free. Without **publisher verification** (a verified Microsoft Partner Network ID linked under
**Branding & properties**) users see the app marked "unverified", and many work tenants block user
consent to unverified multi-tenant apps; there an admin must consent for the organisation.

Paste: variable `NUGGET_OAUTH_MICROSOFT_CLIENT_ID`.

## GitHub

**Settings → Developer settings → OAuth Apps → New OAuth App** (create it under the Nugget
organisation if there is one):

1. Name `Nugget`, homepage the project URL. **Authorization callback URL**: any https URL, for
   example the project homepage; the device flow never uses it.
2. **Register**, then tick **Enable Device Flow** and save.
3. Copy the **Client ID**. Do not generate a client secret; Nugget doesn't use one.

OAuth Apps have no read-only scope, so Nugget asks for `repo` and `read:discussion` and only reads.
Because revoking needs the secret, **Disconnect** deletes tokens locally and users revoke at
github.com/settings/applications.

Paste: variable `NUGGET_OAUTH_GITHUB_CLIENT_ID`.

## Linear

In Linear, **Settings → API → OAuth applications → New**: name `Nugget`, redirect URI
`http://127.0.0.1`, developer name and URL. Turn on **Public** so users in other workspaces can
authorize it. Nugget uses PKCE and no client secret. Copy the **Client ID**. Nugget asks only for
`read`.

Paste: variable `NUGGET_OAUTH_LINEAR_CLIENT_ID`.

## Slack

At [api.slack.com/apps](https://api.slack.com/apps), **Create New App → From scratch**:

1. **OAuth & Permissions → User Token Scopes** (no bot scopes): `channels:history`,
   `groups:history`, `im:history`, `mpim:history`, `channels:read`, `groups:read`, `im:read`,
   `mpim:read`, `users:read`.
2. **Redirect URLs**: add `http://localhost`. Nugget redirects to `http://localhost:<random
   port>/`.
3. **PKCE**: enable it on the same page. This makes the app a public client and cannot be undone.
   Slack expires a PKCE app's refresh token after 30 days without use; the user then reconnects.
4. **Manage Distribution → Activate Public Distribution**, so people in other workspaces can
   install it. Slack Marketplace review is optional. Some workspaces need an admin to approve apps.
5. **Basic Information**: copy the **Client ID**.

**Verify before release:** do one test sign-in from a build with this client. It is unverified
whether Slack accepts a random port on a registered `http://localhost` redirect; if it insists on
the exact port, one-click Slack doesn't work yet (a known gap) and users paste a token instead.

Paste: variable `NUGGET_OAUTH_SLACK_CLIENT_ID`.

## Notion: not possible

Notion's public OAuth requires a confidential client secret (HTTP Basic auth at its token
endpoint) and supports neither PKCE nor public clients. A desktop app with no server can't keep
that secret, so Nugget doesn't ship one. Notion stays "paste an internal integration token".

## Checklist

| Provider | Register at | Value | GitHub variable / secret |
|---|---|---|---|
| Google | console.cloud.google.com | Client ID | variable `NUGGET_OAUTH_GOOGLE_CLIENT_ID` |
| Google | same client | Client secret | **secret** `NUGGET_OAUTH_GOOGLE_CLIENT_SECRET` |
| Microsoft | entra.microsoft.com | Application (client) ID | variable `NUGGET_OAUTH_MICROSOFT_CLIENT_ID` |
| GitHub | github.com → Developer settings → OAuth Apps | Client ID | variable `NUGGET_OAUTH_GITHUB_CLIENT_ID` |
| Linear | Linear → Settings → API | Client ID | variable `NUGGET_OAUTH_LINEAR_CLIENT_ID` |
| Slack | api.slack.com/apps | Client ID | variable `NUGGET_OAUTH_SLACK_CLIENT_ID` |
| Notion | (none) | (none) | (none) |

Variables go under **Actions → Variables → Repository variables**, the secret under **Actions →
Secrets → Repository secrets**. With `gh`:

```sh
gh variable set NUGGET_OAUTH_GOOGLE_CLIENT_ID --body '<id>'
gh secret set NUGGET_OAUTH_GOOGLE_CLIENT_SECRET      # prompts for the value
```

## Verify

Build the daemon locally with the values set, start it, and ask which providers are built in:

```sh
NUGGET_OAUTH_GOOGLE_CLIENT_ID=... NUGGET_OAUTH_GOOGLE_CLIENT_SECRET=... \
NUGGET_OAUTH_MICROSOFT_CLIENT_ID=... NUGGET_OAUTH_GITHUB_CLIENT_ID=... \
NUGGET_OAUTH_LINEAR_CLIENT_ID=... NUGGET_OAUTH_SLACK_CLIENT_ID=... \
cargo build -p nuggetd
```

The IPC method `sources.oauth.builtin` returns `{"providers": [...]}`; each provider you set should
be listed. Then add one source per provider in the desktop app with **Connect** and check that sign-in
completes, sync starts, and **Disconnect** works. After a release, the same call against the released
daemon confirms the workflow passed the values through.
