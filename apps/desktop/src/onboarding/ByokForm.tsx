import { useId, useState, type FormEvent } from "react";
import { aiKeyApi, type AiKeyKind, type AiKeyReason, type AiKeySaveResult, type AiKeyTestResult } from "../api";
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

const NAME_RE = /^[a-z0-9_-]{1,32}$/;

const REASON_KEYS: Record<Exclude<AiKeyReason, "ok">, MessageKey> = {
  bad_key: "onboarding.byok.form.error.bad_key",
  wrong_url: "onboarding.byok.form.error.wrong_url",
  no_network: "onboarding.byok.form.error.no_network",
  server_error: "onboarding.byok.form.error.server_error",
  exists: "onboarding.byok.form.error.exists",
};

type Outcome =
  | { type: "ok"; count: number }
  | { type: "saved" }
  | { type: "failed"; reason: AiKeyReason }
  | { type: "invalid"; key: MessageKey }
  | { type: "error"; error: unknown };

/**
 * Paste a key, check it with the provider (`ai.key.test`) and save it (`ai.key.save`: OS keychain
 * plus a `keychain:NAME` provider in config.toml). Shown under the BYOK guide; `kind` follows the
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

function KeyForm({ info, onSaved }: { info: KindInfo; onSaved?: (provider: string) => void }) {
  const t = useT();
  const id = useId();
  const ids = { base: `${id}-base`, baseHint: `${id}-base-hint`, model: `${id}-model`, models: `${id}-models`, key: `${id}-key`, name: `${id}-name`, nameHint: `${id}-name-hint` };
  const [baseUrl, setBaseUrl] = useState(info.baseUrl);
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [name, setName] = useState(info.name);
  const [busy, setBusy] = useState<"test" | "save" | null>(null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [models, setModels] = useState<string[]>([]);

  /** The address to send: `null` for the provider's own API (so config.toml keeps its default). */
  const base = (): string | null => {
    const b = baseUrl.trim().replace(/\/+$/, "");
    return b === "" || b === info.baseUrl ? null : b;
  };

  const check = (forSave: boolean): MessageKey | null => {
    if (!apiKey.trim()) return "onboarding.byok.form.error.key";
    if (forSave && !NAME_RE.test(name.trim())) return "onboarding.byok.form.error.name";
    if (forSave && !model.trim()) return "onboarding.byok.form.error.model";
    return null;
  };

  const record = (r: AiKeyTestResult) => {
    if (r.models.length) setModels(r.models);
  };

  const run = async (what: "test" | "save") => {
    if (busy) return;
    const invalid = check(what === "save");
    if (invalid) {
      setOutcome({ type: "invalid", key: invalid });
      return;
    }
    setBusy(what);
    setOutcome(null);
    try {
      if (what === "test") {
        const r = await aiKeyApi.aiKeyTest({ kind: info.api, base_url: base(), api_key: apiKey.trim(), model: model.trim() || null });
        record(r);
        setOutcome(r.ok ? { type: "ok", count: r.models.length } : { type: "failed", reason: r.reason });
      } else {
        const r: AiKeySaveResult = await aiKeyApi.aiKeySave({
          name: name.trim(),
          kind: info.api,
          base_url: base(),
          api_key: apiKey.trim(),
          model: model.trim(),
        });
        record(r);
        if (r.saved) {
          setApiKey("");
          setOutcome({ type: "saved" });
          onSaved?.(r.provider_id || name.trim());
        } else {
          setOutcome({ type: "failed", reason: r.ok ? "server_error" : r.reason });
        }
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
    outcome?.type === "saved" ? t("onboarding.byok.form.saved") : outcome?.type === "ok" ? t("onboarding.byok.form.ok", { count: outcome.count }) : "";
  const alert =
    outcome?.type === "failed" && outcome.reason !== "ok"
      ? t(REASON_KEYS[outcome.reason])
      : outcome?.type === "invalid"
        ? t(outcome.key)
        : null;
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
      <div className="field">
        <label htmlFor={ids.key}>{t("onboarding.byok.form.key")}</label>
        <input
          id={ids.key}
          type="password"
          dir="ltr"
          value={apiKey}
          autoComplete="off"
          spellCheck={false}
          required
          placeholder={info.keyExample}
          onChange={(e) => setApiKey(e.target.value)}
        />
      </div>
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
      {outcome?.type === "error" && <ErrorNote error={outcome.error} />}
    </form>
  );
}
