import { MARK_H, MARK_TILE, WORDMARK, WORDMARK_VIEWBOX } from "../brand/marks";

/**
 * The Heed mark: a soft serif "h" on an ultramarine squircle. With `pulse` a quiet ring breathes
 * around it (something needs you); reduced motion keeps it still.
 */
export function Logo({ size = 22, pulse = false }: { size?: number; pulse?: boolean }) {
  return (
    <svg className={pulse ? "mark mark-pulse" : "mark"} width={size} height={size} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
      <path d={MARK_TILE} className="mark-tile" />
      <path d={MARK_H} className="mark-line" />
      <circle cx="54" cy="10" r="7" className="mark-halo" />
    </svg>
  );
}

/** The lowercase "heed" wordmark in the current text colour. */
export function Wordmark({ height = 20 }: { height?: number }) {
  const [, , w, h] = WORDMARK_VIEWBOX.split(" ").map(Number);
  return (
    <svg className="wordmark-svg" height={height} width={(height * w!) / h!} viewBox={WORDMARK_VIEWBOX} aria-hidden="true" focusable="false">
      <path d={WORDMARK} fill="currentColor" />
    </svg>
  );
}
