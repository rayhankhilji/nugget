import { useCallback, useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, errorMessage } from "./api";
import { t } from "./i18n";

/**
 * Subscribe to a Tauri event for the component's lifetime. `handler` may change between
 * renders without re-subscribing.
 */
export function useEvent<T>(subscribe: (h: (payload: T) => void) => Promise<UnlistenFn>, handler: (payload: T) => void): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    subscribe((p) => ref.current(p))
      .then((u) => {
        if (cancelled) u();
        else unlisten = u;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [subscribe]);
}

export interface AsyncState<T> {
  data: T | undefined;
  error: unknown;
  loading: boolean;
  reload: () => void;
}

/** Run `load` on mount (and on `reload()`); keeps the last good data while reloading. */
export function useAsync<T>(load: () => Promise<T>, deps: readonly unknown[] = []): AsyncState<T> {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    load()
      .then((d) => {
        if (!alive) return;
        setData(d);
        setError(null);
      })
      .catch((e: unknown) => alive && setError(e))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [tick, ...deps]);

  return { data, error, loading, reload };
}

/** Current time in ms, refreshed every `intervalMs` (for relative timestamps). */
export function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(t);
  }, [intervalMs]);
  return now;
}

/** `value`, updated only after it has stopped changing for `ms`. */
export function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setV(value), ms);
    return () => window.clearTimeout(t);
  }, [value, ms]);
  return v;
}

/** `open_url`, reporting the outcome for the page's live region. */
export function useOpener() {
  const [status, setStatus] = useState<{ text: string; error: boolean } | null>(null);
  const open = async (url: string, label: string) => {
    setStatus(null);
    try {
      const r = await api.openUrl(url);
      setStatus({
        text: r === "revealed" ? t("hooks.open.revealed", { label }) : t("hooks.open.opened", { label }),
        error: false,
      });
    } catch (e) {
      setStatus({ text: t("hooks.open.failed", { label, error: errorMessage(e) }), error: true });
    }
  };
  return { status, open };
}
