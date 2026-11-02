# Slack, Notion, GitHub and Linear

Each connector reads the service's official API with **your own token**. No Heed server or model
sits in between, so sync works with any AI setup or none.

| Source | What is indexed | Token | Sync |
|---|---|---|---|
| `slack` | Your DMs and channels: one document per conversation per day, and one per thread | A user token (`xoxp-…`) from your own Slack app | Every 5 min; last 3 days re-read every 6 h |
| `notion` | Pages and databases shared with your integration, as Markdown | An internal integration secret | Every 5 min; full listing every 6 h |
| `github` | Issues, pull requests and discussions you take part in | A fine-grained token, or the device flow | Every 10 min; full listing daily |
| `linear` | Issues assigned to, created by or subscribed to by you | A personal API key, or OAuth | Every 5 min; full listing every 6 h |

The full listing removes items deleted, archived, unshared or moved out of scope. Nothing is posted,
edited or marked read.

Tokens are kept in the OS keychain (for headless setups, `token_env` names an environment variable
instead). A missing or rejected token shows **Needs sign-in**: paste a new one or reconnect.

## Slack

1. At [api.slack.com/apps](https://api.slack.com/apps), **Create New App → From scratch** and pick
   the workspace.
2. Under **OAuth & Permissions → User Token Scopes**, add `channels:history`, `groups:history`,
   `im:history`, `mpim:history`, `channels:read`, `groups:read`, `im:read`, `mpim:read` and
   `users:read`. Leave out kinds you don't want (and turn off the matching option, such as
   `include_dms`).
3. **Install to Workspace** (some workspaces need admin approval).
4. Copy the **User OAuth Token** (`xoxp-…`), not a bot token: a bot sees only conversations it was
   invited to.
5. On **Sources**, choose **Add source → Slack** and paste it.

Heed indexes up to 100 conversations (`max_conversations`) from the last 90 days (`window_days`),
or only those listed in `conversations`. Archived conversations are skipped. Files appear as
`[file: <name>]`, not their content. An edit or deletion in an older message shows up at the next
6-hourly re-read.

## Notion

1. At [notion.so/profile/integrations](https://www.notion.so/profile/integrations), create an
   **Internal** integration for the workspace.
2. Under **Capabilities**, keep only **Read content** (plus, optionally, **Read user information
   without email addresses**). Copy the **Internal Integration Secret**.
3. Share each top-level page or database with it (**… → Connections → Add connections**). Child pages
   are included; nothing else is visible to Heed.
4. On **Sources**, choose **Add source → Notion** and paste the secret.

Each page becomes one Markdown document (title, database properties, blocks down to `max_depth`, 6 by
default), cut at 3,000 blocks or 500,000 characters. Databases are indexed as a column description
plus their rows as pages (`include_databases`). The 5,000 most recently edited pages are kept
(`max_pages`). Files appear by caption or name only.

## GitHub

**Token (simplest).** Create a
[fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) for the
repositories you want, with **Read-only** access to **Issues**, **Pull requests**, **Discussions**
and **Metadata**. On **Sources**, choose **Add source → GitHub** and paste it. (A classic `repo` token
works but also grants write access.)

**Device flow.** Register your own [OAuth App](https://github.com/settings/applications/new) (homepage
and callback URLs are unused), enable **Device Flow**, and put its client ID in `client_id`.
**Connect** shows a user code and opens [github.com/login/device](https://github.com/login/device);
approve within 5 minutes. GitHub has no read-only scope for OAuth Apps, so this asks for `repo` and
`read:discussion`; Heed still only reads. A GitHub App's user tokens expire after 8 hours and Heed can't refresh them yet, so prefer a
token or an OAuth App.

**What is indexed.** With `repos` unset: every issue, pull request and discussion that involves you,
updated in the last 365 days (`window_days`). With `repos` (`owner/name`, up to 100): the same within
those repositories, or everything with `all_activity`. Each item has its body, comments and reviews
(up to 300 comments; bodies cut at 60,000 characters). `include_discussions` turns discussions off.

## Linear

**API key (simplest).** In Linear, **Settings → Security & access → Personal API keys**: create a
read-only key and paste it when you add the source.

**OAuth.** Create an OAuth application in Linear (**Settings → API → OAuth applications**) with
redirect URI `http://127.0.0.1`, and put its client ID in `client_id`. **Connect** signs in with PKCE
and only the `read` scope; no client secret is needed.

**What is indexed.** Issues assigned to you, created by you or that you subscribe to (`include`), in
all teams or those in `teams` (for example `["ENG"]`), updated in the last 365 days. Each issue
includes its fields, description and comments. Archived and deleted issues are removed.

All settings: [Connector settings → `slack`](../reference/connectors.md#slack).
