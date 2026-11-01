import { useCallback, useEffect, useRef, useState } from "react";

/** A list fetched a page at a time by `offset`/`limit`. */
export interface PagedList<T> {
  /** Every row loaded so far (undefined before the first page; the previous key's rows while a new key loads). */
  items: T[] | undefined;
  /** The daemon has more rows past `items`. */
  hasMore: boolean;
  /** The first page (of a new key, or after `reload`) or a next page is in flight. */
  loading: boolean;
  /** A background `refresh` is in flight (the rows shown stay current enough: not busy). */
  refreshing: boolean;
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
 *
 * Offset paging over a list that changes underneath (entities appearing during extraction) can
 * return a row twice: pages are merged by `itemKey`, so a row is never listed twice. The next
 * offset follows the rows the daemon returned, not the rows kept, so a page of duplicates doesn't
 * stall the list.
 */
export function usePagedList<T>(
  fetchPage: (offset: number, limit: number) => Promise<T[]>,
  key: string,
  {
    pageSize = 50,
    maxRefresh = 200,
    itemKey,
  }: { pageSize?: number; maxRefresh?: number; itemKey?: (item: T) => string } = {},
): PagedList<T> {
  type State = { key: string; items: T[] | undefined; next: number; hasMore: boolean; error: unknown };
  const [state, setState] = useState<State>({ key, items: undefined, next: 0, hasMore: false, error: null });
  const [inFlight, setInFlight] = useState<null | "first" | "more" | "refresh">("first");
  const fetchRef = useRef(fetchPage);
  fetchRef.current = fetchPage;
  const keyRef = useRef(itemKey);
  keyRef.current = itemKey;
  /** Bumped by every request; a reply to an older one is dropped. */
  const seq = useRef(0);
  /** The offset of the last "more" request (with the `seq` that loaded the first page): asked once. */
  const generation = useRef(0);
  const askedMore = useRef("");
  const [tick, setTick] = useState(0);
  // Until the first page for a new key arrives, the old rows stay (marked loading), like `useAsync`.
  const stale = state.key !== key;

  const run = useCallback(
    (offset: number, limit: number, kind: "first" | "more" | "refresh") => {
      const id = ++seq.current;
      if (kind !== "more") generation.current = id;
      setInFlight(kind);
      fetchRef
        .current(offset, limit + 1)
        .then((rows) => {
          if (id !== seq.current) return;
          const more = rows.length > limit;
          const page = more ? rows.slice(0, limit) : rows;
          setState((s) => {
            const prev = kind === "more" && s.key === key ? (s.items ?? []) : [];
            return { key, items: merge(prev, page, keyRef.current), next: offset + page.length, hasMore: more, error: null };
          });
        })
        .catch((error: unknown) => {
          if (id !== seq.current) return;
          // The rows of this key stay (none of another key's); only `reload` tries again.
          setState((s) =>
            s.key === key ? { ...s, error } : { key, items: undefined, next: 0, hasMore: false, error },
          );
        })
        .finally(() => {
          if (id === seq.current) setInFlight(null);
        });
    },
    [key],
  );

  // The first page, again whenever the key changes or `reload` is called.
  useEffect(() => {
    run(0, pageSize, "first");
    return () => {
      // A reply for a key that has gone is dropped.
      seq.current++;
    };
  }, [run, pageSize, tick]);

  const loaded = state.items?.length ?? 0;
  const loadMore = useCallback(() => {
    if (stale || inFlight || state.error != null || !state.hasMore) return;
    const mark = `${generation.current}:${state.next}`;
    if (askedMore.current === mark) return;
    askedMore.current = mark;
    run(state.next, pageSize, "more");
  }, [stale, inFlight, state.error, state.hasMore, state.next, run, pageSize]);

  const reload = useCallback(() => setTick((n) => n + 1), []);

  const refresh = useCallback(() => {
    if (stale || inFlight || loaded > maxRefresh) return;
    run(0, Math.max(pageSize, loaded), "refresh");
  }, [stale, inFlight, loaded, maxRefresh, pageSize, run]);

  return {
    items: state.items,
    hasMore: !stale && state.hasMore,
    loading: inFlight === "first" || inFlight === "more" || stale,
    loadingMore: !stale && inFlight === "more",
    refreshing: inFlight === "refresh",
    error: state.error,
    loadMore,
    reload,
    refresh,
  };
}

/** `prev` then the rows of `page` not already in it (by `keyOf`; all of them without one). */
function merge<T>(prev: T[], page: T[], keyOf: ((item: T) => string) | undefined): T[] {
  if (!keyOf) return prev.length ? [...prev, ...page] : page;
  const seen = new Set(prev.map(keyOf));
  const out = prev.length ? [...prev] : [];
  for (const item of page) {
    const k = keyOf(item);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(item);
  }
  return out;
}
