import { currentLocale, formatDate, t, type MessageKey } from "./i18n";
import { dateFormat, numberFormat } from "./i18n/icu";

const UNITS = ["byte", "kilobyte", "megabyte", "gigabyte", "terabyte"] as const;

/** Decimal (SI) units, named and spaced for the current locale ("1.5 GB", "1,5 GB", "1.5 GB"). */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "–";
  let i = 0;
  let v = n;
  while (v >= 1000 && i < UNITS.length - 1) {
    v /= 1000;
    i++;
  }
  const digits = i === 0 ? 0 : v < 10 ? 1 : 0;
  return numberFormat(currentLocale(), {
    style: "unit",
    unit: UNITS[i],
    unitDisplay: "short",
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  }).format(v);
}

export function formatDuration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return t("format.duration.dh", { d, h });
  if (h > 0) return t("format.duration.hm", { h, m });
  if (m > 0) return t("format.duration.ms", { m, s: s % 60 });
  return t("format.duration.s", { s });
}

export function formatTime(rfc3339: string): string {
  const d = new Date(rfc3339);
  return Number.isNaN(d.getTime()) ? rfc3339 : formatDate(d, { dateStyle: "medium", timeStyle: "medium" });
}

export function percent(done: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(100, Math.max(0, Math.round((done / total) * 100)));
}

/** Label for a known enum value, else the raw value (a newer daemon may send new ones). */
function label(prefix: string, value: string): string {
  const key = `${prefix}.${value}` as MessageKey;
  const out = t(key);
  return out === key ? value : out;
}

export const localityLabel = (l: string) => label("format.locality", l);
export const tierLabel = (tier: string) => label("format.tier", tier);
export const sourceKindLabel = (kind: string) => label("format.sourceKind", kind);
export const sourceStateLabel = (state: string) => label("format.sourceState", state);

const relFmts = new Map<string, Intl.RelativeTimeFormat>();

function relFmt(locale: string): Intl.RelativeTimeFormat {
  const key = locale;
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
  const loc = locale ?? currentLocale();
  if (!rfc3339) return t("format.never");
  const at = new Date(rfc3339).getTime();
  if (Number.isNaN(at)) return rfc3339;
  const secs = Math.round((at - now) / 1000);
  const abs = Math.abs(secs);
  if (abs < 45) return t("format.justNow");
  for (const [unit, size] of REL_UNITS) {
    if (abs >= size) return relFmt(loc).format(Math.round(secs / size), unit);
  }
  // 45–59 s rounds to a minute.
  return relFmt(loc).format(Math.sign(secs), "minute");
}

/** Latency in ms as "850 ms", "4.2 s", "3m 5s"; `null` as "–". */
export function formatLatency(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "–";
  const num = (v: number, digits: number) =>
    numberFormat(currentLocale(), { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(v);
  if (ms < 1000) return t("format.latency.ms", { n: num(Math.round(ms), 0) });
  if (ms < 60_000) return t("format.latency.s", { n: num(ms / 1000, ms < 10_000 ? 1 : 0) });
  return formatDuration(ms / 1000);
}

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

/**
 * ISO 8601 dates and date-times inside prose (`2026-11-10`, `2026-11-10T13:00:00Z`,
 * `2026-11-10T13:00+02:00`). Not glued to other digits or letters, so ids and versions are left alone.
 */
const ISO_IN_TEXT =
  /(?<![\w.:-])(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.\d{1,9})?)?(Z|[+-]\d{2}:?\d{2})?)?(?![\w:-])/g;

/**
 * `text` with its ISO dates and date-times written for the reader's locale: a bare date, or a
 * date-time at exactly midnight UTC (how a day is often stored), as a date ("Nov 10, 2026");
 * any other date-time as date and time in the local time zone ("Nov 10, 2026, 1:00 PM").
 * For display only: the stored text keeps its ISO form. Invalid dates are left as they are.
 * `locale` and `timeZone` are injectable for tests.
 */
export function formatDatesInText(text: string, locale?: string, timeZone?: string): string {
  const loc = locale ?? currentLocale();
  return text.replace(ISO_IN_TEXT, (whole, y: string, mo: string, d: string, h?: string, mi?: string, sec?: string, zone?: string) => {
    const day = new Date(Date.UTC(+y, +mo - 1, +d));
    // Reject impossible dates (2026-02-31, month 13) instead of letting Date roll them over.
    if (day.getUTCFullYear() !== +y || day.getUTCMonth() !== +mo - 1 || day.getUTCDate() !== +d) return whole;
    const utcDay = { dateStyle: "medium", timeZone: "UTC" } as const;
    if (h === undefined) return dateFormat(loc, utcDay).format(day);
    if (+h > 23 || +mi! > 59 || +(sec ?? 0) > 59) return whole;
    const midnight = +h === 0 && +mi! === 0 && +(sec ?? 0) === 0;
    if (midnight && zone && /^(?:Z|[+-]00:?00)$/.test(zone)) return dateFormat(loc, utcDay).format(day);
    const at = new Date(whole.replace(" ", "T").replace(/([+-]\d{2})(\d{2})$/, "$1:$2"));
    if (Number.isNaN(at.getTime())) return whole;
    return dateFormat(loc, { dateStyle: "medium", timeStyle: "short", ...(timeZone ? { timeZone } : {}) }).format(at);
  });
}
