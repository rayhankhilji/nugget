# Heed website

The public site for Heed: landing, pricing, download, sign in and sign up, account, checkout,
and draft legal pages. It is a static site that talks to Supabase (auth, plan and device rows) and
the Heed cloud API (`crates/nugget-cloud`) from the browser.

## Stack, and why

**Astro 5, TypeScript (strictest), scoped component CSS. No UI kit.**

- **It's a content site with a few interactive islands.** Astro renders every page to static HTML
  and ships no framework runtime. Each page loads only the script it uses: the hero demo
  (about 1 kB), the theme switch, and Supabase on the auth and account pages. Next.js static export
  would also work, but it ships React to every page and gets nothing back for it here. That headroom
  is how every public page scores 100 for performance.
- **Static export is the whole deploy.** `astro build` writes `dist/`, which any static host
  serves. There's no server, no edge functions and no secrets.
- **CSS.** Astro's scoped `<style>` blocks give the same per-component isolation as CSS modules.
  Design tokens live in `src/styles/global.css`, for example `--accent` (Coral) and the
  light and dark surfaces. Tailwind would have added a build dependency and a second vocabulary for
  a site this size.
- **Fonts.** Geist and Geist Mono (SIL OFL 1.1, licences in `public/fonts/`) are self-hosted as
  latin woff2 subsets, and Geist is preloaded.
- **App logos** come from `simple-icons` (CC0). They are inlined as SVG at build time, so they
  don't ship as a dependency. The logos are trademarks of their owners.

## Pages

| Route | What it does |
| --- | --- |
| `/` | The hero, with a looping ghost-text demo in HTML and CSS (mail, chat, docs), plus the app logos, the four features, how it works, privacy and security, pricing, FAQ and the CTA. There are no testimonials, on purpose. |
| `/pricing` | The plans with a monthly/annual toggle (CSS only, via `:has()`), and a comparison table. Both are rendered at build time from `GET {CLOUD_URL}/v1/billing/plans`. |
| `/download` | The macOS builds, from env vars. It leads with the Intel build when the browser reports x86. Windows and Linux show as "Coming soon". |
| `/signin`, `/signup` | Supabase magic link and Google OAuth. `?next=/path` is kept, and only same-site paths are allowed. |
| `/account` | The plan and status, read from `subscriptions` under RLS. Also: devices (list and remove), **Manage billing** (Stripe portal), download, sign out, and delete account. |
| `/checkout?plan=heed\|heed_max&interval=month\|year` | Sends you to sign up if you're signed out, then starts Stripe Checkout. |
| `/legal/privacy`, `/legal/terms` | **Drafts that need legal review**. Each one is marked at the top, and its placeholders are in `[brackets]`. |

The `account` and `checkout` pages are `noindex` and left out of the sitemap. They're also disallowed
in `robots.txt`.

## Backend contract

These come from `crates/nugget-cloud` on `ar/cloud` (`src/api/billing.rs`, `plans.toml`):

- `GET {CLOUD_URL}/v1/billing/plans` is public. The site reads it at build time; see below.
- `POST {CLOUD_URL}/v1/billing/checkout` with `{"plan": "heed" | "heed_max", "interval": "month" | "year", "return_url"}`
  returns `{"url"}`. A `409 already_subscribed` sends the user to Manage billing.
- `POST {CLOUD_URL}/v1/billing/portal` with `{"return_url"}` returns `{"url"}`.
- `DELETE {CLOUD_URL}/v1/me` deletes the account. It revokes the Composio connections first, then
  the auth user, and the delete cascades.
- Errors look like `{"error": code, "message"}`. Every call sends `Authorization: Bearer <Supabase access token>`.
- The account page reads `public.subscriptions` and `public.devices` straight from Supabase. RLS
  limits both tables to the signed-in user. Going through Supabase means it doesn't call `/v1/me`,
  which would register the browser as a device.

### Prices come from the API

`src/lib/plans.ts` fetches the public `GET /v1/billing/plans` **at build time**. It writes the
prices, the trial length, the feature bullets, the comparison table and the device limits from
the plans' entitlements. Pages stay static, so this doesn't depend on CORS. If the API can't be
reached, or the cloud URL isn't set, the build logs a warning and uses a built-in copy of
`plans.toml`. `.pricing[data-plans-source]` records which one was used. **Redeploy after changing
prices.** A Cloudflare Pages deploy hook called from the cloud's release step keeps the two in step.

### What the cloud and Supabase need for the site to work

1. **CORS.** The cloud service must allow the site's origin (`NUGGET_SITE_ORIGINS`, being added by
   AR1) for `POST /v1/billing/*` and `DELETE /v1/me`, including preflight with `Authorization` and
   `Content-Type`. Without that, the browser blocks these calls.
2. **Supabase Auth → URL configuration.** Set the Site URL to the site, and add
   `https://<site>/**` to the redirect allow-list. Enable the Google provider.
3. **Return URL.** Checkout and portal requests send `return_url: {site}/account?billing=done`, and
   the account page shows a "Thanks" message for that parameter. Until the cloud allow-lists and
   uses the field, it ignores it, and Stripe returns to `{NUGGET_PUBLIC_URL}/billing/done`, which
   hands over to the app.

