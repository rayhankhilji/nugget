/** Facts by recency: when a fact was last seen, and the "Today / This week / ..." groups. */
import type { ClaimStatus, ClaimView, EvidenceView } from "./api";
import type { MessageKey } from "./i18n";

export type RecencyBucket = "today" | "week" | "month" | "earlier" | "undated";

export const RECENCY_BUCKETS: readonly RecencyBucket[] = ["today", "week", "month", "earlier", "undated"];

export const RECENCY_LABEL: Record<RecencyBucket, MessageKey> = {
  today: "entity.recency.today",
  week: "entity.recency.week",
  month: "entity.recency.month",
  earlier: "entity.recency.earlier",
  undated: "entity.recency.undated",
};

/** Milliseconds since the epoch, or `null` for a missing or unparseable timestamp. */
export function parseTime(ts: string | null | undefined): number | null {
  if (!ts) return null;
  const ms = Date.parse(ts);
  return Number.isNaN(ms) ? null : ms;
}

/** When an evidence item was observed, if known. */
export function evidenceTime(ev: EvidenceView): string | null {
  return parseTime(ev.observed_at) === null ? null : ev.observed_at!;
}

/**
 * When a fact was last seen: the latest of its evidence times and its own `updated_at`.
 * `null` when none is known (an older daemon sends no timestamps).
 */
export function claimTime(claim: ClaimView): string | null {
  let best: string | null = null;
  let bestMs = -Infinity;
  for (const ts of [claim.updated_at, ...claim.evidence.map((e) => e.observed_at)]) {
    const ms = parseTime(ts);
    if (ms !== null && ms > bestMs) {
      bestMs = ms;
      best = ts!;
    }
  }
  return best;
}

/** Local midnight `days` days before the day of `now`. */
function startOfDay(now: number, days = 0): number {
  const d = new Date(now);
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - days);
  return d.getTime();
}

/**
 * Which group a timestamp falls in, by the local calendar: today (or later, for clock skew),
 * the 6 days before, the 30 days up to today, or earlier.
 */
export function recencyBucket(ts: string | null | undefined, now: number): RecencyBucket {
  const ms = parseTime(ts);
  if (ms === null) return "undated";
  if (ms >= startOfDay(now)) return "today";
  if (ms >= startOfDay(now, 6)) return "week";
  if (ms >= startOfDay(now, 29)) return "month";
  return "earlier";
}

export interface RecencyGroup {
  bucket: RecencyBucket;
  label: MessageKey;
  claims: ClaimView[];
}

const STATUS_RANK: Record<ClaimStatus, number> = { user_asserted: 0, active: 0, superseded: 1, retracted: 2, rejected: 2 };

/** True when at least one fact has a usable timestamp, so grouping by recency means something. */
export function hasClaimTimes(claims: readonly ClaimView[]): boolean {
  return claims.some((c) => claimTime(c) !== null);
}

/**
 * Facts in recency groups (newest group first, empty groups left out). Within a group, newest
 * first, then current before superseded before rejected, then by confidence.
 */
export function groupClaimsByRecency(claims: readonly ClaimView[], now: number): RecencyGroup[] {
  const timed = claims.map((c) => ({ c, ts: claimTime(c) }));
  const rank = (s: string) => STATUS_RANK[s as ClaimStatus] ?? 1;
  return RECENCY_BUCKETS.map((bucket) => ({
    bucket,
    label: RECENCY_LABEL[bucket],
    claims: timed
      .filter((x) => recencyBucket(x.ts, now) === bucket)
      .sort(
        (a, b) =>
          (parseTime(b.ts) ?? 0) - (parseTime(a.ts) ?? 0) ||
          rank(a.c.status) - rank(b.c.status) ||
          b.c.confidence - a.c.confidence ||
          a.c.id - b.c.id,
      )
      .map((x) => x.c),
  })).filter((g) => g.claims.length > 0);
}

/**
 * A source's kind for its icon: from the sources list, else guessed from an id like `gmail-1`.
 * Unknown kinds fall back to the generic icon.
 */
export function sourceKindOf(source: string, kinds?: ReadonlyMap<string, string>): string {
  const known = kinds?.get(source);
  if (known) return known;
  return source.replace(/[-_]\d+$/, "");
}
