/**
 * Plans, prices and entitlements. The source of truth is the cloud's public
 * `GET /v1/billing/plans` (crates/nugget-cloud/plans.toml). The site reads it at build time, so
 * pages stay static and need no CORS. If the API can't be reached, it falls back to FALLBACK below,
 * which mirrors plans.toml. Redeploy the site after a price change.
 */
import { config } from "./config";

export type PlanId = "heed" | "heed_max";
export type Interval = "month" | "year";

export interface Entitlements {
  plan: string;
  name: string;
  /** null = unlimited */
  max_connections: number | null;
  sync_interval_minutes: number;
  /** null = all history */
  history_days: number | null;
  smart_asks_per_month: number;
  default_model: "fast" | "smart";
  devices: number;
  ghost_text: boolean;
  needs_you_inbox: boolean;
  daily_brief: boolean;
  actions_with_approval: boolean;
  agent_tasks: boolean;
  computer_use: boolean;
  priority_support: boolean;
}

export interface ApiPlan {
  id: string;
  name: string;
  monthly_gbp: number;
  annual_gbp: number;
  trial_days: number;
  entitlements: Entitlements;
}

export interface Plan extends ApiPlan {
  id: PlanId;
  pitch: string;
  features: string[];
  highlight: boolean;
}

const FALLBACK: ApiPlan[] = [
  {
    id: "heed",
    name: "Heed",
    monthly_gbp: 39,
    annual_gbp: 390,
    trial_days: 14,
    entitlements: {
      plan: "heed",
      name: "Heed",
      max_connections: 10,
      sync_interval_minutes: 30,
      history_days: 365,
      smart_asks_per_month: 300,
      default_model: "fast",
      devices: 1,
      ghost_text: true,
      needs_you_inbox: true,
      daily_brief: true,
      actions_with_approval: true,
      agent_tasks: false,
      computer_use: false,
      priority_support: false,
    },
  },
  {
    id: "heed_max",
    name: "Heed Max",
    monthly_gbp: 99,
    annual_gbp: 990,
    trial_days: 14,
    entitlements: {
      plan: "heed_max",
      name: "Heed Max",
      max_connections: null,
      sync_interval_minutes: 5,
      history_days: null,
      smart_asks_per_month: 900,
      default_model: "smart",
      devices: 3,
      ghost_text: true,
      needs_you_inbox: true,
      daily_brief: true,
      actions_with_approval: true,
      agent_tasks: true,
      computer_use: true,
      priority_support: true,
    },
  },
];

const PITCH: Record<PlanId, string> = {
  heed: "Heed Cloud: one-click apps, always on, AI included.",
  heed_max: "For people whose job is their inbox, calendar and team.",
};

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

export const apps = (e: Entitlements) => (e.max_connections == null ? "Unlimited apps" : `${e.max_connections} connected apps`);
export const sync = (e: Entitlements) => `Sync every ${plural(e.sync_interval_minutes, "minute")}`;
export const history = (e: Entitlements) => {
  const d = e.history_days;
  if (d == null) return "Full history";
  if (d === 365) return "12 months of history";
  if (d % 365 === 0) return `${d / 365} years of history`;
  if (d % 30 === 0) return `${plural(d / 30, "month")} of history`;
  return `${plural(d, "day")} of history`;
};
export const ai = (e: Entitlements) =>
  e.default_model === "smart" ? "The smartest AI by default" : `Fast AI for everything, plus ${e.smart_asks_per_month} deep answers a month`;
export const devices = (e: Entitlements) => plural(e.devices, "device");

/** The feature bullets on a plan card, written from its entitlements. */
export function featuresOf(e: Entitlements): string[] {
  const core = [e.ghost_text && "Ghost text", e.needs_you_inbox && "Needs you", e.daily_brief && "the daily brief"].filter(Boolean) as string[];
  const list = [ai(e), apps(e), sync(e), history(e)];
  if (core.length) list.push(core.length > 1 ? `${core.slice(0, -1).join(", ")} and ${core.at(-1)}` : core[0]!);
  if (e.actions_with_approval) list.push("Actions with your approval");
  if (e.agent_tasks && e.computer_use) list.push("Agent tasks and computer use");
  else if (e.agent_tasks) list.push("Agent tasks");
  else if (e.computer_use) list.push("Computer use");
  list.push(devices(e));
  if (e.priority_support) list.push("Priority support");
  return list;
}

const valid = (p: unknown): p is ApiPlan => {
  const x = p as ApiPlan;
  return (
    !!x &&
    (x.id === "heed" || x.id === "heed_max") &&
    typeof x.monthly_gbp === "number" &&
    typeof x.annual_gbp === "number" &&
    typeof x.trial_days === "number" &&
    typeof x.entitlements === "object" &&
    x.entitlements !== null
  );
};

let cached: { plans: Plan[]; source: "api" | "fallback" } | null = null;

/** Plans for rendering, cheapest first. Runs at build time only. */
export async function loadPlans(): Promise<{ plans: Plan[]; source: "api" | "fallback" }> {
  if (cached) return cached;
  let raw: ApiPlan[] = FALLBACK;
  let source: "api" | "fallback" = "fallback";
  if (config.cloudUrl) {
    try {
      const res = await fetch(`${config.cloudUrl}/v1/billing/plans`, { signal: AbortSignal.timeout(5000) });
      const body = (await res.json()) as { plans?: unknown[] };
      const got = (body.plans ?? []).filter(valid);
      if (res.ok && got.length === 2) {
        raw = got;
        source = "api";
      }
    } catch (e) {
      const cause = (e as { cause?: unknown }).cause;
      console.warn(`[plans] ${String(e)}${cause ? ` (${String(cause)})` : ""}`);
    }
    if (source === "fallback") console.warn(`[plans] ${config.cloudUrl}/v1/billing/plans unavailable; using the built-in plans`);
  }
  const plans = raw
    .map((p) => ({
      ...p,
      id: p.id as PlanId,
      pitch: PITCH[p.id as PlanId],
      features: featuresOf(p.entitlements),
      highlight: p.id === "heed_max",
    }))
    .sort((a, b) => a.monthly_gbp - b.monthly_gbp);
  cached = { plans, source };
  return cached;
}

export const planName = (id: string | null | undefined): string =>
  id === "heed" ? "Heed" : id === "heed_max" ? "Heed Max" : "No plan";

export const isPlanId = (v: string | null): v is PlanId => v === "heed" || v === "heed_max";
export const isInterval = (v: string | null): v is Interval => v === "month" || v === "year";

/** The open-source app: free, self-run, bring your own AI. Not sold through Stripe, so it's static here. */
export const GITHUB_URL = "https://github.com/rayhankhilji/nugget";
export const FREE_TIER = {
  id: "free",
  name: "Free",
  who: "Open source. Run it yourself, with your own AI.",
  features: [
    "The full Mac app, Apache-2.0",
    "Bring your own AI: ChatGPT sign-in, API key or a local model",
    "Connect apps with your own OAuth, IMAP or CalDAV",
    "Syncs while your Mac is awake",
    "Your data stays on your Mac",
    "Community support",
  ],
} as const;

/** Why people pay for Heed Cloud instead of running Free. */
export const CLOUD_PERKS = [
  "One-click connections, no OAuth setup",
  "Always-on sync, and a morning brief while your Mac sleeps",
  "AI included, no keys",
  "Several devices",
  "Updates and support",
];
