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
| `/` | A full-bleed ultramarine hero with one huge Fraunces line and one large print (El Greco, duotoned) over the drifting dithered sky. Directly under it, DS1's real Home screenshot (light or dark, following the theme) set into a duotone painting, then a quiet line of the 7 connectable apps. Six editorial feature blocks follow: a mono tag, a Fraunces title, two lines of copy and a large image tile, using paintings, dither and engraving, three of them with a real UI detail. Then how it works on hairlines, privacy (the Oxbow block and a plain-English list), a "Get Heed for Mac" block with system requirements in mono, the FAQ, and a footer with the engraved landscape and a giant faint wordmark. There are no invented metrics and no testimonials. |
| `/pricing` | The plans with a monthly/annual toggle (CSS only, via `:has()`), and a comparison table. Both are rendered at build time from `GET {CLOUD_URL}/v1/billing/plans`. The card grid has one column per plan, so a third tier fits. Note that `loadPlans` still only accepts the `heed` and `heed_max` ids. |
| `/download` | The macOS builds, from env vars. It leads with the Intel build when the browser reports x86. Windows and Linux show as "Coming soon". |
| `/signin`, `/signup` | Supabase magic link and Google OAuth. `?next=/path` is kept, and only same-site paths are allowed. |
| `/account` | The plan and status, read from `subscriptions` under RLS. Also: devices (list and remove), **Manage billing** (Stripe portal), download, sign out, and delete account. |
| `/checkout?plan=heed\|heed_max&interval=month\|year` | Sends you to sign up if you're signed out, then starts Stripe Checkout. |
| `/docs/self-host` | The book's self-hosting guide (`docs/book/src/guide/self-host.md`), copied into `src/content/docs/` by `npm run docs` and rendered with a sticky contents list. The smoke test fails if the copy is stale. |
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
  limits both tables to the signed-in user. Going through Supabase means a single round trip, and
  the devices list can be managed from the same place.

### Connected apps come from the cloud's catalogue

The "Connects to" strip and the FAQ list exactly the apps in `crates/nugget-cloud/src/apps/mod.rs`
(`CATALOG`). `npm run apps` regenerates `src/data/apps.json` from that file, and
`node scripts/sync-apps.mjs --check` checks it's up to date. The smoke test fails if the JSON, the
strip or the FAQ drift from the catalogue. Add an app to the cloud first, then run `npm run apps`.

### Waitlist

