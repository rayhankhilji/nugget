/**
 * JJ2: how an Ask searched, as the answer views (Chat, Search, quick ask) show it: the date phrase
 * of the question as a range ("Searched items from 26 Oct to 1 Nov", `AskStarted.date`), and the
 * sources that are at best a near miss (`AskStarted.partial`).
 */
import type { AskDateFilter, AskStarted, Citation } from "./api";
import { dateFormat } from "./i18n/icu";

/** A calendar day. */
interface Day {
  y: number;
  m: number;
  d: number;
}

const RFC3339 = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})/;

/**
 * The day an RFC 3339 instant falls on at its own offset: the daemon writes a range's bounds at
 * the offset it read the phrase with, so this is the day the user meant whatever this machine's
 * zone is. An exclusive `end` at midnight belongs to the day before.
 */
export function dayOf(rfc3339: string, exclusiveEnd = false): Day | null {
  const m = RFC3339.exec(rfc3339);
  if (!m) return null;
  let day = { y: Number(m[1]), m: Number(m[2]), d: Number(m[3]) };
  if (exclusiveEnd && m[4] === "00" && m[5] === "00" && m[6] === "00") {
    const prev = new Date(Date.UTC(day.y, day.m - 1, day.d - 1));
    day = { y: prev.getUTCFullYear(), m: prev.getUTCMonth() + 1, d: prev.getUTCDate() };
  }
  return day;
}

const cmp = (a: Day, b: Day) => a.y - b.y || a.m - b.m || a.d - b.d;

/** A message key and its arguments: what to say about the question's date range. */
export interface DateNote {
  key: `ask.dates.${"applied" | "listed" | "notApplied"}${"" | "Day"}`;
  args: Record<string, string>;
}

/**
 * The note for a date filter, with days written for `locale` ("26 Oct"; with the year when the
 * range is not all in `today`'s year). `null` for a filter that names no usable range.
 */
export function askDateNote(f: AskDateFilter | null | undefined, locale: string, today: Date = new Date()): DateNote | null {
  if (!f) return null;
  const from = dayOf(f.start);
  let to = dayOf(f.end, true);
  if (!from || !to) return null;
  if (cmp(to, from) < 0) to = from;
  const year = from.y !== to.y || from.y !== today.getFullYear();
  const fmt = dateFormat(locale, { day: "numeric", month: "short", timeZone: "UTC", ...(year ? { year: "numeric" } : {}) });
  const show = (x: Day) => fmt.format(new Date(Date.UTC(x.y, x.m - 1, x.d)));
  const base = !f.applied ? ("ask.dates.notApplied" as const) : f.listed ? ("ask.dates.listed" as const) : ("ask.dates.applied" as const);
  return cmp(from, to) === 0 ? { key: `${base}Day`, args: { day: show(from) } } : { key: base, args: { from: show(from), to: show(to) } };
}

/** The citations with `partial` set on those `AskStarted.partial` lists (it travels with them when saved). */
export function withPartial(citations: readonly Citation[], partial: readonly number[] | null | undefined): Citation[] {
  const set = new Set(partial ?? []);
  return citations.map((c) => (set.has(c.n) ? { ...c, partial: true } : c));
}

/** An Ask's citations, partial matches marked. */
export const askCitations = (started: Pick<AskStarted, "citations" | "partial">): Citation[] =>
  withPartial(started.citations, started.partial);
