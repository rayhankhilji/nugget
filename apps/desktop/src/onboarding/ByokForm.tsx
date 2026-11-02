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
import "./open-core.css";

/** The provider choices: each a preset of the `ai.key.*` form. */
export type ByokGuideKind = "anthropic" | "openai" | "openrouter" | "ollama" | "compatible";

interface KindInfo {
  api: AiKeyKind;
  label: MessageKey;
  /** The provider's name as it calls itself (a brand: not translated); `null`: the address's host. */
  brand: string | null;
  /** Prefilled address; the provider's own API, or empty when the user must enter one. */
  baseUrl: string;
  /** Default provider id (matches the guide's `keychain:NAME`). */
  name: string;
  /** Example model id and key prefix, shown as placeholders (never sent). */
  modelExample: string;
  keyExample: string;
  /** `none`: no key field (a server on this computer); `optional`: a key only if the server needs one. */
  key: "required" | "optional" | "none";
  /** Where the provider hands out keys, opened in the browser. */
  keys?: { url: string; site: string };
}

// i18n-ignore-block: API kinds, URLs, brands and provider ids are code, not prose.
export const KINDS: Record<ByokGuideKind, KindInfo> = {
  anthropic: {
    api: "anthropic",
    label: "onboarding.byok.provider.anthropic",
    brand: "Anthropic",
    baseUrl: "https://api.anthropic.com",
    name: "anthropic",
    modelExample: "claude-opus-5-5",
    keyExample: "sk-ant-…",
    key: "required",
    keys: { url: "https://console.anthropic.com/settings/keys", site: "Anthropic Console" },
  },
  openai: {
    api: "openai_responses",
    label: "onboarding.byok.provider.openai",
    brand: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    name: "openai",
    modelExample: "gpt-5",
    keyExample: "sk-…",
    key: "required",
    keys: { url: "https://platform.openai.com/api-keys", site: "OpenAI" },
  },
  openrouter: {
    api: "openai_compatible",
    label: "onboarding.byok.provider.openrouter",
    brand: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    name: "openrouter",
    modelExample: "openai/gpt-4.1-mini",
    keyExample: "sk-or-…",
    key: "required",
    keys: { url: "https://openrouter.ai/keys", site: "OpenRouter" },
  },
  ollama: {
    api: "openai_compatible",
    label: "onboarding.byok.provider.ollama",
    brand: "Ollama",
    baseUrl: "http://127.0.0.1:11434/v1",
    name: "ollama",
    modelExample: "llama3.2:3b",
    keyExample: "",
    key: "none",
  },
  compatible: {
    api: "openai_compatible",
    label: "onboarding.byok.provider.compatible",
    brand: null,
    baseUrl: "",
    name: "custom",
    modelExample: "qwen3:4b",
    keyExample: "sk-…",
    key: "optional",
  },
};
// i18n-ignore-end

/** The order the provider select lists them in. */
const KIND_ORDER: ByokGuideKind[] = ["anthropic", "openai", "openrouter", "ollama", "compatible"];

/** Which preset a saved provider matches: its API kind, and for OpenAI-compatible ones its address. */
export function presetOf(kind: AiKeyKind, baseUrl?: string | null): ByokGuideKind {
  if (kind === "anthropic") return "anthropic";
  if (kind === "openai_responses") return "openai";
  const b = (baseUrl ?? "").trim().replace(/\/+$/, "").toLowerCase();
  if (b === KINDS.openrouter.baseUrl) return "openrouter";
  if (/^http:\/\/(127\.0\.0\.1|localhost):11434(\/v1)?$/.test(b)) return "ollama";
  return "compatible";
}

/** The label of a provider kind (for lists of saved providers); the address tells presets apart. */
export function kindLabel(kind: AiKeyKind, baseUrl?: string | null): MessageKey {
  return KINDS[presetOf(kind, baseUrl)].label;
}

/** The provider's name for "Works: …": its brand, else the address's host. */
function providerName(info: KindInfo, baseUrl: string): string {
  if (info.brand) return info.brand;
  try {
    return new URL(baseUrl.trim()).host || baseUrl.trim();
  } catch {
    return baseUrl.trim();
  }
}

/** What `.key-field[data-state]` shows: nothing checked yet, checking, works, or doesn't. */
export type KeyCheckState = "idle" | "checking" | "valid" | "invalid";

/** Failures that are about the key or the address: said in the key field itself. */
const KEY_REASONS = new Set<AiKeyOutcome>(["bad_key", "wrong_url", "no_network", "server_error"]);
const KEY_INVALID = new Set<MessageKey>(["onboarding.byok.form.error.key", "onboarding.byok.form.error.envName"]);

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

