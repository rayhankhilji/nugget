# Slack, Notion, GitHub and Linear

Each connector reads the service's official API with **your own token**. No Nugget server and no model
sits between Nugget and the API, so a sync is deterministic, free, and works with any AI setup or none.

| Source | What is indexed | Token | Sync |
|---|---|---|---|
| `slack` | Your DMs and the channels you are in: one document per conversation per day, and one per thread | A user token (`xoxp-…`) from your own Slack app | Every 5 min; the last 3 days re-read every 6 h |
| `notion` | Pages and databases shared with your integration, as Markdown | An internal integration secret (`ntn_…` or `secret_…`) | Every 5 min; a full listing every 6 h |
| `github` | Issues, pull requests and discussions you take part in, with comments and reviews | A fine-grained personal access token, or sign-in with the device flow | Every 10 min; a full listing daily |
| `linear` | Issues assigned to you, created by you or that you subscribe to, with comments | A personal API key (`lin_api_…`), or OAuth sign-in | Every 5 min; a full listing every 6 h |

The periodic full listing finds items that were deleted, archived, unshared or moved out of scope, and
removes them from Nugget.

## How tokens are stored

A pasted token goes to your OS keychain through `sources.secret.set`, under `password:<source id>`. It
never appears in `sources.toml`, logs or IPC responses. Tokens from an interactive sign-in (GitHub
device flow, Linear OAuth) are stored under `oauth:<source id>` instead, and take precedence over a
pasted one.

For headless setups, the `token_env` setting names an environment variable to read the token from
instead (for example `"token_env": "NOTION_TOKEN"`). If the setting is present and the variable is
unset, the source goes to **Needs sign-in** (`needs_auth`).

A rejected token also puts the source in `needs_auth`: paste a new one, or reconnect.

## Slack

Slack has no app that Nugget can use on your behalf, so you create a small one in your workspace.

