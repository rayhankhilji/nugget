import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import {
  aiKeysApi,
  api,
  ENV_NAME_RE,
  type AiKeyAddResult,
  type AiKeyCheckResult,
  type AiKeyKind,
  type AiKeyOutcome,
  type AiKeySource,
} from "../api";
import { useT, type MessageKey } from "../i18n";
import { ErrorNote } from "../components/ui";
import { CUSTOM_MODEL_EXAMPLE, isLoopback, PRESETS, preset, type Preset, type PresetId } from "./presets";

/** The provider choice of the onboarding guide: a preset. */
export type ByokGuideKind = PresetId;

const KIND_LABELS: Record<AiKeyKind, MessageKey> = {
  anthropic: "onboarding.byok.provider.anthropic",
  openai_responses: "onboarding.byok.provider.openai",
  openai_compatible: "onboarding.byok.provider.compatible",
};

/** The label of a provider kind (for lists of saved providers). */
export function kindLabel(kind: AiKeyKind): MessageKey {
  return KIND_LABELS[kind];
}

/** A preset's name: its brand, or the custom entry's message. */
export function presetLabel(t: ReturnType<typeof useT>, p: Preset): string {
  return p.label ?? t("onboarding.byok.provider.compatible");
}

const NAME_RE = /^[a-z0-9_-]{1,32}$/;

/** A variable name to suggest for provider `name` (as the daemon does): `NUGGET_OPENAI_KEY`. */
export function suggestedEnv(name: string): string {
  return `NUGGET_${name.replace(/[^A-Za-z0-9]/g, "_").toUpperCase()}_KEY`;
}

const REASON_KEYS: Record<Exclude<AiKeyOutcome, "ok" | "no_keychain">, MessageKey> = {
  bad_key: "onboarding.byok.form.error.bad_key",
  wrong_url: "onboarding.byok.form.error.wrong_url",
  no_network: "onboarding.byok.form.error.no_network",
  server_error: "onboarding.byok.form.error.server_error",
  exists: "onboarding.byok.form.error.exists",
  env_missing: "onboarding.byok.form.error.env_missing",
  not_found: "onboarding.byok.form.error.not_found",
  not_editable: "onboarding.byok.form.error.not_editable",
  reserved: "onboarding.byok.form.error.reserved",
};

type Outcome =
  | { type: "ok"; count: number }
  | { type: "saved" }
  | { type: "replaced" }
  | { type: "failed"; reason: AiKeyOutcome }
  | { type: "invalid"; key: MessageKey }
  | { type: "error"; error: unknown };

/** The key typed in, or an environment variable that holds it (the fallback without a keychain). */
interface KeyEntry {
  mode: "key" | "env";
  key: string;
  env: string;
}

/** The request's key part, or why it cannot be sent. `keyless`: a server on this computer may have none. */
function keySource(e: KeyEntry, keyless = false): AiKeySource | MessageKey {
  if (e.mode === "env") {
    const v = e.env.trim();
    return ENV_NAME_RE.test(v) ? { key_ref: `env:${v}` } : "onboarding.byok.form.error.envName";
  }
  if (!e.key.trim() && keyless) return {};
  return e.key.trim() ? { api_key: e.key.trim() } : "onboarding.byok.form.error.key";
}

/** The alert for a failed outcome (`no_keychain` names the variable to use). */
export function failureText(t: ReturnType<typeof useT>, reason: AiKeyOutcome, env: string): string | null {
  if (reason === "ok") return null;
  if (reason === "no_keychain") return t("onboarding.byok.form.error.no_keychain", { name: env });
  return t(REASON_KEYS[reason] ?? "onboarding.byok.form.error.server_error");
}

/**
 * Paste a key, check it with the provider (`ai.key.test`) and save it (`ai.key.save`: OS keychain
 * plus a `keychain:NAME` provider in config.toml; usable at once). A taken name offers to replace
 * that provider's key (`ai.key.update`); without a keychain the form switches to an environment
 * variable (`env:NAME`). Shown under the BYOK guide and on the Models page; `kind` follows the
 * guide's provider select, otherwise the form has its own.
 */
