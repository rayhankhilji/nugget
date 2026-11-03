import { useSyncExternalStore } from "react";

/**
 * "Sync now" requests in flight. The cloud imports while the request runs, but nothing announces
 * it, so the sidebar's sync pill and Connections watch the connections list while one is open.
 */
let inFlight = 0;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

/** Runs `sync`, marking a sync as requested until it settles. */
export async function trackSync<T>(sync: () => Promise<T>): Promise<T> {
  inFlight++;
  emit();
  try {
    return await sync();
  } finally {
    inFlight--;
    emit();
  }
}

/** Whether a "Sync now" request is in flight. */
export function useSyncRequested(): boolean {
  return useSyncExternalStore(
    (l) => (listeners.add(l), () => listeners.delete(l)),
    () => inFlight > 0,
    () => inFlight > 0,
  );
}
