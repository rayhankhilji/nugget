/**
 * The Heed mark: an attention dot above a line, on a graphite tile. With `pulse` the dot breathes
 * gently (something needs you); reduced motion keeps it still.
 */
export function Logo({ size = 22, pulse = false }: { size?: number; pulse?: boolean }) {
  return (
    <svg className={pulse ? "mark mark-pulse" : "mark"} width={size} height={size} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
      <rect width="64" height="64" rx="15" className="mark-tile" />
      <circle cx="32" cy="25" r="12" className="mark-halo" />
      <circle cx="32" cy="25" r="7.5" className="mark-dot" />
      <rect x="19" y="39" width="26" height="5.5" rx="2.75" className="mark-line" />
    </svg>
  );
}
