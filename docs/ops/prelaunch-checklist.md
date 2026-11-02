# Heed pre-launch checklist

Tick each item before inviting users. Anything left unticked needs a written reason in the launch
notes. Procedures referred to are in [runbook.md](runbook.md).

## Accounts & regions
- [ ] Every provider account (Fly, Supabase, Composio, Anthropic, OpenAI, Stripe, Sentry, domain,
      Apple/Microsoft signing) is owned by a company email, with MFA on and recovery codes stored offline
- [ ] A second person (or a sealed break-glass note) can reach the accounts if the founder cannot
- [ ] Supabase project in `eu-west-2` (London); Fly app `nugget-cloud` with `primary_region = "lhr"`
- [ ] Billing alerts set on Fly, Supabase, Composio and both AI providers

## Supabase
- [ ] `supabase/tests/run.sh` passes on a fresh database (RLS: no cross-user read, write or delete)
- [ ] All migrations applied to production (`supabase db push --dry-run` shows nothing pending)
- [ ] `nugget_service` is `LOGIN` with a strong generated password (32+ random characters), set by
      hand and stored only in Fly secrets and the password manager
- [ ] `nugget_service` has only the rights the migrations grant: not superuser, no `bypassrls`, no
      `createrole`/`createdb`, member of no other role (`\du nugget_service`)
- [ ] Auth → URL Configuration: redirect allow-list contains only `heed://auth/callback` and, during
      the transition, `nugget://auth/callback`; no wildcards, no `localhost`
- [ ] Site URL set to the product site, not `localhost`
- [ ] Email templates (magic link, change email) branded Heed and sent from our domain via custom SMTP
      (SPF, DKIM, DMARC pass)
- [ ] Auth rate limits reviewed (emails per hour, OTP and verification requests); CAPTCHA considered
- [ ] Leaked-password protection on (if passwords are ever enabled); MFA available for the
      dashboard accounts at least
- [ ] Data API: `anon` has no grants on `public` tables and no execute on `nugget_private`
      functions; `authenticated` can only `select`/`delete` own rows (checked in the SQL editor)
- [ ] Unused schemas removed from the Data API's exposed schemas; `nugget_private` not exposed
- [ ] Network restrictions on direct database access (allow only what needs it; the service uses
      the pooler); SSL enforcement on
- [ ] PITR decision written down: buy the add-on, or accept up to 24 h data loss with daily backups
- [ ] Storage bucket `exports` is private; policy limits reads to `<uid>/`

## Fly
- [ ] Secrets set: `DATABASE_URL`, `SUPABASE_URL`, `SUPABASE_ANON_KEY`, `SUPABASE_SERVICE_ROLE_KEY`,
      `COMPOSIO_API_KEY`, `COMPOSIO_AUTH_CONFIGS`, `COMPOSIO_WEBHOOK_SECRET`, `ANTHROPIC_API_KEY`,
      `OPENAI_API_KEY` (if used), `DATABASE_CA_CERT`, `SENTRY_DSN` (if used) — `fly secrets list`
- [ ] No secrets in `fly.toml`, the Dockerfile, the image or the repository (`git log -p` scan or
      secret scanning run)
- [ ] Machine count decided: 2 machines for availability during deploys, remembering that the
      in-process rate limits are **per machine** (effective limit = count × setting) and
      machines × `DATABASE_POOL_MAX` must stay under the Supabase pooler limit
- [ ] Health check in `fly.toml` uses `/readyz`; `kill_timeout = 30`
- [ ] `[metrics] port = 9091, path = "/metrics"` in `fly.toml`; metrics visible in Fly's Grafana;
      `/metrics` not reachable from the public internet (checked with curl)
- [ ] Log retention decided; log shipping set up if logs must be kept beyond Fly's short window
- [ ] Logs checked after a test session: no emails, prompts, tokens or message bodies appear
- [ ] Alerts wired (Grafana or Sentry): `/readyz` failing, 5xx rate, `heed_db_pool_available` at 0,
      sync errors, daily AI cost

## Composio
- [ ] Plan chosen: Hobby (100k tool calls/month, **usage pauses for everyone at the cap**) or Pro;
      a reminder set to upgrade at about 70% of the cap