`WaitlistForm` (on /download, the landing's get-it block, and the "Heed Cloud isn’t open yet" pages)
posts `{email, source}` to `POST {CLOUD_URL}/v1/waitlist`. The cloud answers 202 to any well-formed
request and never says whether an address is new, so the form only confirms "You’re on the list".
A 429 asks the user to wait a bit. A 404 or a network error, which includes a CORS rejection before
the site's origin is in `NUGGET_SITE_ORIGINS`, falls back to an email link. Without
`NEXT_PUBLIC_CLOUD_URL` the form is just that email link. There are no third-party scripts.

### Prices come from the API

`src/lib/plans.ts` fetches the public `GET /v1/billing/plans` **at build time**. It writes the
prices, the trial length, the feature bullets, the comparison table and the device limits from
the plans' entitlements. Pages stay static, so this doesn't depend on CORS. If the API can't be
reached, or the cloud URL isn't set, the build logs a warning and uses a built-in copy of
`plans.toml`. `.pricing[data-plans-source]` records which one was used. **Redeploy after changing
prices.** A Cloudflare Pages deploy hook called from the cloud's release step keeps the two in step.

### What the cloud and Supabase need for the site to work

1. **`NUGGET_SITE_ORIGINS` on the cloud service must include this site's origin**, comma-separated
   and without a path, for example `https://heed.app,https://heed-site.pages.dev`. It does two jobs:
   - **CORS:** the browser may only call `POST /v1/billing/*` and `DELETE /v1/me` from origins on
     the list. Preflight allows `Authorization` and `Content-Type`.
   - **The return-URL allow-list:** a checkout or portal `return_url` must be on one of these
     origins, or the cloud answers `400 bad_return_url`.
2. **Supabase Auth → URL configuration.** Set the Site URL to the site, and add
   `https://<site>/**` to the redirect allow-list. Enable the Google provider.
3. **Return URL.** Checkout and portal requests send `return_url: {origin}/account?billing=done`,
   and the account page shows a "Thanks" message for that parameter. If the origin isn't in
   `NUGGET_SITE_ORIGINS` (a Pages preview deploy, for example), the cloud answers
   `400 bad_return_url`. The site then retries once without `return_url`, so Stripe falls back to
   `{NUGGET_PUBLIC_URL}/billing/done`, which hands over to the app.
4. **Web calls take no device seat.** Calls without an `X-Heed-Device` header are never counted as
   a device (ar/cloud 2d31d240).

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
npm test              # Playwright smoke test, then the bare-build test (see below)
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

## Deploy on Vercel (production)

`vercel.json` holds everything, so the dashboard needs no build settings:
- Astro framework, `npm ci`, `npm run build` and `dist`.
- `cleanUrls` and no trailing slash, to match `build.format: "file"` and `trailingSlash: "never"`.
- Long-lived caching for `/_astro/*` and `/fonts/*`. `/brand/*` gets one day plus
  stale-while-revalidate, because those files keep their names when DS1 redraws them.
- Security headers: nosniff, a referrer policy, a permissions policy, `X-Frame-Options: DENY`,
  and a CSP. The CSP allows `'unsafe-inline'` because of the theme and auth-hint scripts in the
  head, and `connect-src https:`, because Supabase and the cloud URL are build-time settings.

**Click path:**
1. vercel.com, then Add New, then Project, then Import the `rayhankhilji/nugget` GitHub repository.
2. Set **Root Directory** to `site`. Vercel reads the rest from `vercel.json`.
3. In Settings, Git, set **Production Branch** to `claude/inspiring-wozniak-z3gg3n`.
4. Optional: add environment variables (Settings, Environment Variables, Production and Preview):
   `NEXT_PUBLIC_SUPABASE_URL`, `NEXT_PUBLIC_SUPABASE_ANON_KEY`, `NEXT_PUBLIC_CLOUD_URL`, and the
   `NEXT_PUBLIC_DOWNLOAD_MAC_*` URLs.
5. Deploy. Every push to the production branch redeploys.

**Defaults with no environment variables:**
- The site URL comes from `VERCEL_PROJECT_PRODUCTION_URL`, which Vercel sets, so canonical links,
  the sitemap and `robots.txt` point at the production domain. `SITE_URL` overrides it, for example
  after you add a custom domain.
- Prices fall back to the built-in copy of `plans.toml`.
- Account, checkout, sign-in and sign-up show a calm "Heed Cloud isn't open yet" page, with
  Download the free app and Join the waitlist, until the three cloud variables are set.
  `tests/bare.mjs` checks that state.

Add the production origin to the cloud's `NUGGET_SITE_ORIGINS` once accounts are live, and add it to
Supabase's redirect allow-list.

Check it locally the way Vercel builds it. No token is needed if you create a local project stub:

```sh
mkdir -p .vercel && echo '{"projectId":"local","orgId":"local","settings":{"framework":"astro"}}' > .vercel/project.json
npx vercel build      # writes .vercel/output (git-ignored)
```

## Alternative: Cloudflare Pages


`wrangler.toml` sets `pages_build_output_dir = "dist"`. `public/_headers` adds security headers
(nosniff, a referrer policy, a permissions policy, `X-Frame-Options: DENY`, HSTS) and long-lived
caching for the hashed assets and fonts.

**From Git**, in the dashboard:

1. Go to Workers & Pages, then Create, then Pages, then Connect to Git. Pick this repo.
2. Set the root directory to `site`, the build command to `npm ci && npm run build`, and the output
   to `dist`.
3. Add the environment variables above, for Production and for Preview. Set `NODE_VERSION=22`.
4. Add the custom domain, then set `SITE_URL` to it and redeploy. Add the same origin to the cloud
   service's `NUGGET_SITE_ORIGINS` (for example `fly secrets set NUGGET_SITE_ORIGINS=https://heed.app`).

**From the CLI:**

```sh
cd site && npm ci && npm run build && npx wrangler pages deploy dist --project-name heed-site
```

Pages serves `/pricing` from `pricing.html` (the build uses `format: "file"`) and `404.html` for
unknown paths. Any static host that does the same will work, for example Vercel with
`cleanUrls: true`, or Netlify.

## Brand (v4: ultramarine and Fraunces)

**One source with the app.** These files are copied verbatim from DS1's `ds/brand` (182befd2), so the
website and the desktop app can't drift apart:

| In the site | From `ds/brand` |
| --- | --- |
| `src/styles/heed/tokens.css` | `apps/desktop/src/brand/tokens.css` (all `--heed-*` variables) |
| `src/styles/heed/fonts.css` | `apps/desktop/src/brand/fonts.css`, with `url()` paths changed to `/fonts/` |
| `public/fonts/*.woff2`, `OFL-*.txt` | `apps/desktop/src/fonts/` (Fraunces, Geist, Geist Mono) |
| `public/brand/*.svg`, `public/favicon.svg` | `docs/brand/assets/` (mark, wordmarks, favicon, app icon) |
| `public/art/painting-oxbow.webp` | `apps/desktop/src/brand/img/` (CC0, duotoned) |

To update, check those paths out from the newer `ds/brand` commit again. Never edit them here.

`src/styles/global.css` only maps the tokens onto the names the site's components use (`--bg`,
`--text`, `--accent` and so on) and adds layout. It holds no brand values.

