import { useEffect, useState } from "react";
import { api, errorMessage, PROTOCOL_VERSION, type Capabilities, type ProviderInfo } from "../api";
import { useAsync } from "../hooks";
import { formatDuration, LOCALITY_LABEL, TIER_LABEL } from "../format";
import { Badge, ErrorNote, PageHeader, Spinner } from "../components/ui";

export function StatusPage() {
  const status = useAsync(api.status);
  const conn = useAsync(api.connection);

  // Light auto-refresh while the page is open.
  const { reload } = status;
  useEffect(() => {
    const t = window.setInterval(reload, 10_000);
    return () => window.clearInterval(t);
  }, [reload]);

  const s = status.data;

  return (
    <div className="page">
      <PageHeader
        title="Status"
        actions={
          <button type="button" className="btn btn-ghost" onClick={status.reload} disabled={status.loading}>
            Refresh
          </button>
        }
      />
      <ErrorNote error={status.error} onRetry={status.reload} />
      {status.loading && !s && <Spinner />}
      {s && (
        <>
          {s.protocol !== PROTOCOL_VERSION && (
            <p className="note note-error" role="alert">
              This app speaks protocol {PROTOCOL_VERSION} but the daemon speaks {s.protocol}. Update Nugget so both match.
            </p>
          )}
          <dl className="stats">
            <div>
              <dt>Daemon</dt>
              <dd>v{s.version}</dd>
            </div>
            <div>
              <dt>Uptime</dt>
              <dd>{formatDuration(s.uptime_secs)}</dd>
            </div>
            <div>
              <dt>Items</dt>
              <dd>{s.items.toLocaleString()}</dd>
            </div>
            <div>
              <dt>Chunks</dt>
              <dd>{s.chunks.toLocaleString()}</dd>
            </div>
          </dl>

          <h2>Providers</h2>
          {s.providers.length === 0 ? (
            <p className="muted">No providers configured. Add one to config.toml, or install a local model.</p>
          ) : (
            <div className="table-wrap" tabIndex={0} role="region" aria-label="Providers">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Provider</th>
                    <th scope="col">Model</th>
                    <th scope="col">Where</th>
                    <th scope="col">Tiers</th>
                    <th scope="col">Capabilities</th>
                  </tr>
                </thead>
                <tbody>
                  {s.providers.map((p) => (
                    <ProviderRow key={p.id} provider={p} onProbed={status.reload} />
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}

      <h2>Connection</h2>
      <p className="muted small">
        Protocol v{PROTOCOL_VERSION} over <code>{conn.data?.socket || "…"}</code>
      </p>
    </div>
  );
}

const CAPS: [keyof Capabilities, string][] = [
  ["streaming", "Streaming"],
  ["vision", "Vision"],
  ["tools", "Tools"],
  ["json_schema", "JSON schema"],
  ["embeddings", "Embeddings"],
];

function ProviderRow({ provider: p, onProbed }: { provider: ProviderInfo; onProbed: () => void }) {
  const [probing, setProbing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const probe = async () => {
    setProbing(true);
    setError(null);
    try {
      await api.providersProbe(p.id);
      onProbed();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setProbing(false);
    }
  };

  const tone = p.locality === "cloud" ? "warn" : "good";
  return (
    <tr>
      <th scope="row">
        {p.id}
        <div className="muted small">{p.kind}</div>
      </th>
      <td>
        <code>{p.model}</code>
      </td>
      <td>
        <Badge tone={tone}>{LOCALITY_LABEL[p.locality] ?? p.locality}</Badge>
      </td>
      <td>{p.tiers.map((t) => TIER_LABEL[t] ?? t).join(", ") || "–"}</td>
      <td>
        {p.capabilities ? (
          <span className="badges">
            {CAPS.filter(([k]) => p.capabilities?.[k]).map(([k, label]) => (
              <Badge key={k}>{label}</Badge>
            ))}
            {p.capabilities.context_tokens != null && (
              <span className="muted small">{p.capabilities.context_tokens.toLocaleString()} ctx</span>
            )}
          </span>
        ) : (
          <span className="muted small">Not probed</span>
        )}{" "}
        <button type="button" className="btn btn-ghost btn-small" onClick={probe} disabled={probing}>
          {probing ? <Spinner label="Probing" /> : p.capabilities ? "Re-probe" : "Probe"}
        </button>
        {error && (
          <div className="note note-error small" role="alert">
            {error}
          </div>
        )}
      </td>
    </tr>
  );
}
