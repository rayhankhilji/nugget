import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import {
  api,
  doubletapApi,
  errorMessage,
  isMethodNotFound,
  type DoubleTapModifier,
  type DoubleTapSettings,
  type DoubleTapStatus,
  type ProviderInfo,
  type ShortcutStatus,
  type TriggerSpec,
} from "../api";
import { useAsync, useEvent } from "../hooks";
import { formatRelative } from "../format";
import { GoToPage } from "../components/GoToPage";
import { Badge, ErrorNote, Loading, NotAvailable, PageHeader } from "../components/ui";
import { formatList, formatNumber, useT, type MessageKey } from "../i18n";
import {
  addApp,
  chordError,
  clampWindow,
  DEFAULT_WINDOW_MS,
  keyOrText,
  normalizeSettings,
  permissionTone,
  providerRows,
  removeApp,
  sameSettings,
  switchKind,
  triggerError,
  WINDOW_MAX_MS,
  WINDOW_MIN_MS,
} from "../doubletap";

type Notice = { text: string; error: boolean } | null;

export function DoubleTapPage() {
  const t = useT();
  const saved = useAsync(doubletapApi.settingsGet);
  const status = useAsync(doubletapApi.status);
  const [form, setForm] = useState<DoubleTapSettings | null>(null);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<Notice>(null);
  const [firing, setFiring] = useState(false);
  const [fireNotice, setFireNotice] = useState<Notice>(null);
  const readAheadHintId = useId();
  const tryHintId = useId();
  const tryOff = saved.data !== undefined && !saved.data.enabled;

  // Start editing from what's saved; a later reload doesn't clobber unsaved edits.
  useEffect(() => {
    if (saved.data && form === null) setForm(saved.data);
  }, [saved.data, form]);

  // Saved elsewhere (onboarding, another window): show it, unless there are unsaved edits.
  const savedRef = useRef(saved.data);
  savedRef.current = saved.data;
  useEvent(doubletapApi.onSettings, (next) => {
    setForm((f) => (f === null || (savedRef.current !== undefined && sameSettings(f, savedRef.current)) ? next : f));
    saved.reload();
    status.reload();
  });

  // While the desktop asks about the shortcut (or Nugget binds it again), check back so the page
  // flips to active once the user approves.
  const shortcutState = status.data?.shortcut?.state;
  useEffect(() => {
    if (shortcutState !== "awaiting_consent" && shortcutState !== "rebinding") return;
    const id = window.setInterval(status.reload, 2000);
    return () => window.clearInterval(id);
  }, [shortcutState, status.reload]);

  const dirty = form !== null && saved.data !== undefined && !sameSettings(form, saved.data);
  const invalid = form !== null && triggerError(form.trigger) !== null;

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!form || invalid) return;
    setSaving(true);
    setNotice(null);
    try {
      const next = await doubletapApi.settingsSet(normalizeSettings(form));
      setForm(next);
      saved.reload();
      status.reload();
      setNotice({ text: t("doubletap.saved"), error: false });
    } catch (err) {
      setNotice({ text: t("doubletap.saveFailed", { error: errorMessage(err) }), error: true });
    } finally {
      setSaving(false);
    }
  };

  const fire = async () => {
    setFiring(true);
    setFireNotice(null);
    try {
      await doubletapApi.fire();
      setFireNotice({ text: t("doubletap.try.started"), error: false });
    } catch (err) {
      setFireNotice({ text: t("doubletap.try.failed", { error: errorMessage(err) }), error: true });
    } finally {
      setFiring(false);
    }
  };

  return (
    <div className="page dt-page">
      <PageHeader
        title={t("doubletap.title")}
        description={t("doubletap.description")}
        actions={
          // aria-disabled keeps it focusable, so keyboard users reach the reason it can't run.
          <button
            type="button"
            className="btn"
            onClick={() => {
              if (!firing && saved.data?.enabled) void fire();
            }}
            aria-disabled={firing || !saved.data?.enabled || undefined}
            aria-describedby={tryOff ? tryHintId : undefined}
          >
            {t("doubletap.try.button")}
          </button>
        }
      />
      {tryOff && (
        <p id={tryHintId} className="muted small dt-try-hint">
          {t("doubletap.try.offHint")}
        </p>
      )}
      {fireNotice && (
        <p className={fireNotice.error ? "note note-error" : "note"} role="status">
          {fireNotice.text}
        </p>
      )}

      {isMethodNotFound(saved.error) ? (
        <NotAvailable what={t("doubletap.title")} />
      ) : (
        <ErrorNote error={saved.error} onRetry={saved.reload} />
      )}
      {saved.loading && !saved.data && <Loading label={t("doubletap.loading")} />}

      {form && (
        <form onSubmit={(e) => void save(e)} noValidate>
          <section aria-labelledby="dt-general">
            <h2 id="dt-general">{t("doubletap.general")}</h2>
            <div className="switch-stack">
            <label className="switch">
              <input
                type="checkbox"
                role="switch"
                checked={form.enabled}
                onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
              />
              <span className="switch-track" aria-hidden="true" />
              <span>{t("doubletap.enabled")}</span>
            </label>
            {/* Read-ahead only matters while Double Tap is on: nested under it. */}
            <div className="switch-child">
            <label className="switch">
              <input
                type="checkbox"
                role="switch"
                checked={form.read_ahead ?? true}
                aria-describedby={readAheadHintId}
                onChange={(e) => setForm({ ...form, read_ahead: e.target.checked })}
              />
              <span className="switch-track" aria-hidden="true" />
              <span>{t("doubletap.readAhead")}</span>
            </label>
            <p id={readAheadHintId} className="muted small">
              {t("doubletap.readAheadHint")}
            </p>
            </div>
            </div>
          </section>

          <TriggerPicker
            trigger={form.trigger}
            status={status.data}
            onChange={(trigger) => setForm({ ...form, trigger })}
          />

          <ProviderTable
            map={form.app_providers}
            onChange={(app_providers) => setForm({ ...form, app_providers })}
          />

          <ExcludedApps list={form.excluded_apps} onChange={(excluded_apps) => setForm({ ...form, excluded_apps })} />
          <label className="switch dt-terminals">
            <input
              type="checkbox"
              role="switch"
              checked={form.allow_terminals}
              onChange={(e) => setForm({ ...form, allow_terminals: e.target.checked })}
            />
            <span className="switch-track" aria-hidden="true" />
            <span>{t("doubletap.excluded.allowTerminals")}</span>
          </label>

          <div className="inline-row dt-save">
            <button type="submit" className="btn btn-primary" disabled={!dirty || invalid || saving}>
              {saving ? t("doubletap.saving") : t("doubletap.save")}
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              disabled={!dirty || saving}
              onClick={() => {
                if (saved.data) setForm(saved.data);
                setNotice(null);
              }}
            >
              {t("doubletap.revert")}
            </button>
            <span className={notice?.error ? "field-error" : "muted small"} role="status">
              {notice?.text ?? (dirty ? t("doubletap.unsaved") : "")}
            </span>
          </div>
        </form>
      )}

      <PermissionsCard status={status.data} error={status.error} loading={status.loading} onUpdate={status.reload} />
      <VoiceCard />
    </div>
  );
}

