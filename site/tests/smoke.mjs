// Smoke test: builds the site with test settings, then drives every route in Chromium.
// Supabase and the Heed cloud API are mocked at the network layer, so no real backend is needed.
import { createRequire } from "node:module";
import { buildTestSite, signedIn, SB, CLOUD, ME } from "./mocks.mjs";

await buildTestSite();
const { serve, ROUTES } = await import("./serve.mjs");
const { chromium } = createRequire(import.meta.url)("playwright");

let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures++;
};

const { server, base } = await serve(4175);
const browser = await chromium.launch();

// 1. Every route renders, has one h1 and a title, logs no errors, and never scrolls sideways at 360 px.
for (const scheme of ["light", "dark"]) {
  for (const width of [1280, 360]) {
    const ctx = await browser.newContext({ viewport: { width, height: 800 }, colorScheme: scheme });
    const page = await ctx.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    // Pages that need a session redirect to sign-in; stop there.
    for (const r of ROUTES) {
      const res = await page.goto(base + r, { waitUntil: "networkidle" });
      const expected = r === "/no-such-page" ? 404 : 200;
      const h1 = await page.locator("h1").count();
      const title = await page.title();
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      const bg = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
      // The browser logs the 404 document itself as an error; that one is expected.
      if (expected === 404) errors.splice(0, errors.length, ...errors.filter((e) => !e.includes("status of 404")));
      const ok = res?.status() === expected && h1 === 1 && title.length > 0 && overflow <= 0 && errors.length === 0;
      check(ok, `${r} @${width} ${scheme} → ${page.url().replace(base, "")} (h1=${h1}, overflow=${overflow}, bg=${bg}${errors.length ? `, errors=${errors.join(" | ")}` : ""})`);
      errors.length = 0;
    }
    await ctx.close();
  }
}

// 1b. The "Connects to" strip and the FAQ list exactly the cloud's app catalogue, in order.
{
  const { readFileSync, existsSync } = await import("node:fs");
  const apps = JSON.parse(readFileSync(new URL("../src/data/apps.json", import.meta.url), "utf8"));
  const catalogue = new URL("../../crates/nugget-cloud/src/apps/mod.rs", import.meta.url);
  if (existsSync(catalogue)) {
    const { readCatalog } = await import("../scripts/sync-apps.mjs");
    check(JSON.stringify(readCatalog()) === JSON.stringify(apps), "apps.json matches crates/nugget-cloud/src/apps/mod.rs");
  }
  const page = await browser.newPage();
  await page.goto(base + "/");
  const shown = await page.$$eval("[data-apps] [data-app]", (els) => els.map((e) => e.getAttribute("data-app")));
  check(JSON.stringify(shown) === JSON.stringify(apps.map((a) => a.slug)), `app strip shows exactly ${shown.join(", ")}`);
  const faq = await page.locator("#faq details", { hasText: "Which apps can I connect?" }).textContent();
  check(apps.every((a) => faq?.includes(a.name)) && faq?.includes("More coming soon") === true, "FAQ lists the same apps");
  await page.close();
}

// 1c. Canonical links are clean URLs (no .html), matching cleanUrls on Vercel and Pages.
{
  const page = await browser.newPage();
  await page.goto(base + "/pricing");
  const href = await page.getAttribute('link[rel="canonical"]', "href");
  check(!!href && href.endsWith("/pricing") && !href.includes(".html"), `canonical is clean: ${href}`);
  await page.close();
}

// 1d. With release URLs set, /download offers the direct downloads.
{
  const page = await browser.newPage();
  await page.goto(base + "/download");
  check((await page.getAttribute('[data-dl="arm"]', "href"))?.endsWith(".dmg") === true, "download links to the configured release");
  await page.close();
}

// 1e. Each page's social card exists and is a 1200x630 PNG; the manifest is linked.
{
  const page = await browser.newPage();
  for (const [route, slug] of [["/", "home"], ["/pricing", "pricing"], ["/legal/privacy", "privacy"]]) {
    await page.goto(base + route);
    const og = await page.getAttribute('meta[property="og:image"]', "content");
    const tw = await page.getAttribute('meta[name="twitter:image"]', "content");
    const res = await page.request.get(base + new URL(og ?? "/").pathname);
    const buf = await res.body();
    const w = buf.readUInt32BE(16), h = buf.readUInt32BE(20);
    check(og?.endsWith(`/og/${slug}.png`) && tw === og && res.ok() && w === 1200 && h === 630, `${route} social card ${og} (${w}x${h})`);
  }
  check((await page.locator('link[rel="manifest"]').count()) === 1, "manifest linked");
  await page.close();
}

