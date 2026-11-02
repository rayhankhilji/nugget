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
  if (!config.cloudUrl) throw new CloudError(0, "not_configured", "The Heed cloud URL isn't configured for this site.");
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
    throw new CloudError(0, "network", "We couldn't reach Heed. Check your connection and try again.");
  }
  if (res.status === 204) return undefined as T;
  const data = (await res.json().catch(() => ({}))) as { error?: string; message?: string };
  if (!res.ok) {
    throw new CloudError(res.status, data.error ?? "error", data.message ?? `Something went wrong (${res.status}).`);
  }
  return data as T;
}

export const billingCheckout = (token: string, plan: string, interval: string) =>
  cloud<{ url: string }>("/v1/billing/checkout", token, { method: "POST", body: { plan, interval } });

export const billingPortal = (token: string) => cloud<{ url: string }>("/v1/billing/portal", token, { method: "POST" });

export const deleteAccount = (token: string) => cloud<void>("/v1/me", token, { method: "DELETE" });
