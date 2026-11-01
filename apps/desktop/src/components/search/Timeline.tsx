/**
 * Everything about one person, project or organisation in time order (`search.timeline`):
 * the items that mention it and its facts, grouped by month, with "Jump to month". When the
 * daemon kept only the newest events, jumping to an older month asks again for a window that
 * ends with that month. Escape (or Back) returns to the results.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { api, isCommandMissing, isOpenableUrl, type FacetCount, type Timeline as TimelineData, type TimelineEvent } from "../../api";
import { formatDate, useT } from "../../i18n";
import { monthOf, monthRange } from "../../searchState";
import { itemFileName, plainSnippet } from "../Provenance";
import { ErrorNote, Loading, NotAvailable } from "../ui";
import { kindLabel, monthLabel } from "./Facets";

const UNDATED = "undated";

export interface MonthGroup {
  month: string;
  events: TimelineEvent[];
}

/** Events grouped by month in the order they come (ascending), undated ones last. */
export function groupByMonth(events: readonly TimelineEvent[]): MonthGroup[] {
  const groups = new Map<string, TimelineEvent[]>();
  for (const e of events) {
    const m = monthOf(e.at) ?? UNDATED;
    const g = groups.get(m);
    if (g) g.push(e);
    else groups.set(m, [e]);
  }
  const out = [...groups.entries()].map(([month, evs]) => ({ month, events: evs }));
  return [...out.filter((g) => g.month !== UNDATED).sort((a, b) => a.month.localeCompare(b.month)), ...out.filter((g) => g.month === UNDATED)];
}

const headingId = (month: string) => `timeline-month-${month}`;

export function Timeline({
  entityId,
  onBack,
  onOpen,
}: {
  entityId: string;
  onBack: () => void;
  onOpen: (url: string, label: string) => Promise<void>;
}) {
  const t = useT();
  const [data, setData] = useState<TimelineData | null>(null);
  /** Months over all of the entity's events (from the first, unwindowed answer). */
  const [allMonths, setAllMonths] = useState<FacetCount[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(true);
  const [windowTo, setWindowTo] = useState<string | null>(null);
  const [jump, setJump] = useState("");
  const [attempt, setAttempt] = useState(0);
  /** A month to focus once the answer that was asked for it (not `stale`) has arrived. */
  const pendingFocus = useRef<{ month: string; stale: TimelineData | null } | null>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const focusedTitle = useRef(false);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    setLoading(true);
    setError(null);
    api
      .searchTimeline({ entity: entityId, to: windowTo })
      .then((d) => {
        if (!live) return;
        setData(d);
        if (!windowTo) setAllMonths(d.months);
      })
      .catch((e) => live && setError(e))
      .finally(() => live && setLoading(false));
    return () => {
      live = false;
    };
  }, [entityId, windowTo, attempt]);

  useEffect(() => {
    focusedTitle.current = false;
    setJump("");
    setWindowTo(null);
    setAllMonths([]);
    setData(null);
  }, [entityId]);

  // The view changed: put focus on its heading once, so screen readers say where they are.
  useEffect(() => {
    if (data && !focusedTitle.current) {
      focusedTitle.current = true;
      titleRef.current?.focus();
    }
  }, [data]);

  const groups = useMemo(() => groupByMonth(data?.events ?? []), [data]);

  const focusMonth = (month: string): boolean => {
    if (!/^\d{4}-\d{2}$/.test(month)) return false;
    const el = listRef.current?.querySelector<HTMLElement>(`#${headingId(month)}`);
    if (!el) return false;
    el.scrollIntoView?.({ block: "start" });
    el.focus();
    return true;
  };

  // After a refetch for a jump, focus the month asked for.
  useEffect(() => {
    const p = pendingFocus.current;
    if (!loading && p && data && data !== p.stale) {
      pendingFocus.current = null;
      focusMonth(p.month);
    }
  });

  const onJump = (month: string) => {
    setJump(month);
    if (!month) return;
    if (focusMonth(month)) return;
    const r = monthRange(month);
    if (!r) return;
    pendingFocus.current = { month, stale: data };
    setWindowTo(r.to);
  };

  const months = allMonths.length > 0 ? allMonths : (data?.months ?? []);
  const name = data?.entity.name ?? "";

  return (
    <section
      className="sr-timeline"
      aria-labelledby="timeline-heading"
      aria-busy={loading}
      onKeyDown={(e) => {
        // Escape goes back to the results, as Back does (a list box's own Escape comes first).
        if (e.key === "Escape" && !e.defaultPrevented) {
          e.preventDefault();
          e.stopPropagation();
          onBack();
        }
      }}
    >
      <div className="sr-timeline-head">
        <button type="button" className="btn btn-small" onClick={onBack}>
          {t("searchui.backToResults")}
        </button>
        <h2 id="timeline-heading" ref={titleRef} tabIndex={-1}>
          {data ? t("searchui.timelineTitle", { name }) : t("searchui.timeline")}
        </h2>
      </div>

      {isCommandMissing(error) ? <NotAvailable what={t("searchui.timeline")} /> : <ErrorNote error={error} onRetry={() => setAttempt((n) => n + 1)} />}
      {loading && !data && <Loading label={t("searchui.timelineLoading")} lines={4} />}

      {data && (
        <>
          <p className="muted small" role="status">
            {t("searchui.timelineCount", { n: data.total })}
          </p>
          {(data.truncated || windowTo) && (
            <p className="note">
              {data.truncated ? t("searchui.timelineTruncated", { shown: data.events.length, total: data.total }) : null}
              {windowTo && (
                <button
                  type="button"
                  className="btn btn-small"
                  onClick={() => {
                    setJump("");
                    setWindowTo(null);
                  }}
                >
                  {t("searchui.timelineLatest")}
                </button>
              )}
            </p>
          )}
          {months.length > 0 && (
            <label className="field-inline sr-jump">
              <span>{t("searchui.jumpToMonth")}</span>
              <select value={jump} onChange={(e) => onJump(e.target.value)}>
                <option value="">{t("searchui.chooseMonth")}</option>
                {months.map((m) => (
                  <option key={m.value} value={m.value}>
                    {t("searchui.monthOption", { month: monthLabel(m.value), count: m.count })}
                  </option>
                ))}
              </select>
            </label>
          )}
          {data.events.length === 0 && <p className="muted">{t("searchui.timelineEmpty", { name })}</p>}
          <div ref={listRef}>
            {groups.map((g) => (
              <section key={g.month} className="sr-tl-month" aria-labelledby={headingId(g.month)}>
                <h3 id={headingId(g.month)} tabIndex={-1} className="sr-tl-month-title">
                  {g.month === UNDATED ? t("searchui.undated") : monthLabel(g.month)}
                </h3>
                <ol className="sr-tl-list">
                  {g.events.map((e, i) => (
                    <TimelineRow key={i} e={e} onOpen={onOpen} />
                  ))}
                </ol>
              </section>
            ))}
          </div>
        </>
      )}
    </section>
  );
}

