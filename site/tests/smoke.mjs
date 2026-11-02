// Smoke test: builds the site with test settings, then drives every route in Chromium.
// Supabase and the Heed cloud API are mocked at the network layer, so no real backend is needed.
import { createRequire } from "node:module";
import { buildTestSite, signedIn, SB, CLOUD } from "./mocks.mjs";

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

// 2. Pricing toggle switches prices and checkout links.
{
  const page = await browser.newPage();
  await page.goto(base + "/pricing");
  check((await page.getAttribute(".pricing", "data-plans-source")) === "api", "pricing rendered from GET /v1/billing/plans");
  const firstPrice = () => page.locator(".plan .amt:visible").first().textContent();
  check((await firstPrice()) === "£39", "pricing shows £39 monthly");
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
  check((await page.textContent("[data-plan-detail]"))?.includes("Free trial until 16 November 2026") === true, "account shows trial end");
  check((await page.textContent("[data-dev-limit]")) === "1 of 3", "account shows device count");
  await page.click("[data-portal]");
  await page.waitForURL("https://billing.stripe.test/**");
  check(
    calls.some((c) => c.path === "/v1/billing/portal" && c.method === "POST" && JSON.parse(c.body ?? "{}").return_url === `${base}/account?billing=done`),
    "portal POST with return_url",
  );

  await page.goto(base + "/account");
  await page.locator("[data-plan]").filter({ hasText: "Heed Max" }).waitFor();
  await page.fill('[data-delete] input', "delete");
  await page.click('[data-delete] button');
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