The footer's theme switch can force dark on a light system. To support that, `Base.astro` reuses
`tokens.css`'s own dark block under `:root[data-theme="dark"]`, so no values are copied.

**DS1's contrast rules, which the site follows:**
- `ink-3` is used for icons and rules only. Quiet text uses `ink-quiet`.
- ok, warn and danger text use the `-ink` variants. The base colours are used for fills only.
- Filled buttons use `signal-fill`, and signal-coloured text uses `signal-ink`, only at 18px and up.

**Look:**
- Warm cream canvas and warm ink, with one bold ultramarine used in big fields: the hero sky, the
  feature band, the Heed Max card and the privacy block.
- Calm white product cards float over the imagery.
- Fraunces is the display face, used at SOFT 100, WONK 1 and a light weight, for headlines, prices
  and the greeting.
- Geist is the UI face. Geist Mono is for numbers and labels.

**Imagery:** original procedural art (from `scripts/art/`) plus one CC0 painting from DS1:
- the dithered sky in the hero, which drifts in a 36-second loop with at most 12px of parallax;
- the engraved landscape in the footer;
- Thomas Cole's *The Oxbow*, duotoned, in the privacy section, with a flat signal block over it.

The landing page also uses HTML recreations of the product cards. Real v4 app screenshots replace
them once they exist. Sources and licences are in `CREDITS.md`.

**Social cards:** each page gets a 1200×630 Open Graph and Twitter image: a Fraunces line over the
dithered sky, with the wordmark. They are rendered once at build time by
`src/pages/og/[slug].png.ts`, using satori and resvg-js with the static TTF instances in `src/og/`.
The lines live in `src/lib/og.ts`. `site.webmanifest` lists the app icons.

**Accessibility:**
- The smoke test runs axe-core on every route, in light and dark, at 1280 and 390 px, and fails on
  any violation.
- It also tabs through every route in both themes and checks that each tab stop shows a 2px focus
  ring (`--focus`: ink on cream, cream on dark and on the blue hero).
- The skip link comes first and moves focus to `<main>`.
- The hero has a "Pause motion" button (WCAG 2.2.2). It stops the sky drift, the parallax and the
  ghost-text loop, and the choice is remembered. Under reduced motion nothing moves, so the button
  is hidden.
- The FAQ uses native `<details>`/`<summary>`, which exposes the expanded state.
- Scrolling tables are focusable, labelled regions.

**Typography:** copy uses typographic quotes (’ ‘ ’ “ ”). `integrations/smartquotes.mjs` rewrites the
text of every built page after the build, leaving code, scripts and attributes alone.
`src/lib/smartquotes.mjs` handles the head, the social cards and the FAQ structured data, and the
client-side messages are written with ’ in source. The smoke test fails on straight apostrophes in
visible text.

**Performance:**
- CSS background art is lazy: `[data-bg]` elements apply their `--bg-img` only when they come within
  200px of the viewport.
- The logo and wordmark are cached SVG files, drawn as an `img` and a CSS mask, rather than inline
  SVG.
- The hero print has a 720w variant for phones.
- `tests/serve.mjs` gzips text like the production host does.

**Motion:**
- Easing is `cubic-bezier(.2,.8,.2,1)`. UI transitions take 200ms; brand moments take 600–900ms.
- `[data-reveal]` cards rise 8px and fade in, and lists stagger by 30ms. Feature blocks stagger by
  90ms (`--stagger`).
- On scroll, the hero sky sinks and the print rises, each by at most 12px.
- Agent steps tick to a check with a 180ms stroke draw.
- Under `prefers-reduced-motion`, only fades remain: the sky doesn't drift, the agent card renders
  in its final state, and the ghost text shows its accepted state.
