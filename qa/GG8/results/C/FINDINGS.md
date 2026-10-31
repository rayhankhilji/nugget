# GG8 area C: connectors, OAuth and plugins, observed from outside

Binary: `target/debug/nuggetd` built from the integration branch at 3aeda23 (merged into `qa/privacy`
as d2751ae). Lab slot 2 (namespace `gg8-2`), sinkhole mode: every name resolves to the sink at
203.0.113.138 (classified `cloud`). The sink terminates TLS with the GG8 test CA and records every
request. Each scenario ran 3 times on 2026-10-30. Evidence from run1 is in `<scenario>/`.

Mocks added: `harness/mock_saas.py` (Slack at slack.com, Notion at api.notion.com),
`harness/mock_caldav.py` (caldav.gg8-synthetic.test; a variant whose calendar-home-set points at
collector.gg8-evil.test), and `harness/mock_oauth.py` (Google, Microsoft, Linear and GitHub token
endpoints plus stub Gmail; `GG8_OAUTH_EXPIRES` sets the token lifetime).

## Capability table

| Capability | Verdict | Checks (3 runs) | Scenario |
|---|---|---|---|
| Slack and Notion sync (token_env): one `sync:slack` / `sync:notion` ledger row per request, right host | Works | 27/27 | s40 |
| Connector tokens only to their own service; none in logs or plaintext files | Works | 3/3 | s40 |
| Sensitive connector content (card number in a Slack DM and a Notion page) withheld from a cloud Ask | Works | 3/3 (`excluded_sensitive=2`) | s40 |
| `client_secret` refused in settings | Works | 3/3 | s40 |
| CalDAV over https: `sync:caldav` rows, Basic credentials only to the CalDAV host, https only | Works | 15/15 | s41 |
| CalDAV over plain http to a LAN address | Refused at `sources.add` (as documented) | 3/3 | s41 |
| CalDAV calendar-home-set on a foreign host | No credentials sent: 0 requests to the foreign host, source in `error` | 6/6 | s42, s43 |
| IMAP to a hostile server: TLS ClientHello first on 993; nothing sent before the greeting on 143; no plaintext credentials | Works | 9/9 | s41 |
| IMAP failed attempts in the ledger | Works with caveats: 993 recorded, STARTTLS (143) not (GG8-C1) | 0 of 12 STARTTLS connections recorded | s41 |
| OAuth begin (gmail, outlook, linear): nothing contacted before the user acts; no secret in the URL | Works | 18/18 | s42, s43 |
| GitHub device flow: contacts github.com only (documented); polling ledgered `sync:github` | Works | 6/6 | s42, s43 |
| Google code exchange and refresh: only to oauth2.googleapis.com, ledgered `sync:gmail`; tokens, code and client secret confined to Google hosts | Works | 18/18 | s42, s43 |
| Plugin network (`sync:plugin:<id>`, `tool:plugin:<id>`) | **Not tested**: no built `.nuggetplugin` exists, and the guests need a `wasm32-wasip2` cargo build (target not installed; out of bounds for this area) | – | – |

147/150 checks passed. All 3 failures are one non-privacy check (`caldav.needs_auth_before_secret`, see
the note below). Across 12 runs there were no unledgered hosts. No packets left the namespace except
to the sink (DNS, 443, 993, 143).

## Candidate findings

### GG8-C1 (Low): IMAP STARTTLS connection attempts are not in the egress ledger
- **Repro:** run `GG8_LAB=2 python3 qa/GG8/harness/lab.py run qa/GG8/scenarios/s41_caldav_imap.py --out /tmp/gg8-C`.
  It adds `imap` sources at `imap.gg8-synthetic.test` with `tls=implicit` (port 993) and `tls=starttls`
  (port 143). Only a raw TCP logger listens: no greeting, no TLS. Then it sets a password and runs
  `sources.sync`.
- **Expected:** every connection to an off-device host is recorded. The privacy guide says "Every
  request Nugget's own network layer sends to a destination that is not on this device is recorded".
- **Actual (run1):** there were 5 connections on 993 and 4 on 143. The ledger has only
  `{"feature":"sync:imap","host":"imap.gg8-synthetic.test:993","bytes_sent":260,"bytes_received":0}`
  ×5. The STARTTLS attempts did leave the device: the DNS query plus a TCP connect, visible in
  `flows.txt` as `.143`. No content leaked, because the first bytes on 143 were empty.
- **Frequency:** 3/3 runs; 12/12 STARTTLS connections were unrecorded.
- **Owner:** `crates/nugget-connector-imap`, the metering on the STARTTLS path (it probably writes a
  row only when bytes are sent). FF2 connector hardening.
- **Fix direction:** open the row at connect time, or record zero-byte exchanges, in both TLS modes.

### GG8-C2 (Low): the IMAP ledger host carries the port
- **Repro:** as for C1, then `egress.recent`.
- **Expected:** `imap.gg8-synthetic.test`, a bare host as on every other row (`slack.com`,
  `caldav.gg8-synthetic.test`) and in the privacy guide's example.
- **Actual:** `"host": "imap.gg8-synthetic.test:993"`. Per-host grouping on the Privacy page then
  splits one server into several entries.
- **Frequency:** 3/3.
- **Owner:** `crates/nugget-connector-imap`, FF2.
- **Fix direction:** record the bare host, and put the port in a separate field if it is needed.

### Note (not privacy): a CalDAV source with no password is not `needs_auth`
Right after `sources.add` and before `sources.secret.set`, the state is `idle` or `syncing` (4/4)
rather than `needs_auth`. Nothing is sent before the secret is set (0 requests). This is a display
issue only. Owner: `crates/nuggetd/src/sources.rs` / `nugget-connector-caldav`.

## Observations
- **Refresh with 60 s Google tokens (s42):** a `refresh_token` grant came before every Gmail call:
  15 refreshes for 11 calls, sometimes 3–4 within 0.3 s. Each one was ledgered and went to Google
  only. With realistic 1 h tokens (s43) there was 1 exchange and 0 refreshes, 3/3. This matters only
  for providers that issue very short-lived tokens.
- **GitHub device-flow polling:** while the user has not approved, `github.com/login/oauth/access_token`
  is polled every 5 s, about 40 ledger rows in 200 s. Whether polling stops at `expires_in` was not
  observed.

## What works well
- Credentials stay with their own service. Tokens, the OAuth code, the client secret and refresh
  tokens appeared only in requests to their own service, never in logs, config, `sources.toml` or the
  vault (12/12 runs).
- The CalDAV client refuses to send credentials to a foreign host named by the server's own discovery
  response, and refuses plain http to the LAN.
- `sources.auth.begin` (gmail, outlook, linear) sends nothing, and its URLs carry no secret.
- The ledger is complete for the HTTP connectors, including token exchanges, refreshes and device-flow
  polling.
- Sensitive connector content (Slack DM, Notion page, calendar event) is withheld from a cloud Ask.
- IMAP sends nothing before TLS.

## Harness changes wanted
Count raw-TCP connections per host:port in `summary.json` next to the ledger, so a gap like C1 is
flagged automatically.
