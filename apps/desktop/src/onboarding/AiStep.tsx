import { useId, useState } from "react";
import { api, errorMessage, events, type FitReport, type ModelInfo, type ModelProgress } from "../api";
import { useAsync, useEvent } from "../hooks";
import { formatBytes, percent } from "../format";
import { Trans, useT, type MessageKey } from "../i18n";
import { CopyButton } from "../components/CopyButton";
import { Badge, ErrorNote, Spinner } from "../components/ui";
import type { AiChoice } from "./machine";
import { ByokForm } from "./ByokForm";
import { FitNotes, loadFit, MachineLine } from "./ModelFit";
import { detectOs, type Os } from "./platform";
import { OfflineNote, StepHeading, type StepProps } from "./steps";

const CHOICES: { id: AiChoice; label: MessageKey; blurb: MessageKey }[] = [
  { id: "local", label: "onboarding.ai.local", blurb: "onboarding.ai.localBlurb" },
  { id: "byok", label: "onboarding.ai.byok", blurb: "onboarding.ai.byokBlurb" },
  { id: "later", label: "onboarding.ai.later", blurb: "onboarding.ai.laterBlurb" },
];

export function AiStep({ headingRef, state, dispatch, connected }: StepProps) {
  const t = useT();
  const name = useId();
  const choice = state.ai;
  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.ai.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.ai.lead")}</p>
      <fieldset className="ob-fieldset">
        <legend className="visually-hidden">{t("onboarding.ai.legend")}</legend>
        <div className="kind-picker ob-choices">
          {CHOICES.map((c) => (
            <label key={c.id} className="kind-option">
              <input
                type="radio"
                name={name}
                value={c.id}
                checked={choice === c.id}
                onChange={() => dispatch({ type: "chooseAi", ai: c.id })}
              />
              <span>
                <span className="kind-label">
                  {t(c.label)} {c.id === "local" && <Badge tone="accent">{t("onboarding.ai.localBadge")}</Badge>}
                </span>
                <span className="muted small">{t(c.blurb)}</span>
              </span>
            </label>
          ))}
        </div>
      </fieldset>
      {choice === "local" && (connected ? <LocalModels /> : <OfflineNote />)}
      {choice === "byok" && <ByokGuide />}
      {choice === "later" && <p className="muted">{t("onboarding.ai.laterDetail")}</p>}
    </>
  );
}

// ---- local models ----

const PHASE: Record<ModelProgress["phase"], MessageKey> = {
  downloading: "onboarding.models.phase.downloading",
  verifying: "onboarding.models.phase.verifying",
  installed: "onboarding.models.phase.installed",
  failed: "onboarding.models.phase.failed",
};

/** Recommended first, then by size (smallest first), then by id. */
export function sortModels(list: readonly ModelInfo[]): ModelInfo[] {
  return [...list].sort(
    (a, b) => Number(b.recommended) - Number(a.recommended) || a.size_bytes - b.size_bytes || a.id.localeCompare(b.id),
  );
}

function LocalModels() {
  const t = useT();
  const models = useAsync(api.modelsList);
  // Why each model is recommended; `null` on an older daemon (then nothing is explained).
  const fit = useAsync(loadFit);
  const [all, setAll] = useState(false);
  const [progress, setProgress] = useState<Record<string, ModelProgress>>({});
  const [requestError, setRequestError] = useState<Record<string, string>>({});
  const [announce, setAnnounce] = useState("");

  useEvent(events.onModelsProgress, (p) => {
    setProgress((cur) => ({ ...cur, [p.id]: p }));
    if (p.phase === "installed") {
      setAnnounce(t("onboarding.models.installedAnnounce", { model: p.id }));
      models.reload();
    }
  });

  const install = async (id: string) => {
    setRequestError(({ [id]: _, ...rest }) => rest);
    setProgress((cur) => ({ ...cur, [id]: { id, downloaded: 0, total: 0, phase: "downloading", error: null } }));
    try {
      await api.modelsInstall(id);
    } catch (e) {
      setProgress(({ [id]: _, ...rest }) => rest);
      setRequestError((cur) => ({ ...cur, [id]: errorMessage(e) }));
    }
  };

  const sorted = sortModels(models.data ?? []);
  const recommended = sorted.filter((m) => m.recommended);
  const shown = all || recommended.length === 0 ? sorted : recommended;

  return (
    <section className="ob-section" aria-labelledby="ob-models">
      <h2 id="ob-models">{t("onboarding.models.heading")}</h2>
      <p className="muted">{t("onboarding.models.lead")}</p>
      {fit.data && <MachineLine report={fit.data} />}
      <ErrorNote error={models.error} onRetry={models.reload} />
      {models.loading && !models.data && <Spinner />}
      {models.data && recommended.length === 0 && <p className="note">{t("onboarding.models.none")}</p>}
      <ul className="cards">
        {shown.map((m) => (
          <ModelRow
            key={m.id}
            model={m}
            progress={progress[m.id]}
            error={requestError[m.id]}
            fit={fit.data}
            onInstall={() => void install(m.id)}
          />
        ))}
      </ul>
      {recommended.length > 0 && sorted.length > recommended.length && (
        <button type="button" className="btn btn-ghost btn-small" aria-expanded={all} onClick={() => setAll((v) => !v)}>
          {all ? t("onboarding.models.showRecommended") : t("onboarding.models.showAll", { count: sorted.length })}
        </button>
      )}
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>
    </section>
  );
}

