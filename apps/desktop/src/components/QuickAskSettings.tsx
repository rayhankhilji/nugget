/**
 * Quick ask preferences (`src-tauri/src/quickask.rs`): on/off and the global shortcut, with
 * whether the shortcut is registered. On Wayland Nugget can't register it, so this explains the
 * desktop key binding to set up instead.
 */
import { useEffect, useId, useState, type FormEvent } from "react";
import { QUICK_ASK_COMMAND, errorMessage, quickAskApi, type QuickAskSettings as Settings, type QuickAskView } from "../api";
import { useT } from "../i18n";
import { Spinner } from "./ui";

export function QuickAskSettings() {
  const t = useT();
  const hintId = useId();
  const shortcutId = useId();
  const [view, setView] = useState<QuickAskView | null>(null);
  const [shortcut, setShortcut] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    quickAskApi
      .settingsGet()
      .then((v) => {
        if (!live) return;
        setView(v);
        setShortcut(v.settings.shortcut);
      })
      .catch((e: unknown) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, []);

  const save = async (settings: Settings) => {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const v = await quickAskApi.settingsSet(settings);
      setView(v);
      setShortcut(v.settings.shortcut);
      setSaved(true);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  // LL2: controls stay enabled (aria-disabled) while saving or unchanged, so keyboard focus stays
  // on them instead of dropping to the page; these guards stand in for `disabled`.
  const unchanged = !shortcut.trim() || shortcut.trim() === view?.settings.shortcut;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (view && !busy && !unchanged) void save({ ...view.settings, shortcut: shortcut.trim() });
  };

  if (!view) {
    return (
      <>
        <h3 className="ask-sub">{t("quickAsk.settings.title")}</h3>
        {error ? (
          <div className="note note-error" role="alert">
            {error}
          </div>
        ) : (
          <Spinner />
        )}
      </>
    );
  }

  const { settings, status } = view;
  return (
    <>
      <h3 className="ask-sub">{t("quickAsk.settings.title")}</h3>
      <label className="switch">
        <input
          type="checkbox"
          role="switch"
          checked={settings.enabled}
          aria-disabled={busy || undefined}
          aria-describedby={hintId}
          onChange={(e) => busy || void save({ ...settings, enabled: e.target.checked })}
        />
        <span className="switch-track" aria-hidden="true" />
        <span>{t("quickAsk.settings.enabled")}</span>
      </label>
      <p id={hintId} className="muted small switch-hint">
        {t("quickAsk.settings.description")}
      </p>
      {status.wayland_fallback ? (
        <div className="note" role="status">
          <p>{t("quickAsk.settings.wayland")}</p>
          <code>{QUICK_ASK_COMMAND}</code>
        </div>
      ) : (
        <form className="field-inline" onSubmit={submit}>
          <label htmlFor={shortcutId}>{t("quickAsk.settings.shortcut")}</label>
          <input
            id={shortcutId}
            type="text"
            dir="ltr"
            autoComplete="off"
            spellCheck={false}
            value={shortcut}
            readOnly={busy}
            disabled={!settings.enabled}
            aria-describedby={`${shortcutId}-hint`}
            onChange={(e) => {
              setShortcut(e.target.value);
              setSaved(false);
            }}
          />
          <button type="submit" className="btn btn-small" disabled={!settings.enabled} aria-disabled={busy || unchanged || undefined}>
            {t("quickAsk.settings.save")}
          </button>
          <span id={`${shortcutId}-hint`} className="muted small">
            {t("quickAsk.settings.shortcutHint")}
          </span>
        </form>
      )}
      {busy && <Spinner label={t("updates.working")} />}
      {!status.wayland_fallback && (
        <p className="muted small" role="status">
          {status.listening
            ? t("quickAsk.settings.listening", { shortcut: settings.shortcut })
            : status.error
              ? t("quickAsk.settings.notListening", { error: status.error })
              : settings.enabled
                ? null
                : t("quickAsk.settings.off")}
          {saved && <> {t("quickAsk.settings.saved")}</>}
        </p>
      )}
      {error && (
        <div className="note note-error" role="alert">
          {error}
        </div>
      )}
    </>
  );
}
