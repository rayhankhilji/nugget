import { useState } from "react";
import { api, errorMessage, inTauri, type ConnectionInfo } from "../api";
import { Trans, useT } from "../i18n";
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
  const t = useT();
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = async () => {
    setStarting(true);
    setError(null);
    try {
      const c = await api.startDaemon();
      if (c.connected) onConnected(c);
      else setError(c.error ?? t("offline.didNotStart"));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setStarting(false);
    }
  };

  return (
    <section className="offline" aria-labelledby="offline-title">
      <Logo size={48} />
      <h1 id="offline-title">{t("offline.title")}</h1>
      <p className="muted">{t("offline.body")}</p>
      {inTauri() ? (
        <div className="row">
          <button type="button" className="btn btn-primary" onClick={start} disabled={starting} autoFocus>
            {starting ? (
              <>
                <Spinner label={t("offline.startingLabel")} /> {t("offline.starting")}
              </>
            ) : (
              t("offline.start")
            )}
          </button>
          <button type="button" className="btn" onClick={() => void onRetry()} disabled={starting}>
            {t("offline.tryAgain")}
          </button>
        </div>
      ) : (
        <p className="note">{t("offline.openFromApp")}</p>
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
          <summary>{t("common.details")}</summary>
          <p>
            <Trans k="offline.socket" args={{ socket: conn.socket }} tags={{ code: (c) => <code>{c}</code> }} />
          </p>
          {conn.error && <p>{t("offline.lastError", { error: conn.error })}</p>}
        </details>
      )}
    </section>
  );
}
