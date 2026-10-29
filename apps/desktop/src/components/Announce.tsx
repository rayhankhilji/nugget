import { useCallback, useRef, useState } from "react";
import { events, type IngestProgress, type SourceInfo } from "../api";
import { useEvent } from "../hooks";
import { useT } from "../i18n";

/**
 * Screen-reader announcements that don't flood. Text that streams or progress that ticks stays
 * out of live regions (it is marked `aria-busy` while it changes); a page announces only when
 * the work starts and when it ends or fails, through one visually hidden polite status region.
 */
export function Announce({ message }: { message: string }) {
  return (
    <div className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
      {message}
    </div>
  );
}

/** The text for an `<Announce>` region and a function that announces, repeating text when asked to. */
export function useAnnouncer(): [message: string, announce: (text: string) => void] {
  const [message, setMessage] = useState("");
  // The same text twice would not change the region: alternate a trailing no-break space.
  const announce = useCallback((text: string) => setMessage((prev) => (text && prev === text ? `${text} ` : text)), []);
  return [message, announce];
}

/**
 * Announces indexing passes from `ingest.progress` events: "Indexing <source>…" when a pass
 * starts indexing, "<source> indexed, N items" when it ends, or why it failed. Scans that find
 * nothing to index (periodic resyncs) stay silent, and so does the progress in between.
 */
export function useIngestAnnouncements(
  announce: (text: string) => void,
  sourceOf: (id: string) => Pick<SourceInfo, "name" | "items"> | undefined,
): void {
  const t = useT();
  const lookup = useRef(sourceOf);
  lookup.current = sourceOf;
  /** Sources with a pass in progress: the most items seen in it, and whether its start was said. */
  const running = useRef(new Map<string, { done: number; said: boolean }>());
  /** The last failure announced per source, so retries of the same error stay quiet. */
  const failed = useRef(new Map<string, string>());

  useEvent(events.onIngestProgress, (p: IngestProgress) => {
    const s = lookup.current(p.source);
    const pass = running.current.get(p.source);
    if (p.phase === "indexing" || (p.phase === "scanning" && pass)) {
      const next = { done: Math.max(pass?.done ?? 0, p.done), said: pass?.said ?? false };
      // A source the page doesn't know yet (just added) is announced once its name is known.
      if (!next.said && s) {
        next.said = true;
        failed.current.delete(p.source);
        announce(t("announce.indexing", { name: s.name }));
      }
      running.current.set(p.source, next);
    } else if (p.phase === "idle") {
      if (pass) announce(t("announce.indexed", { name: s?.name ?? p.source, count: Math.max(s?.items ?? 0, pass.done) }));
      running.current.delete(p.source);
      failed.current.delete(p.source);
    } else if (p.phase === "error") {
      const error = p.error ?? "";
      running.current.delete(p.source);
      if (failed.current.get(p.source) === error) return;
      failed.current.set(p.source, error);
      announce(t("announce.indexFailed", { name: s?.name ?? p.source, error }));
    }
  });
}
