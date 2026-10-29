import { formatRelative, formatTime } from "../format";
import { useT } from "../i18n";

/** "3 days ago" in the current locale, with the absolute date as the tooltip. */
export function RelativeTime({ ts, now, className }: { ts: string; now?: number; className?: string }) {
  useT(); // re-render when the locale changes
  return (
    <time dateTime={ts} title={formatTime(ts)} className={className}>
      {formatRelative(ts, now)}
    </time>
  );
}
