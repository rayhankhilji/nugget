/** The plans as sold. Ids and intervals match crates/nugget-cloud/plans.toml and its /v1/billing API. */
export type PlanId = "heed" | "heed_max";
export type Interval = "month" | "year";

export interface Plan {
  id: PlanId;
  name: string;
  monthly: number;
  annual: number;
  pitch: string;
  features: string[];
  highlight?: boolean;
}

export const TRIAL_DAYS = 14;

export const PLANS: Plan[] = [
  {
    id: "heed",
    name: "Heed",
    monthly: 39,
    annual: 390,
    pitch: "Everything you need to stay on top of your work.",
    features: [
      "Fast AI for everything, plus 300 deep answers a month",
      "10 connected apps",
      "Sync every 30 minutes",
      "12 months of history",
      "Ghost text, Needs you and the daily brief",
      "Actions with your approval",
      "1 device",
    ],
  },
  {
    id: "heed_max",
    name: "Heed Max",
    monthly: 99,
    annual: 990,
    pitch: "For people whose job is their inbox, calendar and team.",
    highlight: true,
    features: [
      "The smartest AI by default",
      "Unlimited apps",
      "Sync every 5 minutes",
      "Full history",
      "Agent tasks and computer use",
      "3 devices",
      "Priority support",
    ],
  },
];

export const planName = (id: string | null | undefined): string =>
  PLANS.find((p) => p.id === id)?.name ?? "No plan";

export const isPlanId = (v: string | null): v is PlanId => v === "heed" || v === "heed_max";
export const isInterval = (v: string | null): v is Interval => v === "month" || v === "year";
