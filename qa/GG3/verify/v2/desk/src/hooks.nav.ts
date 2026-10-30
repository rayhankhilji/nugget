import { useEffect, useRef } from "react";
import type { PageId } from "./api";

/**
 * In-app requests from the shell (command palette, shortcuts) to a page, e.g. "search for this".
 * A request made before the page mounts is kept until the page picks it up. The caller navigates
 * to the page itself.
 */
function channel<T>() {
  let pending: { value: T } | null = null;
  const listeners = new Set<(v: T) => void>();

  const request = (v: T): void => {
    if (listeners.size === 0) {
      pending = { value: v };
      return;
    }
    for (const l of listeners) l(v);
  };

  const usePending = (on: (v: T) => void): void => {
    const ref = useRef(on);
    ref.current = on;
    useEffect(() => {
      const l = (v: T) => ref.current(v);
      listeners.add(l);
      if (pending !== null) {
        const { value } = pending;
        pending = null;
        l(value);
      }
      return () => {
        listeners.delete(l);
      };
    }, []);
  };

  return { request, usePending };
}

const search = channel<string>();
const newChat = channel<void>();

/** Ask the Search page to run `q`. The caller navigates to the page itself. */
export function requestSearch(q: string): void {
  search.request(q);
}

/** Run `onQuery` for a search requested before mount and for every later request. */
export function usePendingSearch(onQuery: (q: string) => void): void {
  search.usePending(onQuery);
}

/** Ask the Chat page to start a fresh conversation. The caller navigates to the page itself. */
export function requestNewChat(): void {
  newChat.request();
}

/** Run `onNewChat` for a new chat requested before mount and for every later request. */
export function usePendingNewChat(onNewChat: () => void): void {
  newChat.usePending(onNewChat);
}

const page = channel<PageId>();

/** Ask the app to open page `id` (e.g. an empty state's "Add a source" pointing at Sources). */
export function requestPage(id: PageId): void {
  page.request(id);
}

/** Run `onPage` for every page request (the app shell listens; pages call `requestPage`). */
export function usePendingPage(onPage: (id: PageId) => void): void {
  page.usePending(onPage);
}
