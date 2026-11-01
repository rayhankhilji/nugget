import { useId, useState, type FormEvent } from "react";
import {
  aiKeysApi,
  ENV_NAME_RE,
  type AiKeyAddResult,
  type AiKeyCheckResult,
  type AiKeyKind,
  type AiKeyOutcome,
  type AiKeySource,
} from "../api";
import { useT, type MessageKey } from "../i18n";
import { ErrorNote } from "../components/ui";

/** The provider choice of the onboarding guide. */
export type ByokGuideKind = "anthropic" | "openai" | "compatible";

interface KindInfo {
  api: AiKeyKind;
  label: MessageKey;
  /** Prefilled address; the provider's own API, or empty when the user must enter one. */
  baseUrl: string;
  /** Default provider id (matches the guide's `keychain:NAME`). */
  name: string;
  /** Example model id and key prefix, shown as placeholders (never sent). */
  modelExample: string;
  keyExample: string;
}

// i18n-ignore-block: API kinds, URLs and provider ids are code, not prose.
const KINDS: Record<ByokGuideKind, KindInfo> = {
  anthropic: { api: "anthropic", label: "onboarding.byok.provider.anthropic", baseUrl: "https://api.anthropic.com", name: "anthropic", modelExample: "claude-opus-5-5", keyExample: "sk-ant-…" },
  openai: { api: "openai_responses", label: "onboarding.byok.provider.openai", baseUrl: "https://api.openai.com/v1", name: "openai", modelExample: "gpt-5", keyExample: "sk-…" },
  compatible: { api: "openai_compatible", label: "onboarding.byok.provider.compatible", baseUrl: "", name: "custom", modelExample: "qwen3:4b", keyExample: "sk-…" },
};
// i18n-ignore-end

/** The label of a provider kind (for lists of saved providers). */
export function kindLabel(kind: AiKeyKind): MessageKey {
  const k = (Object.keys(KINDS) as ByokGuideKind[]).find((g) => KINDS[g].api === kind);
  return KINDS[k ?? "compatible"].label;
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

/** The request's key part, or why it cannot be sent. */
function keySource(e: KeyEntry): AiKeySource | MessageKey {
  if (e.mode === "env") {
    const v = e.env.trim();
    return ENV_NAME_RE.test(v) ? { key_ref: `env:${v}` } : "onboarding.byok.form.error.envName";
  }
  return e.key.trim() ? { api_key: e.key.trim() } : "onboarding.byok.form.error.key";
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
export function ByokForm({ kind, onSaved }: { kind?: ByokGuideKind; onSaved?: (provider: string) => void }) {
  const t = useT();
  const selectId = useId();
  const [own, setOwn] = useState<ByokGuideKind>("anthropic");
  const current = kind ?? own;
  return (
    <section className="ob-section ob-byok-form" aria-labelledby={`${selectId}-h`}>
      <h3 id={`${selectId}-h`}>{t("onboarding.byok.form.heading")}</h3>
      <p className="muted small">{t("onboarding.byok.form.intro")}</p>
      {kind === undefined && (
        <div className="field">
          <label htmlFor={selectId}>{t("onboarding.byok.form.kind")}</label>
          <select id={selectId} value={own} onChange={(e) => setOwn(e.target.value as ByokGuideKind)}>
            {(Object.keys(KINDS) as ByokGuideKind[]).map((k) => (
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

/** The API key field, or (without a keychain) the environment variable's name, with a switch. */
function KeyFields({
  entry,
  onChange,
  placeholder,
  disabled,
}: {
  entry: KeyEntry;
  onChange: (e: KeyEntry) => void;
  placeholder?: string;
  disabled?: boolean;
}) {
  const t = useT();
  const id = useId();
  return entry.mode === "key" ? (
    <div className="field">
      <label htmlFor={`${id}-key`}>{t("onboarding.byok.form.key")}</label>
      <input
        id={`${id}-key`}
        type="password"
        dir="ltr"
        value={entry.key}
        autoComplete="off"
        spellCheck={false}
        required
        placeholder={placeholder}
        onChange={(e) => onChange({ ...entry, key: e.target.value })}
      />
      <button type="button" className="btn btn-ghost btn-small" style={{ justifySelf: "start", alignSelf: "flex-start" }} disabled={disabled} onClick={() => onChange({ ...entry, mode: "env" })}>
        {t("onboarding.byok.form.useEnv")}
      </button>
    </div>
  ) : (
    <div className="field">
      <label htmlFor={`${id}-env`}>{t("onboarding.byok.form.envVar")}</label>
      <input
        id={`${id}-env`}
        type="text"
        dir="ltr"
        value={entry.env}
        autoComplete="off"
        spellCheck={false}
        required
        maxLength={128}
        aria-describedby={`${id}-env-hint`}
        onChange={(e) => onChange({ ...entry, env: e.target.value })}
      />
      <p id={`${id}-env-hint`} className="muted small">
        {t("onboarding.byok.form.envVarHint")}
      </p>
      <button type="button" className="btn btn-ghost btn-small" style={{ justifySelf: "start", alignSelf: "flex-start" }} disabled={disabled} onClick={() => onChange({ ...entry, mode: "key" })}>
        {t("onboarding.byok.form.useKey")}
      </button>
    </div>
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

  /** The address to send: `null` for the provider's own API (so config.toml keeps its default). */
  const base = (): string | null => {
    const b = baseUrl.trim().replace(/\/+$/, "");
    return b === "" || b === info.baseUrl ? null : b;
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
    const source = keySource(entry);
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
      <KeyFields entry={entry} onChange={setEntry} placeholder={info.keyExample} disabled={busy !== null} />
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
        <div className="row">
          <button type="button" className="btn" disabled={busy !== null} onClick={() => void run("replace")}>
            {busy === "replace" ? t("onboarding.byok.form.replacing") : t("onboarding.byok.form.replace")}
          </button>
        </div>
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
      <KeyFields entry={entry} onChange={setEntry} disabled={busy} />
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