function ModelRow({
  model,
  progress,
  error,
  fit,
  onInstall,
}: {
  model: ModelInfo;
  progress?: ModelProgress;
  error?: string;
  fit?: FitReport | null;
  onInstall: () => void;
}) {
  const t = useT();
  const busy = progress?.phase === "downloading" || progress?.phase === "verifying";
  const failed = progress?.phase === "failed";
  const installed = model.installed || progress?.phase === "installed";
  const total = progress?.total || model.size_bytes;
  const pct = progress ? percent(progress.downloaded, total) : 0;
  const labelId = `ob-model-${model.id.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const phase = progress ? t(PHASE[progress.phase]) : "";
  const modelFit = fit?.models.find((f) => f.id === model.id);

  return (
    <li className="card" aria-labelledby={labelId}>
      <div className="card-head">
        <span id={labelId} className="card-title">
          {model.id}
        </span>
        <span className="badges">
          {model.recommended && <Badge tone="accent">{t("onboarding.ai.localBadge")}</Badge>}
          {installed && <Badge tone="good">{t("onboarding.models.installed")}</Badge>}
        </span>
      </div>
      <p className="muted small">
        {t("onboarding.models.size", { size: formatBytes(model.size_bytes) })}
        {model.license && <> · {t("onboarding.models.license", { license: model.license })}</>}
      </p>
      {modelFit && <FitNotes fit={modelFit} freeDisk={fit?.free_disk_bytes ?? null} installed={installed} />}
      {busy && progress && (
        <div className="progress">
          <progress
            max={100}
            value={pct}
            aria-labelledby={labelId}
            aria-valuetext={t("onboarding.models.progressLabel", { model: model.id, phase, pct })}
          />
          <span className="small muted">
            {t("onboarding.models.progress", {
              phase,
              pct,
              done: formatBytes(progress.downloaded),
              total: formatBytes(total),
            })}
          </span>
        </div>
      )}
      {(failed || error) && (
        <p className="note note-error" role="alert">
          {progress?.error || error || t("onboarding.models.failed")}
        </p>
      )}
      {!installed && !busy && (
        <div className="card-actions">
          <button type="button" className="btn" onClick={onInstall}>
            {failed || error ? t("onboarding.models.retry") : t("onboarding.models.install")}
          </button>
        </div>
      )}
    </li>
  );
}

// ---- bring your own key ----

type ProviderKind = "anthropic" | "openai" | "compatible";

interface ProviderGuide {
  label: MessageKey;
  /** Keychain entry name (`keychain:NAME`) and the env var alternative; `null` when no key is needed. */
  key: { name: string; env: string } | null;
  config: string;
}

// i18n-ignore-block: config snippets and shell commands are code, not prose.
export const PROVIDER_GUIDES: Record<ProviderKind, ProviderGuide> = {
  anthropic: {
    label: "onboarding.byok.provider.anthropic",
    key: { name: "anthropic", env: "ANTHROPIC_API_KEY" },
    config: `[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-opus-5-5"
api_key = "keychain:anthropic"
tiers = ["light", "synthesis", "agentic"]`,
  },
  openai: {
    label: "onboarding.byok.provider.openai",
    key: { name: "openai", env: "OPENAI_API_KEY" },
    config: `[[providers]]
id = "openai"
kind = "openai_responses"
model = "MODEL-ID"   # a model id from your OpenAI account
api_key = "keychain:openai"
tiers = ["light", "synthesis", "agentic"]`,
  },
  compatible: {
    label: "onboarding.byok.provider.compatible",
    key: null,
    config: `[[providers]]
id = "ollama"
kind = "openai_compatible"
base_url = "http://localhost:11434/v1"   # your server's address, with /v1
model = "qwen3:4b"
tiers = ["light", "synthesis"]
# api_key = "env:OPENROUTER_API_KEY"   # only for hosted services`,
  },
};

