const UNITS = ["B", "KB", "MB", "GB", "TB"];

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "–";
  let i = 0;
  let v = n;
  while (v >= 1000 && i < UNITS.length - 1) {
    v /= 1000;
    i++;
  }
  return `${i === 0 ? v : v.toFixed(v < 10 ? 1 : 0)} ${UNITS[i]}`;
}

export function formatDuration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s % 60}s`;
  return `${s}s`;
}

const dateFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });

export function formatTime(rfc3339: string): string {
  const d = new Date(rfc3339);
  return Number.isNaN(d.getTime()) ? rfc3339 : dateFmt.format(d);
}

export function percent(done: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(100, Math.max(0, Math.round((done / total) * 100)));
}

export const LOCALITY_LABEL = {
  on_device: "On device",
  local_network: "Local network",
  cloud: "Cloud",
} as const;

export const TIER_LABEL = {
  triage: "Triage",
  light: "Light",
  synthesis: "Synthesis",
  agentic: "Agentic",
  embedding: "Embedding",
} as const;

const relFmts = new Map<string, Intl.RelativeTimeFormat>();

function relFmt(locale: string | undefined): Intl.RelativeTimeFormat {
  const key = locale ?? "";
  let f = relFmts.get(key);
  if (!f) {
    f = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
    relFmts.set(key, f);
  }
  return f;
}

const REL_UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 365 * 86_400],
  ["month", 30 * 86_400],
  ["week", 7 * 86_400],
  ["day", 86_400],
  ["hour", 3_600],
  ["minute", 60],
];

/**
 * "just now", "5 minutes ago", "yesterday", "in 2 hours". `null`/invalid input gives "never" /
 * the raw string. `now` and `locale` are injectable for tests.
 */
export function formatRelative(
  rfc3339: string | null | undefined,
  now: number = Date.now(),
  locale?: string,
): string {
  if (!rfc3339) return "never";
  const t = new Date(rfc3339).getTime();
  if (Number.isNaN(t)) return rfc3339;
  const secs = Math.round((t - now) / 1000);
  const abs = Math.abs(secs);
  if (abs < 45) return "just now";
  for (const [unit, size] of REL_UNITS) {
    if (abs >= size) return relFmt(locale).format(Math.round(secs / size), unit);
  }
  // 45–59 s rounds to a minute.
  return relFmt(locale).format(Math.sign(secs), "minute");
}

/** Latency in ms as "850 ms", "4.2 s", "3m 5s"; `null` as "–". */
export function formatLatency(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "–";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s`;
  return formatDuration(ms / 1000);
}

export const SOURCE_KIND_LABEL: Record<string, string> = {
  files: "Folder",
  obsidian: "Obsidian vault",
  gmail: "Gmail",
  gcal: "Google Calendar",
  imap: "IMAP mail",
  caldav: "CalDAV calendar",
};

export const SOURCE_STATE_LABEL = {
  idle: "Idle",
  syncing: "Syncing",
  watching: "Watching",
  needs_auth: "Needs sign-in",
  disabled: "Disabled",
  error: "Error",
} as const;

export const SOURCE_STATE_TONE = {
  idle: "neutral",
  syncing: "accent",
  watching: "good",
  needs_auth: "warn",
  disabled: "neutral",
  error: "bad",
} as const;

/** Split comma- or newline-separated patterns/folders into a trimmed, de-duplicated list. */
export function splitList(text: string): string[] {
  const out: string[] = [];
  for (const part of text.split(/[\n,]/)) {
    const t = part.trim();
    if (t && !out.includes(t)) out.push(t);
  }
  return out;
}

/** Last path segment of a folder path (`/a/b/` → `b`, `C:\a\b` → `b`). */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}
