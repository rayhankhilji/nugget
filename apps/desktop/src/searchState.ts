/**
 * The Search page's state as a URL-like string (`q=budget&kind=mail&person=ent_1&from=2024-01-01`),
 * so a search can be saved, pinned and replayed. Encoding is stable (fixed key order, sorted and
 * de-duplicated values) so equal states give equal strings; decoding ignores anything it does not
 * understand.
 */
import { ITEM_KINDS, type FacetedFilters, type ItemKind } from "./api";

export interface SearchState {
  q: string;
  /** Source ids. */
  sources: string[];
  kinds: ItemKind[];
  /** Entity ids. */
  people: string[];
  /** Inclusive local dates, `YYYY-MM-DD`. */
  from: string | null;
  to: string | null;
  /** Show the timeline of this entity instead of results. */
  timeline?: string | null;
}

export const EMPTY_STATE: SearchState = { q: "", sources: [], kinds: [], people: [], from: null, to: null, timeline: null };

const DATE = /^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])$/;
/** Ids are short opaque tokens; anything longer or with control characters is junk. */
const MAX_VALUE = 200;
const MAX_VALUES = 50;
const MAX_QUERY = 1000;

export function isDate(s: string | null | undefined): s is string {
  return !!s && DATE.test(s) && !Number.isNaN(Date.parse(`${s}T00:00:00Z`));
}

function clean(values: readonly string[]): string[] {
  const ok = values.map((v) => v.trim()).filter((v) => v.length > 0 && v.length <= MAX_VALUE && !/[\u0000-\u001f]/.test(v));
  return [...new Set(ok)].sort().slice(0, MAX_VALUES);
}

/** A well-formed copy: trimmed query, valid dates (swapped when reversed), known kinds. */
export function normalize(s: Partial<SearchState>): SearchState {
  let from = isDate(s.from) ? s.from : null;
  let to = isDate(s.to) ? s.to : null;
  if (from && to && from > to) [from, to] = [to, from];
  const timeline = typeof s.timeline === "string" && s.timeline.trim() && s.timeline.length <= MAX_VALUE ? s.timeline.trim() : null;
  return {
    q: typeof s.q === "string" ? s.q.trim().slice(0, MAX_QUERY) : "",
    sources: clean(Array.isArray(s.sources) ? s.sources.filter((v) => typeof v === "string") : []),
    kinds: clean(Array.isArray(s.kinds) ? s.kinds.filter((v) => typeof v === "string") : []).filter((k): k is ItemKind =>
      (ITEM_KINDS as readonly string[]).includes(k),
    ),
    people: clean(Array.isArray(s.people) ? s.people.filter((v) => typeof v === "string") : []),
    from,
    to,
    timeline,
  };
}

export function encode(s: Partial<SearchState>): string {
  const n = normalize(s);
  const p = new URLSearchParams();
  if (n.q) p.append("q", n.q);
  for (const v of n.sources) p.append("source", v);
  for (const v of n.kinds) p.append("kind", v);
  for (const v of n.people) p.append("person", v);
  if (n.from) p.append("from", n.from);
  if (n.to) p.append("to", n.to);
  if (n.timeline) p.append("timeline", n.timeline);
  return p.toString();
}

export function decode(text: string | null | undefined): SearchState {
  if (typeof text !== "string") return normalize({});
  let p: URLSearchParams;
  try {
    p = new URLSearchParams(text.replace(/^[?#]/, ""));
  } catch {
    return normalize({});
  }
  return normalize({
    q: p.get("q") ?? "",
    sources: p.getAll("source"),
    kinds: p.getAll("kind") as ItemKind[],
    people: p.getAll("person"),
    from: p.get("from"),
    to: p.get("to"),
    timeline: p.get("timeline"),
  });
}

/** Any filter set (the query and the timeline don't count). */
export function hasFilters(s: SearchState): boolean {
  return s.sources.length > 0 || s.kinds.length > 0 || s.people.length > 0 || !!s.from || !!s.to;
}

/** The daemon's filter object for `search.faceted`. */
export function toFilters(s: SearchState): FacetedFilters {
  return { sources: s.sources, kinds: s.kinds, people: s.people, from: s.from, to: s.to };
}

/** The first and last day of a `YYYY-MM` month, or null for anything else. */
export function monthRange(month: string): { from: string; to: string } | null {
  const m = /^(\d{4})-(0[1-9]|1[0-2])$/.exec(month);
  if (!m) return null;
  const y = Number(m[1]);
  const mo = Number(m[2]);
  const last = new Date(Date.UTC(y, mo, 0)).getUTCDate();
  return { from: `${m[1]}-${m[2]}-01`, to: `${m[1]}-${m[2]}-${String(last).padStart(2, "0")}` };
}

/** `YYYY-MM` of a date-only or RFC 3339 string, in local time for full timestamps. */
export function monthOf(at: string | null | undefined): string | null {
  if (!at) return null;
  if (/^\d{4}-\d{2}(-\d{2})?$/.test(at)) return at.slice(0, 7);
  const d = new Date(at);
  if (Number.isNaN(d.getTime())) return null;
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}`;
}

/** A `Date` for the middle of a `YYYY-MM` month (safe to format in any time zone). */
export function monthDate(month: string): Date {
  const [y, m] = month.split("-").map(Number);
  return new Date(y!, (m ?? 1) - 1, 15);
}
