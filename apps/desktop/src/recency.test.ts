import { describe, expect, it } from "vitest";
import type { ClaimView } from "./api";
import { formatRelative } from "./format";
import { claimTime, groupClaimsByRecency, hasClaimTimes, recencyBucket, sourceKindOf } from "./recency";

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

  it("treats missing or invalid timestamps as undated", () => {
    expect(recencyBucket(null, now)).toBe("undated");
    expect(recencyBucket(undefined, now)).toBe("undated");
    expect(recencyBucket("", now)).toBe("undated");
    expect(recencyBucket("not a date", now)).toBe("undated");
  });
});

describe("claimTime", () => {
  it("takes the latest of evidence and update times", () => {
    const c = claim(1, { updated_at: at(2026, 10, 1), evidence: [ev(at(2026, 10, 20)), ev(at(2026, 9, 1)), ev(null)] });
    expect(claimTime(c)).toBe(at(2026, 10, 20));
    expect(claimTime(claim(2, { updated_at: at(2026, 10, 25), evidence: [ev(at(2026, 10, 20))] }))).toBe(at(2026, 10, 25));
  });

  it("is null without usable timestamps", () => {
    expect(claimTime(claim(1))).toBeNull();
    expect(claimTime(claim(1, { updated_at: "garbage", evidence: [ev(null)] }))).toBeNull();
    expect(hasClaimTimes([claim(1), claim(2)])).toBe(false);
    expect(hasClaimTimes([claim(1), claim(2, { updated_at: at(2026, 10, 1) })])).toBe(true);
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

  it("gives nothing for no facts", () => {
    expect(groupClaimsByRecency([], now)).toEqual([]);
  });
});

describe("relative dates for facts", () => {
  it("says how long ago, in the given locale", () => {
    const threeDays = new Date(now - 3 * 86_400_000).toISOString();
    expect(formatRelative(threeDays, now, "en")).toBe("3 days ago");
    expect(formatRelative(threeDays, now, "de")).toBe("vor 3 Tagen");
    expect(formatRelative(threeDays, now, "fr")).toBe("il y a 3 jours");
  });
});

describe("sourceKindOf", () => {
  it("uses the sources list, else guesses from the id", () => {
    const kinds = new Map([["work-mail", "imap"]]);
    expect(sourceKindOf("work-mail", kinds)).toBe("imap");
    expect(sourceKindOf("gmail-1", kinds)).toBe("gmail");
    expect(sourceKindOf("files_12")).toBe("files");
    expect(sourceKindOf("custom")).toBe("custom");
  });
});