/** Where `config.toml` lives (`directories::ProjectDirs` for dev/Nugget/Nugget), unless NUGGET_HOME is set. */
export function configPath(os: Os): string {
  switch (os) {
    case "macos":
      return "~/Library/Application Support/dev.Nugget.Nugget/config.toml";
    case "windows":
      return "%APPDATA%\\Nugget\\Nugget\\config\\config.toml";
    default:
      return "~/.config/nugget/config.toml";
  }
}

/** Store a key in the OS keychain under service `dev.nugget.Nugget`, account `provider:NAME`. */
export function keychainCommand(os: Os, name: string): string {
  switch (os) {
    case "macos":
      return `security add-generic-password -s dev.nugget.Nugget -a provider:${name} -w`;
    case "windows":
      return `cmdkey /generic:provider:${name}.dev.nugget.Nugget /user:provider:${name} /pass`;
    default:
      return `secret-tool store --label="Nugget ${name}" service dev.nugget.Nugget username provider:${name}`;
  }
}

export function envCommand(os: Os, env: string): string {
  return os === "windows" ? `setx ${env} "…"` : `export ${env}="…"`;
}
// i18n-ignore-end

function ByokGuide() {
  const t = useT();
  const os = detectOs();
  const selectId = useId();
  const [kind, setKind] = useState<ProviderKind>("anthropic");
  const guide = PROVIDER_GUIDES[kind];

  return (
    <section className="ob-section" aria-labelledby="ob-byok">
      <h2 id="ob-byok">{t("onboarding.byok.heading")}</h2>
      <p className="note">{t("onboarding.byok.never")}</p>
      <div className="field-inline">
        <label htmlFor={selectId}>{t("onboarding.byok.provider")}</label>
        <select id={selectId} value={kind} onChange={(e) => setKind(e.target.value as ProviderKind)}>
          {(Object.keys(PROVIDER_GUIDES) as ProviderKind[]).map((k) => (
            <option key={k} value={k}>
              {t(PROVIDER_GUIDES[k].label)}
            </option>
          ))}
        </select>
      </div>
      <ByokForm kind={kind} />
      <ol className="ob-guide">
        <li>
          <h3>{t("onboarding.byok.step1")}</h3>
          {guide.key ? (
            <>
              <p>{t("onboarding.byok.step1.keychain")}</p>
              <Snippet text={keychainCommand(os, guide.key.name)} what={t("onboarding.byok.copyKeychain")} />
              <p>{t("onboarding.byok.step1.env")}</p>
              <Snippet text={envCommand(os, guide.key.env)} what={t("onboarding.byok.copyEnv")} />
            </>
          ) : (
            <p>{t("onboarding.byok.step1.noKey")}</p>
          )}
        </li>
        <li>
          <h3>{t("onboarding.byok.step2")}</h3>
          <p>
            <Trans k="onboarding.byok.step2.where" args={{ path: configPath(os) }} tags={{ code: (c) => <code>{c}</code> }} />
          </p>
          <Snippet text={guide.config} what={t("onboarding.byok.copyConfig")} />
          {guide.key && (
            <p className="muted small">
              <Trans
                k="onboarding.byok.step2.envSwap"
                args={{ ref: `env:${guide.key.env}` }}
                tags={{ code: (c) => <code>{c}</code> }}
              />
            </p>
          )}
        </li>
        <li>
          <h3>{t("onboarding.byok.step3")}</h3>
          <p>{t("onboarding.byok.step3.body")}</p>
        </li>
      </ol>
      <p className="muted small">{t("onboarding.byok.egress")}</p>
    </section>
  );
}

function Snippet({ text, what }: { text: string; what: string }) {
  return (
    <div className="ob-snippet">
      <pre tabIndex={0} aria-label={what} dir="ltr">
        {text}
      </pre>
      <CopyButton text={text} what={what} />
    </div>
  );
}
