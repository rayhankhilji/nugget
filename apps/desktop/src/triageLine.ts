/**
 * JJ2: what the Status page says about AI sorting (background triage). Items are searchable as
 * soon as they are indexed; until sorted they count as sensitive and stay private (never sent to
 * cloud models). Off or without a model, that is said plainly with what it means for privacy.
 */
import type { TriageProgress } from "./api";

export type TriageLineKey =
  | "status.triageOffNone"
  | "status.triageOffPending"
  | "status.triageRunning"
  | "status.triageYielding"
  | "status.triagePaused"
  | "status.triageIdle";

export interface TriageLine {
  /** Whether sorting can run (a model is routable and it is on). */
  on: boolean;
  key: TriageLineKey;
  args: { n: number };
  /** Items that could not be sorted (still private), shown as a second note. */
  failed: number;
}

export function triageLine(provider: string | null | undefined, p: TriageProgress | null | undefined): TriageLine {
  const n = Math.max(0, p?.pending ?? 0);
  const failed = Math.max(0, p?.failed ?? 0);
  const off = !provider || p?.state === "off";
  if (off) return { on: false, key: n > 0 ? "status.triageOffPending" : "status.triageOffNone", args: { n }, failed };
  const key: TriageLineKey =
    n === 0
      ? "status.triageIdle"
      : p?.state === "yielding"
        ? "status.triageYielding"
        : p?.state === "paused"
          ? "status.triagePaused"
          : "status.triageRunning";
  return { on: true, key, args: { n }, failed };
}
