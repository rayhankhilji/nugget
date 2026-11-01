/**
 * Desktop-app preferences backed by the Tauri shell: "open Nugget in the tray at login"
 * (`src-tauri/src/autostart.rs`), and (II4) OS notifications with quiet hours, what quitting does
 * to the background service (`src-tauri/src/platform.rs`), and the quick-ask shortcut. Distinct
 * from the background-service toggle above it.
 */
import { useEffect, useId, useState, type ReactNode } from "react";
import { autostart, errorMessage, platformApi, type AutostartStatus, type PlatformSettings, type QuietHours } from "../api";
import { useT } from "../i18n";
import { QuickAskSettings } from "./QuickAskSettings";
import { Spinner } from "./ui";

/** Default quiet hours when first turned on: 22:00–07:00. */
export const DEFAULT_QUIET: QuietHours = { start: 22 * 60, end: 7 * 60 };

/** Minutes after midnight as `HH:MM` (what `<input type="time">` takes). */
export function toTime(minutes: number): string {
  const m = ((Math.round(minutes) % 1440) + 1440) % 1440;
  return `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
}

/** `HH:MM` as minutes after midnight, or `null` if it isn't a time of day. */
export function fromTime(value: string): number | null {
  const m = /^(\d{1,2}):(\d{2})/.exec(value);
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  return h < 24 && min < 60 ? h * 60 + min : null;
}

export function Switch({
  checked,
  disabled,
  onChange,
  describedBy,
  children,
}: {
  checked: boolean;
  disabled?: boolean;
  onChange: (on: boolean) => void;
  describedBy?: string;
  children: ReactNode;
}) {
  return (
    <label className="switch">
      <input
        type="checkbox"
        role="switch"
        checked={checked}
        disabled={disabled}
        aria-describedby={describedBy}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="switch-track" aria-hidden="true" />
      <span>{children}</span>
    </label>
  );
}

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
      {/* A subsection of the Updates card (Status page). */}
      <h3 className="ask-sub">{t("appSettings.title")}</h3>
      {!status ? (
        error ? null : <Spinner />
      ) : status.supported ? (
        <>
          <Switch checked={status.enabled} disabled={busy} describedBy={hintId} onChange={(on) => void toggle(on)}>
            {t("appSettings.autostart")}
          </Switch>
          {busy && <Spinner label={t("updates.working")} />}
          <p id={hintId} className="muted small switch-hint">
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
      <PlatformPrefs />
      <QuickAskSettings />
    </>
  );
}

/** Notifications, quiet hours and quit behaviour (`platform.json`). */
export function PlatformPrefs() {
  const t = useT();
  const notifyHint = useId();
  const quitHint = useId();
  const fromId = useId();
  const toId = useId();
  const [settings, setSettings] = useState<PlatformSettings | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    platformApi
      .settingsGet()
      .then((s) => live && setSettings(s))
      .catch((e: unknown) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, []);

  if (!settings) return error ? <p className="muted small">{error}</p> : null;

  const save = async (next: PlatformSettings) => {
    const before = settings;
    setSettings(next);
    setError(null);
    try {
      setSettings(await platformApi.settingsSet(next));
    } catch (e) {
      setSettings(before);
      setError(errorMessage(e));
    }
  };
  const n = settings.notifications;
  const notify = (patch: Partial<typeof n>) => void save({ ...settings, notifications: { ...n, ...patch } });
  const quietTime = (which: "start" | "end", value: string) => {
    const m = fromTime(value);
    if (m !== null && n.quiet) notify({ quiet: { ...n.quiet, [which]: m } });
  };

  return (
    <>
      <h3 className="ask-sub">{t("appSettings.notifyHeading")}</h3>
      <Switch checked={n.enabled} describedBy={notifyHint} onChange={(enabled) => notify({ enabled })}>
        {t("appSettings.notify")}
      </Switch>
      <p id={notifyHint} className="muted small switch-hint">
        {t("appSettings.notifyHint")}
      </p>
      {n.enabled && (
        <fieldset className="plain-fieldset">
          <legend className="visually-hidden">{t("appSettings.notifyHeading")}</legend>
          <Switch checked={n.brief} onChange={(brief) => notify({ brief })}>
            {t("appSettings.notifyBrief")}
          </Switch>
          <Switch checked={n.agent} onChange={(agent) => notify({ agent })}>
            {t("appSettings.notifyAgent")}
          </Switch>
          <Switch checked={n.quiet !== null} onChange={(on) => notify({ quiet: on ? DEFAULT_QUIET : null })}>
            {t("appSettings.quiet")}
          </Switch>
          {n.quiet && (
            <div className="quiet-hours">
              <label htmlFor={fromId}>{t("appSettings.quietFrom")}</label>
              <input id={fromId} type="time" dir="ltr" value={toTime(n.quiet.start)} onChange={(e) => quietTime("start", e.target.value)} />
              <label htmlFor={toId}>{t("appSettings.quietTo")}</label>
              <input id={toId} type="time" dir="ltr" value={toTime(n.quiet.end)} onChange={(e) => quietTime("end", e.target.value)} />
            </div>
          )}
        </fieldset>
      )}
      <h3 className="ask-sub">{t("appSettings.quitHeading")}</h3>
      <Switch checked={settings.quit_stops_daemon} describedBy={quitHint} onChange={(on) => void save({ ...settings, quit_stops_daemon: on })}>
        {t("appSettings.quitStops")}
      </Switch>
      <p id={quitHint} className="muted small switch-hint">
        {t("appSettings.quitHint")}
      </p>
      {error && (
        <div className="note note-error" role="alert">
          {error}
        </div>
      )}
    </>
  );
}
