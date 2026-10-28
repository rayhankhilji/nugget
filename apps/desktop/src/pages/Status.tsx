import { useEffect, useState } from "react";
import {
  api,
  errorMessage,
  events,
  isMethodNotFound,
  PROTOCOL_VERSION,
  type Capabilities,
  type IngestStatus,
  type KnowledgeStatus,
  type ProviderInfo,
  type RuntimeInfo,
  type RuntimeState,
} from "../api";
import { useAsync, useEvent, useNow, useOpener } from "../hooks";
import { fileUrl, vaultLinks } from "../knowledge";
import { formatDuration, formatLatency, formatRelative, localityLabel, sourceKindLabel, tierLabel } from "../format";
import { Badge, ErrorNote, NotAvailable, PageHeader, Spinner } from "../components/ui";
import { UpdateSettings } from "../components/UpdateSettings";
import { formatList, formatNumber, Trans, useT, type MessageKey } from "../i18n";

export function StatusPage() {
  const t = useT();
  const status = useAsync(api.status);
  const conn = useAsync(api.connection);
  const ingest = useAsync(api.ingestStatus);
  const runtime = useAsync(api.runtimeStatus);
  const knowledge = useAsync(api.knowledgeStatus);

  // Light auto-refresh while the page is open. Methods the daemon lacks are not re-polled.
  const { reload } = status;
  const reloadIngest = ingest.reload;
  const reloadRuntime = runtime.reload;
  const reloadKnowledge = knowledge.reload;
  const ingestMissing = isMethodNotFound(ingest.error);
  const runtimeMissing = isMethodNotFound(runtime.error);
  const knowledgeMissing = isMethodNotFound(knowledge.error);
  useEffect(() => {
    const timer = window.setInterval(() => {
      reload();
      if (!ingestMissing) reloadIngest();
      if (!runtimeMissing) reloadRuntime();
      if (!knowledgeMissing) reloadKnowledge();
    }, 10_000);
    return () => window.clearInterval(timer);
  }, [reload, reloadIngest, reloadRuntime, reloadKnowledge, ingestMissing, runtimeMissing, knowledgeMissing]);
  const refreshAll = () => {
    status.reload();
    ingest.reload();
    runtime.reload();
    knowledge.reload();
  };

  const s = status.data;

  return (
    <div className="page">
      <PageHeader
        title={t("status.title")}
        actions={
          <button type="button" className="btn btn-ghost" onClick={refreshAll} disabled={status.loading}>
            {t("common.refresh")}
          </button>
        }
      />
      <ErrorNote error={status.error} onRetry={status.reload} />
      {status.loading && !s && <Spinner />}
      {s && (
        <>
          {s.protocol !== PROTOCOL_VERSION && (
            <p className="note note-error" role="alert">
              {t("status.protocolMismatch", { app: PROTOCOL_VERSION, daemon: s.protocol })}
            </p>
          )}
          <dl className="stats">
            <div>
              <dt>{t("status.daemon")}</dt>
              <dd>{t("status.version", { version: s.version })}</dd>
            </div>
            <div>
              <dt>{t("status.uptime")}</dt>
              <dd>{formatDuration(s.uptime_secs)}</dd>
            </div>
            <div>
              <dt>{t("status.items")}</dt>
              <dd>{formatNumber(s.items)}</dd>
            </div>
            <div>
              <dt>{t("status.chunks")}</dt>
              <dd>{formatNumber(s.chunks)}</dd>
            </div>
            <div>
              <dt>{t("status.auditLog")}</dt>
              <dd>
                <AuditChain ok={s.audit_chain_ok} />
              </dd>
            </div>
          </dl>
          {s.audit_chain_ok === false && (
            <p className="note note-error" role="alert">
              {t("status.auditTampered")}
            </p>
          )}

          <h2>{t("status.providers")}</h2>
          {s.providers.length === 0 ? (
            <p className="muted">
              <Trans k="status.noProviders" tags={{ code: (c) => <code>{c}</code> }} />
            </p>
          ) : (
            <div className="table-wrap" tabIndex={0} role="region" aria-label={t("status.providers")}>
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t("status.col.provider")}</th>
                    <th scope="col">{t("status.col.model")}</th>
                    <th scope="col">{t("status.col.where")}</th>
                    <th scope="col">{t("status.col.tiers")}</th>
                    <th scope="col">{t("status.col.capabilities")}</th>
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
      <KnowledgeCard data={knowledge.data} error={knowledge.error} loading={knowledge.loading} onRetry={knowledge.reload} />
      <UpdateSettings />

      <h2>{t("status.connection")}</h2>
      <p className="muted small">
        <Trans
          k="status.connectionLine"
          args={{ protocol: PROTOCOL_VERSION, socket: conn.data?.socket || "…" }}
          tags={{ code: (c) => <code>{c}</code> }}
        />
      </p>
    </div>
  );
}

function AuditChain({ ok }: { ok: boolean | null | undefined }) {
  const t = useT();
  if (ok === true) return <Badge tone="good">{t("status.auditVerified")}</Badge>;
  if (ok === false) return <Badge tone="bad">{t("status.auditTamperedBadge")}</Badge>;
  return <span className="muted small">{t("status.auditChecking")}</span>;
}

const CAPS: [keyof Capabilities, MessageKey][] = [
  ["streaming", "status.cap.streaming"],
  ["vision", "status.cap.vision"],
  ["tools", "status.cap.tools"],
  ["json_schema", "status.cap.jsonSchema"],
  ["embeddings", "status.cap.embeddings"],
];

/** Tier names as a localised list, or a dash when there are none. */
function tierList(tiers: readonly string[]): string {
  return tiers.length ? formatList(tiers.map((tr) => tierLabel(tr))) : "–";
}

function ProviderRow({ provider: p, onProbed }: { provider: ProviderInfo; onProbed: () => void }) {
  const t = useT();
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
          <Badge tone={tone}>{localityLabel(p.locality)}</Badge>
        </td>
        <td>{tierList(p.tiers)}</td>
        <td>
          <Badge tone="bad">{t("status.unavailable")}</Badge>
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
        <Badge tone={tone}>{localityLabel(p.locality)}</Badge>
      </td>
      <td>{tierList(p.tiers)}</td>
      <td>
        {p.capabilities ? (
          <span className="badges">
            {CAPS.filter(([k]) => p.capabilities?.[k]).map(([k, label]) => (
              <Badge key={k}>{t(label)}</Badge>
            ))}
            {p.capabilities.context_tokens != null && (
              <span className="muted small">{t("status.contextTokens", { n: p.capabilities.context_tokens })}</span>
            )}
          </span>
        ) : (
          <span className="muted small">{t("status.notProbed")}</span>
        )}{" "}
        <button type="button" className="btn btn-ghost btn-small" onClick={probe} disabled={probing}>
          {probing ? <Spinner label={t("status.probing")} /> : p.capabilities ? t("status.reprobe") : t("status.probe")}
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
  const t = useT();
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
      <h2 id="indexing-title">{t("status.indexing")}</h2>
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
                  <dt>{t("status.queue")}</dt>
                  <dd>
                    {formatNumber(data.queue_depth)}
                    <span className="muted small stat-unit"> {t("status.queueUnit", { n: data.queue_depth })}</span>
                  </dd>
                </div>
                <div>
                  <dt>{t("status.freshnessP50")}</dt>
                  <dd>{formatLatency(data.freshness_p50_ms)}</dd>
                </div>
                <div>
                  <dt>{t("status.freshnessP95")}</dt>
                  <dd>
                    {formatLatency(data.freshness_p95_ms)}
                    {data.freshness_p95_ms != null && data.freshness_p95_ms > 60_000 && (
                      <>
                        {" "}
                        <Badge tone="warn">{t("status.over60s")}</Badge>
                      </>
                    )}
                  </dd>
                </div>
                <div>
                  <dt>{t("status.embeddings")}</dt>
                  <dd className="stat-text">{data.embedding_provider ?? <span className="muted">{t("status.textOnly")}</span>}</dd>
                </div>
                <div>
                  <dt>{t("status.triage")}</dt>
                  <dd className="stat-text">{data.triage_provider ?? <span className="muted">{t("status.off")}</span>}</dd>
                </div>
              </dl>
              <p className="muted small">{t("status.freshnessHint")}</p>
              {failing.length > 0 && (
                <>
                  <h3 className="ask-sub">{t("status.errors")}</h3>
                  <ul className="error-list">
                    {failing.map((s) => (
                      <li key={s.id}>
                        <b>{s.name}</b> <span className="muted small">
                          {t("status.sourceSynced", { kind: sourceKindLabel(s.kind), when: formatRelative(s.last_sync, now) })}
                        </span>
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

const RUNTIME_STATE: Record<RuntimeState, { label: MessageKey; tone: "neutral" | "accent" | "good" | "warn" | "bad" }> = {
  not_installed: { label: "status.runtime.not_installed", tone: "neutral" },
  stopped: { label: "status.runtime.stopped", tone: "neutral" },
  starting: { label: "status.runtime.starting", tone: "accent" },
  ready: { label: "status.runtime.ready", tone: "good" },
  failed: { label: "status.runtime.failed", tone: "bad" },
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
  const t = useT();
  return (
    <section className="status-card" aria-labelledby="runtime-title">
      <h2 id="runtime-title">{t("status.localRuntime")}</h2>
      {isMethodNotFound(error) ? (
        <NotAvailable />
      ) : (
        <>
          <ErrorNote error={error} onRetry={onRetry} />
          {loading && !data && <Spinner />}
          {data && data.length === 0 && (
            <p className="muted">{t("status.noRuntimes")}</p>
          )}
          {data && data.length > 0 && (
            <div className="table-wrap" tabIndex={0} role="region" aria-label={t("status.localRuntimes")}>
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t("status.col.model")}</th>
                    <th scope="col">{t("status.col.role")}</th>
                    <th scope="col">{t("status.col.state")}</th>
                    <th scope="col">{t("status.col.provider")}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.map((r) => {
                    const known = RUNTIME_STATE[r.state];
                    const st = known ? { label: t(known.label), tone: known.tone } : { label: r.state, tone: "neutral" as const };
                    return (
                      <tr key={`${r.provider}\u0000${r.model_id}`}>
                        <th scope="row">
                          <code>{r.model_id}</code>
                        </th>
                        <td>{r.role === "embedding" ? t("status.role.embedding") : t("status.role.chat")}</td>
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

// ---- knowledge graph (M2) ----

function KnowledgeCard({
  data,
  error,
  loading,
  onRetry,
}: {
  data: KnowledgeStatus | undefined;
  error: unknown;
  loading: boolean;
  onRetry: () => void;
}) {
  const t = useT();
  const now = useNow();
  const opener = useOpener();
  const vault = data?.vault;
  const vaultUrl = vault?.path ? fileUrl(vault.path) : null;
  const conflicts = vault?.conflicts ?? [];
  const invalid = vault?.invalid ?? [];
  return (
    <section className="status-card" aria-labelledby="knowledge-title">
      <h2 id="knowledge-title">{t("status.knowledge")}</h2>
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
                  <dt>{t("status.entities")}</dt>
                  <dd>{formatNumber(data.entities)}</dd>
                </div>
                <div>
                  <dt>{t("status.facts")}</dt>
                  <dd>{formatNumber(data.claims)}</dd>
                </div>
                <div>
                  <dt>{t("status.mentions")}</dt>
                  <dd>{formatNumber(data.mentions)}</dd>
                </div>
                <div>
                  <dt>{t("status.waiting")}</dt>
                  <dd>
                    {formatNumber(data.pending_items)}
                    <span className="muted small stat-unit"> {t("status.itemsUnit", { n: data.pending_items })}</span>
                  </dd>
                </div>
                <div>
                  <dt>{t("status.extraction")}</dt>
                  <dd className="stat-text">
                    {data.extractor_provider ?? <span className="muted">{t("status.rulesOnly")}</span>}
                    <div className="muted small">{t("status.lastRun", { when: formatRelative(data.last_run, now) })}</div>
                  </dd>
                </div>
              </dl>
              <p className="muted small">
                {t(data.extractor_provider ? "status.rulesHintModel" : "status.rulesHintNoModel")}
              </p>

              <h3 className="ask-sub">{t("status.vault")}</h3>
              {vault?.path ? (
                <p className="vault-line">
                  <code>{vault.path}</code>{" "}
                  {vaultUrl && (
                    <button type="button" className="btn btn-small" onClick={() => void opener.open(vaultUrl, t("status.vaultFolder"))}>
                      {t("status.openFolder")}
                    </button>
                  )}
                  <span className="muted small">
                    {" "}
                    {t("status.vaultPages", { n: vault.pages, when: formatRelative(vault.last_render, now) })}
                  </span>
                </p>
              ) : (
                <p className="muted small">{t("status.noVault")}</p>
              )}
              <div aria-live="polite">
                {opener.status && <p className={opener.status.error ? "note note-error small" : "note small"}>{opener.status.text}</p>}
              </div>
              {conflicts.length > 0 && (
                <PageList
                  title={t("status.conflicts")}
                  hint={t("status.conflictsHint")}
                  pages={conflicts}
                  root={vault?.path ?? null}
                  tone="warn"
                  open={opener.open}
                />
              )}
              {invalid.length > 0 && (
                <PageList
                  title={t("status.invalid")}
                  hint={t("status.invalidHint")}
                  pages={invalid}
                  root={vault?.path ?? null}
                  tone="bad"
                  open={opener.open}
                />
              )}
            </>
          )}
        </>
      )}
    </section>
  );
}

const PAGES_SHOWN = 20;

function PageList({
  title,
  hint,
  pages,
  root,
  tone,
  open,
}: {
  title: string;
  hint: string;
  pages: string[];
  root: string | null;
  tone: "warn" | "bad";
  open: (url: string, label: string) => Promise<void>;
}) {
  const t = useT();
  const [all, setAll] = useState(false);
  const shown = all ? pages : pages.slice(0, PAGES_SHOWN);
  return (
    <>
      <h4 className="ask-sub">
        {title} <Badge tone={tone}>{formatNumber(pages.length)}</Badge>
      </h4>
      <p className="muted small">{hint}</p>
      <ul className="vault-pages">
        {shown.map((p) => {
          const links = vaultLinks(root, p);
          return (
            <li key={p}>
              <code>{p}</code>
              {links && (
                <button type="button" className="btn btn-ghost btn-small" onClick={() => void open(links.file, p)} aria-label={t("status.openPage", { page: p })}>
                  {t("status.open")}
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {pages.length > PAGES_SHOWN && (
        <button type="button" className="btn btn-ghost btn-small" onClick={() => setAll((a) => !a)} aria-expanded={all}>
          {all ? t("status.showFewer") : t("status.showAll", { n: pages.length })}
        </button>
      )}
    </>
  );
}
