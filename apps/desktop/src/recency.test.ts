import { describe, expect, it } from "vitest";
import type { ClaimView } from "./api";
import { groupClaimsByRecency, recencyBucket } from "./recency";

// Local times, so the calendar buckets hold in any time zone.
const now = new Date(2026, 9, 28, 15, 0, 0).getTime();
const at = (y: number, m: number, d: number, h = 12) => new Date(y, m - 1, d, h).toISOString();

function claim(id: number, over: Partial<ClaimView> = {}): ClaimView {
  return {
    id,
    subject: { id: "e1", name: "Ada" },
    predicate: "works_on",
    object: { literal: `thing ${id}` },
    text: `fact ${id}`,
    confidence: 0.9,
    status: "active",
    evidence: [],
    ...over,
  };
}
const ev = (observed_at: string | null) => ({ item: { source: "files-1", key: "k" }, observed_at });

describe("recencyBucket", () => {
  it("buckets by the local calendar", () => {
    expect(recencyBucket(at(2026, 10, 28, 0), now)).toBe("today");
    expect(recencyBucket(at(2026, 10, 28, 23), now)).toBe("today"); // clock skew: later today
    expect(recencyBucket(at(2026, 10, 29), now)).toBe("today"); // future
    expect(recencyBucket(at(2026, 10, 27, 23), now)).toBe("week");
    expect(recencyBucket(at(2026, 10, 22, 0), now)).toBe("week");
    expect(recencyBucket(at(2026, 10, 21, 23), now)).toBe("month");
    expect(recencyBucket(at(2026, 9, 29, 0), now)).toBe("month");
    expect(recencyBucket(at(2026, 9, 28, 23), now)).toBe("earlier");
    expect(recencyBucket(at(2020, 1, 1), now)).toBe("earlier");
  });
});

describe("groupClaimsByRecency", () => {
  it("orders groups newest first, drops empty ones and sorts within a group", () => {
    const claims = [
      claim(1, { updated_at: at(2020, 5, 1) }),
      claim(2, { evidence: [ev(at(2026, 10, 28, 9))] }),
      claim(3),
      claim(4, { updated_at: at(2026, 10, 26) }),
      claim(5, { updated_at: at(2026, 10, 28, 11) }),
      claim(6, { updated_at: at(2026, 10, 28, 11), status: "retracted" }),
      claim(7, { updated_at: at(2026, 10, 28, 11), confidence: 0.95 }),
    ];
    const groups = groupClaimsByRecency(claims, now);
    expect(groups.map((g) => g.bucket)).toEqual(["today", "week", "earlier", "undated"]);
    expect(groups.map((g) => g.label)).toEqual([
      "entity.recency.today",
      "entity.recency.week",
      "entity.recency.earlier",
      "entity.recency.undated",
    ]);
    // Newest first; ties: current before rejected, then higher confidence.
    expect(groups[0]!.claims.map((c) => c.id)).toEqual([7, 5, 6, 2]);
    expect(groups[1]!.claims.map((c) => c.id)).toEqual([4]);
    expect(groups[2]!.claims.map((c) => c.id)).toEqual([1]);
    expect(groups[3]!.claims.map((c) => c.id)).toEqual([3]);
  });
});
