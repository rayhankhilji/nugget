<!-- Generated from docs/book/src/guide/self-host.md by scripts/sync-docs.mjs. Edit the book page, not this copy. -->

# Self-hosting Heed Cloud

> **Community-supported. No warranty or support from Heed.**
> Self-hosting is documented so you can run the cloud server yourself, but Heed does not support
> it. Questions go to the community, not to Heed support. You are responsible for your server's
> security, updates and backups.

Heed is open core. The desktop app is free and open source and works on its own, with local data.
**Heed Cloud** adds a server that indexes your mail and calendar, answers questions over them, and
connects apps through Composio. This page shows how to run that server on your own machine.

Two things to know first:
- **Your data still reaches third parties.** The server reads mail and calendars through
  [Composio](https://composio.dev) and sends questions and the matching text to the AI providers
  whose keys you set (Anthropic, and OpenAI if you add it). Self-hosting moves the index and the
  database onto your machine. It does not make Heed fully local.
- **Sign-in needs GoTrue**, Supabase's open-source auth server. The server and the desktop app
  speak its API (magic links, Google, PKCE), and there is no other auth backend. You do not need a
  Supabase account: the stack below runs GoTrue itself. A hosted Supabase project also works, as
  shown in [Using a Supabase project instead](#using-a-supabase-project-instead).

## What runs

`selfhost/docker-compose.yml` starts these services:

| Service | Image | Does |
|---|---|---|
| `db` | `pgvector/pgvector:pg16` | Postgres 16 with pgvector: your index, accounts and settings |
| `auth` | `supabase/gotrue` | Sign-in (magic link, Google), in the `auth` schema of the same database |
| `migrate` | `pgvector/pgvector:pg16` | Runs once on every start: applies new migrations, then exits |
| `cloud` | built from `crates/nugget-cloud/Dockerfile` | The Heed API and the sync loop |
| `gateway` | `caddy` | One address for the app: `/auth/v1/*` goes to `auth`, the rest to `cloud`. TLS for a domain |
| `mail` | `axllent/mailpit` | Catches magic-link emails for testing (profile `dev`). Use real SMTP otherwise |

## Quick start (on one computer, for testing)

You need Docker with Compose v2, `openssl`, and a checkout of the repository.

```sh
cd selfhost
cp .env.example .env
./gen-keys.sh          # prints passwords, the JWT secret and two keys
```

1. Paste the six printed lines into `.env`, replacing the empty ones.
2. Add your Composio and model keys (see [Composio](#composio) and [Models](#models)).

Then build and start:

```sh
docker compose --profile dev up -d --build
docker compose ps        # migrate: exited (0); the others: running
curl http://localhost:8000/healthz            # ok
curl http://localhost:8000/auth/v1/health     # GoTrue's version
```

Building the `cloud` image compiles the Rust workspace and takes a while. If you already build Heed
locally, package your own binary instead:

```sh
cargo build --release -p nugget-cloud
docker build -f selfhost/Dockerfile.prebuilt -t heed-cloud:local .    # from the repository root
docker compose --profile dev up -d --no-build
```

Magic-link emails land in Mailpit at <http://localhost:8025>. Next,
[point the desktop app at the server](#pointing-the-desktop-app-at-your-server).

## Running it for real

For a server other people (or your phone) can reach:

1. **Point a domain at the machine.** In `.env`, set `SITE_ADDRESS=heed.example.com` and
   `PUBLIC_URL=https://heed.example.com`.
2. **Start with the domain override.** It publishes ports 80 and 443, and Caddy gets and renews the
   TLS certificate itself:
   ```sh
   docker compose -f docker-compose.yml -f docker-compose.domain.yml up -d
   ```
3. **Use real SMTP** for magic links: set `SMTP_HOST`, `SMTP_PORT` (465 or 587), `SMTP_USER`,
   `SMTP_PASS` and `SMTP_SENDER`. Do not start the `dev` profile. GoTrue refuses to send a
   password over an unencrypted connection.
4. **Close sign-up** once your own account exists: `DISABLE_SIGNUP=true`. Otherwise anyone who
   finds the address can make an account and spend your Composio and AI credit.
5. **Google sign-in** (optional): create an OAuth web client in Google Cloud with the redirect URI
   `https://heed.example.com/auth/v1/callback`. Then set `GOOGLE_ENABLED=true`,
   `GOOGLE_CLIENT_ID` and `GOOGLE_SECRET`.

Never publish the database port. Only the gateway faces the internet.

## Settings (`.env`)

`selfhost/.env.example` lists every setting, with comments. The ones you must set:

| Setting | What |
|---|---|
| `PUBLIC_URL` | The address the app and browsers use: `http://localhost:8000` locally, `https://your.domain` for real |
| `POSTGRES_PASSWORD`, `AUTH_DB_PASSWORD`, `SERVICE_DB_PASSWORD` | Database passwords for the admin, GoTrue and the Heed service (from `gen-keys.sh`) |
| `JWT_SECRET` | Signs sign-in tokens (from `gen-keys.sh`) |
| `SUPABASE_ANON_KEY` | The public key the desktop app sends (from `gen-keys.sh`). Not a secret |
| `SUPABASE_SERVICE_ROLE_KEY` | Lets the server delete accounts in GoTrue (from `gen-keys.sh`). Secret |
| `COMPOSIO_API_KEY`, `COMPOSIO_AUTH_CONFIGS` | See [Composio](#composio) |
| `ANTHROPIC_API_KEY` | See [Models](#models) |
| `NUGGET_BILLING` | `off` for self-hosting (the example's value); see [Billing](#billing) |

Optional settings:
- `OPENAI_API_KEY`
- `NUGGET_MODEL_FAST` and `NUGGET_MODEL_SMART`
- `NUGGET_SELFHOST_PLAN`
- `NUGGET_PLANS_FILE`
- `NUGGET_SITE_ORIGINS`
- `NUGGET_LOG_FORMAT`
- `NUGGET_APP_SCHEME`
- `NUGGET_CLIENT_IP_HEADER`: leave it unset unless a trusted proxy in front of Caddy sets it.
- `NUGGET_WAITLIST_SALT` and `NUGGET_RATE_WAITLIST_PER_HOUR`: for the website's waitlist route. A
  self-hosted server can ignore both: nothing reads the list.
- `COMPOSIO_WEBHOOK_SECRET`

`cloud` reads `.env` directly. `DATABASE_URL` and `SUPABASE_URL` are set by the compose file.

## Composio

Heed reaches Gmail, Google Calendar and other apps through Composio, using **your own** Composio
account:

1. Create a Composio project and copy its API key into `COMPOSIO_API_KEY`.
2. In the Composio dashboard, create an **auth config** for each toolkit you want: Gmail and Google
   Calendar today. Composio-managed OAuth is the simplest choice. List them as
   `COMPOSIO_AUTH_CONFIGS=gmail=ac_…,googlecalendar=ac_…`. A toolkit missing from this list shows as
   "coming soon" in the app.
3. Optional: set the project's webhook URL to `https://your.domain/v1/webhooks/composio`, and put
   its signing secret in `COMPOSIO_WEBHOOK_SECRET`. Without webhooks, Heed simply polls on its sync
   schedule.

Composio bills you directly. Its free tier covers a handful of users at Heed's sync cadence.

## Models

- `ANTHROPIC_API_KEY` is used for answers. Without any model key, Ask answers `503 llm_not_configured`.
- `OPENAI_API_KEY` is optional. It adds embeddings (`text-embedding-3-small`) for semantic search,
  and is the chat fallback when no Anthropic key is set. Without it, search is full-text only, which
  still finds most things.
- `NUGGET_MODEL_FAST` and `NUGGET_MODEL_SMART` override the model ids.

You pay the providers directly. The usage ledger (`usage_events`) records the cost of every call,
per user.

## Billing

With **`NUGGET_BILLING=off`**, Stripe is never contacted:
- Every signed-in user gets the limits of `NUGGET_SELFHOST_PLAN` (default `heed_max`).
- The checkout, portal and Stripe-webhook routes answer `404 billing_disabled`.
- The desktop app hides its plans and billing buttons.

Any other value than `on` or `off` stops the server at start-up, so a typo cannot turn billing on
by accident. The default is `on`, which is Heed's own service and needs Stripe.

The limits come from `crates/nugget-cloud/plans.toml`:
- monthly AI budget
- connected apps
- sync interval
- history
- devices

To change them, copy the file, edit it, mount it into `cloud`, and set `NUGGET_PLANS_FILE` to its
path. The file is checked at start-up.

## The database: `nugget_service` and row-level security

The migrations in `supabase/migrations/` create the schema. `migrate` applies them in name order and
records each one in `selfhost.schema_migrations`.

Every table has a `user_id` column and **row-level security (RLS)**:
- **The service's role.** The server connects as `nugget_service`, a role that does **not**
  bypass RLS. Each request runs in a transaction that sets the signed-in user, so even a buggy
  query cannot read another user's rows.
- **Cross-user lookups.** The few lookups that must span users go through `security definer`
  functions in the `nugget_private` schema, which only `nugget_service` may call. Examples are
  "which user owns this Composio account?" and "which connections are due?".
- **Passwords.** `migrate` sets the role's password from `SERVICE_DB_PASSWORD` on every run.
  To change the password, change the setting and restart.

Do not point `DATABASE_URL` at the `postgres` superuser. It would bypass RLS.

## Pointing the desktop app at your server

The desktop app reads three settings when it starts, and falls back to values built into it.

| Variable | Value |
|---|---|
| `NUGGET_CLOUD_URL` | Your `PUBLIC_URL` |
| `NUGGET_SUPABASE_URL` | Your `PUBLIC_URL` too: the gateway serves Auth under `/auth/v1` |
| `NUGGET_SUPABASE_ANON_KEY` | `SUPABASE_ANON_KEY` from `.env` |

The app accepts only `https://` addresses, except `http://localhost` for testing. There is no
in-app setting yet, so set the variables where the app is launched:

- **macOS:** `open -a Heed --env NUGGET_CLOUD_URL=https://heed.example.com --env NUGGET_SUPABASE_URL=https://heed.example.com --env NUGGET_SUPABASE_ANON_KEY=…`
- **Windows:** set them as user environment variables (`setx NUGGET_CLOUD_URL https://heed.example.com`, and so on), then start Heed.
- **Linux:** export them in the environment that starts Heed, or in its `.desktop` file's `Exec=env NUGGET_CLOUD_URL=… heed` line.
- **Builds from source:** set the same three variables when building the app, and they become its defaults.

Then open **Cloud** in the app, sign in with a magic link (or Google), and connect your apps.
The sign-in link returns to the app through `heed://auth/callback`. `APP_REDIRECTS` must list it,
and the example already does.

## Checking it works

The repository has a smoke test that signs in and uses the server exactly as the desktop app does.
It uses the app's own client library, with fake Composio and model providers, so no keys are spent.
It needs the `dev` profile, for Mailpit.

1. Start the fake providers on the Docker host:
   ```sh
   FAKE_PROVIDERS_BIND=0.0.0.0:9100 cargo run -p nugget-cloud --example fake_providers
   ```
2. In `.env`, uncomment the three "testing only" base-URL lines. Set the Composio, Anthropic and
   OpenAI keys to any value, and `COMPOSIO_AUTH_CONFIGS=gmail=ac_gmail,googlecalendar=ac_cal`.
   Restart `cloud`:
   ```sh
   docker compose up -d cloud
   ```
3. Run the smoke test:
   ```sh
   HEED_URL=http://localhost:8000 HEED_ANON_KEY=<SUPABASE_ANON_KEY> \
     cargo run -p nugget-cloud --example selfhost_smoke
   ```

It goes through these steps:
1. Requests a magic link (PKCE) and reads it from Mailpit.
2. Opens the link and exchanges the returned code.
3. Checks the account is entitled with billing off.
4. Connects Gmail and Calendar, then syncs.
5. Asks a question and checks the cited answer.
6. Exports the data, then deletes the account.

It ends with `selfhost smoke: OK`. Undo step 2 before real use.

## Backups

Everything is in the `db` volume: Heed's data, and GoTrue's users and sessions (the `auth`
schema). Back it up with `pg_dump`. The command runs inside the container, so you need no
database port:

```sh
docker compose exec -T db pg_dump -U postgres -Fc postgres > heed-$(date +%F).dump
```

Keep copies off the machine, and encrypted: the dump holds everyone's indexed mail. To restore
onto a fresh stack:

```sh
docker compose stop        # everything: running services cache database type ids
docker compose up -d db
docker compose exec -T db pg_restore -U postgres -d postgres --clean --if-exists < heed-2026-11-02.dump
docker compose up -d
```

Also keep `.env`. Its `JWT_SECRET` must match the restored database, or everyone has to sign in
again. The `caddy-data` volume only holds certificates, and Caddy fetches new ones by itself.
Composio holds the Google tokens, so you don't back those up, but a restored server must keep the
same `COMPOSIO_API_KEY`.

## Upgrades

1. Read the release notes for anything marked **self-hosting**.
2. Back up (see above).
3. Update your checkout:
   ```sh
   git pull
   ```
4. Rebuild and restart. `migrate` applies only the new migrations before `cloud` starts:
   ```sh
   docker compose up -d --build
   ```
5. Check the migration result and the server:
   ```sh
   docker compose logs migrate      # "migrations: up to date"
   curl https://your.domain/healthz
   ```

Some things to know about upgrades:
- **Migrations go forward only.** To roll back, restore the backup you took and check out the
  matching version.
- **GoTrue migrates its own `auth` schema when it starts.** Upgrade its image tag only when the
  compose file in the repository changes it.
- **Install new desktop releases** too. Old apps may lack fields a newer server expects. The other
  direction is tolerant: the server ignores fields it doesn't know.

## Using a Supabase project instead

If you prefer Supabase to run the database and sign-in, either their hosted service or their own
self-hosted stack:

1. **Database:** apply `supabase/migrations/*.sql` in order, in the SQL editor or with `psql`.
2. **Service role:** run `alter role nugget_service with login password '…'` and connect as
   `nugget_service.<project-ref>` through the pooler, with `sslmode=require`. For TLS checks, set
   `DATABASE_CA_CERT` to Supabase's root certificate.
3. **Auth settings:** in Auth → URL Configuration, allow `heed://auth/callback` and
   `nugget://auth/callback`.
4. **The server:** run `cloud` alone, with `SUPABASE_URL=https://<ref>.supabase.co`, the project's
   anon and service-role keys, and `NUGGET_BILLING=off`.
5. **The desktop app:** `NUGGET_SUPABASE_URL` is the Supabase URL and `NUGGET_CLOUD_URL` is your
   server.