// 1f. The self-hosting guide renders from the book, and the copy is in sync with docs/book.
{
  const { existsSync } = await import("node:fs");
  if (existsSync(new URL("../../docs/book/src/guide/self-host.md", import.meta.url))) {
    const { DOCS, render } = await import("../scripts/sync-docs.mjs");
    const { readFileSync } = await import("node:fs");
    const fresh = DOCS.every((d) => readFileSync(new URL(`../${d.to}`, import.meta.url), "utf8") === render(d.from));
    check(fresh, "src/content/docs matches docs/book (run npm run docs)");
  }
  const page = await browser.newPage();
  await page.goto(base + "/docs/self-host");
  check((await page.textContent("h1")) === "Self-hosting Heed Cloud", "/docs/self-host renders the guide");
  const anchor = await page.getAttribute('.side a[href^="#"]', "href");
  check(!!anchor && (await page.locator(anchor).count()) === 1, `self-host contents link resolves (${anchor})`);
  await page.goto(base + "/");
  check((await page.locator('#faq a[href="/docs/self-host"]').count()) === 1, "FAQ links to /docs/self-host");
  await page.close();
}

// 1g. Copy uses typographic quotes: no straight apostrophes in visible text outside code.
{
  const page = await browser.newPage();
  for (const r of ["/", "/pricing", "/download", "/legal/privacy"]) {
    await page.goto(base + r);
    const straight = await page.evaluate(() => {
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      const hits = [];
      for (let n = walker.nextNode(); n; n = walker.nextNode()) {
        if (n.parentElement?.closest("script, style, pre, code, kbd")) continue;
        if (/\w'\w|"/.test(n.textContent ?? "")) hits.push((n.textContent ?? "").trim().slice(0, 40));
      }
      return hits;
    });
    check(straight.length === 0, `${r} uses typographic quotes${straight.length ? `: ${straight.slice(0, 3).join(" | ")}` : ""}`);
  }
  await page.close();
}

// 1h. Waitlist: 202 shows the calm confirmation, 429 asks to wait, a network error falls back to email.
{
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  let mode = 202;
  const bodies = [];
  await ctx.route(`${CLOUD}/v1/waitlist`, async (route) => {
    bodies.push(route.request().postDataJSON());
    if (mode === 0) return route.abort("failed");
    return route.fulfill({ status: mode, body: "" });
  });
  // The test build has release URLs set, so /download shows downloads; the landing form is the one here.
  await page.goto(base + "/");
  const form = page.locator('form[data-waitlist="landing"]');
  await form.locator("input").fill("ada@example");
  await form.locator("button").click();
  check((await form.locator("[data-msg]").textContent())?.includes("doesn’t look quite right") === true && bodies.length === 0, "waitlist catches a typo locally");
  await form.locator("input").fill("ada@example.com");
  await form.locator("button").click();
  await form.locator("[data-msg]", { hasText: "on the list" }).waitFor();
  check(JSON.stringify(bodies.at(-1)) === JSON.stringify({ email: "ada@example.com", source: "landing" }), `waitlist 202 → confirmation (${JSON.stringify(bodies.at(-1))})`);
  mode = 429;
  await form.locator("input").fill("ada@example.com");
  await form.locator("button").click();
  await form.locator("[data-msg]", { hasText: "Too many tries" }).waitFor();
  check(true, "waitlist 429 → please wait");
  mode = 0;
  await form.locator("input").fill("ada@example.com");
  await form.locator("button").click();
  await form.locator("[data-msg] a[href^='mailto:']").waitFor();
  check(bodies.at(-1)?.source === "landing", "waitlist network error → email fallback");
  await ctx.close();
}

// 1i. axe-core: no accessibility violations on any route, light and dark, desktop and phone.
{
  const { axe } = await import("./axe.mjs");
  for (const scheme of ["light", "dark"]) {
    for (const width of [1280, 390]) {
      const ctx = await browser.newContext({ viewport: { width, height: 900 }, colorScheme: scheme, reducedMotion: "reduce" });
      const page = await ctx.newPage();
      for (const r of ROUTES) {
        await page.goto(base + r, { waitUntil: "networkidle" });
        // Scroll through so reveal-on-scroll content is visible, as a reader would see it.
        await page.evaluate(async () => {
          for (let y = 0; y < document.body.scrollHeight; y += 600) {
            window.scrollTo(0, y);
            await new Promise((res) => setTimeout(res, 30));
          }
        });
        await page.waitForTimeout(500);
        const v = await axe(page);
        check(v.length === 0, `axe ${r} @${width} ${scheme}${v.length ? `: ${v.join(" | ")}` : ""}`);
      }
      await ctx.close();
    }
  }
}

// 1j. Keyboard: the skip link comes first and moves focus to main; every tab stop shows a focus ring;
//     the hero's "Pause motion" stops the drift.
{
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  await page.goto(base + "/", { waitUntil: "networkidle" });
  await page.keyboard.press("Tab");
  check((await page.evaluate(() => document.activeElement?.textContent?.trim())) === "Skip to content", "first Tab reaches the skip link");
  await page.keyboard.press("Enter");
  check((await page.evaluate(() => document.activeElement?.id)) === "main", "skip link moves focus to main");
  const unringed = [];
  for (let i = 0; i < 45; i++) {
    await page.keyboard.press("Tab");
    const r = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return null;
      const cs = getComputedStyle(el);
      const ring = cs.outlineStyle !== "none" && parseFloat(cs.outlineWidth) >= 2;
      return ring ? null : `${el.tagName.toLowerCase()}.${String(el.className).split(" ")[0]} "${(el.textContent ?? "").trim().slice(0, 24)}"`;
    });
    if (r) unringed.push(r);
  }
  check(unringed.length === 0, `every tab stop on / shows a focus ring${unringed.length ? `: ${unringed.slice(0, 4).join(" | ")}` : ""}`);
  // The same on every other route, in both themes.
  for (const scheme of ["light", "dark"]) {
    const kp = await browser.newPage({ viewport: { width: 1280, height: 900 }, colorScheme: scheme });
    for (const r of ROUTES.filter((x) => x !== "/")) {
      await kp.goto(base + r, { waitUntil: "networkidle" });
      const miss = [];
      for (let i = 0; i < 30; i++) {
        await kp.keyboard.press("Tab");
        const m = await kp.evaluate(() => {
          const el = document.activeElement;
          if (!el || el === document.body) return null;
          const cs = getComputedStyle(el);
          return cs.outlineStyle !== "none" && parseFloat(cs.outlineWidth) >= 2 ? null : `${el.tagName.toLowerCase()} "${(el.textContent ?? "").trim().slice(0, 20)}"`;
        });
        if (m) miss.push(m);
      }
      check(miss.length === 0, `focus rings on ${r} (${scheme})${miss.length ? `: ${miss.slice(0, 3).join(" | ")}` : ""}`);
    }
    await kp.close();
  }
  const btn = page.locator("[data-motion]");
  check(await btn.isVisible(), "hero has a Pause motion control");
  await btn.click();
  const paused = await page.evaluate(() => getComputedStyle(document.querySelector(".drift")).animationPlayState);
  check(paused === "paused" && (await btn.getAttribute("aria-pressed")) === "true", `Pause motion stops the drift (${paused})`);
  await btn.click();
  await page.close();
}

