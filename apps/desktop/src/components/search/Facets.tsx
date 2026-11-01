/**
 * Search filters: kind, source, person and date range, each value with its count from
 * `search.faceted`. Every group is a fieldset of checkboxes (Tab between them, Space toggles),
 * so it works the same with a keyboard and a screen reader. Each change reruns the search.
 */
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import type { FacetedResult, ItemKind } from "../../api";
import { formatDate, formatNumber, useT, type MessageKey } from "../../i18n";
import { hasFilters, isDate, monthDate, monthRange, type SearchState } from "../../searchState";

/** How many months the quick picks show (the most recent ones). */
const MONTHS_SHOWN = 24;

export function kindLabel(t: ReturnType<typeof useT>, kind: string): string {
  return ["mail", "event", "chat", "file"].includes(kind) ? t(`searchui.kind.${kind}` as MessageKey) : kind;
}

export function monthLabel(month: string): string {
  return formatDate(monthDate(month), { year: "numeric", month: "long" });
}

function toggle<T extends string>(list: readonly T[], value: T, on: boolean): T[] {
  return on ? [...new Set([...list, value])] : list.filter((v) => v !== value);
}

export function Facets({
  facets,
  state,
  sourceName,
  onChange,
  onTimeline,
  poolLimited = null,
  announce,
}: {
  facets: FacetedResult["facets"];
  state: SearchState;
  sourceName: (id: string) => string;
  /** Gets an update of the latest state (two quick changes both apply). */
  onChange: (update: (s: SearchState) => SearchState) => void;
  onTimeline: (entityId: string) => void;
  /** The pool size when the daemon's candidate pool was capped (counts may be partial). */
  poolLimited?: number | null;
  announce?: (text: string) => void;
}) {
  const t = useT();
  const id = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const set = (patch: Partial<SearchState> | ((s: SearchState) => Partial<SearchState>)) =>
    onChange((s) => ({ ...s, ...(typeof patch === "function" ? patch(s) : patch) }));
  /** A control that removes itself: keep focus in the filters and say what happened. */
  const clearing = (patch: Partial<SearchState>, said: string) => {
    set(patch);
    headingRef.current?.focus();
    announce?.(said);
  };
  const months = facets.months.slice(-MONTHS_SHOWN);
  const maxMonth = Math.max(1, ...months.map((m) => m.count));
  const monthPicked = (m: string) => {
    const r = monthRange(m);
    return !!r && state.from === r.from && state.to === r.to;
  };

  return (
    <section id="search-filters" className="sr-facets" aria-labelledby={`${id}-h`}>
      <div className="sr-facets-head">
        <h2 id={`${id}-h`} ref={headingRef} className="sr-facets-title" tabIndex={-1}>
          {t("searchui.filters")}
        </h2>
        {hasFilters(state) && (
          <button type="button" className="btn btn-small btn-ghost" onClick={() => clearing({ sources: [], kinds: [], people: [], from: null, to: null }, t("searchui.filtersCleared"))}>
            {t("searchui.clearFilters")}
          </button>
        )}
      </div>
      {poolLimited !== null && <p className="muted small sr-pool-note">{t("searchui.poolLimited", { pool: poolLimited })}</p>}

      {facets.kinds.length > 0 && (
        <fieldset className="sr-facet" data-facet="kind">
          <legend>{t("searchui.facet.kind")}</legend>
          {facets.kinds.map((f) => (
            <Check
              key={f.value}
              label={kindLabel(t, f.value)}
              count={f.count}
              checked={state.kinds.includes(f.value as ItemKind)}
              onChange={(on) => set((s) => ({ kinds: toggle(s.kinds, f.value as ItemKind, on) }))}
            />
          ))}
        </fieldset>
      )}

      {facets.sources.length > 0 && (
        <fieldset className="sr-facet" data-facet="source">
          <legend>{t("searchui.facet.source")}</legend>
          {facets.sources.map((f) => (
            <Check
              key={f.value}
              label={sourceName(f.value)}
              count={f.count}
              checked={state.sources.includes(f.value)}
              onChange={(on) => set((s) => ({ sources: toggle(s.sources, f.value, on) }))}
            />
          ))}
        </fieldset>
      )}

      {facets.people.length > 0 && (
        <fieldset className="sr-facet" data-facet="person">
          <legend>{t("searchui.facet.person")}</legend>
          {facets.people.map((f) => (
            <div key={f.value} className="sr-facet-row">
              <Check label={f.name} count={f.count} checked={state.people.includes(f.value)} onChange={(on) => set((s) => ({ people: toggle(s.people, f.value, on) }))} />
              {/* JJ2: an icon, so the row fits the filter column in every language; the name says what it does. */}
              <button
                type="button"
                className="btn btn-ghost btn-small btn-icon sr-facet-tl"
                onClick={() => onTimeline(f.value)}
                aria-label={t("searchui.timelineFor", { name: f.name })}
                title={t("searchui.timelineFor", { name: f.name })}
              >
                <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true" focusable="false">
                  <path d="M1.5 7h11M4 4.5v5M7 3v8M10 5v4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" fill="none" />
                </svg>
              </button>
            </div>
          ))}
        </fieldset>
      )}

      <fieldset className="sr-facet" data-facet="date">
        <legend>{t("searchui.facet.date")}</legend>
        <div className="sr-dates">
          <DateInput label={t("searchui.from")} value={state.from} max={state.to} onCommit={(v) => set({ from: v })} />
          <DateInput label={t("searchui.to")} value={state.to} min={state.from} onCommit={(v) => set({ to: v })} />
        </div>
        {(state.from || state.to) && (
          <button type="button" className="btn btn-small btn-ghost" onClick={() => clearing({ from: null, to: null }, t("searchui.datesCleared"))}>
            {t("searchui.clearDates")}
          </button>
        )}
        {months.length > 0 && (
          <ul className="sr-months" aria-label={t("searchui.months")}>
            {months.map((m) => {
              const picked = monthPicked(m.value);
              const label = monthLabel(m.value);
              return (
                <li key={m.value}>
                  <button
                    type="button"
                    className="sr-month"
                    aria-pressed={picked}
                    aria-label={t("searchui.monthPick", { month: label, count: m.count })}
                    title={t("searchui.monthPick", { month: label, count: m.count })}
                    onClick={() => {
                      const r = monthRange(m.value);
                      if (r) set(picked ? { from: null, to: null } : r);
                    }}
                  >
                    <span className="sr-month-bar" aria-hidden="true" style={{ blockSize: `${Math.max(8, (m.count / maxMonth) * 100)}%` }} />
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </fieldset>
    </section>
  );
}

/** How long a typed date waits before it applies (a complete, valid date only). */
export const DATE_DEBOUNCE_MS = 400;

/** A complete date a person means: valid and not a partly typed year like 0002. */
function completeDate(v: string): string | null {
  return isDate(v) && Number(v.slice(0, 4)) >= 1000 ? v : null;
}

/**
 * A date field that applies its value only when it is a complete date (after a short pause, on
 * blur or on Enter) or when it is emptied, so typing a year doesn't search on every keystroke.
 */
function DateInput({ label, value, min, max, onCommit }: { label: string; value: string | null; min?: string | null; max?: string | null; onCommit: (v: string | null) => void }) {
  const [text, setText] = useState(value ?? "");
  const timer = useRef<number | undefined>(undefined);
  const commitRef = useRef(onCommit);
  commitRef.current = onCommit;
  // A new value from outside (a month pick, Clear dates) replaces what is typed.
  useEffect(() => setText(value ?? ""), [value]);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const commit = (v: string) => {
    window.clearTimeout(timer.current);
    const d = v === "" ? null : completeDate(v);
    if (v !== "" && d === null) return;
    if (d !== (value ?? null)) commitRef.current(d);
  };
  return (
    <label className="sr-date">
      <span>{label}</span>
      <input
        type="date"
        value={text}
        min={min ?? undefined}
        max={max ?? undefined}
        onChange={(e) => {
          const v = e.target.value;
          setText(v);
          window.clearTimeout(timer.current);
          if (v === "" || completeDate(v)) timer.current = window.setTimeout(() => commit(v), DATE_DEBOUNCE_MS);
        }}
        onBlur={(e) => commit(e.target.value)}
        onKeyDown={(e: KeyboardEvent<HTMLInputElement>) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit(e.currentTarget.value);
          }
        }}
      />
    </label>
  );
}

function Check({ label, count, checked, onChange }: { label: string; count: number; checked: boolean; onChange: (on: boolean) => void }) {
  const t = useT();
  return (
    <label className="sr-check">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} aria-label={t("searchui.facetOption", { label, count })} />
      <bdi className="sr-check-label">{label}</bdi>
      <span className="sr-count">{formatNumber(count)}</span>
    </label>
  );
}