function When({ at }: { at: string | null }) {
  if (!at) return null;
  return (
    <time dateTime={at} className="muted small">
      {formatDate(at)}
    </time>
  );
}

function TimelineRow({ e, onOpen }: { e: TimelineEvent; onOpen: (url: string, label: string) => Promise<void> }) {
  const t = useT();
  if (e.type === "fact") {
    return (
      <li className="sr-tl-event sr-tl-fact">
        <div className="sr-tl-meta">
          <span className="badge badge-accent">{t("searchui.fact")}</span>
          <When at={e.at} />
          <span className="muted small">{t("searchui.confidence", { pct: e.claim.confidence })}</span>
        </div>
        <p className="sr-tl-text" dir="auto">
          {e.claim.text}
        </p>
        {e.claim.evidence.some((ev) => ev.quote) && (
          <ul className="sr-tl-evidence">
            {e.claim.evidence
              .filter((ev) => ev.quote)
              .map((ev, i) => (
                <li key={i}>
                  <blockquote dir="auto">{plainSnippet(ev.quote!)}</blockquote>
                  {ev.title && (
                    <span className="muted small" dir="auto">
                      {ev.title}
                    </span>
                  )}
                </li>
              ))}
          </ul>
        )}
      </li>
    );
  }
  const title = e.title || itemFileName(e.item.key) || t("search.untitled");
  return (
    <li className="sr-tl-event sr-tl-item">
      <div className="sr-tl-meta">
        <span className="badge">{kindLabel(t, e.kind)}</span>
        <When at={e.at} />
        {e.sensitive && !e.unsorted && (
          <span className="badge badge-warn" title={t("search.sensitiveTitle")}>
            {t("search.sensitive")}
          </span>
        )}
      </div>
      <p className="sr-tl-title" dir="auto">
        {title}
      </p>
      {e.snippet && (
        <p className="muted small" dir="auto">
          {plainSnippet(e.snippet)}
        </p>
      )}
      {isOpenableUrl(e.url) && (
        <button type="button" className="btn btn-small" onClick={() => void onOpen(e.url!, title)} aria-label={t("search.openTitle", { title })}>
          {t("search.open")}
        </button>
      )}
    </li>
  );
}