export function ByokForm({ kind, onSaved }: { kind?: ByokGuideKind; onSaved?: (provider: string) => void }) {
  const t = useT();
  const selectId = useId();
  const [own, setOwn] = useState<ByokGuideKind>("anthropic");
  const current = preset(kind ?? own);
  return (
    <section className="ob-section ob-byok-form" aria-labelledby={`${selectId}-h`}>
      <h3 id={`${selectId}-h`}>{t("onboarding.byok.form.heading")}</h3>
      <p className="muted small">{t("onboarding.byok.form.intro")}</p>
      {kind === undefined && (
        <div className="field">
          <label htmlFor={selectId}>{t("onboarding.byok.form.kind")}</label>
          <PresetSelect id={selectId} value={own} onChange={setOwn} />
        </div>
      )}
      {/* Keyed by kind: switching provider starts from that provider's defaults. */}
      <KeyForm key={current.id} info={current} onSaved={onSaved} />
      <p className="muted small">{t("onboarding.byok.form.privateLocal")}</p>
    </section>
  );
}

/** The provider presets as a select. */
export function PresetSelect({ id, value, onChange }: { id: string; value: PresetId; onChange: (p: PresetId) => void }) {
  const t = useT();
  return (
    <select id={id} value={value} onChange={(e) => onChange(e.target.value as PresetId)}>
      {PRESETS.map((p) => (
        <option key={p.id} value={p.id}>
          {presetLabel(t, p)}
        </option>
      ))}
    </select>
  );
}

/**
 * The API key field, or (without a keychain) the environment variable's name, with a switch.
 * Switching moves focus to the field that appears (the button that was pressed is gone).
 * `autoFocus` marks the field for a dialog's initial focus.
 */
function KeyFields({
  entry,
  onChange,
  placeholder,
  disabled,
  autoFocus,
  optional,
}: {
  entry: KeyEntry;
  onChange: (e: KeyEntry) => void;
  placeholder?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  /** A server on this computer: the key may stay empty. */
  optional?: boolean;
}) {
  const t = useT();
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  const switched = useRef(false);
  useEffect(() => {
    if (switched.current) {
      switched.current = false;
      input.current?.focus();
    }
  }, [entry.mode]);
  const switchTo = (mode: KeyEntry["mode"]) => {
    switched.current = true;
    onChange({ ...entry, mode });
  };
  const env = entry.mode === "env";
  const fieldId = `${id}-${entry.mode}`;
  return (
    <div className="field">
      <label htmlFor={fieldId}>{t(env ? "onboarding.byok.form.envVar" : "onboarding.byok.form.key")}</label>
      <input
        key={entry.mode}
        ref={input}
        id={fieldId}
        type={env ? "text" : "password"}
        dir="ltr"
        value={env ? entry.env : entry.key}
        autoComplete="off"
        spellCheck={false}
        required={!optional}
        maxLength={env ? 128 : undefined}
        placeholder={env ? undefined : placeholder}
        aria-describedby={env ? `${id}-env-hint` : undefined}
        {...(autoFocus ? { "data-autofocus": "" } : {})}
        onChange={(e) => onChange(env ? { ...entry, env: e.target.value } : { ...entry, key: e.target.value })}
      />
      {env && (
        <p id={`${id}-env-hint`} className="muted small">
          {t("onboarding.byok.form.envVarHint")}
        </p>
      )}
      {!env && optional && <p className="muted small">{t("onboarding.byok.form.keyOptional")}</p>}
      <button
        type="button"
        className="btn btn-ghost btn-small"
        style={{ justifySelf: "start", alignSelf: "flex-start" }}
        disabled={disabled}
        onClick={() => switchTo(env ? "key" : "env")}
      >
        {t(env ? "onboarding.byok.form.useKey" : "onboarding.byok.form.useEnv")}
      </button>
    </div>
  );
}

