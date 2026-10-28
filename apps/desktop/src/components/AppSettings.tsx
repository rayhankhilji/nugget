/**
 * Desktop-app preferences backed by the Tauri shell: currently "open Nugget in the tray at login"
 * (`src-tauri/src/autostart.rs`). Distinct from the background-service toggle above it.
 */
import { useEffect, useId, useState } from "react";
import { autostart, errorMessage, type AutostartStatus } from "../api";
import { useT } from "../i18n";
import { Spinner } from "./ui";

export function AppSettings() {
  const t = useT();
  const hintId = useId();
  const [status, setStatus] = useState<AutostartStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    autostart
      .get()
      .then((s) => live && setStatus(s))
      .catch((e: unknown) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, []);

  const toggle = async (enabled: boolean) => {
    setBusy(true);
    setError(null);
    try {
      setStatus(await autostart.set(enabled));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <h2>{t("appSettings.title")}</h2>
      {!status ? (
        error ? null : <Spinner />
      ) : status.supported ? (
        <>
          <label className="field-inline">
            <input
              type="checkbox"
              checked={status.enabled}
              disabled={busy}
              aria-describedby={hintId}
              onChange={(e) => void toggle(e.target.checked)}
            />
            {t("appSettings.autostart")}
          </label>
          {busy && <Spinner label={t("updates.working")} />}
          <p id={hintId} className="muted small">
            {t("appSettings.autostartHint")}
          </p>
        </>
      ) : (
        <p className="muted small">{t("appSettings.unsupported")}</p>
      )}
      {error && (
        <div className="note note-error" role="alert">
          {error}
        </div>
      )}
    </>
  );
}
