import { useSyncExternalStore } from "react";
import { cloudApi, type CloudAction } from "../../cloud";

/**
 * "Sent · Undo": an approved action is held here for a few seconds before the approval reaches
 * Heed Cloud, so Undo can still stop it. The hold lives outside any page, so leaving Needs you
 * doesn't cancel it.
 *
 * If the window goes away during the hold (`pagehide`), the held approvals are sent at once,
 * best effort: the person did approve them. If that request never arrives, the action simply
 * stays pending in the queue and nothing is sent, so the failure is the safe one. Approve
 * carries the digest of exactly what was shown and is idempotent, so a repeat can't send twice.
 */
export const HOLD_MS = 6000;

export type ApprovalResult = { ok: true; action: CloudAction } | { ok: false; error: unknown };

interface Held {
  action: CloudAction;
  timer: ReturnType<typeof setTimeout>;
  onResult: (r: ApprovalResult) => void;
}

const held = new Map<string, Held>();
let snapshot: ReadonlySet<string> = new Set();
const listeners = new Set<() => void>();
const emit = () => {
  snapshot = new Set(held.keys());
  listeners.forEach((l) => l());
};

function fire(id: string): void {
  const h = held.get(id);
  if (!h) return;
  clearTimeout(h.timer);
  held.delete(id);
  emit();
  approvalsChanged();
  cloudApi.approveAction(h.action.id, h.action.digest).then(
    (action) => h.onResult({ ok: true, action }),
    (error) => h.onResult({ ok: false, error }),
  );
}

/** Approve `a` after the hold; `onResult` hears how it went. */
export function holdApproval(a: CloudAction, onResult: (r: ApprovalResult) => void, ms = HOLD_MS): void {
  if (held.has(a.id)) return;
  held.set(a.id, { action: a, onResult, timer: setTimeout(() => fire(a.id), ms) });
  emit();
}

/** Stop a held approval; false when it already went. */
export function undoApproval(id: string): boolean {
  const h = held.get(id);
  if (!h) return false;
  clearTimeout(h.timer);
  held.delete(id);
  emit();
  return true;
}

/** Send every held approval now. */
export function flushApprovals(): void {
  for (const id of [...held.keys()]) fire(id);
}

/** How many things wait in Needs you: the cards plus the approvals not already held for Undo.
 * The page header and the sidebar badge both count with this, so they always agree. */
export function needsTotal(cards: number, pending: readonly { id: string }[], held: ReadonlySet<string>): number {
  return cards + pending.filter((a) => !held.has(a.id)).length;
}

/** Fired when the approval queue changed (approved, rejected, edited, snoozed), for counts elsewhere. */
export const APPROVALS_CHANGED = "heed:approvals-changed";
export function approvalsChanged(): void {
  if (typeof window !== "undefined") window.dispatchEvent(new Event(APPROVALS_CHANGED));
}

/** The ids being held, re-rendering when they change. */
export function useHeldApprovals(): ReadonlySet<string> {
  return useSyncExternalStore(
    (l) => (listeners.add(l), () => listeners.delete(l)),
    () => snapshot,
    () => snapshot,
  );
}

if (typeof window !== "undefined") window.addEventListener("pagehide", flushApprovals);
