/**
 * II4: onboarding's opt-ins for the desktop app: open at login (hidden in the tray) and OS
 * notifications. Both start off; each switch saves at once. Shown only inside the app.
 */
import { useEffect, useId, useState } from "react";
import { autostart, errorMessage, platformApi, type AutostartStatus, type PlatformSettings } from "../api";
import { useT } from "../i18n";
import { Switch } from "../components/AppSettings";

export function DesktopOptIns() {
  const t = useT();
  const headingId = useId();
  const notifyHint = useId();
  const [login, setLogin] = useState<AutostartStatus | null>(null);
  const [prefs, setPrefs] = useState<PlatformSettings | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    autostart
      .get()
      .then((s) => live && setLogin(s))
      .catch(() => {});
    platformApi
      .settingsGet()
      .then((s) => live && setPrefs(s))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);

  if (!login?.supported && !prefs) return null;

  const setAutostart = async (on: boolean) => {
    setError(null);
    try {
      setLogin(await autostart.set(on));
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  const setNotify = async (enabled: boolean) => {
    if (!prefs) return;
    setError(null);
    try {
      setPrefs(await platformApi.settingsSet({ ...prefs, notifications: { ...prefs.notifications, enabled } }));
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  return (
    <section className="ob-card ob-desktop" aria-labelledby={headingId}>
      <h2 id={headingId}>{t("onboarding.done.desktopHeading")}</h2>
      {login?.supported && (
        <Switch checked={login.enabled} onChange={(on) => void setAutostart(on)}>
          {t("appSettings.autostart")}
        </Switch>
      )}
      {prefs && (
        <>
          <Switch checked={prefs.notifications.enabled} describedBy={notifyHint} onChange={(on) => void setNotify(on)}>
            {t("appSettings.notify")}
          </Switch>
          <p id={notifyHint} className="muted small switch-hint">
            {t("appSettings.notifyHint")}
          </p>
        </>
      )}
      <p className="muted small">{t("onboarding.done.desktopHint")}</p>
      {error && (
        <p className="note note-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
