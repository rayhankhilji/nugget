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

**Scopes.** Each token is set up to read only:

- **Slack**: nine user-token scopes, `channels:`, `groups:`, `im:` and `mpim:` `history` and `read`,
  plus `users:read`. Those are exactly the API methods the connector calls; none of them can post or
  change anything.
- **Notion**: an internal integration with **Read content** only. It sees only the pages you share
  with it.
- **GitHub**: a fine-grained token with read-only Issues, Pull requests, Discussions and Metadata, on
  the repositories you pick. The device flow asks for `repo` and `read:discussion` instead, which is
  broader (see [GitHub](#github)).
- **Linear**: a personal API key with read access, or OAuth with the `read` scope.

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

## Checking SaaS sources with a real account

These connectors are tested against mock servers; nobody has run these checklists against a live
account yet. Each takes about 10 minutes. Use a test channel, page, repository or team you can change
freely. If a step fails, see [Reporting a failed check](sources.md#reporting-a-failed-check).

### Checking Slack with a real account

Set **window_days** to 7 and list one or two test conversations in `conversations` for a short first
sync.

1. **Connect.** Create the Slack app with the nine user scopes above, install it, and paste the
   `xoxp-` token. Expected: Slack's install screen lists only permissions to view messages and basic
   information about conversations and people, nothing that posts or changes anything. The source
   switches to `syncing`.
2. **First sync.** Expected: one item per conversation per day with top-level messages, plus one per
   thread. Names appear instead of user ids.
3. **New and edited messages.** Post a message, then edit a message you posted today, and post
   another. Choose **Sync now**. Expected: today's item shows both new messages and the edit. An edit
   on its own, with no newer message in that conversation, shows up at the next re-check of the last
   3 days (every 6 hours).
4. **Thread reply.** Reply in a thread and choose **Sync now**. Expected: the thread's item includes
   the reply.
5. **Rename.** Rename a test channel. Expected: items rebuilt after the rename (new messages, or the
   6-hourly re-check) carry the new name; older days keep the old one.
6. **Delete.** Delete a message posted today and post another. Choose **Sync now**. Expected: today's
   item no longer contains the deleted message. A deletion in an older day shows up at the next
   re-check of the last 3 days.
7. **Unicode.** Post `Grüße 🍰 日本語 test`. Expected: the text shows as written and is searchable.
8. **Large file.** Upload a 50 MB file to the channel. Expected: the message shows `[file: <name>]`;
   the file's content is not indexed.
9. **Revoke.** Uninstall the app from the workspace and choose **Sync now**. Expected: **Needs
   sign-in** (health `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused." and the fix to paste a new token; **Set token** with a new token recovers it.
10. **Egress.** Expected: the **Privacy** page shows traffic under `sync:slack`, to `slack.com` only.

### Checking Notion with a real account

Share one test page (with a child page and a small database) with the integration.

1. **Connect.** Create the integration with **Read content** only and paste the secret. Expected: the
   integration's **Capabilities** show no update or insert rights. The source syncs only what you
   shared.
2. **First sync.** Expected: one item per shared page, per child page and per database row, plus one
   per database. Pages you did not share are absent.
3. **Edit.** Change a paragraph and choose **Sync now**. Expected: the new text is searchable. Notion
   rounds edit times to the minute, so an edit a few seconds after the previous pass can need a second
   **Sync now**.
4. **Rename and move.** Rename the child page and move it under another shared page. Expected: the
   item shows the new title.
5. **Delete.** Move a page to Trash and choose **Sync now**. Expected: it disappears, at the latest at
   the next full listing (every 6 hours). Removing the integration's connection from a page removes it
   the same way.
6. **Unicode.** Title a page `Notizen über Kuchen 🍰 日本語`. Expected: the title shows exactly that and
   is searchable.
7. **Large page and files.** Add a 20 MB PDF and an image to a page, and paste a very long text.
   Expected: files appear by caption or name only (Notion's file links expire, so they are not kept);
   a page past 3,000 blocks ends with "… (some nested content was not included)", and its text is cut
   at 500,000 characters.
8. **Revoke.** Delete the integration at notion.so/profile/integrations and choose **Sync now**.
   Expected: **Needs sign-in** (health `reconnect`); **Set token** with a new secret recovers it.
9. **Egress.** Expected: traffic under `sync:notion`, to `api.notion.com` only.

### Checking GitHub with a real account

Use a fine-grained token, and set `repos` to one test repository for a short first sync.

1. **Connect.** Create the token with read-only Issues, Pull requests, Discussions and Metadata, and
   paste it. Expected: GitHub's token page lists only those four permissions, all read-only. With the
   device flow instead, GitHub's approval page asks for `repo` and `read:discussion`: that is GitHub's
   limit for OAuth Apps, not a sign that Nugget writes.
2. **First sync.** Expected: one item per issue, pull request and discussion in the repository that
   involves you (or all of them with `all_activity`), updated in the window.
3. **Edit.** Comment on an issue and edit its body. Choose **Sync now**. Expected: the item shows both.
   GitHub's search can lag by a minute or so.
4. **Rename and move.** Change an issue's title, and transfer another to a second repository you can
   read. Expected: the title updates on the next pass. The transferred issue disappears from the old
   repository at the next full listing (daily), and appears under the new one if that repository is
   in scope.
5. **Delete.** Delete an issue (repository admins can). Expected: it disappears at the next full
   listing, at most a day later. Closing an issue does not remove it.
6. **Unicode.** Title an issue `Grüße 🍰 日本語 test`. Expected: the title shows exactly that and is
   searchable.
7. **Large issue.** Attach a large file to a comment, and paste a very long body. Expected: the file
   appears as a link only; bodies are cut at 60,000 characters and threads at 300 comments.
8. **Revoke.** Delete the token (or revoke the OAuth App under **Settings → Applications**) and choose
   **Sync now**. Expected: **Needs sign-in** (health `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused."; a new token, or **Sign in** with the device
   flow, recovers it.
9. **Egress.** Expected: traffic under `sync:github`, to `api.github.com` (and `github.com` for the
   device flow) only.

### Checking Linear with a real account

Set `teams` to one test team for a short first sync.

1. **Connect.** Paste a personal API key with read access, or connect with OAuth. Expected: Linear's
   OAuth consent screen asks for read access only (the `read` scope).
2. **First sync.** Expected: one item per issue assigned to you, created by you or that you
   subscribe to, in that team, updated in the last 365 days.
3. **Edit.** Change an issue's description and add a comment. Choose **Sync now**. Expected: the item
   shows both.
4. **Rename and move.** Rename an issue and move it to another team you sync. Expected: the same item
   updates, with the new title and the new identifier. Moved to a team you do not sync, it disappears
   at the next full listing (every 6 hours).
5. **Delete.** Delete an issue (it goes to Linear's trash), and archive another. Choose **Sync now**.
   Expected: both disappear from search.
6. **Unicode.** Title an issue `Café ☕ 会議`. Expected: the title shows exactly that and is
   searchable.
7. **Large issue.** Attach a large file and paste a very long description. Expected: attachments are
   not indexed; the description is cut at 60,000 characters and comments at 300.
8. **Revoke.** Revoke the API key (or the OAuth app's access) in Linear's settings and choose **Sync
   now**. Expected: **Needs sign-in** (health `reconnect`) with the reason "<source name> no longer accepts Nugget's sign-in, so syncing is paused."; a new key or **Sign in** recovers it.
9. **Egress.** Expected: traffic under `sync:linear`, to `api.linear.app` only.
