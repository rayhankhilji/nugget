import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { api, errorMessage, events, modelsCancel, type FitReport, type ModelInfo, type ModelProgress } from "../api";
import { useAsync, useEvent } from "../hooks";
import { formatBytes, percent } from "../format";
import { Trans, useT, type MessageKey } from "../i18n";
import { Badge, ErrorNote, Spinner } from "../components/ui";
import { defaultOption, type AiOption } from "./machine";
import type { ModelFit } from "../api";
import { ByokForm, type ByokGuideKind } from "./ByokForm";
import { ChatgptPanel } from "./Chatgpt";
import { detectAiSetup, isSearchModel, sortModels } from "./detect";
import { FitNotes, loadFit, MachineLine } from "./ModelFit";
import { DownloadFailureNote, isCancelled, useInstallFocus } from "./DownloadFailure";
import { detectOs, type Os } from "./platform";
import { CommandSnippet, localMode, OfflineNote, StepHeading, type StepProps } from "./steps";

const ICON_PROPS = { width: 18, height: 18, viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", strokeWidth: 1.8, strokeLinecap: "round", strokeLinejoin: "round" } as const;

const ICONS: Record<"chatgpt" | "computer" | "key" | "server" | "later", ReactNode> = {
  chatgpt: (
    <svg {...ICON_PROPS}>
      <circle cx="12" cy="12" r="9" />
      <path d="M8 12h8M12 8v8" />
    </svg>
  ),
  computer: (
    <svg {...ICON_PROPS}>
      <rect x="3" y="5" width="18" height="12" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </svg>
  ),
  key: (
    <svg {...ICON_PROPS}>
      <circle cx="8" cy="15" r="4" />
      <path d="M11 12l9-9M16 7l3 3" />
    </svg>
  ),
  server: (
    <svg {...ICON_PROPS}>
      <rect x="4" y="4" width="16" height="7" rx="2" />
      <rect x="4" y="13" width="16" height="7" rx="2" />
      <path d="M8 7.5h.01M8 16.5h.01" />
    </svg>
  ),
  later: (
    <svg {...ICON_PROPS}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </svg>
  ),
};

interface OptionInfo {
  id: AiOption;
  title: MessageKey;
  blurb: MessageKey;
  /** The quiet right-hand line ("Opens your browser", "Private"). */
  status?: MessageKey;
  icon: keyof typeof ICONS;
}

/** Every way in, one row each. ChatGPT is left out when Heed Cloud is the path. */
const OPTIONS: OptionInfo[] = [
  { id: "chatgpt", title: "onboarding.ai.chatgpt", blurb: "onboarding.ai.chatgptBlurb", status: "onboarding.ai.chatgptStatus", icon: "chatgpt" },
  { id: "local", title: "onboarding.ai.local", blurb: "onboarding.ai.localBlurb", status: "onboarding.ai.private", icon: "computer" },
  { id: "ollama", title: "onboarding.ai.ollama", blurb: "onboarding.ai.ollamaBlurb", status: "onboarding.ai.private", icon: "computer" },
  { id: "anthropic", title: "onboarding.ai.anthropic", blurb: "onboarding.ai.anthropicBlurb", icon: "key" },
  { id: "openai", title: "onboarding.ai.openai", blurb: "onboarding.ai.openaiBlurb", icon: "key" },
  { id: "openrouter", title: "onboarding.ai.openrouter", blurb: "onboarding.ai.openrouterBlurb", icon: "key" },
  { id: "compatible", title: "onboarding.ai.compatible", blurb: "onboarding.ai.compatibleBlurb", icon: "server" },
  { id: "later", title: "onboarding.ai.later", blurb: "onboarding.ai.laterBlurb", icon: "later" },
];

/** The rows that set up a provider through `ai.key.*`. */
function isKeyOption(o: AiOption): o is ByokGuideKind {
  return o === "ollama" || o === "anthropic" || o === "openai" || o === "openrouter" || o === "compatible";
}

/**
 * "Choose your AI": one row per way in (`.ai-choices` of `.ai-choice` rows, docs/brand/open-core.html);
 * the chosen row opens beneath it to show its one step. Sign in with ChatGPT is offered only when
 * running Heed yourself.
 */
export function AiStep({ headingRef, state, dispatch, connected, cloud }: StepProps) {
  const t = useT();
  const name = useId();
  const local = localMode(state, cloud);
  const options = OPTIONS.filter((o) => o.id !== "chatgpt" || local);
  const saved = state.aiOption ?? defaultOption(state.ai);
  const option = saved === "chatgpt" && !local ? null : saved;
  const group = useRef<HTMLDivElement>(null);
  // The option badged "Recommended" starts selected; the user can still pick another.
  useEffect(() => {
    if (state.ai === null || option === null) dispatch({ type: "chooseAiOption", option: "local" });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  // Picking a row opens its body: bring that into view.
  const picked = useRef(false);
  useEffect(() => {
    if (!picked.current) return;
    const body = group.current?.querySelector(".ai-choice-body");
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    body?.scrollIntoView?.({ block: "nearest", behavior: reduce ? "auto" : "smooth" });
  }, [option]);
  const choose = (o: AiOption) => {
    picked.current = true;
    dispatch({ type: "chooseAiOption", option: o });
  };
  // "Use another AI": back to the rows, with nothing switched for the user.
  const toChoices = () => group.current?.querySelector<HTMLInputElement>("input:checked")?.focus();

  const body = (o: AiOption) => {
    if (o === "chatgpt")
      return (
        <ChatgptPanel
          onReady={(what) => dispatch({ type: "aiSetUp", ai: "chatgpt", what })}
          onUseKey={() => choose("openai")}
          onOtherAi={toChoices}
        />
      );
    if (o === "local")
      return connected ? <LocalModels onReady={(what) => dispatch({ type: "aiSetUp", ai: "local", what })} /> : <OfflineNote />;
    if (isKeyOption(o)) return <ByokGuide kind={o} onSaved={(what) => dispatch({ type: "aiSetUp", ai: "byok", what })} />;
    return <p className="muted">{t("onboarding.ai.laterDetail")}</p>;
  };

  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.ai.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.ai.lead")}</p>
      <div className="ai-choices" role="radiogroup" aria-label={t("onboarding.ai.legend")} ref={group}>
        {options.map((o) => {
          const checked = option === o.id;
          return (
            <div key={o.id} className={checked ? "ai-choice is-selected" : "ai-choice"} data-option={o.id}>
              <label className="ai-choice-label">
                <input type="radio" name={name} value={o.id} checked={checked} onChange={() => choose(o.id)} />
                <span className="ai-choice-icon" aria-hidden="true">
                  {ICONS[o.icon]}
                </span>
                <span>
                  <span className="ai-choice-title">{t(o.title)}</span>
                  {o.id === "local" && <Badge tone="accent">{t("onboarding.ai.localBadge")}</Badge>}
                  <span className="ai-choice-blurb">{t(o.blurb)}</span>
                </span>
                <span className="ai-choice-status">{o.status ? t(o.status) : ""}</span>
              </label>
              {checked && <div className="ai-choice-body">{body(o.id)}</div>}
            </div>
          );
        })}
      </div>
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

export { isSearchModel, sortModels };

/**
 * The one chat model to suggest: the daemon's own pick for this machine (the largest that fits
 * memory) when `models.fit` says so, else the first recommended one.
 */
export function bestModel(chat: readonly ModelInfo[], fits: readonly ModelFit[] | undefined): ModelInfo | undefined {
  const rec = chat.filter((m) => m.recommended);
  const largest = rec.find((m) => fits?.find((f) => f.id === m.id)?.reasons.includes("ram_fits_largest"));
  return largest ?? rec[0];
}

function LocalModels({ onReady }: { onReady: (model: string) => void }) {
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
    // A retry resumes from the staged bytes: keep showing them until the daemon reports.
    setProgress((cur) => ({ ...cur, [id]: { id, downloaded: cur[id]?.downloaded ?? 0, total: cur[id]?.total ?? 0, phase: "downloading", error: null } }));
    try {
      await api.modelsInstall(id);
    } catch (e) {
      setProgress(({ [id]: _, ...rest }) => rest);
      setRequestError((cur) => ({ ...cur, [id]: errorMessage(e) }));
    }
  };

  // The daemon answers with a final `failed` / `cancelled` progress; staged bytes are kept.
  const cancel = async (id: string): Promise<boolean> => {
    try {
      return (await modelsCancel(id)).cancelled;
    } catch (e) {
      setRequestError((cur) => ({ ...cur, [id]: errorMessage(e) }));
      return false;
    }
  };

  const fitOf = (m: ModelInfo) => fit.data?.models.find((f) => f.id === m.id);
  const sorted = sortModels(models.data ?? []);
  const chat = sorted.filter((m) => !isSearchModel(m, fitOf(m)));
  const search = sorted.filter((m) => isSearchModel(m, fitOf(m)));
  const recommended = sorted.filter((m) => m.recommended);
  const visible = (list: ModelInfo[]) => (all || recommended.length === 0 ? list : list.filter((m) => m.recommended));
  const best = bestModel(chat, fit.data?.models);

  // Only an installed chat model sets up this path (Continue then marks the step done): a download
  // can still fail or be cancelled.
  const ready = chat.find((m) => m.installed || progress[m.id]?.phase === "installed");
  useEffect(() => {
    if (ready) onReady(ready.id);
  }, [ready?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const rows = (list: ModelInfo[]) =>
    list.map((m) => (
      <ModelRow
        key={m.id}
        model={m}
        best={m === best}
        progress={progress[m.id]}
        error={requestError[m.id]}
        fit={fit.data}
        onInstall={() => void install(m.id)}
        onCancel={() => cancel(m.id)}
      />
    ));
  const shownChat = visible(chat);
  const shownSearch = visible(search);

  return (
    <section className="ob-section" aria-labelledby="ob-models">
      <h2 id="ob-models">{t("onboarding.models.heading")}</h2>
      <p className="muted">{t("onboarding.models.lead")}</p>
      {fit.data && <MachineLine report={fit.data} />}
      <ErrorNote error={models.error} onRetry={models.reload} />
      {models.loading && !models.data && <Spinner />}
      {models.data && recommended.length === 0 && <p className="note">{t("onboarding.models.none")}</p>}
      {shownChat.length > 0 && (
        <>
          {shownSearch.length > 0 && <h3 className="ob-models-group">{t("onboarding.models.chatGroup")}</h3>}
          <ul className="cards ob-models">{rows(shownChat)}</ul>
        </>
      )}
      {shownSearch.length > 0 && (
        <>
          <h3 className="ob-models-group">{t("onboarding.models.searchGroup")}</h3>
          <p className="muted small">{t("onboarding.models.searchLead")}</p>
          <ul className="cards ob-models ob-models-search">{rows(shownSearch)}</ul>
        </>
      )}
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
  best = false,
  progress,
  error,
  fit,
  onInstall,
  onCancel,
}: {
  model: ModelInfo;
  /** The one model to suggest: badged, explained, and its Install is the primary button. */
  best?: boolean;
  progress?: ModelProgress;
  error?: string;
  fit?: FitReport | null;
  onInstall: () => void;
  /** Resolves `false` if nothing was cancelled (the request failed or the install had ended). */
  onCancel: () => Promise<boolean>;
}) {
  const t = useT();
  const busy = progress?.phase === "downloading" || progress?.phase === "verifying";
  const failed = progress?.phase === "failed";
  const cancelled = isCancelled(progress);
  const installed = model.installed || progress?.phase === "installed";
  const focus = useInstallFocus(busy, installed);
  const total = progress?.total || model.size_bytes;
  const pct = progress ? percent(progress.downloaded, total) : 0;
  const labelId = `ob-model-${model.id.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const phase = progress ? t(PHASE[progress.phase]) : "";
  const modelFit = fit?.models.find((f) => f.id === model.id);
  const left = useEta(progress, total);

  return (
    <li className={best ? "card ob-model-best" : "card"} aria-labelledby={labelId}>
      <div className="card-head">
        <bdi id={labelId} ref={focus.titleRef} className="card-title" tabIndex={-1}>
          {model.id}
        </bdi>
        <span className="badges">
          {best && <Badge tone="accent">{t("onboarding.ai.localBadge")}</Badge>}
          {installed && <Badge tone="good">{t("onboarding.models.installed")}</Badge>}
        </span>
      </div>
      <p className="muted small">
        {t("onboarding.models.size", { size: formatBytes(model.size_bytes) })}
        {/* FSI…PDI: the license id keeps its own direction inside right-to-left text. */}
        {model.license && <> · {t("onboarding.models.license", { license: `\u2068${model.license}\u2069` })}</>}
      </p>
      {!installed && !busy && model.size_bytes > 0 && (
        <p className="muted small ob-model-estimate">
          {t("onboarding.models.estimate", {
            fast: etaText(t, downloadSeconds(model.size_bytes, FAST_MBPS)),
            slow: etaText(t, downloadSeconds(model.size_bytes, SLOW_MBPS)),
          })}
        </p>
      )}
      {modelFit && <FitNotes fit={modelFit} freeDisk={fit?.free_disk_bytes ?? null} installed={installed} why={best} />}
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
            {left !== null && progress.phase === "downloading" && <> · {t("onboarding.models.left", { time: etaText(t, left) })}</>}
          </span>
          <div className="card-actions">
            <button type="button" className="btn btn-ghost" aria-label={t("models.cancelLabel", { model: model.id })} {...focus.cancelProps(onCancel)}>
              {t("models.cancel")}
            </button>
          </div>
        </div>
      )}
      <DownloadFailureNote t={t} progress={progress} error={error} fallback={t("onboarding.models.failed")} />
      {!installed && !busy && (
        <div className="card-actions">
          <button
            ref={focus.actionRef}
            type="button"
            className={best ? "btn btn-primary" : "btn"}
            onClick={(e) => {
              focus.onInstallClick(e);
              onInstall();
            }}
          >
            {cancelled ? t("models.resume") : failed || error ? t("onboarding.models.retry") : t("onboarding.models.install")}
          </button>
        </div>
      )}
    </li>
  );
}

// ---- download time ----

/** Typical fast and slow home connections, in megabits per second, for the estimate before a download. */
export const FAST_MBPS = 100;
export const SLOW_MBPS = 10;

/** Seconds to download `bytes` at `mbps` megabits per second. */
export function downloadSeconds(bytes: number, mbps: number): number {
  return (bytes * 8) / (mbps * 1_000_000);
}

/** "less than a minute", "about 4 minutes", "about 2 hours". */
export function etaText(t: ReturnType<typeof useT>, secs: number): string {
  if (secs < 60) return t("onboarding.models.etaUnderMinute");
  const min = Math.ceil(secs / 60);
  if (min < 90) return t("onboarding.models.etaMinutes", { count: min });
  return t("onboarding.models.etaHours", { count: Math.round(min / 60) });
}

/**
 * Time left for a download, from the throughput measured over the last half minute (null until
 * there are a few seconds of samples, so the first burst doesn't promise too much).
 */
export function useEta(progress: ModelProgress | undefined, total: number): number | null {
  const samples = useRef<{ at: number; bytes: number }[]>([]);
  const [left, setLeft] = useState<number | null>(null);
  const downloaded = progress?.phase === "downloading" ? progress.downloaded : null;
  useEffect(() => {
    if (downloaded === null) {
      samples.current = [];
      setLeft(null);
      return;
    }
    const now = Date.now();
    const list = samples.current;
    // A restarted download starts the measurement again.
    if (list.length > 0 && downloaded < list[list.length - 1]!.bytes) list.length = 0;
    list.push({ at: now, bytes: downloaded });
    while (list.length > 2 && now - list[0]!.at > 30_000) list.shift();
    setLeft(estimateLeft(list, total));
  }, [downloaded, total]);
  return left;
}

/** Seconds left from (time, bytes) samples, or null when they span under 3 s or show no progress. */
export function estimateLeft(samples: readonly { at: number; bytes: number }[], total: number): number | null {
  if (samples.length < 2 || total <= 0) return null;
  const first = samples[0]!;
  const last = samples[samples.length - 1]!;
  const ms = last.at - first.at;
  const bytes = last.bytes - first.bytes;
  if (ms < 3000 || bytes <= 0) return null;
  return Math.max(0, total - last.bytes) / (bytes / (ms / 1000));
}

// ---- bring your own key ----

type ProviderKind = ByokGuideKind;

interface ProviderGuide {
  /** Keychain entry name (`keychain:NAME`) and the env var alternative; `null` when no key is needed. */
  key: { name: string; env: string } | null;
  config: string;
}

// i18n-ignore-block: config snippets and shell commands are code, not prose.
export const PROVIDER_GUIDES: Record<ProviderKind, ProviderGuide> = {
  anthropic: {
    key: { name: "anthropic", env: "ANTHROPIC_API_KEY" },
    config: `[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-opus-5-5"
api_key = "keychain:anthropic"
tiers = ["light", "synthesis", "agentic"]`,
  },
  openai: {
    key: { name: "openai", env: "OPENAI_API_KEY" },
    config: `[[providers]]
id = "openai"
kind = "openai_responses"
model = "MODEL-ID"   # a model id from your OpenAI account
api_key = "keychain:openai"
tiers = ["light", "synthesis", "agentic"]`,
  },
  openrouter: {
    key: { name: "openrouter", env: "OPENROUTER_API_KEY" },
    config: `[[providers]]
id = "openrouter"
kind = "openai_compatible"
base_url = "https://openrouter.ai/api/v1"
model = "MODEL-ID"   # a model id from openrouter.ai/models
api_key = "keychain:openrouter"
tiers = ["light", "synthesis", "agentic"]`,
  },
  ollama: {
    key: null,
    config: `[[providers]]
id = "ollama"
kind = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
model = "llama3.2:3b"   # a model you pulled with ollama pull
tiers = ["light", "synthesis"]`,
  },
  compatible: {
    key: null,
    config: `[[providers]]
id = "custom"
kind = "openai_compatible"
base_url = "http://localhost:8080/v1"   # your server's address, with /v1
model = "MODEL-ID"
tiers = ["light", "synthesis"]
# api_key = "env:NUGGET_CUSTOM_KEY"   # only if the server needs a key`,
  },
};
// i18n-ignore-end

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

function ByokGuide({ kind, onSaved }: { kind: ProviderKind; onSaved: (provider: string) => void }) {
  const t = useT();
  const os = detectOs();
  const guide = PROVIDER_GUIDES[kind];
  // A key saved before (running setup again) sets up this path too.
  useEffect(() => {
    let alive = true;
    void detectAiSetup("byok").then((id) => alive && id && onSaved(id));
    return () => {
      alive = false;
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <section className="ob-section" aria-labelledby="ob-byok">
      <h2 id="ob-byok" className="visually-hidden">
        {t("onboarding.byok.heading")}
      </h2>
      <p className="note">{t(kind === "ollama" ? "onboarding.byok.ollamaNever" : "onboarding.byok.never")}</p>
      <ByokForm kind={kind} onSaved={onSaved} />
      <details className="ob-manual">
        <summary>{t("onboarding.byok.manual")}</summary>
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
      </details>
      <p className="muted small">{t("onboarding.byok.egress")}</p>
    </section>
  );
}

const Snippet = CommandSnippet;