// ---- trigger ----

function TriggerPicker({
  trigger,
  status,
  onChange,
}: {
  trigger: TriggerSpec;
  status: DoubleTapStatus | undefined;
  onChange: (t: TriggerSpec) => void;
}) {
  const t = useT();
  const name = useId();
  const keyId = useId();
  const windowId = useId();
  const chordId = useId();
  const chordErrId = useId();
  const listenErrId = useId();
  // Remember both halves so switching kinds back and forth keeps the choice.
  const memory = useRef<{ modifier: DoubleTapModifier; window_ms: number; chord: string }>({
    modifier: trigger.kind === "double_tap" ? trigger.modifier : (status?.modifiers[0]?.id ?? "right_control"),
    window_ms: trigger.kind === "double_tap" ? trigger.window_ms : DEFAULT_WINDOW_MS,
    chord: trigger.kind === "chord" ? trigger.chord : "",
  });
  const set = (next: TriggerSpec) => {
    if (next.kind === "double_tap") memory.current = { ...memory.current, modifier: next.modifier, window_ms: next.window_ms };
    else memory.current = { ...memory.current, chord: next.chord };
    onChange(next);
  };

  const modifiers = status?.modifiers ?? [];
  const options =
    trigger.kind === "double_tap" && !modifiers.some((m) => m.id === trigger.modifier)
      ? [...modifiers, { id: trigger.modifier, label: trigger.modifier }]
      : modifiers;
  const chordErr = trigger.kind === "chord" ? chordError(trigger.chord) : null;
  const recommended = status?.recommended_trigger;

  return (
    <section aria-labelledby="dt-trigger">
      <h2 id="dt-trigger">{t("doubletap.trigger.title")}</h2>
      <fieldset className="field">
        <legend className="visually-hidden">{t("doubletap.trigger.kind")}</legend>
        <div className="kind-picker">
          <label className="kind-option">
            <input
              type="radio"
              name={name}
              checked={trigger.kind === "double_tap"}
              onChange={() => set(switchKind(trigger, "double_tap", memory.current))}
            />
            <span>
              <span className="kind-label">{t("doubletap.trigger.doubleTap")}</span>
              <span className="muted small">{t("doubletap.trigger.doubleTapBlurb")}</span>
            </span>
          </label>
          <label className="kind-option">
            <input
              type="radio"
              name={name}
              checked={trigger.kind === "chord"}
              onChange={() => set(switchKind(trigger, "chord", memory.current))}
            />
            <span>
              <span className="kind-label">{t("doubletap.trigger.chord")}</span>
              <span className="muted small">{t("doubletap.trigger.chordBlurb")}</span>
            </span>
          </label>
        </div>
      </fieldset>

      {trigger.kind === "double_tap" ? (
        <div className="inline-row">
          <div className="field">
            <label htmlFor={keyId}>{t("doubletap.trigger.key")}</label>
            <select
              id={keyId}
              value={trigger.modifier}
              onChange={(e) => set({ ...trigger, modifier: e.target.value as DoubleTapModifier })}
              aria-describedby={status?.trigger_error ? listenErrId : undefined}
            >
              {options.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor={windowId}>{t("doubletap.trigger.window")}</label>
            <input
              id={windowId}
              type="number"
              min={WINDOW_MIN_MS}
              max={WINDOW_MAX_MS}
              step={10}
              value={trigger.window_ms}
              onChange={(e) => set({ ...trigger, window_ms: Number(e.target.value) })}
              onBlur={() => set({ ...trigger, window_ms: clampWindow(trigger.window_ms) })}
            />
          </div>
        </div>
      ) : (
        <div className="field">
          <label htmlFor={chordId}>{t("doubletap.trigger.combination")}</label>
          <input
            id={chordId}
            type="text"
            value={trigger.chord}
            placeholder={t("doubletap.trigger.chordPlaceholder")}
            spellCheck={false}
            autoComplete="off"
            aria-invalid={chordErr !== null}
            aria-describedby={[chordErr ? chordErrId : "", status?.trigger_error ? listenErrId : ""].filter(Boolean).join(" ") || undefined}
            onChange={(e) => set({ kind: "chord", chord: e.target.value })}
          />
          {chordErr && (
            <p id={chordErrId} className="field-error small">
              {t(chordErr)}
            </p>
          )}
        </div>
      )}

      {status?.trigger_error && (
        <p id={listenErrId} className="field-error" role="alert">
          {t("doubletap.trigger.listenError", { error: keyOrText(status.trigger_error) })}
        </p>
      )}
      <ShortcutNote shortcut={status?.shortcut} />
      {recommended && (
        <button type="button" className="btn btn-ghost btn-small" onClick={() => set(recommended)}>
          {t("doubletap.trigger.useRecommended")}
        </button>
      )}
    </section>
  );
}

/** Where the desktop's shortcut stands (Wayland portal); nothing when it is simply active. */
function ShortcutNote({ shortcut }: { shortcut: ShortcutStatus | null | undefined }) {
  const t = useT();
  if (!shortcut) return null;
  if (shortcut.state === "failed") {
    return (
      <p className="field-error" role="alert">
        {t("doubletap.shortcut.failed", { error: keyOrText(shortcut.error ?? "") })}
      </p>
    );
  }
  let text: string | null = null;
  if (shortcut.state === "awaiting_consent") text = t("doubletap.shortcut.awaiting_consent");
  else if (shortcut.state === "not_assigned") text = t("doubletap.shortcut.not_assigned");
  else if (shortcut.state === "rebinding") text = t("doubletap.shortcut.rebinding");
  else if (shortcut.state === "active" && shortcut.trigger) text = t("doubletap.shortcut.active_desktop", { trigger: shortcut.trigger });
  return text ? (
    <p className="note" role="status">
      {text}
    </p>
  ) : null;
}

// ---- provider per app ----

function ProviderTable({ map, onChange }: { map: Record<string, string>; onChange: (m: Record<string, string>) => void }) {
  const t = useT();
  const providers = useAsync(api.providersList);
  const appId = useId();
  const providerId = useId();
  const [app, setApp] = useState("");
  const [provider, setProvider] = useState("");
  const rows = providerRows(map);
  const ids = (providers.data ?? []).filter((p: ProviderInfo) => !p.error).map((p) => p.id);
  const choices = (current: string) => (current && !ids.includes(current) ? [current, ...ids] : ids);
  const chosen = provider || ids[0] || "";

  const add = (e: FormEvent) => {
    e.preventDefault();
    const a = app.trim();
    if (!a || !chosen) return;
    onChange({ ...map, [a]: chosen });
    setApp("");
  };

  return (
    <section aria-labelledby="dt-providers">
      <h2 id="dt-providers">{t("doubletap.providers.title")}</h2>
      <p className="muted small">{t("doubletap.providers.description")}</p>
      <ErrorNote error={providers.error} onRetry={providers.reload} />
      {providers.loading && !providers.data && <Loading label={t("doubletap.providers.loading")} lines={1} />}
      {providers.data && ids.length === 0 && rows.length === 0 && (
        <div className="note dt-no-models">
          <span>{t("doubletap.providers.noModels")}</span>
          <GoToPage page="models" className="btn" />
        </div>
      )}
      {rows.length === 0 ? (
        providers.data && ids.length > 0 && <p className="muted small dt-empty-line">{t("doubletap.providers.empty")}</p>
      ) : (
        <ul className="path-list">
          {rows.map((r) => (
            <li key={r.app}>
              <bdi dir="ltr">
                <code>{r.app}</code>
              </bdi>
              <span className="inline-row">
                <select
                  aria-label={t("doubletap.providers.providerFor", { app: r.app })}
                  value={r.provider}
                  onChange={(e) => onChange({ ...map, [r.app]: e.target.value })}
                >
                  {choices(r.provider).map((id) => (
                    <option key={id} value={id}>
                      {id}
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  className="btn btn-ghost btn-small"
                  aria-label={t("doubletap.removeApp", { app: r.app })}
                  onClick={() => {
                    const next = { ...map };
                    delete next[r.app];
                    onChange(next);
                  }}
                >
                  {t("doubletap.remove")}
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}
      {/* Not a nested <form>: the page is one form, so Enter here adds the row instead of saving. */}
      {ids.length > 0 && (
      <div
        className="inline-row dt-add-row"
        onKeyDown={(e) => {
          if (e.key === "Enter" && e.target instanceof HTMLInputElement) add(e);
        }}
      >
        <label className="visually-hidden" htmlFor={appId}>
          {t("doubletap.providers.app")}
        </label>
        <input
          id={appId}
          type="text"
          value={app}
          placeholder={t("doubletap.providers.appPlaceholder")}
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => setApp(e.target.value)}
        />
        <label className="visually-hidden" htmlFor={providerId}>
          {t("doubletap.providers.provider")}
        </label>
        <select id={providerId} value={chosen} onChange={(e) => setProvider(e.target.value)} disabled={ids.length === 0}>
          {ids.map((id) => (
            <option key={id} value={id}>
              {id}
            </option>
          ))}
        </select>
        <button type="button" className="btn" disabled={!app.trim() || !chosen} onClick={add}>
          {t("doubletap.add")}
        </button>
      </div>
      )}
    </section>
  );
}

// ---- excluded apps ----

function ExcludedApps({ list, onChange }: { list: string[]; onChange: (l: string[]) => void }) {
  const t = useT();
  const inputId = useId();
  const [value, setValue] = useState("");
  const [restoreError, setRestoreError] = useState<unknown>(null);
  // The shell's built-in list (password managers, banking). It is saved in `excluded_apps` like
  // the user's own entries; here it is shown apart and read-only. An older shell without the
  // command, or a failed call, shows every entry as the user's own, as before.
  const defaults = useAsync(doubletapApi.defaultExcluded);
  const builtIn = defaults.data ?? [];
  const isBuiltIn = (a: string) => builtIn.some((d) => d.toLowerCase() === a.toLowerCase());
  const activeBuiltIn = list.filter(isBuiltIn);
  const own = list.filter((a) => !isBuiltIn(a));
  const removed = builtIn.filter((d) => !list.some((a) => a.toLowerCase() === d.toLowerCase())).length;

  const add = (e: { preventDefault: () => void }) => {
    e.preventDefault();
    if (!value.trim()) return;
    onChange(addApp(list, value));
    setValue("");
  };

  const restore = async () => {
    setRestoreError(null);
    try {
      const defaults = await doubletapApi.defaultExcluded();
      onChange(defaults.reduce((acc, a) => addApp(acc, a), list));
    } catch (e) {
      setRestoreError(e);
    }
  };

  return (
    <section aria-labelledby="dt-excluded">
      <h2 id="dt-excluded">{t("doubletap.excluded.title")}</h2>
      <p className="muted small">{t("doubletap.excluded.description")}</p>
      <ErrorNote error={restoreError} />
      {builtIn.length > 0 && (
        <>
          <h3>{t("doubletap.excluded.builtInTitle")}</h3>
          <p className="muted small">{t("doubletap.excluded.builtInBody")}</p>
          {activeBuiltIn.length > 0 && (
            <details className="help">
              <summary>{t("doubletap.excluded.builtInShow", { n: activeBuiltIn.length })}</summary>
              <ul className="path-list" aria-label={t("doubletap.excluded.builtInTitle")}>
                {activeBuiltIn.map((a) => (
                  <li key={a}>
                    <bdi dir="ltr">
                      <code>{a}</code>
                    </bdi>
                    <Badge>{t("doubletap.excluded.builtInBadge")}</Badge>
                  </li>
                ))}
              </ul>
            </details>
          )}
          {removed > 0 && (
            <p className="note" role="note">
              <span>{t("doubletap.excluded.builtInRemoved", { n: removed })}</span>
              <button type="button" className="btn btn-ghost btn-small" onClick={() => void restore()}>
                {t("doubletap.excluded.restore")}
              </button>
            </p>
          )}
          <h3>{t("doubletap.excluded.ownTitle")}</h3>
        </>
      )}
      {own.length === 0 ? (
        <p className="muted small dt-empty-line">{builtIn.length > 0 ? t("doubletap.excluded.ownEmpty") : t("doubletap.excluded.empty")}</p>
      ) : (
        <ul className="path-list">
          {own.map((a) => (
            <li key={a}>
              <bdi dir="ltr">
                <code>{a}</code>
              </bdi>
              <button
                type="button"
                className="btn btn-ghost btn-small"
                aria-label={t("doubletap.removeApp", { app: a })}
                onClick={() => onChange(removeApp(list, a))}
              >
                {t("doubletap.remove")}
              </button>
            </li>
          ))}
        </ul>
      )}
      <div
        className="inline-row dt-add-row"
        onKeyDown={(e) => {
          if (e.key === "Enter" && e.target instanceof HTMLInputElement) add(e);
        }}
      >
        <label className="visually-hidden" htmlFor={inputId}>
          {t("doubletap.excluded.app")}
        </label>
        <input
          id={inputId}
          type="text"
          value={value}
          placeholder={t("doubletap.providers.appPlaceholder")}
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => setValue(e.target.value)}
        />
        <button type="button" className="btn" disabled={!value.trim()} onClick={add}>
          {t("doubletap.add")}
        </button>
        {builtIn.length === 0 && (
          <button type="button" className="btn btn-ghost" onClick={() => void restore()}>
            {t("doubletap.excluded.restore")}
          </button>
        )}
      </div>
    </section>
  );
}

// ---- permissions ----

/** Display-server / platform names as people write them (`x11` → X11). Unknown values: first letter up. */
const SESSION_NAMES: Record<string, string> = { x11: "X11", wayland: "Wayland", macos: "macOS", windows: "Windows" };
function sessionLabel(session: string): string {
  return SESSION_NAMES[session.toLowerCase()] ?? session.charAt(0).toLocaleUpperCase() + session.slice(1);
}

const ACCESS_LABEL: Record<string, MessageKey> = {
  granted: "doubletap.perm.granted",
  denied: "doubletap.perm.denied",
  not_required: "doubletap.perm.notRequired",
  unknown: "doubletap.perm.unknown",
};

function PermissionsCard({
  status,
  error,
  loading,
  onUpdate,
}: {
  status: DoubleTapStatus | undefined;
  error: unknown;
  loading: boolean;
  onUpdate: () => void;
}) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [requestError, setRequestError] = useState<unknown>(null);
  const [latest, setLatest] = useState<DoubleTapStatus | null>(null);
  const s = latest ?? status;
  useEffect(() => setLatest(null), [status]);

  const request = async () => {
    setBusy(true);
    setRequestError(null);
    try {
      setLatest(await doubletapApi.requestPermissions());
      onUpdate();
    } catch (e) {
      setRequestError(e);
    } finally {
      setBusy(false);
    }
  };

  const perm = (state: string) => t(ACCESS_LABEL[state] ?? "doubletap.perm.unknown");
  const needsPrompt = s && (s.platform === "macos" || s.accessibility === "denied" || s.input_monitoring === "denied");

  return (
    <section className="status-card" aria-labelledby="dt-perm">
      <h2 id="dt-perm">{t("doubletap.perm.title")}</h2>
      {isMethodNotFound(error) ? <NotAvailable /> : <ErrorNote error={error} onRetry={onUpdate} />}
      {loading && !s && <Loading label={t("doubletap.perm.loading")} lines={1} />}
      {s && (
        <>
          <dl className="stats">
            <div>
              <dt>{t("doubletap.perm.listener")}</dt>
              <dd className="stat-text">
                <Badge tone={s.listening ? "good" : "warn"}>
                  {s.listening ? t("doubletap.perm.listening") : t("doubletap.perm.notListening")}
                </Badge>
              </dd>
            </div>
            {s.accessibility !== "not_required" && (
              <div>
                <dt>{t("doubletap.perm.accessibility")}</dt>
                <dd className="stat-text">
                  <Badge tone={permissionTone(s.accessibility)}>{perm(s.accessibility)}</Badge>
                </dd>
              </div>
            )}
            {s.input_monitoring !== "not_required" && (
              <div>
                <dt>{t("doubletap.perm.inputMonitoring")}</dt>
                <dd className="stat-text">
                  <Badge tone={permissionTone(s.input_monitoring)}>{perm(s.input_monitoring)}</Badge>
                </dd>
              </div>
            )}
            <div>
              <dt>{t("doubletap.perm.session")}</dt>
              <dd className="stat-text">{sessionLabel(s.session)}</dd>
            </div>
          </dl>
          {s.platform === "macos" && (
            <>
              <p className="muted small">{t("doubletap.perm.accessibilityWhy")}</p>
              <p className="muted small">{t("doubletap.perm.inputMonitoringWhy")}</p>
            </>
          )}
          {s.hint && (
            <p className="note" role="note">
              {keyOrText(s.hint)}
            </p>
          )}
          <ErrorNote error={requestError} />
          {needsPrompt && (
            <button type="button" className="btn" onClick={() => void request()} disabled={busy}>
              {t("doubletap.perm.request")}
            </button>
          )}
        </>
      )}
    </section>
  );
}

// ---- voice ----

function VoiceCard() {
  const t = useT();
  const voice = useAsync(doubletapApi.voiceStatus);
  const v = voice.data;
  return (
    <section className="status-card" aria-labelledby="dt-voice">
      <h2 id="dt-voice">{t("doubletap.voice.title")}</h2>
      {isMethodNotFound(voice.error) ? <NotAvailable /> : <ErrorNote error={voice.error} onRetry={voice.reload} />}
      {voice.loading && !v && <Loading label={t("doubletap.voice.loading")} lines={1} />}
      {v && (
        <>
          <dl className="stats">
            <div>
              <dt>{t("doubletap.voice.samples")}</dt>
              <dd>{formatNumber(v.samples)}</dd>
            </div>
            <div>
              <dt>{t("doubletap.voice.channels")}</dt>
              <dd className="stat-text">{v.channels.length ? formatList(v.channels) : t("doubletap.voice.noChannels")}</dd>
            </div>
            <div>
              <dt>{t("doubletap.voice.lastBuilt")}</dt>
              <dd className="stat-text">{formatRelative(v.last_built)}</dd>
            </div>
          </dl>
          {v.samples === 0 && (
            <div className="note dt-no-models">
              <span>{t("doubletap.voice.empty")}</span>
              <GoToPage page="sources" className="btn" />
            </div>
          )}
        </>
      )}
    </section>
  );
}
