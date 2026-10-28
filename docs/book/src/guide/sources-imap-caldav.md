# IMAP and CalDAV

The `imap` and `caldav` sources work with almost any mail or calendar provider. They sign in with a
username and a password. For most providers that password should be an **app password**: a separate,
revocable password made just for Nugget, which also works when your account has two-factor
authentication.

The password is stored in your OS keychain (account `password:<source id>`). It never appears in
`sources.toml`, logs or IPC responses.

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