## Environment

All of these are **public** values that get compiled into the browser bundle. Never put secrets
here. `.env*` is git-ignored, and `.env.example` lists the names.

| Variable | Required | Notes |
| --- | --- | --- |
| `NEXT_PUBLIC_SUPABASE_URL` | yes | `https://<ref>.supabase.co` (London project) |
| `NEXT_PUBLIC_SUPABASE_ANON_KEY` | yes | The anon key. It's public by design and protected by RLS. |
| `NEXT_PUBLIC_CLOUD_URL` | yes | The cloud API base URL, for example `https://heed-cloud.fly.dev` |
| `NEXT_PUBLIC_DOWNLOAD_MAC_ARM_URL` | no | The Apple silicon `.dmg`. If neither download URL is set, `/download` says downloads open at launch. |
| `NEXT_PUBLIC_DOWNLOAD_MAC_INTEL_URL` | no | The Intel `.dmg` |
| `SITE_URL` | no | The canonical origin, used for the sitemap, canonical links and OG tags. It defaults to `https://heed.pages.dev`. |

Astro is set up with `envPrefix: ["PUBLIC_", "NEXT_PUBLIC_"]`, so the same names work as in the other
Heed deploys. If the Supabase or cloud variables are missing, the build still succeeds and the auth
pages say they aren't configured.

The contact addresses `support@heed.app` and `privacy@heed.app` (in `src/lib/config.ts`) are
placeholders. Confirm them before launch.

## Develop

```sh
cd site
npm ci
cp .env.example .env   # fill in your values
npm run dev            # http://localhost:4321
```

## Verify

```sh
npm run build         # static build into dist/
npm run check         # astro check (TypeScript, strictest)
npm run lint          # eslint (typescript-eslint and eslint-plugin-astro)
npm test              # Playwright smoke test (see below)
npm run lighthouse    # Lighthouse, mobile and desktop, every page; fails under 95
npm run screenshots   # every page at 1440 and 390 px, light and dark, into screenshots/
```

These scripts use the Playwright and Chromium that are already installed in the dev container. They
are resolved with `require("playwright")`, so nothing has to be downloaded.

- **`npm test`** builds a copy of the site with test settings into `dist-test/`. It mocks Supabase
  and the cloud API at the network layer, then checks:
  - every route in light and dark at 1280 and 360 px: status, one `h1`, no console errors, and no
    horizontal overflow;
  - the pricing toggle;
  - the redirects for signed-out users;
  - the magic-link request and its redirect;
  - pricing is rendered from a local `/v1/billing/plans` stub served during the build;
  - checkout (`POST /v1/billing/checkout` with the right body, `return_url` and bearer token, then
    following the returned URL);
  - the account page: trial status, device count, the portal, and deleting the account.

## Deploy (Cloudflare Pages, free)

`wrangler.toml` sets `pages_build_output_dir = "dist"`. `public/_headers` adds security headers
(nosniff, a referrer policy, a permissions policy, `X-Frame-Options: DENY`, HSTS) and long-lived
caching for the hashed assets and fonts.

**From Git**, in the dashboard:

1. Go to Workers & Pages, then Create, then Pages, then Connect to Git. Pick this repo.
2. Set the root directory to `site`, the build command to `npm ci && npm run build`, and the output
   to `dist`.
3. Add the environment variables above, for Production and for Preview. Set `NODE_VERSION=22`.
4. Add the custom domain, then set `SITE_URL` to it and redeploy.

**From the CLI:**

```sh
cd site && npm ci && npm run build && npx wrangler pages deploy dist --project-name heed-site
```

Pages serves `/pricing` from `pricing.html` (the build uses `format: "file"`) and `404.html` for
unknown paths. Any static host that does the same will work, for example Vercel with
`cleanUrls: true`, or Netlify.

## Brand

These match the desktop app (`ds/brand`):
- **Mark:** `public/favicon.svg` is `packaging/icons/nugget.svg`: a coral attention dot above a
  chalk line, on a graphite tile. `src/components/Logo.astro` inlines it, and the dot's glow pulses
  unless reduced motion is on. The wordmark is "heed" in Geist 600 at -0.04em. The Geist woff2 is
  byte-identical to `apps/desktop/src/fonts`.
- **Tokens:** the same two as `apps/desktop/src/tokens.css`:
  - `--brand` is `#FF5A3C`, for the mark, dots and the ghost-text shimmer;
  - `--accent` is for actions: `#BC3416` with white text in light mode (meets AA), and coral with
    graphite text in dark mode;
  - the rest of the palette is Graphite `#16171B`, Chalk `#F7F7F5` and Mist `#9AA3AE`. In light mode,
    secondary text uses a deeper Mist, `#4F5661`, to meet AA.
- **App screenshots:** `public/app/*.webp` are converted from `docs/book/src/images/screenshots/` on
  `ds/brand`, which are taken from the real app running against a fictional corpus. The landing
  page shows them in tabs (Needs you, Home, Answers, Connections), and switches light and dark with
  the site theme. To refresh them, regenerate the app screenshots and convert again with sharp
  (WebP, quality 82).
