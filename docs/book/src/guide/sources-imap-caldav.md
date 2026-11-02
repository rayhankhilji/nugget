# IMAP and CalDAV

The `imap` and `caldav` sources work with almost any mail or calendar provider. They sign in with a
username and a password, which for most providers should be an **app password**: a separate,
revocable password for Heed that also works with two-factor authentication. Where the provider lets
you choose, give it only IMAP or only CalDAV access. The password is kept in your OS keychain.

Both only read (nothing is marked as read). IMAP requires TLS: implicit (port 993, the default) or
`starttls` (port 143). CalDAV requires https, except for `localhost`. Certificates are checked against
your OS trust store, so a self-signed one must be added there first.

## Add a mailbox

1. On **Sources**, choose **Add source → IMAP mail**.
2. Enter the server, username and optionally folders. The default is `INBOX` and your Sent folder;
   `*` means every mailbox except Trash, Spam, Junk and virtual views such as Gmail's All Mail.
3. Enter the app password when asked.

Mail from the last 365 days is indexed (`window_days`, `0` for all). Messages over 25 MB
(`max_message_mb`) are indexed by headers only; attachments are listed by name only. New mail arrives
through IMAP IDLE on the inbox.

## Add a calendar

1. On **Sources**, choose **Add source → CalDAV calendar**.
2. Enter the server URL (the root, your principal, your calendar home or one calendar: Heed
   discovers the rest) and username.
3. Enter the app password when asked.

Every calendar with events or tasks is synced unless you list some in `calendars`. A repeating event
is one item, including its changed occurrences.

## Provider settings

| Provider | IMAP server | CalDAV URL | Password |
|---|---|---|---|
| **Fastmail** | `imap.fastmail.com`, port 993 | `https://caldav.fastmail.com/` | **Settings → Privacy & Security → Manage app passwords**, with IMAP and CalDAV access. Username: your full address |
| **iCloud** | `imap.mail.me.com`, port 993 | `https://caldav.icloud.com/` | [account.apple.com](https://account.apple.com) → **Sign-In and Security → App-Specific Passwords** (needs two-factor authentication). Username: your Apple Account email |
| **Proton Mail** (via Bridge) | `127.0.0.1`, port `1143`, `tls = "starttls"` | None (Proton Calendar has no CalDAV) | The username and password **Bridge** shows |

Other IMAP and CalDAV servers (Nextcloud, Radicale, Baïkal, Dovecot) work too. For Gmail and Google
Calendar, prefer the [Google sources](sources-google.md), which use OAuth and sync labels natively.

### Proton Mail Bridge: the certificate

Bridge uses a **self-signed certificate**, which Heed rejects until you trust it:

1. In Bridge, **Settings → Advanced settings → Export TLS certificates**.
2. Add it to your OS trust store:
   - **macOS**: add it to the **login** keychain in Keychain Access and set **Always Trust** for SSL.
   - **Windows**: **Install Certificate** into **Current User → Trusted Root Certification
     Authorities**.
   - **Debian, Ubuntu**: copy it to `/usr/local/share/ca-certificates/proton-bridge.crt` and run
     `sudo update-ca-certificates`.
3. Choose **Sync now** on the source.

These steps haven't been tested against a real Bridge install yet; if they fail, please open an
issue with the error.

All settings: [Connector settings → `imap`, `caldav`](../reference/connectors.md#imap).
