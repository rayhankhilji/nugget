import { useEffect, useState } from "react";
import {
  api,
  errorMessage,
  events,
  isMethodNotFound,
  PROTOCOL_VERSION,
  type Capabilities,
  type IngestStatus,
  type ProviderInfo,
  type RuntimeInfo,
  type RuntimeState,
} from "../api";
import { useAsync, useEvent, useNow } from "../hooks";
import { formatDuration, formatLatency, formatRelative, LOCALITY_LABEL, SOURCE_KIND_LABEL, TIER_LABEL } from "../format";
import { Badge, ErrorNote, NotAvailable, PageHeader, Spinner } from "../components/ui";

export function StatusPage() {
  const status = useAsync(api.status);
  const conn = useAsync(api.connection);
  const ingest = useAsync(api.ingestStatus);
  const runtime = useAsync(api.runtimeStatus);

  // Light auto-refresh while the page is open. Methods the daemon lacks are not re-polled.
  const { reload } = status;
  const reloadIngest = ingest.reload;
  const reloadRuntime = runtime.reload;
  const ingestMissing = isMethodNotFound(ingest.error);
  const runtimeMissing = isMethodNotFound(runtime.error);
  useEffect(() => {
    const t = window.setInterval(() => {
      reload();
      if (!ingestMissing) reloadIngest();
      if (!runtimeMissing) reloadRuntime();
    }, 10_000);
    return () => window.clearInterval(t);
  }, [reload, reloadIngest, reloadRuntime, ingestMissing, runtimeMissing]);
  const refreshAll = () => {
    status.reload();
    ingest.reload();
    runtime.reload();
  };

  const s = status.data;

  return (
    <div className="page">
      <PageHeader
        title="Status"
        actions={
          <button type="button" className="btn btn-ghost" onClick={refreshAll} disabled={status.loading}>
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
            <div>
              <dt>Audit log</dt>
              <dd>
                <AuditChain ok={s.audit_chain_ok} />
              </dd>
            </div>
          </dl>
          {s.audit_chain_ok === false && (
            <p className="note note-error" role="alert">
              The audit log failed its integrity check: an entry was changed, removed or inserted outside Nugget. The
              daemon log has the details.
            </p>
          )}

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

      <IndexingCard data={ingest.data} error={ingest.error} loading={ingest.loading} onRetry={ingest.reload} />
      <RuntimeCard data={runtime.data} error={runtime.error} loading={runtime.loading} onRetry={runtime.reload} />

      <h2>Connection</h2>
      <p className="muted small">
        Protocol v{PROTOCOL_VERSION} over <code>{conn.data?.socket || "…"}</code>
      </p>
    </div>
  );
}

function AuditChain({ ok }: { ok: boolean | null | undefined }) {
  if (ok === true) return <Badge tone="good">Verified</Badge>;
  if (ok === false) return <Badge tone="bad">Tampered</Badge>;
  return <span className="muted small">Checking…</span>;
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
  if (p.error) {
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
          <Badge tone="bad">Unavailable</Badge>
          <div className="note note-error small" role="status">
            {p.error}
          </div>
        </td>
      </tr>
    );
  }
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

// ---- indexing (M1) ----

function IndexingCard({
  data,
  error,
  loading,
  onRetry,
}: {
  data: IngestStatus | undefined;
  error: unknown;
  loading: boolean;
  onRetry: () => void;
}) {
  const now = useNow();
  // Queue depth moves fast while syncing; refresh on progress, at most every 2 s.
  const [lastPoke, setLastPoke] = useState(0);
  useEvent(events.onIngestProgress, () => {
    if (Date.now() - lastPoke > 2000) {
      setLastPoke(Date.now());
      onRetry();
    }
  });

  const failing = (data?.sources ?? []).filter((s) => s.last_error);
  return (
    <section className="status-card" aria-labelledby="indexing-title">
      <h2 id="indexing-title">Indexing</h2>
      {isMethodNotFound(error) ? (
        <NotAvailable />
      ) : (
        <>
          <ErrorNote error={error} onRetry={onRetry} />
          {loading && !data && <Spinner />}
          {data && (
            <>
              <dl className="stats">
                <div>
                  <dt>Queue</dt>
                  <dd>
                    {data.queue_depth.toLocaleString()}
                    <span className="muted small stat-unit"> waiting</span>
                  </dd>
                </div>
                <div>
                  <dt>Freshness p50</dt>
                  <dd>{formatLatency(data.freshness_p50_ms)}</dd>
                </div>
                <div>
                  <dt>Freshness p95</dt>
                  <dd>
                    {formatLatency(data.freshness_p95_ms)}
                    {data.freshness_p95_ms != null && data.freshness_p95_ms > 60_000 && (
                      <>
                        {" "}
                        <Badge tone="warn">Over 60 s</Badge>
                      </>
                    )}
                  </dd>
                </div>
                <div>
                  <dt>Embeddings</dt>
                  <dd className="stat-text">{data.embedding_provider ?? <span className="muted">Text-only search</span>}</dd>
                </div>
                <div>
                  <dt>Triage</dt>
                  <dd className="stat-text">{data.triage_provider ?? <span className="muted">Off</span>}</dd>
                </div>
              </dl>
              <p className="muted small">Freshness is the time from a change in a source to it being searchable, over recent changes.</p>
              {failing.length > 0 && (
                <>
                  <h3 className="ask-sub">Errors</h3>
                  <ul className="error-list">
                    {failing.map((s) => (
                      <li key={s.id}>
                        <b>{s.name}</b> <span className="muted small">({SOURCE_KIND_LABEL[s.kind] ?? s.kind}, synced {formatRelative(s.last_sync, now)})</span>
                        <div className="note note-error small">{s.last_error}</div>
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </>
          )}
        </>
      )}
    </section>
  );
}

// ---- local runtime (M1) ----

const RUNTIME_STATE: Record<RuntimeState, { label: string; tone: "neutral" | "accent" | "good" | "warn" | "bad" }> = {
  not_installed: { label: "Not installed", tone: "neutral" },
  stopped: { label: "Stopped", tone: "neutral" },
  starting: { label: "Starting", tone: "accent" },
  ready: { label: "Ready", tone: "good" },
  failed: { label: "Failed", tone: "bad" },
};

function RuntimeCard({
  data,
  error,
  loading,
  onRetry,
}: {
  data: RuntimeInfo[] | undefined;
  error: unknown;
  loading: boolean;
  onRetry: () => void;
}) {
  return (
    <section className="status-card" aria-labelledby="runtime-title">
      <h2 id="runtime-title">Local runtime</h2>
      {isMethodNotFound(error) ? (
        <NotAvailable />
      ) : (
        <>
          <ErrorNote error={error} onRetry={onRetry} />
          {loading && !data && <Spinner />}
          {data && data.length === 0 && (
            <p className="muted">No local model servers. Install a local model on the Models page to run chat and embeddings on this device.</p>
          )}
          {data && data.length > 0 && (
            <div className="table-wrap" tabIndex={0} role="region" aria-label="Local runtimes">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Model</th>
                    <th scope="col">Role</th>
                    <th scope="col">State</th>
                    <th scope="col">Provider</th>
                  </tr>
                </thead>
                <tbody>
                  {data.map((r) => {
                    const st = RUNTIME_STATE[r.state] ?? { label: r.state, tone: "neutral" as const };
                    return (
                      <tr key={`${r.provider}\u0000${r.model_id}`}>
                        <th scope="row">
                          <code>{r.model_id}</code>
                        </th>
                        <td>{r.role === "embedding" ? "Embeddings" : "Chat"}</td>
                        <td>
                          <Badge tone={st.tone}>{st.label}</Badge>
                          {r.error && <div className="note note-error small">{r.error}</div>}
                        </td>
                        <td>
                          <code>{r.provider}</code>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </section>
  );
}