// 2. Pricing toggle switches prices and checkout links.
{
  const page = await browser.newPage();
  await page.goto(base + "/pricing");
  check((await page.getAttribute(".pricing", "data-plans-source")) === "api", "pricing rendered from GET /v1/billing/plans");
  const firstPrice = () => page.locator(".plan .amt:visible").first().textContent();
  const tiers = await page.$$eval("[data-tier]", (els) => els.map((e) => e.getAttribute("data-tier")));
  check(JSON.stringify(tiers) === JSON.stringify(["free", "heed", "heed_max"]), `pricing has three tiers: ${tiers.join(", ")}`);
  check((await page.locator('[data-tier="free"] a[href*="github.com/rayhankhilji/nugget"]').count()) === 1, "free tier links to GitHub");
  check(!/no free (plan|tier)/i.test(await page.textContent("body") ?? ""), "pricing no longer says there's no free plan");
  check((await firstPrice()) === "£39", "pricing shows £39 monthly");
  check((await page.textContent(".toggle em"))?.trim() === "Save 17%", "annual toggle says Save 17%");
  await page.locator('input[value="year"]').check();
  check((await firstPrice()) === "£390", "pricing toggle shows £390 annual");
  const href = await page.locator(".plan a.go:visible").first().getAttribute("href");
  check(href === "/checkout?plan=heed&interval=year", `annual CTA links to ${href}`);
  await page.close();
}

// 3. Signed out: checkout and account send you to sign up / sign in, keeping where you were going.
{
  const page = await browser.newPage();
  await page.goto(base + "/checkout?plan=heed_max&interval=year");
  await page.waitForURL(/\/signup\?next=/);
  check(decodeURIComponent(page.url()).includes("/checkout?plan=heed_max&interval=year"), "checkout → signup keeps next");
  await page.goto(base + "/account");
  await page.waitForURL(/\/signin\?next=/);
  check(true, "account → signin when signed out");
  await page.close();
}

// 4. Magic link request goes to Supabase with the right redirect.
{
  const page = await browser.newPage();
  let otp = null;
  await page.route(`${SB}/auth/v1/otp*`, async (route) => {
    otp = { url: route.request().url(), body: route.request().postDataJSON() };
    await route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
  });
  await page.goto(base + "/signup?next=" + encodeURIComponent("/checkout?plan=heed&interval=month"));
  await page.fill('input[type="email"]', "ada@example.com");
  await page.click('button[type="submit"]');
  await page.getByText("Check ada@example.com").waitFor();
  check(otp?.body?.email === "ada@example.com" && otp?.body?.create_user === true, "magic link requested for signup");
  check(decodeURIComponent(otp?.url ?? "").includes(`redirect_to=${base}/checkout?plan=heed&interval=month`), "magic link redirects back to checkout");
  await page.close();
}

