/**
 * Updates and start-at-login, backed by `src-tauri/src/updater.rs`.
 *
 * Nothing installs without the user: the default is notify-only, the release notes are shown
 * before the Install button, and Install names the exact version shown. Auto-install is opt-in.
 * The desktop app's own preferences (`AppSettings`) render at the end of the same card.
 */
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errorMessage, inTauri } from "../api";
import { useT, type MessageKey } from "../i18n";
import { AppSettings } from "./AppSettings";
import { Badge, Spinner } from "./ui";

type Channel = "stable" | "beta" | "off";

interface UpdateStatus {
  current_version: string;
  configured: boolean;
  settings: { channel: Channel; auto_install: boolean; start_at_login: boolean | null };
  available: { version: string; notes: string | null; date: number | null } | null;
  staged: boolean;
  busy: boolean;
  last_checked: number | null;
  last_error: string | null;
}

interface Progress {
  downloaded: number;
  total: number | null;
}

const CHANNELS: { id: Channel; label: MessageKey }[] = [
  { id: "stable", label: "updates.channel.stable" },
  { id: "beta", label: "updates.channel.beta" },
  { id: "off", label: "updates.channel.off" },
];

const date = (secs: number) => new Date(secs * 1000);

export function UpdateSettings() {
  const t = useT();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  const run = useCallback(async (command: string, args?: Record<string, unknown>) => {
    setError(null);
    setWorking(true);
    try {
      const next = await invoke<UpdateStatus | null>(command, args);
      if (next) setStatus(next);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setWorking(false);
    }
  }, []);

  useEffect(() => {
    if (!inTauri()) return;
    void run("updates_status");
    const subs = [
      listen<UpdateStatus>("nugget:update-status", (e) => setStatus(e.payload)),
      listen<Progress>("nugget:update-progress", (e) => setProgress(e.payload)),
    ];
    return () => subs.forEach((p) => void p.then((un) => un()).catch(() => {}));
  }, [run]);

  if (!inTauri()) return null;
  if (!status) {
    return (
      <section className="status-card" aria-labelledby="updates-title">
        <h2 id="updates-title">{t("updates.title")}</h2>
        {error ? <div className="note note-error">{error}</div> : <Spinner />}
      </section>
    );
  }

  const { settings, available } = status;
  const busy = working || status.busy;
  const setSettings = (channel: Channel, autoInstall: boolean) =>
    void run("updates_set_settings", { channel, autoInstall });

  return (
    <section className="status-card" aria-labelledby="updates-title">
      <h2 id="updates-title">{t("updates.title")}</h2>
      <p className="muted small">{t("updates.intro", { version: status.current_version })}</p>

      {!status.configured ? (
        <div className="note small">{t("updates.unconfigured")}</div>
      ) : (
        <>
          <div className="row" style={{ justifyContent: "flex-start", alignItems: "center" }}>
            <label className="field-inline">
              {t("updates.channel")}
              <select value={settings.channel} disabled={busy} onChange={(e) => setSettings(e.target.value as Channel, settings.auto_install)}>
                {CHANNELS.map((c) => (
                  <option key={c.id} value={c.id}>
                    {t(c.label)}
                  </option>
                ))}
              </select>
            </label>
            <label className="switch">
              <input
                type="checkbox"
                role="switch"
                checked={settings.auto_install}
                disabled={busy || settings.channel === "off"}
                onChange={(e) => setSettings(settings.channel, e.target.checked)}
              />
              <span className="switch-track" aria-hidden="true" />
              <span>{t("updates.autoInstall")}</span>
            </label>
            <button type="button" className="btn btn-ghost btn-small" disabled={busy || settings.channel === "off"} onClick={() => void run("updates_check")}>
              {t("updates.checkNow")}
            </button>
            {busy && <Spinner label={t("updates.working")} />}
          </div>
          <p className="muted small">
            {status.last_checked ? t("updates.lastChecked", { when: date(status.last_checked) }) : t("updates.neverChecked")}
            {progress && busy && progress.total ? ` · ${t("updates.downloading", { progress: progress.downloaded / progress.total })}` : ""}
          </p>
          {status.last_error && <div className="note note-warn">{t("updates.problem", { error: status.last_error })}</div>}

          {available ? (
            <div className="note" role="status" style={{ display: "block" }}>
              <p>
                <strong>{t("updates.available", { version: available.version })}</strong>{" "}
                {settings.channel !== "off" && <Badge tone="accent">{t(`updates.badge.${settings.channel}`)}</Badge>}
                {available.date ? <span className="muted small"> · {t("updates.released", { when: date(available.date) })}</span> : null}
              </p>
              <h3 className="small">{t("updates.whatsNew")}</h3>
              <div className="small" style={{ whiteSpace: "pre-wrap", maxHeight: 240, overflow: "auto" }}>
                {available.notes?.trim() || t("updates.noNotes")}
              </div>
              <div className="row" style={{ justifyContent: "flex-start", marginTop: 8 }}>
                <button
                  type="button"
                  className="btn btn-primary btn-small"
                  disabled={busy}
                  onClick={() => void run("updates_install", { version: available.version })}
                >
                  {t("updates.install", { version: available.version })}
                </button>
                {status.staged && <span className="muted small">{t("updates.staged")}</span>}
              </div>
            </div>
          ) : (
            settings.channel !== "off" && status.last_checked && <p className="muted small">{t("updates.upToDate")}</p>
          )}
        </>
      )}

      <h3 className="ask-sub">{t("updates.login.title")}</h3>
      {settings.start_at_login === null ? (
        <div className="note small" style={{ display: "block" }}>
          <p>{t("updates.login.ask")}</p>
          <div className="row" style={{ justifyContent: "flex-start" }}>
            <button type="button" className="btn btn-primary btn-small" disabled={busy} onClick={() => void run("login_set", { enabled: true })}>
              {t("updates.login.yes")}
            </button>
            <button type="button" className="btn btn-ghost btn-small" disabled={busy} onClick={() => void run("login_set", { enabled: false })}>
              {t("updates.login.notNow")}
            </button>
          </div>
        </div>
      ) : (
        <label className="switch">
          <input
            type="checkbox"
            role="switch"
            checked={settings.start_at_login}
            disabled={busy}
            onChange={(e) => void run("login_set", { enabled: e.target.checked })}
          />
          <span className="switch-track" aria-hidden="true" />
          <span>{t("updates.login.toggle")}</span>
        </label>
      )}
      <AppSettings />
      {error && (
        <div className="note note-error" role="alert">
          {error}
        </div>
      )}
    </section>
  );
}
