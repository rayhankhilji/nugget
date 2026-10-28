import { useEffect, useRef } from "react";

/**
 * In-app requests from the shell (command palette, shortcuts) to a page, e.g. "search for this".
 * A request made before the page mounts is kept until the page picks it up.
 */
let pendingSearch: string | null = null;
const searchListeners = new Set<(q: string) => void>();

/** Ask the Search page to run `q`. The caller navigates to the page itself. */
export function requestSearch(q: string): void {
  if (searchListeners.size === 0) {
    pendingSearch = q;
    return;
  }
  for (const l of searchListeners) l(q);
}

/** Run `onQuery` for a search requested before mount and for every later request. */
export function usePendingSearch(onQuery: (q: string) => void): void {
  const ref = useRef(onQuery);
  ref.current = onQuery;
  useEffect(() => {
    const l = (q: string) => ref.current(q);
    searchListeners.add(l);
    if (pendingSearch !== null) {
      const q = pendingSearch;
      pendingSearch = null;
      l(q);
    }
    return () => {
      searchListeners.delete(l);
    };
  }, []);
}
