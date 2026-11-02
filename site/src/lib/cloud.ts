import { config } from "./config";

export class CloudError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

/** Calls the Heed cloud API (crates/nugget-cloud) as the signed-in user. */
export async function cloud<T>(path: string, token: string, init: { method: string; body?: unknown }): Promise<T> {
  if (!config.cloudUrl) throw new CloudError(0, "not_configured", "The Heed cloud URL isn’t configured for this site.");
  let res: Response;
  try {
    res = await fetch(`${config.cloudUrl}${path}`, {
      method: init.method,
      headers: {
        Authorization: `Bearer ${token}`,
        ...(init.body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      ...(init.body === undefined ? {} : { body: JSON.stringify(init.body) }),
    });
  } catch {
    throw new CloudError(0, "network", "We couldn’t reach Heed. Check your connection and try again.");
  }
  if (res.status === 204) return undefined as T;
  const data = (await res.json().catch(() => ({}))) as { error?: string; message?: string };
  if (!res.ok) {
    throw new CloudError(res.status, data.error ?? "error", data.message ?? `Something went wrong (${res.status}).`);
  }
  return data as T;
}

/**
 * Where Stripe sends the browser back to: this site's account page. The cloud only accepts origins
 * listed in its NUGGET_SITE_ORIGINS. If this origin isn't listed (a preview deploy, say), it answers
 * 400 bad_return_url, and we retry without return_url, so Stripe falls back to the app hand-off page.
 */
const returnUrl = () => `${location.origin}/account?billing=done`;

async function withReturnUrl<T>(call: (body: Record<string, unknown>) => Promise<T>, body: Record<string, unknown>): Promise<T> {
  try {
    return await call({ ...body, return_url: returnUrl() });
  } catch (e) {
    if (e instanceof CloudError && e.code === "bad_return_url") return call(body);
    throw e;
  }
}

export const billingCheckout = (token: string, plan: string, interval: string) =>
  withReturnUrl((body) => cloud<{ url: string }>("/v1/billing/checkout", token, { method: "POST", body }), { plan, interval });

export const billingPortal = (token: string) =>
  withReturnUrl((body) => cloud<{ url: string }>("/v1/billing/portal", token, { method: "POST", body }), {});

export const deleteAccount = (token: string) => cloud<void>("/v1/me", token, { method: "DELETE" });

export interface Me {
  plan: string | null;
  entitlements: { smart_asks_per_month: number; default_model: string } | null;
  usage: { smart_asks_used: number; month_cost_micros: number };
}
/** The account, plan and this month's usage. Web calls carry no device header, so they take no device seat. */
export const getMe = (token: string) => cloud<Me>("/v1/me", token, { method: "GET" });

/** Everything Heed stores for the user, as JSON (GET /v1/export; rate-limited by the cloud). */
export const exportData = (token: string) => cloud<unknown>("/v1/export", token, { method: "GET" });
