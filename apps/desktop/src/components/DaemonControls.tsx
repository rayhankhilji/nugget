/**
 * Status-page pieces for the background service's life around the app's
 * (`src-tauri/src/lifecycle.rs`):
 *
 * - {@link VersionBanner} (GG6-10): the service is another version than the app, and what Nugget
 *   did or what the user has to do about it.
 * - {@link NoTrayControls} (GG6-20): on desktops without a tray icon, the tray's controls the
 *   docs point to (pause indexing; the kill switch lives on the Safety page).
 */
import { useEffect, useState } from "react";
import { errorMessage, lifecycleApi, type ShellInfo, type VersionReport } from "../api";
import { useEvent } from "../hooks";
import { useT, type MessageKey } from "../i18n";
import { GoToPage } from "./GoToPage";

const MISMATCH: Record<NonNullable<VersionReport["reason"]>, MessageKey> = {
  not_owned: "status.versionCheck.notOwned",
  service_restart_failed: "status.versionCheck.serviceFailed",
  restart_failed: "status.versionCheck.restartFailed",
};

/** What the shell knows about the tray and the logs; `null` until loaded (or outside the app). */
export function useShellInfo(): [ShellInfo | null, (s: ShellInfo) => void] {
  const [info, setInfo] = useState<ShellInfo | null>(null);
  useEffect(() => {
    let live = true;
    lifecycleApi
      .shellInfo()
      .then((s) => live && setInfo(s))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  return [info, setInfo];
}

export function VersionBanner() {
  const t = useT();
  const [report, setReport] = useState<VersionReport | null>(null);
  const [checking, setChecking] = useState(false);
  useEffect(() => {
    let live = true;
    lifecycleApi
      .daemonVersion()
      .then((r) => live && setReport(r))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  useEvent(lifecycleApi.onDaemonVersion, setReport);

  if (!report || (report.state !== "mismatch" && report.state !== "restarting")) return null;
  const args = { app: report.app_version, daemon: report.daemon_version ?? "?", detail: report.detail ?? "?" };
  if (report.state === "restarting") {
    return (
      <p className="note" role="status">
        {t("status.versionCheck.restarting", args)}
      </p>
    );
  }
  const recheck = async () => {
    setChecking(true);
    try {
      setReport(await lifecycleApi.recheckVersion());
    } catch {
      // The event or the next check brings the current state.
    } finally {
      setChecking(false);
    }
  };
  return (
    <div className="note note-error" role="alert">
      <p>{t(MISMATCH[report.reason ?? "not_owned"], args)}</p>
      <button type="button" className="btn" onClick={() => void recheck()} disabled={checking}>
        {t("status.versionCheck.again")}
      </button>
    </div>
  );
}

/** Shown only when the desktop has no tray icon. */
export function NoTrayControls({ info, onChange }: { info: ShellInfo; onChange: (s: ShellInfo) => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (info.has_tray) return null;

  const toggle = async () => {
    setBusy(true);
    setError(null);
    try {
      onChange(await lifecycleApi.togglePause());
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="status-card" aria-labelledby="no-tray-title">
      <h2 id="no-tray-title">{t("status.noTray.title")}</h2>
      <p className="muted small">{t("status.noTray.body")}</p>
      {info.indexing_paused && <p>{t("status.noTray.paused")}</p>}
      <button type="button" className="btn" onClick={() => void toggle()} disabled={busy} aria-busy={busy}>
        {info.indexing_paused ? t("status.noTray.resume") : t("status.noTray.pause")}
      </button>
      {error && (
        <div className="note note-error" role="alert">
          {t("status.noTray.failed", { error })}
        </div>
      )}
      <p className="muted small">{t("status.noTray.agents")}</p>
      <GoToPage page="safety" className="btn btn-ghost" />
    </section>
  );
}
