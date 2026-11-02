# Slack, Notion, GitHub and Linear

Each connector reads the service's official API directly. No Nugget server or model sits in between,
so sync works with any AI setup or none.

| Source | What is indexed | Sign-in | Sync |
|---|---|---|---|
| `slack` | Your DMs and channels: one document per conversation per day, and one per thread | **Connect**, or a pasted user token (`xoxp-…`) | Every 5 min; last 3 days re-read every 6 h |
| `notion` | Pages and databases shared with your integration, as Markdown | A pasted internal integration secret | Every 5 min; full listing every 6 h |
| `github` | Issues, pull requests and discussions you take part in | **Connect** (device flow), or a pasted fine-grained token | Every 10 min; full listing daily |
| `linear` | Issues assigned to, created by or subscribed to by you | **Connect**, or a pasted personal API key | Every 5 min; full listing every 6 h |

The full listing removes items deleted, archived, unshared or moved out of scope. Nothing is posted,
edited or marked read.

**Connect** signs in with Nugget's own app for that service, with no setup. Builds without a built-in
app (usually builds from source) offer only the pasted token, or your own app under **Advanced: use
your own OAuth client** (`client_id`); your app always wins when set. Tokens are kept in the OS
keychain and refreshed automatically (for headless setups, `token_env` names an environment variable
instead). A missing or rejected token shows **Needs sign-in**: reconnect or paste a new one.

**Disconnect** revokes the grant at Slack and Linear and deletes the tokens. GitHub needs the app's
secret to revoke, so Nugget deletes the tokens on this computer and you revoke at
[github.com/settings/applications](https://github.com/settings/applications). Pasted tokens are kept
until you remove the source; revoke those where you created them.

## Slack

**Connect.** Choose **Add source → Slack → Connect** and approve in the browser (some workspaces need
an admin to approve the app). Nugget gets a user token with read-only scopes: `channels:history`,
`groups:history`, `im:history`, `mpim:history`, `channels:read`, `groups:read`, `im:read`,
`mpim:read` and `users:read`. If Nugget doesn't sync for 30 days, Slack expires the sign-in; then reconnect.

**Paste a token.**

1. At [api.slack.com/apps](https://api.slack.com/apps), **Create New App → From scratch** and pick
   the workspace.
2. Under **OAuth & Permissions → User Token Scopes**, add the scopes above. Leave out kinds you don't
   want (and turn off the matching option, such as `include_dms`).
3. **Install to Workspace**, copy the **User OAuth Token** (`xoxp-…`), not a bot token (a bot sees
   only conversations it was invited to), and paste it when you add the source.

**Advanced: your own app.** In that app, add the redirect URL `http://localhost`, turn on **PKCE**
under **OAuth & Permissions** (this cannot be undone), and put its client ID in `client_id`.

Nugget indexes up to 100 conversations (`max_conversations`) from the last 90 days (`window_days`),
or only those listed in `conversations`. Archived conversations are skipped. Files appear as
`[file: <name>]`, not their content. An edit or deletion in an older message shows up at the next
6-hourly re-read.

## Notion

1. At [notion.so/profile/integrations](https://www.notion.so/profile/integrations), create an
   **Internal** integration for the workspace.
2. Under **Capabilities**, keep only **Read content** (plus, optionally, **Read user information
   without email addresses**). Copy the **Internal Integration Secret**.
3. Share each top-level page or database with it (**… → Connections → Add connections**). Child pages
   are included; nothing else is visible to Nugget.
4. On **Sources**, choose **Add source → Notion** and paste the secret.

There is no **Connect** for Notion: its OAuth needs a client secret kept on a server, which a
local-only app doesn't have.

Each page becomes one Markdown document (title, database properties, blocks down to `max_depth`, 6 by
default), cut at 3,000 blocks or 500,000 characters. Databases are indexed as a column description
plus their rows as pages (`include_databases`). The 5,000 most recently edited pages are kept
(`max_pages`). Files appear by caption or name only.

## GitHub

**Connect.** **Connect** shows a user code and opens
[github.com/login/device](https://github.com/login/device); approve within 5 minutes. GitHub has no
read-only scope for OAuth Apps, so this asks for `repo` and `read:discussion`; Nugget still only
reads.

**Token (narrowest access).** Create a
[fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) for the
repositories you want, with **Read-only** access to **Issues**, **Pull requests**, **Discussions**
and **Metadata**, and paste it when you add the source. (A classic `repo` token works but also grants
write access.)

**Advanced: your own app.** Register an [OAuth App](https://github.com/settings/applications/new)
(homepage and callback URLs are unused), enable **Device Flow**, and put its client ID in
`client_id`. A GitHub App's user tokens expire after 8 hours and Nugget can't refresh them yet, so
prefer an OAuth App.

**What is indexed.** With `repos` unset: every issue, pull request and discussion that involves you,
updated in the last 365 days (`window_days`). With `repos` (`owner/name`, up to 100): the same within
those repositories, or everything with `all_activity`. Each item has its body, comments and reviews
(up to 300 comments; bodies cut at 60,000 characters). `include_discussions` turns discussions off.

## Linear

**Connect.** Choose **Connect** and approve in the browser. Nugget signs in with PKCE and asks
only for the `read` scope.

**API key.** In Linear, **Settings → Security & access → Personal API keys**: create a read-only key
and paste it when you add the source.

**Advanced: your own app.** Create an OAuth application in Linear (**Settings → API → OAuth
applications**) with redirect URI `http://127.0.0.1`, and put its client ID in `client_id`. No client
secret is needed.

**What is indexed.** Issues assigned to you, created by you or that you subscribe to (`include`), in
all teams or those in `teams` (for example `["ENG"]`), updated in the last 365 days. Each issue
includes its fields, description and comments. Archived and deleted issues are removed.

All settings: [Connector settings → `slack`](../reference/connectors.md#slack).
