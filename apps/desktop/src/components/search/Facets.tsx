/**
 * Search filters: kind, source, person and date range, each value with its count from
 * `search.faceted`. Every group is a fieldset of checkboxes (Tab between them, Space toggles),
 * so it works the same with a keyboard and a screen reader. Each change reruns the search.
 */
import { useId } from "react";
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
}: {
  facets: FacetedResult["facets"];
  state: SearchState;
  sourceName: (id: string) => string;
  onChange: (next: SearchState) => void;
  onTimeline: (entityId: string) => void;
}) {
  const t = useT();
  const id = useId();
  const set = (patch: Partial<SearchState>) => onChange({ ...state, ...patch });
  const months = facets.months.slice(-MONTHS_SHOWN);
  const maxMonth = Math.max(1, ...months.map((m) => m.count));
  const monthPicked = (m: string) => {
    const r = monthRange(m);
    return !!r && state.from === r.from && state.to === r.to;
  };

  return (
    <section id="search-filters" className="sr-facets" aria-labelledby={`${id}-h`}>
      <div className="sr-facets-head">
        <h2 id={`${id}-h`} className="sr-facets-title" tabIndex={-1}>
          {t("searchui.filters")}
        </h2>
        {hasFilters(state) && (
          <button type="button" className="btn btn-small btn-ghost" onClick={() => set({ sources: [], kinds: [], people: [], from: null, to: null })}>
            {t("searchui.clearFilters")}
          </button>
        )}
      </div>

      {facets.kinds.length > 0 && (
        <fieldset className="sr-facet" data-facet="kind">
          <legend>{t("searchui.facet.kind")}</legend>
          {facets.kinds.map((f) => (
            <Check
              key={f.value}
              label={kindLabel(t, f.value)}
              count={f.count}
              checked={state.kinds.includes(f.value as ItemKind)}
              onChange={(on) => set({ kinds: toggle(state.kinds, f.value as ItemKind, on) })}
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
              onChange={(on) => set({ sources: toggle(state.sources, f.value, on) })}
            />
          ))}
        </fieldset>
      )}

      {facets.people.length > 0 && (
        <fieldset className="sr-facet" data-facet="person">
          <legend>{t("searchui.facet.person")}</legend>
          {facets.people.map((f) => (
            <div key={f.value} className="sr-facet-row">
              <Check label={f.name} count={f.count} checked={state.people.includes(f.value)} onChange={(on) => set({ people: toggle(state.people, f.value, on) })} />
              <button type="button" className="btn btn-ghost btn-small sr-facet-tl" onClick={() => onTimeline(f.value)} aria-label={t("searchui.timelineFor", { name: f.name })}>
                {t("searchui.timeline")}
              </button>
            </div>
          ))}
        </fieldset>
      )}

      <fieldset className="sr-facet" data-facet="date">
        <legend>{t("searchui.facet.date")}</legend>
        <div className="sr-dates">
          <label className="sr-date">
            <span>{t("searchui.from")}</span>
            <input type="date" value={state.from ?? ""} max={state.to ?? undefined} onChange={(e) => set({ from: isDate(e.target.value) ? e.target.value : null })} />
          </label>
          <label className="sr-date">
            <span>{t("searchui.to")}</span>
            <input type="date" value={state.to ?? ""} min={state.from ?? undefined} onChange={(e) => set({ to: isDate(e.target.value) ? e.target.value : null })} />
          </label>
        </div>
        {(state.from || state.to) && (
          <button type="button" className="btn btn-small btn-ghost" onClick={() => set({ from: null, to: null })}>
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

function Check({ label, count, checked, onChange }: { label: string; count: number; checked: boolean; onChange: (on: boolean) => void }) {
  return (
    <label className="sr-check">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <bdi className="sr-check-label">{label}</bdi>
      <span className="sr-count">{formatNumber(count)}</span>
    </label>
  );
}