1. Go to [api.slack.com/apps](https://api.slack.com/apps) → **Create New App → From scratch**. Pick
   the workspace.
2. Under **OAuth & Permissions → User Token Scopes**, add:
   - `channels:history`, `groups:history`, `im:history`, `mpim:history` (read messages in public
     channels, private channels, DMs and group DMs);
   - `channels:read`, `groups:read`, `im:read`, `mpim:read` (list the conversations);
   - `users:read` (show names instead of user ids).

   Leave out the kinds you do not want: without `im:history`, for example, turn off `include_dms`.
3. **Install to Workspace** and approve. Some workspaces need an admin to approve the app.
4. Copy the **User OAuth Token** (`xoxp-…`), not a bot token (`xoxb-…`): a bot sees only the
   conversations it was invited to.
5. On **Sources**, choose **Add source → Slack**, and paste the token.

By default Nugget indexes up to 100 conversations (`max_conversations`) from the last 90 days
(`window_days`): every DM, group DM and channel you are a member of. List `conversations` (ids such as
`C0123ABCD`, or names such as `#general`) to index only those. Archived conversations are skipped.

Requests are sequential, which keeps a pass inside Slack's rate limits for `conversations.history`;
`Retry-After` is honoured.

## Notion

1. Go to [notion.so/profile/integrations](https://www.notion.so/profile/integrations) → **New
   integration**. Type: **Internal**. Pick the workspace.
2. Under **Capabilities**, keep **Read content** and turn off **Update content** and **Insert
   content**: Nugget never writes. **Read user information without email addresses** lets Nugget show
   authors' names; it is optional.
3. Copy the **Internal Integration Secret**.
4. In Notion, open each top-level page or database you want indexed and use **… → Connections → Add
   connections** to share it with the integration. Child pages are included automatically. Nugget sees
   nothing you have not shared.
5. On **Sources**, choose **Add source → Notion**, and paste the secret.

Each page becomes one Markdown document: its title, database properties (for database rows), then its
blocks, down to `max_depth` levels (6 by default). Very large pages are cut at 3,000 blocks or 500,000
characters. Child pages are separate documents. Databases are indexed as a short description of their
columns, and their rows as pages (`include_databases`). The most recently edited 5,000 pages are kept
(`max_pages`).

## GitHub

Either paste a token, or sign in with the device flow.

**Token (simplest).** Create a
[fine-grained personal access token](https://github.com/settings/personal-access-tokens/new):

1. Resource owner: you, or an organisation whose repositories you want (some organisations must
   approve tokens).
2. Repository access: **All repositories**, or the ones you want.
3. Repository permissions, all **Read-only**: **Issues**, **Pull requests**, **Discussions** and
   **Metadata** (Metadata is selected automatically).
4. On **Sources**, choose **Add source → GitHub**, and paste the token.

A classic token with the `repo` scope also works, but grants write access too, so the fine-grained
token is the better choice.

**Device flow.** If you would rather not handle a token, register your own
[OAuth App](https://github.com/settings/applications/new) (any homepage and callback URL; they are not
used), enable **Device Flow** on it, and put its client ID in the source's `client_id` setting. Then:

1. Choose **Connect**. Nugget shows a short **user code** and opens
   [github.com/login/device](https://github.com/login/device).
2. Enter the code if GitHub does not fill it in, and approve.
3. Nugget polls GitHub until you approve, for up to 5 minutes, then stores the token.

On an OAuth App the device flow requests the scopes `repo` and `read:discussion`, because GitHub has no
read-only repository scope for OAuth Apps; Nugget still only reads. A GitHub App with read-only
permissions avoids that, but its user tokens expire after 8 hours and refreshing them needs the app's
client secret, which Nugget has no way to store yet; prefer an OAuth App or a token for now.

> `sources.auth.begin` returns the device code in a separate `user_code` field, and the Sources
> wizard shows it next to the link to GitHub.

**What is indexed.** With `repos` unset: every issue, pull request and discussion that involves you
(author, assignee, commenter or mentioned), updated in the last 365 days (`window_days`). With `repos`
(`owner/name`, up to 100): the same, limited to those repositories, or everything in them with
`all_activity`. Each item is one Markdown document: the body, then comments and reviews in order
(review comments included, up to 300 comments). Discussions can be turned off with
`include_discussions`.

Nugget uses the GraphQL API, which needs far fewer requests than REST. Rate limits are honoured, and a
pass stops early (after saving its progress) when the hourly budget runs low.

## Linear

**API key (simplest).** In Linear, open **Settings → Security & access → Personal API keys**, create a
key (read-only access is enough), and paste it when you add the source.

**OAuth.** Create your own OAuth application in Linear (**Settings → API → OAuth applications**) with
the redirect URI `http://127.0.0.1`, and put its client ID in `client_id`. **Connect** then opens
Linear's consent screen and signs in with PKCE, asking only for the `read` scope; no client secret is
needed. The redirect reaches a one-time listener on `127.0.0.1` (random port, open for at most
5 minutes).

**What is indexed.** Issues assigned to you, created by you, or that you subscribe to (`include`), in
every team or the team keys you list (`teams`, for example `["ENG"]`), updated in the last 365 days.
Each issue is one Markdown document: identifier, title, team, state, priority, project, people and
labels, the description, then comments with replies indented. Archived and deleted issues are removed.

## What every SaaS source has in common

- **Read-only.** Nothing is posted, edited or marked as read.
- **Voice hints.** Each document records whether you wrote part of it (`from_me` in its metadata), so
  features that learn your writing style can find your own messages.
- **Sensitivity** is decided per item at indexing time, as for every source (see
  [Privacy](privacy.md)).
- **Egress.** Every request is in the egress ledger under `sync:slack`, `sync:notion`, `sync:github` or
  `sync:linear`. Redirects are never followed.
- **Resumable.** Rate limits (`429`, `Retry-After`) are retried up to 4 times in a pass; longer waits
  end the pass, and the source resumes from its last checkpoint after a backoff.

All settings: [Connector settings → `slack`](../reference/connectors.md#slack).