function KeyForm({ info, onSaved }: { info: Preset; onSaved?: (provider: string) => void }) {
  const t = useT();
  const id = useId();
  const ids = { base: `${id}-base`, baseHint: `${id}-base-hint`, model: `${id}-model`, light: `${id}-light`, models: `${id}-models`, name: `${id}-name`, nameHint: `${id}-name-hint` };
  const name0 = info.id === "compatible" ? "custom" : info.id;
  const [baseUrl, setBaseUrl] = useState(info.baseUrl);
  const [model, setModel] = useState(info.model);
  const [lightModel, setLightModel] = useState(info.lightModel);
  const [entry, setEntry] = useState<KeyEntry>({ mode: "key", key: "", env: suggestedEnv(name0) });
  const [name, setName] = useState(name0);
  const [busy, setBusy] = useState<"test" | "save" | "replace" | null>(null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [models, setModels] = useState<string[]>([]);

  /** The address to send: `null` for the provider's own API (so config.toml keeps its default). */
  const base = (): string | null => {
    const b = baseUrl.trim().replace(/\/+$/, "");
    return b === "" || (b === info.baseUrl && info.api !== "openai_compatible") ? null : b;
  };
  const keyless = info.api === "openai_compatible" && isLoopback(baseUrl);

  const record = (r: AiKeyCheckResult) => {
    if (r.models.length) setModels(r.models);
  };

  /** A failed save: without a keychain, switch to the environment variable. */
  const failed = (r: AiKeyAddResult) => {
    if (r.reason === "no_keychain") setEntry((e) => ({ ...e, mode: "env", key: "", env: r.suggested_env || e.env }));
    setOutcome({ type: "failed", reason: r.ok ? "server_error" : r.reason });
  };

  const run = async (what: "test" | "save" | "replace") => {
    if (busy) return;
    const source = keySource(entry, keyless);
    const invalid =
      typeof source === "string"
        ? source
        : what === "save" && !NAME_RE.test(name.trim())
          ? "onboarding.byok.form.error.name"
          : what === "save" && !model.trim()
            ? "onboarding.byok.form.error.model"
            : null;
    if (invalid || typeof source === "string") {
      setOutcome({ type: "invalid", key: (invalid ?? source) as MessageKey });
      return;
    }
    setBusy(what);
    setOutcome(null);
    try {
      if (what === "test") {
        const r = await aiKeysApi.test({ kind: info.api, base_url: base(), model: model.trim() || null, ...source });
        record(r);
        setOutcome(r.ok ? { type: "ok", count: r.models.length } : { type: "failed", reason: r.reason });
        return;
      }
      const r =
        what === "save"
          ? await aiKeysApi.save({ name: name.trim(), kind: info.api, base_url: base(), model: model.trim(), light_model: lightModel.trim() || null, ...source })
          : await aiKeysApi.update({ name: name.trim(), base_url: base(), ...source });
      record(r);
      if (r.saved) {
        setEntry((e) => ({ ...e, key: "" }));
        setOutcome({ type: what === "save" ? "saved" : "replaced" });
        onSaved?.(r.provider_id || name.trim());
      } else {
        failed(r);
      }
    } catch (e) {
      setOutcome({ type: "error", error: e });
    } finally {
      setBusy(null);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    void run("save");
  };

  const status =
    outcome?.type === "saved"
      ? t("onboarding.byok.form.saved")
      : outcome?.type === "replaced"
        ? t("onboarding.byok.form.replaced")
        : outcome?.type === "ok"
          ? t("onboarding.byok.form.ok", { count: outcome.count })
          : "";
  const alert = outcome?.type === "failed" ? failureText(t, outcome.reason, entry.env) : outcome?.type === "invalid" ? t(outcome.key) : null;
  const exists = outcome?.type === "failed" && outcome.reason === "exists";
  const requireBase = info.api === "openai_compatible";

  return (
    <form className="ob-byok-fields" onSubmit={submit} noValidate aria-busy={busy !== null}>
      <div className="field">
        <label htmlFor={ids.base}>{t("onboarding.byok.form.baseUrl")}</label>
        <input
          id={ids.base}
          type="url"
          inputMode="url"
          dir="ltr"
          value={baseUrl}
          required={requireBase}
          autoComplete="off"
          spellCheck={false}
          aria-describedby={requireBase ? ids.baseHint : undefined}
          onChange={(e) => setBaseUrl(e.target.value)}
        />
        {requireBase && (
          <p id={ids.baseHint} className="muted small">
            {t("onboarding.byok.form.baseUrlHint")}
          </p>
        )}
      </div>
      <KeyFields entry={entry} onChange={setEntry} placeholder={info.keyExample} disabled={busy !== null} optional={keyless} />
      {info.keyUrl && (
        <p className="small">
          <button type="button" className="btn btn-ghost btn-small" onClick={() => void api.openUrl(info.keyUrl!).catch(() => {})}>
            {t("onboarding.byok.form.getKey", { provider: presetLabel(t, info) })}
          </button>
        </p>
      )}
      <div className="field">
        <label htmlFor={ids.model}>{t("onboarding.byok.form.model")}</label>
        <input
          id={ids.model}
          type="text"
          dir="ltr"
          value={model}
          autoComplete="off"
          spellCheck={false}
          required
          placeholder={t("onboarding.byok.form.modelPlaceholder", { example: info.model || CUSTOM_MODEL_EXAMPLE })}
          list={models.length ? ids.models : undefined}
          onChange={(e) => setModel(e.target.value)}
        />
        {models.length > 0 && (
          <datalist id={ids.models}>
            {models.map((m) => (
              <option key={m} value={m} />
            ))}
          </datalist>
        )}
      </div>
      <div className="field">
        <label htmlFor={ids.light}>{t("onboarding.byok.form.lightModel")}</label>
        <input
          id={ids.light}
          type="text"
          dir="ltr"
          value={lightModel}
          autoComplete="off"
          spellCheck={false}
          list={models.length ? ids.models : undefined}
          onChange={(e) => setLightModel(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor={ids.name}>{t("onboarding.byok.form.name")}</label>
        <input
          id={ids.name}
          type="text"
          dir="ltr"
          value={name}
          maxLength={32}
          autoComplete="off"
          spellCheck={false}
          required
          aria-describedby={ids.nameHint}
          onChange={(e) => setName(e.target.value)}
        />
        <p id={ids.nameHint} className="muted small">
          {t("onboarding.byok.form.nameHint")}
        </p>
      </div>
      <div className="row">
        <button type="button" className="btn btn-ghost" disabled={busy !== null} onClick={() => void run("test")}>
          {busy === "test" ? t("onboarding.byok.form.testing") : t("onboarding.byok.form.test")}
        </button>
        <button type="submit" className="btn btn-primary" disabled={busy !== null}>
          {busy === "save" ? t("onboarding.byok.form.saving") : t("onboarding.byok.form.save")}
        </button>
      </div>
      <p role="status" className={status ? "note" : undefined}>
        {status}
      </p>
      {alert && (
        <p role="alert" className="note note-error">
          {alert}
        </p>
      )}
      {exists && (
        <>
          {/* Replacing changes only the key (and the address); the provider keeps its type and model. */}
          <p id={`${id}-replace-hint`} className="muted small">
            {t("onboarding.byok.form.replaceHint", { name: name.trim() })}
          </p>
          <div className="row">
            <button type="button" className="btn" disabled={busy !== null} aria-describedby={`${id}-replace-hint`} onClick={() => void run("replace")}>
              {busy === "replace" ? t("onboarding.byok.form.replacing") : t("onboarding.byok.form.replace")}
            </button>
          </div>
        </>
      )}
      {outcome?.type === "error" && <ErrorNote error={outcome.error} />}
    </form>
  );
}

/**
 * Replace a saved provider's key (`ai.key.update`): the new key is checked with the provider
 * first, and the provider uses it at once. For the Models page's "Replace key" dialog.
 */
export function ReplaceKeyForm({
  provider,
  onDone,
  onCancel,
}: {
  provider: { id: string; key: "keychain" | "env" | "none"; env_var: string | null };
  onDone: () => void;
  onCancel: () => void;
}) {
  const t = useT();
  const [entry, setEntry] = useState<KeyEntry>({
    mode: provider.key === "env" ? "env" : "key",
    key: "",
    env: provider.env_var ?? suggestedEnv(provider.id),
  });
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const source = keySource(entry);
    if (typeof source === "string") {
      setOutcome({ type: "invalid", key: source });
      return;
    }
    setBusy(true);
    setOutcome(null);
    try {
      const r = await aiKeysApi.update({ name: provider.id, ...source });
      if (r.saved) {
        setEntry((x) => ({ ...x, key: "" }));
        setOutcome({ type: "replaced" });
        onDone();
      } else {
        if (r.reason === "no_keychain") setEntry((x) => ({ ...x, mode: "env", key: "", env: r.suggested_env || x.env }));
        setOutcome({ type: "failed", reason: r.reason });
      }
    } catch (err) {
      setOutcome({ type: "error", error: err });
    } finally {
      setBusy(false);
    }
  };

  const alert = outcome?.type === "failed" ? failureText(t, outcome.reason, entry.env) : outcome?.type === "invalid" ? t(outcome.key) : null;
  return (
    <form className="ob-byok-fields" onSubmit={(e) => void submit(e)} noValidate aria-busy={busy}>
      <KeyFields entry={entry} onChange={setEntry} disabled={busy} autoFocus />
      <p role="status" className={outcome?.type === "replaced" ? "note" : undefined}>
        {outcome?.type === "replaced" ? t("onboarding.byok.form.replaced") : ""}
      </p>
      {alert && (
        <p role="alert" className="note note-error">
          {alert}
        </p>
      )}
      {outcome?.type === "error" && <ErrorNote error={outcome.error} />}
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onCancel} disabled={busy}>
          {t("common.cancel")}
        </button>
        <button type="submit" className="btn btn-primary" disabled={busy}>
          {busy ? t("onboarding.byok.form.replacing") : t("onboarding.byok.form.replace")}
        </button>
      </div>
    </form>
  );
}
