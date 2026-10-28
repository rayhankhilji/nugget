import { useEffect, useRef, useState } from "react";
import { api, isMethodNotFound, type McpClientInfo, type SourceInfo } from "../api";
import { useAsync, useNow } from "../hooks";
import { formatRelative, formatTime } from "../format";
import { summarizeScopes } from "../mcp";
import { ConnectAppWizard } from "../components/ConnectAppWizard";
import { Dialog } from "../components/Dialog";
import { Badge, EmptyState, ErrorNote, Loading, NotAvailable, PageHeader, Spinner } from "../components/ui";
import { useT } from "../i18n";

/** Connected AI apps: MCP clients with their scopes, plus connect and revoke. */
export function AppsPage() {
  const t = useT();
  const loaded = useAsync(api.mcpClientsList);
  const sources = useAsync(() => api.sourcesList().catch(() => [] as SourceInfo[]));
  const [clients, setClients] = useState<McpClientInfo[] | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [revoking, setRevoking] = useState<McpClientInfo | null>(null);
  const [announce, setAnnounce] = useState("");
  const now = useNow();
  // The revoked row (and its button) disappears; focus goes back to a stable control.
  const connectRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (loaded.data) setClients(loaded.data);
  }, [loaded.data]);

  const unavailable = isMethodNotFound(loaded.error);
  const list = clients ?? [];

  return (
    <div className="page">
      <PageHeader
        title={t("apps.title")}
        description={t("apps.description")}
        actions={
          !unavailable && (
            <button type="button" className="btn btn-primary" ref={connectRef} onClick={() => setConnecting(true)}>
              {t("apps.connect")}
            </button>
          )
        }
      />
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>

      {unavailable ? (
        <NotAvailable what={t("apps.title")} />
      ) : (
        <>
          <ErrorNote error={loaded.error} onRetry={loaded.reload} />
          {loaded.loading && !clients && <Loading label={t("apps.loading")} />}
          {clients && list.length === 0 && (
            <EmptyState
              title={t("apps.emptyTitle")}
              action={
                <button type="button" className="btn" onClick={() => setConnecting(true)}>
                  {t("apps.connect")}
                </button>
              }
            >
              {t("apps.emptyBody")}
            </EmptyState>
          )}
          {list.length > 0 && (
            <div
              className={loaded.loading ? "table-wrap pg-stale" : "table-wrap"}
              tabIndex={0}
              role="region"
              aria-label={t("apps.tableLabel")}
              aria-busy={loaded.loading}
            >
              <table>
                <caption className="visually-hidden">{t("apps.tableLabel")}</caption>
                <thead>
                  <tr>
                    <th scope="col">{t("apps.col.app")}</th>
                    <th scope="col">{t("apps.col.canRead")}</th>
                    <th scope="col">{t("apps.col.tools")}</th>
                    <th scope="col">{t("apps.col.created")}</th>
                    <th scope="col">{t("apps.col.lastUsed")}</th>
                    <th scope="col">
                      <span className="visually-hidden">{t("apps.col.actions")}</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {list.map((c) => {
                    const sum = summarizeScopes(c.scopes, sources.data);
                    return (
                      <tr key={c.id}>
                        <th scope="row">
                          {c.name}
                          <div className="muted small">
                            <code>{c.id}</code>
                          </div>
                        </th>
                        <td>
                          {sum.sources}
                          {sum.sensitive && (
                            <div>
                              <Badge tone="warn">{t("apps.sensitiveBadge")}</Badge>
                            </div>
                          )}
                        </td>
                        <td>{sum.tools}</td>
                        <td title={formatTime(c.created_at)}>{formatRelative(c.created_at, now)}</td>
                        <td title={c.last_used ? formatTime(c.last_used) : undefined}>
                          {c.last_used ? formatRelative(c.last_used, now) : <span className="muted">{t("apps.neverUsed")}</span>}
                        </td>
                        <td>
                          <button type="button" className="btn btn-small btn-danger-ghost" onClick={() => setRevoking(c)} aria-label={t("apps.revokeLabel", { name: c.name })}>
                            {t("apps.revoke")}
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
          <details className="help apps-help">
            <summary>{t("apps.help.summary")}</summary>
            <ol>
              <li>{t("apps.help.step1")}</li>
              <li>{t("apps.help.step2")}</li>
              <li>{t("apps.help.step3")}</li>
            </ol>
            <p className="small muted">{t("apps.help.revokeNote")}</p>
          </details>
        </>
      )}

      {connecting && (
        <ConnectAppWizard
          onClose={() => {
            setConnecting(false);
            loaded.reload();
          }}
          onCreated={(client) => {
            setClients((l) => [...(l ?? []).filter((c) => c.id !== client.id), client]);
            setAnnounce(t("apps.announce.created", { name: client.name }));
          }}
        />
      )}
      {revoking && (
        <RevokeDialog
          client={revoking}
          onClose={() => setRevoking(null)}
          onRevoked={() => {
            setClients((l) => (l ?? []).filter((c) => c.id !== revoking.id));
            setAnnounce(t("apps.announce.revoked", { name: revoking.name }));
            setRevoking(null);
            // After the dialog has closed and restored focus to the (now removed) row.
            window.setTimeout(() => connectRef.current?.focus(), 0);
          }}
        />
      )}
    </div>
  );
}

function RevokeDialog({ client, onClose, onRevoked }: { client: McpClientInfo; onClose: () => void; onRevoked: () => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const revoke = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.mcpClientsRevoke(client.id);
      onRevoked();
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={t("apps.revokeDialog.title", { name: client.name })}
      description={t("apps.revokeDialog.description")}
      onClose={onClose}
    >
      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose} disabled={busy} data-autofocus="">
          {t("common.cancel")}
        </button>
        <button type="button" className="btn btn-danger" onClick={() => void revoke()} disabled={busy}>
          {busy ? <Spinner label={t("apps.revoking")} /> : t("apps.revoke")}
        </button>
      </div>
    </Dialog>
  );
}