- [ ] Auth configs for Gmail and Google Calendar created; ids in `COMPOSIO_AUTH_CONFIGS`
- [ ] Webhook URL `https://<api-host>/v1/webhooks/composio` registered and `COMPOSIO_WEBHOOK_SECRET` set
- [ ] Connect callback URL matches the app's registered scheme (`NUGGET_CONNECT_CALLBACK`)
- [ ] Google OAuth consent screen: app name, privacy policy link, scopes minimal; verification
      status understood (unverified apps are limited to 100 test users)

## AI providers
- [ ] Monthly spend limits set in the Anthropic and OpenAI consoles (and alert thresholds below them)
- [ ] Each key belongs to one dedicated project/workspace used only by production
- [ ] Zero-data-retention requested from Anthropic/OpenAI, or the default retention (up to 30 days)
      accepted and stated in the privacy policy
- [ ] Model ids (`NUGGET_MODEL_FAST`, `NUGGET_MODEL_SMART`) and per-plan quotas checked against
      current prices

## Stripe (once billing lands)
- [ ] Live keys only in production, test keys only elsewhere; restricted secret key
- [ ] Webhook endpoint registered; `STRIPE_WEBHOOK_SECRET` set; signature verification tested
- [ ] Customer deletion added to the account-deletion path

## Desktop app
- [ ] Only the public anon/publishable key, `SUPABASE_URL` and the API base URL are baked in (search
      the built bundle for `sb_secret`, `service_role`, `sk-` and other key prefixes)
- [ ] macOS build signed and notarised; Windows build signed
- [ ] Deep-link scheme `heed://` registered (and `nugget://` during the transition); callback
      handling rejects unexpected hosts and paths
- [ ] Updater signing key kept offline (not in CI logs or the repository); feed served over HTTPS
- [ ] Refresh token stored in the OS keychain; sign-out revokes the session

## Security
- [ ] `cargo deny check` green
- [ ] Threat model `docs/security/cloud-threat-model.md` reviewed; every open High item resolved or
      accepted in writing with a date
- [ ] `SECURITY.md` contact works (send a test report) and covers the cloud service
- [ ] Rate limits and body size limits verified (429 with `Retry-After`, 413 over 64 KiB)
- [ ] `DELETE /v1/me` and `GET /v1/export` tested against a second account (no cross-user data)

## Privacy & legal
- [ ] Privacy policy published; lists processors: Supabase, Fly.io, Composio, Anthropic, OpenAI,
      Stripe, and Sentry if used, with locations and international transfer safeguards
- [ ] DPAs signed or accepted with every processor
- [ ] Record of processing activities (ROPA) written
- [ ] ICO registration (data protection fee) paid
- [ ] DPIA completed for processing users' email and calendar content with AI
- [ ] Onboarding says plainly what leaves the device (indexed mail and events, questions, screen
      text sent with a query) and where it goes
- [ ] Retention stated: items kept until disconnect or account deletion; backups 7 days;
      provider logs up to 30 days
- [ ] Terms of service published; Google API Services User Data Policy (Limited Use) disclosure
      included if required
- [ ] Deletion log and breach log exist, outside the database

## Ops
- [ ] Runbook rehearsed: a backup restored into a scratch Supabase project and verified
- [ ] One key rotated end to end (e.g. `COMPOSIO_API_KEY`) with no user-facing errors
- [ ] A test user deleted end to end (app, then manual procedure), all counts 0, Composio accounts gone
- [ ] Deploy and rollback rehearsed (`fly releases --image`, `fly deploy --image ...`)
- [ ] Load test run against a staging app: Ask latency, sync pass time, pool usage at the expected
      peak; results written down
- [ ] On-call contact details and provider support links in the password manager

## Launch day
- [ ] Production deploy from a tagged commit; `/healthz` and `/readyz` green
- [ ] Fresh install of the signed desktop build: sign in, connect Gmail and Calendar, first sync, Ask
      with citations, export, delete account
- [ ] Dashboards open: Fly metrics, Sentry, Supabase reports, Composio usage, AI spend
- [ ] Spend limits and Composio usage checked again at end of day
- [ ] No deploys for 24 hours after launch except fixes
