import { useCallback, useEffect, useRef, useState } from "react";

/** One page from a cursor-paged daemon method (LL1): its rows and the cursor of the next page. */
export interface CursorPageResult<T> {
  items: T[];
  /** `null` at the end. */
  next: string | null;
}

/** A list fetched a page at a time by an opaque cursor (`egress.page`, `search.page`). */
export interface CursorList<T> {
  /** Every row loaded so far (undefined before the first page; the previous key's rows while a new key loads). */
  items: T[] | undefined;
  /** The daemon has more rows past `items`. */
  hasMore: boolean;
  /** The first page (of a new key, or after `reload`) is in flight, or a next page is. */
  loading: boolean;
  /** A page after the first is in flight. */
  loadingMore: boolean;
  error: unknown;
  /** Fetch the next page (no-op while one is in flight, after an error, or at the end). */
  loadMore: () => void;
  /** Start over from the first page. */
  reload: () => void;
  /** After an error: ask for the page that failed again (the first page if nothing is loaded). */
  retry: () => void;
}

/**
 * Pages through `fetchPage(cursor)`, `null` asking for the first page; the daemon says whether
 * more exist with the next cursor. Changing `key` starts over; replies to a superseded key or
 * request are dropped. Pages are merged by `itemKey` when given, so a ranking that shifted
 * between two pages never lists a row twice. The `usePagedList` counterpart for cursors.
 */
export function useCursorList<T>(
  fetchPage: (cursor: string | null) => Promise<CursorPageResult<T>>,
  key: string,
  { itemKey }: { itemKey?: (item: T) => string } = {},
): CursorList<T> {
  type State = { key: string; items: T[] | undefined; next: string | null; error: unknown };
  const [state, setState] = useState<State>({ key, items: undefined, next: null, error: null });
  const [inFlight, setInFlight] = useState<null | "first" | "more">("first");
  const fetchRef = useRef(fetchPage);
  fetchRef.current = fetchPage;
  const keyRef = useRef(itemKey);
  keyRef.current = itemKey;
  /** Bumped by every request; a reply to an older one is dropped. */
  const seq = useRef(0);
  /** The cursor last asked for: each one is asked once (until a reload). */
  const asked = useRef<string | null>(null);
  const [tick, setTick] = useState(0);
  const stale = state.key !== key;

  const run = useCallback(
    (cursor: string | null) => {
      const id = ++seq.current;
      const kind = cursor === null ? "first" : "more";
      setInFlight(kind);
      fetchRef
        .current(cursor)
        .then((page) => {
          if (id !== seq.current) return;
          setState((s) => {
            const prev = kind === "more" && s.key === key ? (s.items ?? []) : [];
            return { key, items: merge(prev, page.items, keyRef.current), next: page.next || null, error: null };
          });
        })
        .catch((error: unknown) => {
          if (id !== seq.current) return;
          setState((s) => (s.key === key ? { ...s, error } : { key, items: undefined, next: null, error }));
        })
        .finally(() => {
          if (id === seq.current) setInFlight(null);
        });
    },
    [key],
  );

  useEffect(() => {
    asked.current = null;
    run(null);
    return () => {
      seq.current++;
    };
  }, [run, tick]);

  const loadMore = useCallback(() => {
    if (stale || inFlight || state.error != null || !state.next) return;
    if (asked.current === state.next) return;
    asked.current = state.next;
    run(state.next);
  }, [stale, inFlight, state.error, state.next, run]);

  const reload = useCallback(() => setTick((n) => n + 1), []);

  const retry = useCallback(() => {
    if (stale || !state.items || !state.next) {
      reload();
      return;
    }
    asked.current = state.next;
    run(state.next);
  }, [stale, state.items, state.next, run, reload]);

  return {
    items: state.items,
    hasMore: !stale && !!state.next,
    loading: inFlight !== null || stale,
    loadingMore: !stale && inFlight === "more",
    error: state.error,
    loadMore,
    reload,
    retry,
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
