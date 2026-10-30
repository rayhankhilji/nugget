import { useEffect, useRef, useState } from "react";
import { api, errorMessage, inTauri, type ConnectionInfo } from "../api";
import { Trans, useT } from "../i18n";
import { Logo } from "./Logo";

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
  const [checking, setChecking] = useState(false);
  // Set after a manual check that found the service still down; cleared on the next attempt.
  const [stillDown, setStillDown] = useState(false);
  const mounted = useRef(true);
  useEffect(
    () => () => {
      mounted.current = false;
    },
    [],
  );

  const start = async () => {
    setStarting(true);
    setError(null);
    setStillDown(false);
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

  // Once connected, App swaps this screen out; still here afterwards means it's still down.
  const retry = async () => {
    setChecking(true);
    setStillDown(false);
    try {
      await onRetry();
    } finally {
      if (mounted.current) {
        setChecking(false);
        setStillDown(true);
      }
    }
  };

  return (
    <section className="offline" aria-labelledby="offline-title" aria-busy={starting || checking}>
      <Logo size={48} />
      <h1 id="offline-title">{t("offline.title")}</h1>
      <p className="muted">{t("offline.body")}</p>
      {inTauri() ? (
        <div className="row">
          <button type="button" className="btn btn-primary" onClick={start} disabled={starting} autoFocus>
            {starting ? (
              <>
                {/* The visible text names the state; the spinner is decoration. */}
                <span className="spinner" aria-hidden="true" /> {t("offline.starting")}
              </>
            ) : (
              t("offline.start")
            )}
          </button>
          <button type="button" className="btn" onClick={() => void retry()} disabled={starting || checking}>
            {t("offline.tryAgain")}
          </button>
        </div>
      ) : (
        <p className="note">{t("offline.openFromApp")}</p>
      )}
      {/* role=alert announces on insertion; no extra live region, so it is read once. */}
      <div>
        {error && (
          <p className="note note-error" role="alert">
            <span>{t("offline.startFailed", { error })}</span> <span>{t("offline.startFailedHint")}</span>
          </p>
        )}
      </div>
      <p className="small muted" role="status">
        {checking ? t("offline.checking") : stillDown ? t("offline.stillDown") : ""}
      </p>
      {inTauri() && (
        <details className="small">
          <summary>{t("offline.manual.summary")}</summary>
          <p>
            <Trans k="offline.manual.body" tags={{ code: (c) => <code>{c}</code> }} />
          </p>
          <p className="muted">{t("offline.manual.auto")}</p>
        </details>
      )}
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
