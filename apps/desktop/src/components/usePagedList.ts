import { useCallback, useEffect, useRef, useState } from "react";

/** A list fetched a page at a time by `offset`/`limit` (II5). */
export interface PagedList<T> {
  /** Every row loaded so far (undefined before the first page; the previous key's rows while a new key loads). */
  items: T[] | undefined;
  /** The daemon has more rows past `items`. */
  hasMore: boolean;
  /** Any page is in flight. */
  loading: boolean;
  /** A page after the first is in flight. */
  loadingMore: boolean;
  error: unknown;
  /** Fetch the next page (no-op while one is in flight, after an error, or at the end). */
  loadMore: () => void;
  /** Start over from the first page. */
  reload: () => void;
  /**
   * Re-fetch what is loaded in one request (up to `maxRefresh` rows), keeping the scroll depth;
   * deeper lists are left as they are rather than refetched.
   */
  refresh: () => void;
}

/**
 * Pages through `fetchPage(offset, limit)`. Each request asks for one row more than a page to
 * learn whether more exist. Changing `key` (a filter) starts over; results of a superseded key or
 * request are dropped.
 */
export function usePagedList<T>(
  fetchPage: (offset: number, limit: number) => Promise<T[]>,
  key: string,
  { pageSize = 50, maxRefresh = 200 }: { pageSize?: number; maxRefresh?: number } = {},
): PagedList<T> {
  const [state, setState] = useState<{ key: string; items: T[] | undefined; hasMore: boolean; error: unknown }>({
    key,
    items: undefined,
    hasMore: false,
    error: null,
  });
  const [inFlight, setInFlight] = useState<null | "first" | "more">("first");
  const fetchRef = useRef(fetchPage);
  fetchRef.current = fetchPage;
  /** Bumped by every request; a reply to an older one is dropped. */
  const seq = useRef(0);
  const [tick, setTick] = useState(0);
  // Until the first page for a new key arrives, the old rows stay (marked loading), like `useAsync`.
  const stale = state.key !== key;

  const run = useCallback(
    (offset: number, limit: number, merge: (prev: T[] | undefined, page: T[]) => T[], kind: "first" | "more") => {
      const id = ++seq.current;
      setInFlight(kind);
      fetchRef
        .current(offset, limit + 1)
        .then((rows) => {
          if (id !== seq.current) return;
          const more = rows.length > limit;
          const page = more ? rows.slice(0, limit) : rows;
          setState((s) => ({ key, items: merge(s.key === key ? s.items : undefined, page), hasMore: more, error: null }));
        })
        .catch((error: unknown) => {
          if (id !== seq.current) return;
          // The rows shown stay; only another request (`reload`) tries again.
          setState((s) => ({ key, items: s.items, hasMore: s.key === key && s.hasMore, error }));
        })
        .finally(() => {
          if (id === seq.current) setInFlight(null);
        });
    },
    [key],
  );

  // The first page, again whenever the key changes or `reload` is called.
  useEffect(() => {
    run(0, pageSize, (_, page) => page, "first");
    return () => {
      // A reply for a key that has gone is dropped.
      seq.current++;
    };
  }, [run, pageSize, tick]);

  const loaded = state.items?.length ?? 0;
  const loadMore = useCallback(() => {
    if (stale || inFlight || state.error != null || !state.hasMore) return;
    run(loaded, pageSize, (prev, page) => [...(prev ?? []), ...page], "more");
  }, [stale, inFlight, state.error, state.hasMore, run, loaded, pageSize]);

  const reload = useCallback(() => setTick((n) => n + 1), []);

  const refresh = useCallback(() => {
    if (stale || inFlight || loaded > maxRefresh) return;
    run(0, Math.max(pageSize, loaded), (_, page) => page, "first");
  }, [stale, inFlight, loaded, maxRefresh, pageSize, run]);

  return {
    items: state.items,
    hasMore: !stale && state.hasMore,
    loading: inFlight !== null || stale,
    loadingMore: !stale && inFlight === "more",
    error: state.error,
    loadMore,
    reload,
    refresh,
  };
}
