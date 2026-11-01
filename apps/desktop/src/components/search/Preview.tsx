/**
 * The selected result in a side panel (`search.preview`): its text with the query's words
 * marked, the passage that matched, the people it mentions (each opens their timeline) and the
 * facts it is evidence for, plus Open in app. Escape or Close hands focus back to the result.
 */
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { api, isCommandMissing, isOpenableUrl, type ItemPreview, type ItemRef, type SourceInfo } from "../../api";
import { formatDate, useT } from "../../i18n";
import { sourceKindOf } from "../../recency";
import { Highlight, itemFileName, plainSnippet, SourceBadge } from "../Provenance";
import { ErrorNote, Loading, NotAvailable } from "../ui";
import { kindLabel } from "./Facets";

export function Preview({
  item,
  chunkId,
  fallbackTitle,
  query,
  source,
  onClose,
  onTimeline,
  onOpen,
}: {
  item: ItemRef;
  chunkId?: number | null;
  /** The result's title, shown while the preview loads. */
  fallbackTitle: string;
  query: string;
  source: SourceInfo | undefined;
  onClose: () => void;
  onTimeline: (entityId: string) => void;
  onOpen: (url: string, label: string) => Promise<void>;
}) {
  const t = useT();
  const [data, setData] = useState<ItemPreview | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  const titleRef = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    let live = true;
    setData(null);
    setError(null);
    api
      .searchPreview({ item, chunk_id: chunkId ?? null })
      .then((d) => live && setData(d))
      .catch((e) => live && setError(e));
    return () => {
      live = false;
    };
    // The item's identity, not the object, decides what to load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item.source, item.key, chunkId, attempt]);

  useEffect(() => {
    titleRef.current?.focus();
  }, [item.source, item.key]);

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "Escape" && !e.defaultPrevented) {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    }
  };

  const kind = source?.kind ?? sourceKindOf(item.source);
  const title = data?.title || fallbackTitle || itemFileName(item.key, kind) || t("search.untitled");

  return (
    <section id="search-preview" className="sr-preview" role="region" aria-labelledby="search-preview-title" onKeyDown={onKeyDown}>
      <div className="sr-preview-head">
        <h2 id="search-preview-title" ref={titleRef} tabIndex={-1} dir="auto">
          {title}
        </h2>
        <button type="button" className="btn btn-ghost btn-icon" onClick={onClose} aria-label={t("searchui.closePreview")}>
          <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true" focusable="false">
            <path d="M2 2l10 10M12 2L2 12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
          </svg>
        </button>
      </div>
      <p className="muted small sr-meta">
        <SourceBadge sourceId={item.source} name={source?.name} kind={source?.kind} itemKey={item.key} title={title} />
        {data && (
          <>
            <span className="badge">{kindLabel(t, data.kind)}</span>
            {data.date && <time dateTime={data.date}>{formatDate(data.date)}</time>}
            {data.sensitive && !data.unsorted && (
              <span className="badge badge-warn" title={t("search.sensitiveTitle")}>
                {t("search.sensitive")}
              </span>
            )}
            {data.sensitive && data.unsorted && (
              <span className="badge" title={t("search.unsortedTitle")}>
                {t("search.unsorted")}
              </span>
            )}
          </>
        )}
      </p>

      {isCommandMissing(error) ? <NotAvailable what={t("searchui.preview")} /> : <ErrorNote error={error} onRetry={() => setAttempt((n) => n + 1)} />}
      {!data && !error && <Loading label={t("searchui.previewLoading")} lines={5} />}

      {data && (
        <>
          {isOpenableUrl(data.url) && (
            <p>
              <button type="button" className="btn btn-small btn-primary" onClick={() => void onOpen(data.url!, title)} aria-label={t("search.openTitle", { title })}>
                {t("searchui.openInApp")}
              </button>
            </p>
          )}
          {data.passage && (
            <>
              <h3 className="sr-preview-sub">{t("searchui.matchingPassage")}</h3>
              <blockquote className="sr-passage" dir="auto">
                <Highlight text={plainSnippet(data.passage)} query={query} />
              </blockquote>
            </>
          )}
          {data.people.length > 0 && (
            <>
              <h3 className="sr-preview-sub">{t("searchui.people")}</h3>
              <ul className="chips sr-people">
                {data.people.map((p) => (
                  <li key={p.id}>
                    <button type="button" className="chip" onClick={() => onTimeline(p.id)} aria-label={t("searchui.timelineFor", { name: p.name })}>
                      <bdi>{p.name}</bdi>
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}
          {data.facts.length > 0 && (
            <>
              <h3 className="sr-preview-sub">{t("searchui.facts")}</h3>
              <ul className="sr-facts">
                {data.facts.map((f) => (
                  <li key={f.id}>
                    <span dir="auto">{f.text}</span>{" "}
                    <span className="muted small">{t("searchui.confidence", { pct: f.confidence })}</span>
                    {f.quote && <blockquote dir="auto">{plainSnippet(f.quote)}</blockquote>}
                  </li>
                ))}
              </ul>
            </>
          )}
          <h3 className="sr-preview-sub">{t("searchui.text")}</h3>
          <div className="sr-preview-text" dir="auto" tabIndex={0} role="region" aria-label={t("searchui.itemText", { title })}>
            <Highlight text={data.text} query={query} />
          </div>
          {data.truncated && <p className="muted small">{t("searchui.textTruncated")}</p>}
        </>
      )}
    </section>
  );
}
