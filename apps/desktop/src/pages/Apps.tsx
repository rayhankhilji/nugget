import { useEffect, useState } from "react";
import { api, isMethodNotFound, type McpClientInfo, type SourceInfo } from "../api";
import { useAsync, useNow } from "../hooks";
import { formatRelative, formatTime } from "../format";
import { summarizeScopes } from "../mcp";
import { ConnectAppWizard } from "../components/ConnectAppWizard";
import { Dialog } from "../components/Dialog";
import { Badge, Empty, ErrorNote, NotAvailable, PageHeader, Spinner } from "../components/ui";

/** Connected AI apps: MCP clients with their scopes, plus connect and revoke. */
export function AppsPage() {
  const loaded = useAsync(api.mcpClientsList);
  const sources = useAsync(() => api.sourcesList().catch(() => [] as SourceInfo[]));
  const [clients, setClients] = useState<McpClientInfo[] | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [revoking, setRevoking] = useState<McpClientInfo | null>(null);
  const [announce, setAnnounce] = useState("");
  const now = useNow();

  useEffect(() => {
    if (loaded.data) setClients(loaded.data);
  }, [loaded.data]);

  const unavailable = isMethodNotFound(loaded.error);
  const list = clients ?? [];

  return (
    <div className="page">
      <PageHeader
        title="Connected AI apps"
        description="Let apps such as Claude Desktop, Claude Code and Cursor answer from what Nugget knows, over MCP. Apps connect on this device only, each with its own token and permissions, and every request goes in the audit log."
        actions={
          !unavailable && (
            <button type="button" className="btn btn-primary" onClick={() => setConnecting(true)}>
              Connect an app
            </button>
          )
        }
      />
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>

      {unavailable ? (
        <NotAvailable what="Connected AI apps" />
      ) : (
        <>
          <ErrorNote error={loaded.error} onRetry={loaded.reload} />
          {loaded.loading && !clients && <Spinner />}
          {clients && list.length === 0 && (
            <Empty title="No apps connected">Connect an app to let it search your sources and knowledge graph, with the access you choose.</Empty>
          )}
          {list.length > 0 && (
            <div className="table-wrap" tabIndex={0} role="region" aria-label="Connected apps">
              <table>
                <thead>
                  <tr>
                    <th scope="col">App</th>
                    <th scope="col">Can read</th>
                    <th scope="col">Tools</th>
                    <th scope="col">Created</th>
                    <th scope="col">Last used</th>
                    <th scope="col">
                      <span className="visually-hidden">Actions</span>
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
                              <Badge tone="warn">Includes sensitive items</Badge>
                            </div>
                          )}
                        </td>
                        <td>{sum.tools}</td>
                        <td title={formatTime(c.created_at)}>{formatRelative(c.created_at, now)}</td>
                        <td title={c.last_used ? formatTime(c.last_used) : undefined}>
                          {c.last_used ? formatRelative(c.last_used, now) : <span className="muted">Never</span>}
                        </td>
                        <td>
                          <button type="button" className="btn btn-small btn-danger-ghost" onClick={() => setRevoking(c)} aria-label={`Revoke ${c.name}`}>
                            Revoke
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
            <summary>How connecting works</summary>
            <ol>
              <li>Choose a name and what the app may read. Sensitive items stay hidden unless you allow them.</li>
              <li>Nugget creates a token for that app. Copy it, or copy the ready-made setup for your app.</li>
              <li>Paste it into the app's MCP settings and restart the app. It shows up here as used once it connects.</li>
            </ol>
            <p className="small muted">Revoking an app stops its token working immediately.</p>
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
            setAnnounce(`Created a token for ${client.name}.`);
          }}
        />
      )}
      {revoking && (
        <RevokeDialog
          client={revoking}
          onClose={() => setRevoking(null)}
          onRevoked={() => {
            setClients((l) => (l ?? []).filter((c) => c.id !== revoking.id));
            setAnnounce(`Revoked ${revoking.name}.`);
            setRevoking(null);
          }}
        />
      )}
    </div>
  );
}

function RevokeDialog({ client, onClose, onRevoked }: { client: McpClientInfo; onClose: () => void; onRevoked: () => void }) {
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
      title={`Revoke ${client.name}?`}
      description="The app loses access to Nugget straight away. To connect it again later, create a new token."
      onClose={onClose}
    >
      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose} disabled={busy} data-autofocus="">
          Cancel
        </button>
        <button type="button" className="btn btn-danger" onClick={() => void revoke()} disabled={busy}>
          {busy ? <Spinner label="Revoking" /> : "Revoke"}
        </button>
      </div>
    </Dialog>
  );
}
