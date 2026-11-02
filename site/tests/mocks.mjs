// Shared test fixtures: a build with test settings, and a signed-in browser context with Supabase mocked.
import { exec } from "node:child_process";
import { createServer } from "node:http";
import { promisify } from "node:util";

export const OUT = "dist-test";
export const SB = "https://test-ref.supabase.co";
// A local stand-in for the cloud API. It serves /v1/billing/plans during the build; in the
// browser, Playwright intercepts calls to this origin.
export const CLOUD = "http://127.0.0.1:4310";

const ent = (o) => ({
  ghost_text: true,
  needs_you_inbox: true,
  daily_brief: true,
  actions_with_approval: true,
  ...o,
});
export const PLANS_API = {
  plans: [
    {
      id: "heed_max",
      name: "Heed Max",
      monthly_gbp: 99,
      annual_gbp: 990,
      trial_days: 14,
      entitlements: ent({ plan: "heed_max", name: "Heed Max", max_connections: null, sync_interval_minutes: 5, history_days: null, smart_asks_per_month: 900, default_model: "smart", devices: 3, agent_tasks: true, computer_use: true, priority_support: true }),
    },
    {
      id: "heed",
      name: "Heed",
      monthly_gbp: 39,
      annual_gbp: 390,
      trial_days: 14,
      entitlements: ent({ plan: "heed", name: "Heed", max_connections: 10, sync_interval_minutes: 30, history_days: 365, smart_asks_per_month: 300, default_model: "fast", devices: 1, agent_tasks: false, computer_use: false, priority_support: false }),
    },
  ],
};

/** GET /v1/me for the signed-in test user: Heed Max, 720 of 900 deep answers used. */
export const ME = {
  plan: "heed_max",
  entitlements: { smart_asks_per_month: 900, default_model: "smart" },
  usage: { smart_asks_used: 720, month_cost_micros: 4200000 },
};

/** Builds the site with test settings into dist-test/, with the plans API served locally. */
export async function buildTestSite() {
  process.env.ASTRO_OUT_DIR = OUT;
  const api = createServer((req, res) => {
    res.writeHead(req.url === "/v1/billing/plans" ? 200 : 404, { "Content-Type": "application/json" });
    res.end(req.url === "/v1/billing/plans" ? JSON.stringify(PLANS_API) : "{}");
  });
  await new Promise((r) => api.listen(4310, "127.0.0.1", r));
  await promisify(exec)("npx astro build", {
    env: {
      ...process.env,
      ASTRO_OUT_DIR: OUT,
      NEXT_PUBLIC_SUPABASE_URL: SB,
      NEXT_PUBLIC_SUPABASE_ANON_KEY: "test-anon-key",
      NEXT_PUBLIC_CLOUD_URL: CLOUD,
      NEXT_PUBLIC_DOWNLOAD_MAC_ARM_URL: "https://downloads.heed.test/Heed_aarch64.dmg",
      NEXT_PUBLIC_DOWNLOAD_MAC_INTEL_URL: "https://downloads.heed.test/Heed_x64.dmg",
    },
  }).finally(() => api.close());
}

/** Seeds a Supabase session and mocks the subscriptions and devices tables for that user. */
export async function signedIn(ctx) {
  const session = {
    access_token: "test-access-token",
    refresh_token: "test-refresh",
    token_type: "bearer",
    expires_in: 3600,
    expires_at: Math.floor(Date.now() / 1000) + 3600,
    user: { id: "00000000-0000-0000-0000-000000000001", email: "ada@example.com", aud: "authenticated", app_metadata: {}, user_metadata: {}, created_at: "2026-11-01T00:00:00Z" },
  };
  await ctx.addInitScript((s) => localStorage.setItem("sb-test-ref-auth-token", s), JSON.stringify(session));
  await ctx.route(`${SB}/rest/v1/subscriptions*`, (route) =>
    route.fulfill({ json: [{ plan: "heed_max", interval: "year", status: "trialing", trial_end: "2026-11-16T00:00:00Z", current_period_end: "2026-11-16T00:00:00Z", cancel_at_period_end: false }] }),
  );
  await ctx.route(`${SB}/rest/v1/devices*`, (route) =>
    route.request().method() === "DELETE"
      ? route.fulfill({ status: 204 })
      : route.fulfill({ json: [{ device_id: "a1b2c3d4e5f6", first_seen_at: "2026-11-01T00:00:00Z", last_seen_at: "2026-11-02T00:00:00Z" }] }),
  );
  await ctx.route(`${SB}/auth/v1/logout*`, (route) => route.fulfill({ status: 204 }));
}
