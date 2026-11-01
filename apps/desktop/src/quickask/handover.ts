/**
 * The main window's side of quick ask's "Open in Chat" / "Show in Search". The shell keeps
 * the handover (`quickask_open_main`) and tells this window (`quickask:handover`); the app takes
 * it once and passes it to the page: Search runs the question, Chat continues the conversation.
 */
import { useEffect } from "react";
import { quickAskApi, type Citation, type QuickAskHandover } from "../api";
import { requestChatHandover, requestSearch } from "../hooks.nav";

/** How long a handover may wait for its page (e.g. behind onboarding) before it is dropped. */
export const HANDOVER_TTL_MS = 60_000;

/** Pass a handover to its page (the shell has already navigated there). */
export function deliverHandover(h: QuickAskHandover | null | undefined, now: number = Date.now()): void {
  if (!h || typeof h.question !== "string" || !h.question.trim()) return;
  if (h.page === "search") requestSearch(h.question);
  else if (h.page === "chat") requestChatHandover({ ...h, received: now });
}

/** A handover that waited too long for Chat: opening Chat hours later must not replace a conversation. */
export function handoverExpired(h: QuickAskHandover, now: number = Date.now()): boolean {
  return typeof h.received === "number" && now - h.received > HANDOVER_TTL_MS;
}

/** Take a waiting handover now (one may have been left before this window loaded) and on each event. */
export function useQuickAskHandover(): void {
  useEffect(() => {
    let alive = true;
    const take = () => {
      quickAskApi
        .takeHandover()
        .then((h) => alive && deliverHandover(h))
        .catch(() => {});
    };
    take();
    const unlisten = quickAskApi.onHandover(take);
    return () => {
      alive = false;
      void unlisten.then((u) => u()).catch(() => {});
    };
  }, []);
}

/** Citations as the Chat page can show them: anything malformed is dropped. */
export function handoverCitations(v: unknown): Citation[] {
  if (!Array.isArray(v)) return [];
  return v.filter((c): c is Citation => {
    const x = c as Partial<Citation> | null;
    return (
      !!x &&
      typeof x === "object" &&
      typeof x.n === "number" &&
      Number.isInteger(x.n) &&
      x.n > 0 &&
      !!x.item &&
      typeof x.item.source === "string" &&
      typeof x.item.key === "string"
    );
  });
}
