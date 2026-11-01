# IMAP and CalDAV

The `imap` and `caldav` sources work with almost any mail or calendar provider. They sign in with a
username and a password. For most providers that password should be an **app password**: a separate,
revocable password made just for Nugget, which also works when your account has two-factor
authentication.

The password is stored in your OS keychain (account `password:<source id>`). It never appears in
`sources.toml`, logs or IPC responses.

**Access.** An app password grants whatever the provider attaches to it. Where the provider lets you
choose (Fastmail does), give it only IMAP for an `imap` source and only CalDAV for a `caldav` source.
Nugget only reads: IMAP mailboxes are opened read-only, and CalDAV sync uses read requests
(`PROPFIND` and `REPORT`).

## Rules both connectors enforce

- **IMAP is TLS only.** Either implicit TLS (IMAPS, port 993, the default) or `starttls` (port 143),
  which fails if the server does not upgrade. There is no plaintext mode.
- **CalDAV is https only.** Plain `http://` is accepted only for a loopback host (`localhost`,
  `127.0.0.1`), for local servers. The password is never sent over plain http to anything else.
- **Certificates are checked against your OS trust store.** A self-signed certificate is rejected
  unless you add it to the OS trust store yourself.
- **Read-only.** Mailboxes are opened with `EXAMINE`, and bodies are fetched with `BODY.PEEK`, so
  nothing is marked as read.
- **No credentials in URLs.** A CalDAV URL that contains a username or password is refused.

## Add a mailbox

1. On **Sources**, choose **Add source → IMAP mail**.
2. Enter the server, username and (optionally) folders. By default Nugget indexes `INBOX` and your Sent
   folder. `*` means every mailbox except Trash, Spam, Junk and virtual views such as Gmail's All Mail.
3. Enter the app password when asked.

Mail from the last 365 days is indexed (`window_days`, `0` for all). Messages over 25 MB
(`max_message_mb`) are indexed by their headers only. New mail arrives through IMAP IDLE on the inbox.

## Add a calendar

1. On **Sources**, choose **Add source → CalDAV calendar**.
2. Enter the server URL and username. The URL can be the server root, your principal, your calendar
   home, or one calendar: Nugget discovers the rest.
3. Enter the app password when asked.

Every calendar that holds events or tasks is synced, unless you list some in `calendars` (by display
name, href, or the last part of the path).

## Provider settings

