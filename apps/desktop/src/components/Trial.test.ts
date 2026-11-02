import { describe, expect, it } from "vitest";
import type { CloudMe } from "../cloud";
import { trialDaysLeft } from "./Trial";

const me = (status: string, trial_end: string | null): CloudMe => ({
  user_id: "u",
  email: null,
  plan: "heed",
  embeddings: false,
  subscription: { plan: "heed", status, interval: "month", trial_end, current_period_end: trial_end, cancel_at_period_end: false },
  entitlements: null,
  usage: { month_cost_micros: 0, smart_asks_used: 0 },
});

describe("trialDaysLeft", () => {
  const now = Date.parse("2026-11-03T08:00:00Z");
  it("counts whole days left while trialing, rounding up", () => {
    expect(trialDaysLeft(me("trialing", "2026-11-12T00:00:00Z"), now)).toBe(9);
    expect(trialDaysLeft(me("trialing", "2026-11-03T20:00:00Z"), now)).toBe(1);
  });
  it("is 0 once the end has passed, and null when not trialing or unknown", () => {
    expect(trialDaysLeft(me("trialing", "2026-11-02T00:00:00Z"), now)).toBe(0);
    expect(trialDaysLeft(me("active", "2026-11-12T00:00:00Z"), now)).toBeNull();
    expect(trialDaysLeft(me("trialing", null), now)).toBeNull();
    expect(trialDaysLeft(null, now)).toBeNull();
  });
});
