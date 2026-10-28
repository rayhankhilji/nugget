import { useState } from "react";
import { api } from "../api";
import { useAsync } from "../hooks";
import { formatBytes, formatTime } from "../format";
import { Empty, ErrorNote, PageHeader, Spinner } from "../components/ui";

const LIMITS = [50, 200, 1000];

export function PrivacyPage() {
  const [limit, setLimit] = useState(200);
  const egress = useAsync(() => api.egressRecent(limit), [limit]);
  const rows = egress.data ?? [];
  const sent = rows.reduce((n, r) => n + r.bytes_sent, 0);
  const received = rows.reduce((n, r) => n + r.bytes_received, 0);
  const hosts = new Set(rows.map((r) => r.host)).size;

  return (
    <div className="page">
      <PageHeader
        title="Privacy"
        description="Every network request Nugget makes on your behalf is recorded here: where it went, why, and how much data left this device."
        actions={
          <>
            <label className="field-inline">
              <span>Show</span>
              <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
                {LIMITS.map((l) => (
                  <option key={l} value={l}>
                    last {l}
                  </option>
                ))}
              </select>
            </label>
            <button type="button" className="btn btn-ghost" onClick={egress.reload} disabled={egress.loading}>
              Refresh
            </button>
          </>
        }
      />
      <ErrorNote error={egress.error} onRetry={egress.reload} />
      {egress.loading && !egress.data && <Spinner />}

      {egress.data && rows.length === 0 && (
        <Empty title="Nothing has left this device">Requests to cloud providers and model downloads will appear here.</Empty>
      )}

      {rows.length > 0 && (
        <>
          <dl className="stats">
            <div>
              <dt>Requests</dt>
              <dd>{rows.length}</dd>
            </div>
            <div>
              <dt>Hosts</dt>
              <dd>{hosts}</dd>
            </div>
            <div>
              <dt>Sent</dt>
              <dd>{formatBytes(sent)}</dd>
            </div>
            <div>
              <dt>Received</dt>
              <dd>{formatBytes(received)}</dd>
            </div>
          </dl>
          <div className="table-wrap" tabIndex={0} role="region" aria-label="Egress ledger">
            <table>
              <caption className="visually-hidden">Egress ledger, newest first</caption>
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">Host</th>
                  <th scope="col">Feature</th>
                  <th scope="col" className="num">
                    Sent
                  </th>
                  <th scope="col" className="num">
                    Received
                  </th>
                  <th scope="col" className="num">
                    Items
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
                      {r.items.length}
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
