import { useId, useState } from "react";
import { aiKeysApi, type LocalServer } from "../api";
import { useAsync } from "../hooks";
import { useT } from "../i18n";
import { Badge, ErrorNote } from "../components/ui";
import { failureText } from "./ByokForm";

/**
 * Ollama and LM Studio found running on this computer (`ai.local.detect`), each with its models
 * and a button that adds it as a provider without a key (checked first, like a key). Renders
 * nothing when none is running. Models that forward prompts to the cloud are not offered.
 */
export function LocalServers({ onSaved }: { onSaved?: (provider: string) => void }) {
  const t = useT();
  const id = useId();
  const found = useAsync(aiKeysApi.detectLocal);
  const servers = (found.data ?? []).filter((s) => s.models.length > 0);
  if (servers.length === 0) return null;
  return (
    <section className="ob-section" aria-labelledby={`${id}-h`}>
      <h3 id={`${id}-h`}>{t("onboarding.byok.local.heading")}</h3>
      <ul className="cards">
        {servers.map((s) => (
          <LocalServerRow
            key={s.id}
            server={s}
            onSaved={(p) => {
              found.reload();
              onSaved?.(p);
            }}
          />
        ))}
      </ul>
    </section>
  );
}

function LocalServerRow({ server: s, onSaved }: { server: LocalServer; onSaved: (provider: string) => void }) {
  const t = useT();
  const id = useId();
  const [model, setModel] = useState(s.models[0] ?? "");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [error, setError] = useState<unknown>(null);

  const add = async () => {
    setBusy(true);
    setResult(null);
    setError(null);
    try {
      const r = await aiKeysApi.save({ name: s.id, kind: "openai_compatible", base_url: s.base_url, model });
      if (r.saved) {
        setResult({ ok: true, text: t("onboarding.byok.local.added", { server: s.label }) });
        onSaved(r.provider_id || s.id);
      } else {
        setResult({ ok: false, text: failureText(t, r.reason, "") ?? "" });
      }
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className="card">
      <div className="row">
        <strong>{s.label}</strong>
        {s.configured && <Badge tone="good">{t("onboarding.byok.local.inUse")}</Badge>}
      </div>
      {!s.configured && (
        <div className="row">
          <label htmlFor={`${id}-m`} className="visually-hidden">
            {t("onboarding.byok.form.model")}
          </label>
          <select id={`${id}-m`} value={model} disabled={busy} onChange={(e) => setModel(e.target.value)}>
            {s.models.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
          <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void add()}>
            {t("onboarding.byok.local.use", { server: s.label })}
          </button>
        </div>
      )}
      <p role="status" className={result?.ok ? "note" : undefined}>
        {result?.ok ? result.text : ""}
      </p>
      {result && !result.ok && (
        <p role="alert" className="note note-error">
          {result.text}
        </p>
      )}
      {error !== null && <ErrorNote error={error} />}
    </li>
  );
}
