import { useCallback, useEffect, useRef, useState } from "react";
import { api, egressPage, isCommandMissing, type EgressEvent } from "../api";
import { formatBytes, formatTime } from "../format";
import { EmptyState, ErrorNote, Loading, PageHeader, Spinner } from "../components/ui";
import { Announce, useAnnouncer } from "../components/Announce";
import { VirtualList } from "../components/VirtualList";
import { useCursorList, type CursorPageResult } from "../components/useCursorList";
import { formatNumber, useT } from "../i18n";

/** Rows per page (older pages load as the ledger scrolls). */
const LIMITS = [50, 200, 1000];
/** Ledger rows have no id; time and position keep keys stable for a loaded ledger (pages only append). */
const egressKey = (r: EgressEvent, i: number) => `${r.at}-${i}`;

export function PrivacyPage() {
  const t = useT();
  const [limit, setLimit] = useState(200);
  /** The daemon has no `egress.page` (older daemon): one page from `egress.recent`. */
  const legacy = useRef(false);
  const fetchPage = useCallback(
    async (cursor: string | null): Promise<CursorPageResult<EgressEvent>> => {
      if (!legacy.current) {
        try {
          const page = await egressPage(cursor ? { limit, cursor } : { limit });
          return { items: page.items, next: page.next_cursor ?? null };
        } catch (err) {
          if (cursor || !isCommandMissing(err)) throw err;
          legacy.current = true;
        }
      }
      return { items: await api.egressRecent(limit), next: null };
    },
    [limit],
  );
  const egress = useCursorList(fetchPage, String(limit));
  const rows = egress.items ?? [];
  const sent = rows.reduce((n, r) => n + r.bytes_sent, 0);
  const received = rows.reduce((n, r) => n + r.bytes_received, 0);
  const hosts = new Set(rows.map((r) => r.host)).size;
  const firstLoad = egress.loading && !egress.loadingMore;
  const loadedText = egress.hasMore ? t("privacy.loadedSome", { n: rows.length }) : t("privacy.loadedAll", { n: rows.length });

  // Say how many rows are loaded once a page arrives (not on the first load: the page says it).
  const [announcement, announce] = useAnnouncer();
  const shown = useRef(0);
  useEffect(() => {
    if (egress.loading) return;
    if (shown.current > 0 && rows.length > shown.current) announce(loadedText);
    shown.current = rows.length;
  }, [egress.loading, rows.length, loadedText, announce]);

  return (
    <div className="page">
      <PageHeader
        title={t("privacy.title")}
        description={t("privacy.description")}
        actions={
          <>
            <label className="field-inline">
              <span className="visually-hidden">{t("privacy.show")}</span>
              <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
                {LIMITS.map((l) => (
                  <option key={l} value={l}>
                    {t("privacy.pageN", { n: l })}
                  </option>
                ))}
              </select>
            </label>
            <button type="button" className="btn btn-ghost" onClick={egress.reload} disabled={egress.loading && !egress.items}>
              {t("common.refresh")}
            </button>
          </>
        }
      />
      <ErrorNote error={egress.error} onRetry={egress.retry} />
      <Announce message={announcement} />
      {egress.loading && !egress.items && <Loading label={t("privacy.loading")} lines={5} />}

      {egress.items && rows.length === 0 && (
        <div className="privacy-clear">
          <EmptyState title={t("privacy.emptyTitle")}>{t("privacy.emptyBody")}</EmptyState>
        </div>
      )}

      {rows.length > 0 && (
        <div className={firstLoad ? "pg-stale" : undefined} aria-busy={firstLoad}>
          <dl className="stats">
            <div>
              <dt>{t("privacy.stat.requests")}</dt>
              <dd>{formatNumber(rows.length)}</dd>
            </div>
            <div>
              <dt>{t("privacy.stat.hosts")}</dt>
              <dd>{formatNumber(hosts)}</dd>
            </div>
            <div>
              <dt>{t("privacy.stat.sent")}</dt>
              <dd>{formatBytes(sent)}</dd>
            </div>
            <div>
              <dt>{t("privacy.stat.received")}</dt>
              <dd>{formatBytes(received)}</dd>
            </div>
          </dl>
          <div className="table-wrap" tabIndex={0} role="region" aria-label={t("privacy.ledgerLabel")}>
            {/* Only a window of rows is rendered: the row count tells assistive tech the whole size. */}
            <table aria-rowcount={rows.length + 1}>
              <caption className="visually-hidden">{t("privacy.ledgerCaption")}</caption>
              <thead>
                <tr aria-rowindex={1}>
                  <th scope="col">{t("privacy.col.time")}</th>
                  <th scope="col">{t("privacy.col.host")}</th>
                  <th scope="col">{t("privacy.col.feature")}</th>
                  <th scope="col" className="num">
                    {t("privacy.col.sent")}
                  </th>
                  <th scope="col" className="num">
                    {t("privacy.col.received")}
                  </th>
                  <th scope="col" className="num">
                    {t("privacy.col.items")}
                  </th>
                </tr>
              </thead>
              <VirtualList
                as="tbody"
                items={rows}
                itemKey={egressKey}
                estimateSize={37}
                columns={6}
                rowIndexBase={2}
                focus="none"
                hasMore={egress.hasMore}
                loadingMore={egress.loadingMore}
                onEndReached={egress.loadMore}
                renderItem={(r) => (
                  <>
                    <td className="nowrap">{formatTime(r.at)}</td>
                    <td>
                      <bdi dir="ltr">
                        <code>{r.host}</code>
                      </bdi>
                    </td>
                    <td>{r.feature}</td>
                    <td className="num">{formatBytes(r.bytes_sent)}</td>
                    <td className="num">{formatBytes(r.bytes_received)}</td>
                    <td className="num" title={r.items.map((it) => `${it.source}: ${it.key}`).join("\n") || undefined}>
                      {formatNumber(r.items.length)}
                    </td>
                  </>
                )}
              />
            </table>
          </div>
          <p className="muted small privacy-loaded">{loadedText}</p>
          {egress.hasMore && (
            <button type="button" className="btn" onClick={egress.loadMore} disabled={egress.loadingMore || egress.error != null}>
              {egress.loadingMore ? <Spinner label={t("list.loadingMore")} /> : t("privacy.loadOlder")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