| Provider | IMAP server | CalDAV URL | Username | Password |
|---|---|---|---|---|
| **Fastmail** | `imap.fastmail.com`, port 993 | `https://caldav.fastmail.com/` | Your full Fastmail address | An app password: **Settings → Privacy & Security → Manage app passwords → New app password**. Give it IMAP and CalDAV access (or "Mail, Contacts & Calendars"). |
| **iCloud** | `imap.mail.me.com`, port 993 | `https://caldav.icloud.com/` | Your Apple Account email address | An app-specific password: [account.apple.com](https://account.apple.com) → **Sign-In and Security → App-Specific Passwords**. Two-factor authentication must be on. |
| **Proton Mail** (through Proton Mail Bridge) | `127.0.0.1`, port `1143`, `tls = "starttls"` | Not available: Proton Calendar has no CalDAV | The username Bridge shows | The password **Bridge** shows for the account (not your Proton password) |

### Proton Mail Bridge: the certificate

Bridge serves IMAP on your own machine with a **self-signed certificate**. Nugget checks certificates
against the OS trust store and has no "accept any certificate" switch, so a fresh Bridge setup is
rejected with a TLS error. To fix it:

1. In Bridge, open **Settings → Advanced settings → Export TLS certificates** and save the certificate.
2. Add it to your OS trust store:
   - **macOS**: double-click it in Keychain Access, add it to the **login** keychain, and set it to
     **Always Trust** for SSL.
   - **Windows**: open it and **Install Certificate** into **Current User → Trusted Root Certification
     Authorities**.
   - **Linux (Debian, Ubuntu)**: copy it to `/usr/local/share/ca-certificates/proton-bridge.crt` and run
     `sudo update-ca-certificates`.
3. Choose **Sync now** on the source.

This has not been tested against a real Bridge install yet; the steps follow from how certificate
verification works. If it fails for you, please open an issue with the error from the source's status.

### Other providers

Any IMAP server over TLS and any CalDAV server over https works, including Nextcloud, Radicale, Baïkal
and self-hosted Dovecot. For Gmail and Google Calendar, prefer the [Google sources](sources-google.md):
they sign in with OAuth instead of a password, and they sync Gmail's labels and deletions natively.

All settings: [Connector settings → `imap`, `caldav`](../reference/connectors.md#imap).

## Checking IMAP with a real account

The IMAP connector is tested against a mock IMAP server; nobody has run this checklist against a live
account yet. It takes about 10 minutes with **window_days** set to 30. Use a provider from the table
above. If a step fails, see [Reporting a failed check](sources.md#reporting-a-failed-check).

1. **Connect.** Create an app password (IMAP access only, where the provider allows), add an IMAP
   source and enter it when asked. Expected: there is no consent screen; the source switches to
   `syncing`. A wrong password instead gives **Needs sign-in** with **Set password**.
2. **First sync.** Wait for `idle` or `watching`. Expected: the item count is close to the number of
   messages from the last 30 days in INBOX and Sent together. Messages that were unread stay unread in
   your other mail apps.
3. **New mail and changes.** Send yourself a message. Expected: it appears within seconds without
   **Sync now** (IMAP IDLE on the inbox). Flag an older message and choose **Sync now**: it is re-read.
4. **Move.** Move a message from INBOX to a folder Nugget does not sync, and choose **Sync now**.
   Expected: it disappears. Add that folder to `folders` (keep INBOX): it appears there, under the new
   folder. A move is a delete in one folder plus a new item in the other.
5. **Delete.** Delete a message so it is expunged (or moved to Trash, which is not synced) and choose
   **Sync now**. Expected: it disappears from search.
6. **Unicode.** Send yourself a message with the subject `Grüße 🍰 日本語 test`, and add a folder with a
   non-Latin name (such as `Rechnungen ü`) to `folders`. Expected: the subject shows exactly and is
   searchable, and the folder syncs.
7. **Large message.** Set **max_message_mb** to 1 and send yourself a message with a 3 MB attachment.
   Expected: the message is indexed by its headers only: its subject and sender are searchable, its
   body is not, and its details say `truncated`. Under the limit, the body is indexed and attachments
   are listed by name only.
8. **Revoke.** Delete the app password at your provider, then choose **Sync now**. Expected: **Needs
   sign-in** (health `reconnect`), with the reason that the source no longer accepts Nugget's
   sign-in and the fix "Enter a new app password". **Set password** with a new app password recovers
   it.
9. **Egress.** Expected: the **Privacy** page shows the traffic under `sync:imap`, to your IMAP server
   only.

## Checking CalDAV with a real account

Nobody has run this checklist against a live account yet. It takes about 10 minutes; a test calendar
helps. If a step fails, see [Reporting a failed check](sources.md#reporting-a-failed-check).

1. **Connect.** Create an app password (CalDAV access only, where the provider allows), add a CalDAV
   source with the URL from the table above, and enter the password. Expected: no consent screen;
   Nugget discovers your calendars and starts `syncing`.
2. **First sync.** Wait for `idle`. Expected: one item per event or task stored on the server, with
   no date window. A repeating event is **one** item, its changed occurrences included.
3. **Edit.** Change an event's title and choose **Sync now**. Expected: the item shows the new title.
4. **Move.** Move an event to another calendar on the same account and choose **Sync now**. Expected:
   it disappears from the old calendar and appears under the new one.
5. **Delete.** Delete an event and choose **Sync now**. Expected: it disappears from search.
6. **Unicode.** Create an event titled `Café ☕ 会議`. Expected: the title shows exactly that and is
   searchable.
7. **Large event.** Paste a very long description. Expected: the event is indexed; each property is
   cut at 64 KiB, and attachments are not indexed. A server response over 32 MiB stops the pass with
   health `too_large`.
8. **Recurring event.** Create a weekly event and move one occurrence. Expected: one item whose text
   includes a `Repeats:` line with the rule (for example `FREQ=WEEKLY`), and a section for the moved
   occurrence.
9. **Another time zone.** Create an event in a different time zone from yours. Expected: the time is
   shown as written in the event, followed by its zone, for example
   `2026-11-03 09:00 (America/New_York)`. If the server stored the time in UTC, it shows `(UTC)`
   instead, which is still the right moment.
10. **Revoke.** Delete the app password and choose **Sync now**. Expected: **Needs sign-in** (health
    `reconnect`) with the fix "Enter a new app password"; **Set password** recovers it.
11. **Egress.** Expected: traffic under `sync:caldav`, to your CalDAV server only.
