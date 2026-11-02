// Shared test fixtures: a build with test settings, and a signed-in browser context with Supabase mocked.
import { execSync } from "node:child_process";

export const OUT = "dist-test";
export const SB = "https://test-ref.supabase.co";
export const CLOUD = "https://cloud.heed.test";

export function buildTestSite() {
  process.env.ASTRO_OUT_DIR = OUT;
  execSync("npx astro build", {
    stdio: "ignore",
    env: {
      ...process.env,
      ASTRO_OUT_DIR: OUT,
      NEXT_PUBLIC_SUPABASE_URL: SB,
      NEXT_PUBLIC_SUPABASE_ANON_KEY: "test-anon-key",
      NEXT_PUBLIC_CLOUD_URL: CLOUD,
      NEXT_PUBLIC_DOWNLOAD_MAC_ARM_URL: "https://downloads.heed.test/Heed_aarch64.dmg",
      NEXT_PUBLIC_DOWNLOAD_MAC_INTEL_URL: "https://downloads.heed.test/Heed_x64.dmg",
    },
  });
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