/** The request's key part, or why it cannot be sent. `optional`: no key is fine (a keyless server). */
function keySource(e: KeyEntry, optional = false): AiKeySource | MessageKey {
  if (e.mode === "env") {
    const v = e.env.trim();
    return ENV_NAME_RE.test(v) ? { key_ref: `env:${v}` } : "onboarding.byok.form.error.envName";
  }
  if (e.key.trim()) return { api_key: e.key.trim() };
  return optional ? {} : "onboarding.byok.form.error.key";
}

/** The alert for a failed outcome (`no_keychain` names the variable to use). */
function failureText(t: ReturnType<typeof useT>, reason: AiKeyOutcome, env: string): string | null {
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
export function ByokForm({
  kind,
  onSaved,
  initialKind = "anthropic",
}: {
  kind?: ByokGuideKind;
  onSaved?: (provider: string) => void;
  /** The select's first choice when `kind` is not given. */
  initialKind?: ByokGuideKind;
}) {
  const t = useT();
  const selectId = useId();
  const [own, setOwn] = useState<ByokGuideKind>(initialKind);
  const current = kind ?? own;
  return (
    <section className="ob-section ob-byok-form" aria-labelledby={`${selectId}-h`}>
      <h3 id={`${selectId}-h`}>{t("onboarding.byok.form.heading")}</h3>
      <p className="muted small">{t("onboarding.byok.form.intro")}</p>
      {kind === undefined && (
        <div className="field">
          <label htmlFor={selectId}>{t("onboarding.byok.form.kind")}</label>
          <select id={selectId} value={own} onChange={(e) => setOwn(e.target.value as ByokGuideKind)}>
            {KIND_ORDER.map((k) => (
              <option key={k} value={k}>
                {t(KINDS[k].label)}
              </option>
            ))}
          </select>
        </div>
      )}
      {/* Keyed by kind: switching provider starts from that provider's defaults. */}
      <KeyForm key={current} info={KINDS[current]} onSaved={onSaved} />
    </section>
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
  optional = false,
  check,
  onPasted,
  keys,
}: {
  entry: KeyEntry;
  onChange: (e: KeyEntry) => void;
  placeholder?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  /** The key is optional (a server that may not need one). */
  optional?: boolean;
  /** The check's state and what it says, for `.key-field[data-state]`. */
  check?: { state: KeyCheckState; text: string };
  /** A key was pasted: check it straight away. */
  onPasted?: () => void;
  /** Where to get a key. */
  keys?: KindInfo["keys"];
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
    <div className="field key-field" data-state={check?.state ?? "idle"}>
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
        aria-describedby={env ? `${id}-env-hint` : optional ? `${id}-optional-hint` : undefined}
        aria-invalid={check?.state === "invalid" ? true : undefined}
        {...(autoFocus ? { "data-autofocus": "" } : {})}
        onPaste={env ? undefined : () => onPasted?.()}
        onChange={(e) => onChange(env ? { ...entry, env: e.target.value } : { ...entry, key: e.target.value })}
      />
      {optional && !env && (
        <p id={`${id}-optional-hint`} className="muted small">
          {t("onboarding.byok.form.keyOptional")}
        </p>
      )}
      {check && <KeyCheckStatus state={check.state} text={check.text} />}
      {keys && !env && (
        <button type="button" className="btn btn-ghost btn-small key-field-link" onClick={() => void api.openUrl(keys.url).catch(() => {})}>
          {t("onboarding.byok.form.getKey", { site: keys.site })}
        </button>
      )}
      {env && (
        <p id={`${id}-env-hint`} className="muted small">
          {t("onboarding.byok.form.envVarHint")}
        </p>
      )}
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

/**
 * The line under a checked field: "Checking…", "✓ Works: …" or why it didn't (an alert). Polite,
 * so a check that finishes while typing elsewhere is still heard.
 */
function KeyCheckStatus({ state, text }: { state: KeyCheckState; text: string }) {
  return (
    <span className="key-field-status" aria-live={state === "invalid" ? undefined : "polite"}>
      {state === "checking" && <span className="spinner" aria-hidden="true" />}
      {state === "valid" && <span aria-hidden="true">✓</span>}
      {state === "invalid" && <span aria-hidden="true">✕</span>}
      {state === "invalid" ? <span role="alert">{text}</span> : text}
    </span>
  );
}

function KeyForm({ info, onSaved }: { info: KindInfo; onSaved?: (provider: string) => void }) {
  const t = useT();
  const id = useId();
  const ids = { base: `${id}-base`, baseHint: `${id}-base-hint`, model: `${id}-model`, models: `${id}-models`, name: `${id}-name`, nameHint: `${id}-name-hint` };
  const [baseUrl, setBaseUrl] = useState(info.baseUrl);
  const [model, setModel] = useState("");
  const [entry, setEntry] = useState<KeyEntry>({ mode: "key", key: "", env: suggestedEnv(info.name) });
  const [name, setName] = useState(info.name);
  const [busy, setBusy] = useState<"test" | "save" | "replace" | null>(null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const pasted = useRef(false);

  /**
   * The address to send: `null` for the provider's own API (so config.toml keeps its default).
   * OpenAI-compatible presets (Ollama, OpenRouter) always send theirs: that kind has no default.
   */
  const base = (): string | null => {
    const b = baseUrl.trim().replace(/\/+$/, "");
    if (b === "") return null;
    return info.api !== "openai_compatible" && b === info.baseUrl ? null : b;
  };

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
    const source = keySource(entry, info.key !== "required");
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
          ? await aiKeysApi.save({ name: name.trim(), kind: info.api, base_url: base(), model: model.trim(), ...source })
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

  // A pasted key is checked as soon as it is in (when there is an address to check it at).
  useEffect(() => {
    if (!pasted.current) return;
    pasted.current = false;
    if (entry.mode === "key" && entry.key.trim() && (info.api !== "openai_compatible" || baseUrl.trim())) void run("test");
  }, [entry.key]); // eslint-disable-line react-hooks/exhaustive-deps

  // What the check found about the key (or, without a key field, the address).
  const keyFailed =
    (outcome?.type === "failed" && KEY_REASONS.has(outcome.reason)) || (outcome?.type === "invalid" && KEY_INVALID.has(outcome.key));
  const checkState: KeyCheckState =
    busy !== null
      ? "checking"
      : outcome?.type === "ok" || outcome?.type === "saved" || outcome?.type === "replaced"
        ? "valid"
        : keyFailed
          ? "invalid"
          : "idle";
  const checkText =
    checkState === "checking"
      ? t("onboarding.byok.form.checking")
      : checkState === "valid"
        ? t("onboarding.byok.form.keyValid", { provider: providerName(info, baseUrl), count: models.length })
        : checkState === "invalid" && outcome
          ? outcome.type === "failed"
            ? (failureText(t, outcome.reason, entry.env) ?? "")
            : outcome.type === "invalid"
              ? t(outcome.key)
              : ""
          : "";
  const check = { state: checkState, text: checkText };
  /** An edit makes the last check stale (the outcome of a save stays). */
  const edited = () => {
    if (outcome && outcome.type !== "saved" && outcome.type !== "replaced" && outcome.type !== "error") setOutcome(null);
  };

  const status =
    outcome?.type === "saved" ? t("onboarding.byok.form.saved") : outcome?.type === "replaced" ? t("onboarding.byok.form.replaced") : "";
  const alert = keyFailed
    ? null
    : outcome?.type === "failed"
      ? failureText(t, outcome.reason, entry.env)
      : outcome?.type === "invalid"
        ? t(outcome.key)
        : null;
  const exists = outcome?.type === "failed" && outcome.reason === "exists";
  const requireBase = info.api === "openai_compatible";
  const keyless = info.key === "none";

  return (
    <form className="ob-byok-fields" onSubmit={submit} noValidate aria-busy={busy !== null}>
      <div className={keyless ? "field key-field" : "field"} data-state={keyless ? checkState : undefined}>
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
          onChange={(e) => {
            edited();
            setBaseUrl(e.target.value);
          }}
        />
        {requireBase && (
          <p id={ids.baseHint} className="muted small">
            {t(keyless ? "onboarding.byok.form.ollamaHint" : "onboarding.byok.form.baseUrlHint")}
          </p>
        )}
        {keyless && <KeyCheckStatus state={check.state} text={check.text} />}
      </div>
      {!keyless && (
        <KeyFields
          entry={entry}
          onChange={(e) => {
            edited();
            setEntry(e);
          }}
          placeholder={info.keyExample}
          disabled={busy !== null}
          optional={info.key === "optional"}
          check={check}
          onPasted={() => (pasted.current = true)}
          keys={info.keys}
        />
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
          placeholder={t("onboarding.byok.form.modelPlaceholder", { example: info.modelExample })}
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
          {busy === "test" ? t("onboarding.byok.form.testing") : t(keyless ? "onboarding.byok.form.testServer" : "onboarding.byok.form.test")}
        </button>
        <button type="submit" className="btn btn-primary" disabled={busy !== null}>
          {busy === "save" ? t("onboarding.byok.form.saving") : t(keyless ? "onboarding.byok.form.saveServer" : "onboarding.byok.form.save")}
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
  provider: { id: string; key: "keychain" | "env"; env_var: string | null };
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
