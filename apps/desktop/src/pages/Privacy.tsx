import { useState } from "react";
import { api } from "../api";
import { useAsync } from "../hooks";
import { formatBytes, formatTime } from "../format";
import { Empty, ErrorNote, PageHeader, Spinner } from "../components/ui";
import { formatNumber, useT } from "../i18n";

const LIMITS = [50, 200, 1000];

export function PrivacyPage() {
  const t = useT();
  const [limit, setLimit] = useState(200);
  const egress = useAsync(() => api.egressRecent(limit), [limit]);
  const rows = egress.data ?? [];
  const sent = rows.reduce((n, r) => n + r.bytes_sent, 0);
  const received = rows.reduce((n, r) => n + r.bytes_received, 0);
  const hosts = new Set(rows.map((r) => r.host)).size;

  return (
    <div className="page">
      <PageHeader
        title={t("privacy.title")}
        description={t("privacy.description")}
        actions={
          <>
            <label className="field-inline">
              <span>{t("privacy.show")}</span>
              <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
                {LIMITS.map((l) => (
                  <option key={l} value={l}>
                    {t("privacy.lastN", { n: l })}
                  </option>
                ))}
              </select>
            </label>
            <button type="button" className="btn btn-ghost" onClick={egress.reload} disabled={egress.loading}>
              {t("common.refresh")}
            </button>
          </>
        }
      />
      <ErrorNote error={egress.error} onRetry={egress.reload} />
      {egress.loading && !egress.data && <Spinner />}

      {egress.data && rows.length === 0 && (
        <Empty title={t("privacy.emptyTitle")}>{t("privacy.emptyBody")}</Empty>
      )}

      {rows.length > 0 && (
        <>
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
            <table>
              <caption className="visually-hidden">{t("privacy.ledgerCaption")}</caption>
              <thead>
                <tr>
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
              <tbody>
                {rows.map((r, i) => (
                  <tr key={`${r.at}-${i}`}>
                    <td className="nowrap">{formatTime(r.at)}</td>
                    <td>
                      <code>{r.host}</code>
                    </td>
                    <td>{r.feature}</td>
                    <td className="num">{formatBytes(r.bytes_sent)}</td>
                    <td className="num">{formatBytes(r.bytes_received)}</td>
                    <td className="num" title={r.items.map((it) => `${it.source}: ${it.key}`).join("\n") || undefined}>
                      {formatNumber(r.items.length)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </div>
  );
}
