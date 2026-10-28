import { useState } from "react";
import { api, errorMessage, inTauri, type ConnectionInfo } from "../api";
import { Logo } from "./Logo";
import { Spinner } from "./ui";

export function Offline({
  conn,
  onConnected,
  onRetry,
}: {
  conn: ConnectionInfo;
  onConnected: (c: ConnectionInfo) => void;
  onRetry: () => Promise<void>;
}) {
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = async () => {
    setStarting(true);
    setError(null);
    try {
      const c = await api.startDaemon();
      if (c.connected) onConnected(c);
      else setError(c.error ?? "Nugget did not start.");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setStarting(false);
    }
  };

  return (
    <section className="offline" aria-labelledby="offline-title">
      <Logo size={48} />
      <h1 id="offline-title">Nugget isn't running</h1>
      <p className="muted">
        The Nugget background service keeps your data and models on this device. Start it to chat, search and manage
        models.
      </p>
      {inTauri() ? (
        <div className="row">
          <button type="button" className="btn btn-primary" onClick={start} disabled={starting} autoFocus>
            {starting ? (
              <>
                <Spinner label="Starting" /> Starting…
              </>
            ) : (
              "Start Nugget"
            )}
          </button>
          <button type="button" className="btn" onClick={() => void onRetry()} disabled={starting}>
            Try again
          </button>
        </div>
      ) : (
        <p className="note">Open this page from the Nugget desktop app.</p>
      )}
      <div aria-live="polite">
        {error && (
          <p className="note note-error" role="alert">
            {error}
          </p>
        )}
      </div>
      {conn.socket && (
        <details className="small muted">
          <summary>Details</summary>
          <p>
            Socket: <code>{conn.socket}</code>
          </p>
          {conn.error && <p>Last error: {conn.error}</p>}
        </details>
      )}
    </section>
  );
}