// 5. Signed in: checkout calls the billing API and follows the returned URL; account reads plan and opens the portal.
{
  const ctx = await browser.newContext();
  await signedIn(ctx);
  const calls = [];
  await ctx.route(`${CLOUD}/**`, async (route) => {
    const req = route.request();
    calls.push({ method: req.method(), path: new URL(req.url()).pathname, body: req.postData(), auth: req.headers()["authorization"] });
    const path = new URL(req.url()).pathname;
    if (path === "/v1/billing/checkout") return route.fulfill({ json: { url: "https://checkout.stripe.test/c/pay_123" } });
    if (path === "/v1/billing/portal") return route.fulfill({ json: { url: "https://billing.stripe.test/p/session_123" } });
    if (path === "/v1/me" && req.method() === "DELETE") return route.fulfill({ status: 204 });
    if (path === "/v1/me") return route.fulfill({ json: ME });
    return route.fulfill({ status: 404, json: { error: "not_found", message: "no" } });
  });
  await ctx.route("https://*.stripe.test/**", (route) => route.fulfill({ contentType: "text/html", body: "<title>Stripe</title><h1>Stripe</h1>" }));

  const page = await ctx.newPage();
  await page.goto(base + "/checkout?plan=heed_max&interval=year");
  await page.waitForURL("https://checkout.stripe.test/**");
  const co = calls.find((c) => c.path === "/v1/billing/checkout");
  check(co?.method === "POST" && co.body === JSON.stringify({ plan: "heed_max", interval: "year", return_url: `${base}/account?billing=done` }) && co.auth === "Bearer test-access-token", `checkout POST ${co?.body}`);

  await page.goto(base + "/account");
  await page.locator("[data-plan]").filter({ hasText: "Heed Max" }).waitFor();
  check(
    (await page.textContent("[data-when]")) === "16 November 2026" &&
      (await page.textContent("[data-plan-detail]"))?.includes("free trial ends on 16 November 2026") === true,
    "account shows the trial end date",
  );
  check((await page.textContent("[data-dev-limit]")) === "1 of 3", "account shows device count");
  await page.click("[data-portal]");
  await page.waitForURL("https://billing.stripe.test/**");
  check(
    calls.some((c) => c.path === "/v1/billing/portal" && c.method === "POST" && JSON.parse(c.body ?? "{}").return_url === `${base}/account?billing=done`),
    "portal POST with return_url",
  );

  await page.goto(base + "/account");
  await page.locator("[data-plan]").filter({ hasText: "Heed Max" }).waitFor();
  check((await page.textContent("[data-usage-text]")) === "720 of 900", "account shows deep-answer usage");
  check((await page.getAttribute("[data-usage-bar]", "data-level")) === "high", "usage meter turns amber at 80%");
  await page.click("[data-delete-open]");
  check(await page.isDisabled("[data-delete-confirm]"), "delete stays disabled until 'delete' is typed");
  await page.fill('[data-delete] input', "delete");
  await page.click("[data-delete-confirm]");
  await page.waitForURL(`${base}/?deleted=1`);
  check(calls.some((c) => c.path === "/v1/me" && c.method === "DELETE"), "delete account DELETE /v1/me");
  await ctx.close();
}

// 6. An origin the cloud doesn't allow-list gets 400 bad_return_url; checkout retries without it.
{
  const ctx = await browser.newContext();
  await signedIn(ctx);
  const bodies = [];
  await ctx.route(`${CLOUD}/v1/billing/checkout`, async (route) => {
    const body = route.request().postDataJSON();
    bodies.push(body);
    if (body.return_url) return route.fulfill({ status: 400, json: { error: "bad_return_url", message: "return_url must be on the Heed website" } });
    return route.fulfill({ json: { url: "https://checkout.stripe.test/c/pay_456" } });
  });
  await ctx.route("https://*.stripe.test/**", (route) => route.fulfill({ contentType: "text/html", body: "<title>Stripe</title>" }));
  const page = await ctx.newPage();
  await page.goto(base + "/checkout?plan=heed&interval=month");
  await page.waitForURL("https://checkout.stripe.test/**");
  check(bodies.length === 2 && bodies[0].return_url && !("return_url" in bodies[1]), "bad_return_url → retried without return_url");
  await ctx.close();
}

await browser.close();
server.close();
console.log(failures ? `\n${failures} failure(s)` : "\nall smoke checks passed");
process.exit(failures ? 1 : 0);
