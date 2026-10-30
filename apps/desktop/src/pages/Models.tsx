import { useState } from "react";
import { api, errorMessage, events, type FitReport, type ModelFit, type ModelInfo, type ModelProgress } from "../api";
import { useAsync, useEvent } from "../hooks";
import { formatBytes, percent } from "../format";
import { Badge, EmptyState, ErrorNote, Loading, PageHeader } from "../components/ui";
import { t as translate, useT, type MessageKey } from "../i18n";
import { FitNotes, loadFit, MachineLine } from "../onboarding/ModelFit";

const PHASES = new Set(["downloading", "verifying", "installed", "failed"]);

/** Label for an install phase; unknown phases (a newer daemon) show as sent. */
export function phaseLabel(phase: string): string {
  return PHASES.has(phase) ? translate(`models.phase.${phase}` as MessageKey) : phase;
}

/** What a model is for, from its tiers (or, on an older daemon, its id). */
export type ModelRole = "chat" | "embedding" | "rerank";

export function modelRole(m: Pick<ModelInfo, "id" | "tiers">, fit?: Pick<ModelFit, "tiers"> | null): ModelRole {
  const tiers = m.tiers?.length ? m.tiers : (fit?.tiers ?? []);
  if (tiers.length) {
    if (tiers.every((x) => x === "rerank")) return "rerank";
    if (tiers.every((x) => x === "embedding" || x === "rerank")) return "embedding";
    return "chat";
  }
  // No tier data (older daemon): known search families and name patterns.
  if (/(^|[-_])rerank/i.test(m.id)) return "rerank";
  if (/(^|[-_])embed/i.test(m.id) || /(^|[-_])(bge|e5|gte|minilm|mpnet)([-_.]|$)/i.test(m.id)) return "embedding";
  return "chat";
}

const ROLE_LABEL: Record<ModelRole, MessageKey> = {
  chat: "models.role.chat",
  embedding: "models.role.embedding",
  rerank: "models.role.rerank",
};

/** Quantisation suffixes in manifest ids (`q4km`, `q8`, `q4_k_m`, `f16`): not part of the name. */
const ACRONYMS = new Set(["bge", "gte", "e5", "ai", "llm"]);
const QUANT = /^(q\d.*|f16|f32|bf16|gguf|iq\d.*)$/i;

/**
 * A readable name from a manifest id, since the manifest carries none:
 * `qwen3-4b-q4km` → "Qwen3 4B", `bge-reranker-v2-m3-q4km` → "BGE Reranker v2 M3".
 */
export function modelName(id: string): string {
  return baseName(id);
}

/** The quantisation suffix as people write it (`q4km` → "Q4_K_M", `q8` → "Q8"), or null. */
export function quantLabel(id: string): string | null {
  const m = /[-_](i?q\d\w*|f16|bf16|f32)$/i.exec(id);
  if (!m) return null;
  const q = m[1]!.toUpperCase();
  return /^I?Q\d[A-Z]+$/.test(q) ? q.replace(/^(I?Q\d)([A-Z])([A-Z])?$/, (_, a: string, b: string, c?: string) => [a, b, c].filter(Boolean).join("_")) : q;
}

/**
 * Readable names for a list of ids: the base name, plus the quantisation when two ids would
 * otherwise read the same (`qwen3-4b-q4km`, `qwen3-4b-q8` → "Qwen3 4B Q4_K_M", "Qwen3 4B Q8").
 */
export function modelNames(ids: readonly string[]): Map<string, string> {
  const base = new Map(ids.map((id) => [id, baseName(id)]));
  const count = new Map<string, number>();
  for (const n of base.values()) count.set(n, (count.get(n) ?? 0) + 1);
  return new Map(
    ids.map((id) => {
      const n = base.get(id)!;
      const q = quantLabel(id);
      return [id, (count.get(n) ?? 0) > 1 ? (q ? `${n} ${q}` : `${n} (${id})`) : n];
    }),
  );
}

function baseName(id: string): string {
  const words = id
    .replace(/[-_](i?q\d\w*|f16|bf16|f32)$/i, "")
    .split(/[-_\s]+/)
    .filter((w) => w && !QUANT.test(w))
    .map((w) => {
      if (/^\d+(\.\d+)?[bmk]$/i.test(w)) return w.toUpperCase(); // 4b → 4B
      if (/^v\d/i.test(w)) return w.toLowerCase(); // v1.5
      if (/^[a-z]\d+$/i.test(w)) return w.toUpperCase(); // m3 → M3
      if (ACRONYMS.has(w.toLowerCase())) return w.toUpperCase(); // bge → BGE
      return w.charAt(0).toUpperCase() + w.slice(1);
    });
  return words.length ? words.join(" ") : id;
}

/**
 * The one chat model to badge "Best for this computer": the daemon's largest that fits memory.
 * Without fit data (an older daemon) there is no basis for one, so none (cards keep "Recommended").
 */
export function bestChatModel(chat: readonly ModelInfo[], fit: FitReport | null | undefined): ModelInfo | undefined {
  if (!fit) return undefined;
  return chat.find((m) => m.recommended && fit.models.find((f) => f.id === m.id)?.reasons.includes("ram_fits_largest"));
}

export function ModelsPage() {
  const t = useT();
  const models = useAsync(api.modelsList);
  const fit = useAsync(loadFit); // `null` on an older daemon: no explanations
  const [progress, setProgress] = useState<Record<string, ModelProgress>>({});
  const [requestError, setRequestError] = useState<Record<string, string>>({});

  useEvent(events.onModelsProgress, (p) => {
    setProgress((all) => ({ ...all, [p.id]: p }));
    if (p.phase === "installed") models.reload();
  });

  const install = async (id: string) => {
    setRequestError((e) => ({ ...e, [id]: "" }));
    setProgress((all) => ({ ...all, [id]: { id, downloaded: 0, total: 0, phase: "downloading", error: null } }));
    try {
      await api.modelsInstall(id);
    } catch (err) {
      setProgress(({ [id]: _, ...rest }) => rest);
      setRequestError((e) => ({ ...e, [id]: errorMessage(err) }));
    }
  };

  // Recommended first, then installed, then by id.
  const list = [...(models.data ?? [])].sort(
    (a, b) => Number(b.recommended) - Number(a.recommended) || Number(b.installed) - Number(a.installed) || a.id.localeCompare(b.id),
  );
  const fitOf = (m: ModelInfo) => fit.data?.models.find((f) => f.id === m.id);
  const chat = list.filter((m) => modelRole(m, fitOf(m)) === "chat");
  const search = list.filter((m) => modelRole(m, fitOf(m)) !== "chat");
  const best = bestChatModel(chat, fit.data);
  const names = modelNames(list.map((m) => m.id));

  const anyInstalled = list.some((m) => m.installed || progress[m.id]?.phase === "installed");
  const card = (m: ModelInfo) => (
    <ModelCard
      key={m.id}
      model={m}
      role={modelRole(m, fitOf(m))}
      name={names.get(m.id) ?? modelName(m.id)}
      best={m === best}
      hasBest={best !== undefined}
      recommended={m.recommended}
      progress={progress[m.id]}
      error={requestError[m.id]}
      fit={fit.data}
      onInstall={() => void install(m.id)}
    />
  );
  const groupClass = models.loading ? "cards pg-stale" : "cards";

  return (
    <div className="page">
      <PageHeader
        title={t("models.title")}
        description={t("models.description")}
        actions={
          <button type="button" className="btn btn-ghost" onClick={models.reload} disabled={models.loading && !models.data}>
            {t("common.refresh")}
          </button>
        }
      />
      {fit.data && <MachineLine report={fit.data} />}
      <ErrorNote error={models.error} onRetry={models.reload} />
      {models.loading && !models.data && (
        <>
          <Loading label={t("models.loading")} lines={0} />
          <ul className="cards" aria-hidden="true">
            {[0, 1, 2, 3].map((i) => (
              <li key={i} className="card pg-skeleton-card">
                <span className="pg-skeleton" />
                <span className="pg-skeleton" />
                <span className="pg-skeleton" />
              </li>
            ))}
          </ul>
        </>
      )}
      {models.data && list.length === 0 && (
        <EmptyState
          title={t("models.emptyTitle")}
          action={
            <button type="button" className="btn" onClick={models.reload}>
              {t("common.refresh")}
            </button>
          }
        >
          {t("models.emptyBody")}
        </EmptyState>
      )}
      {list.length > 0 && !anyInstalled && <p className="models-intro">{t("models.noneInstalledBody")}</p>}
      {chat.length > 0 && (
        <section className="models-group" aria-labelledby="models-chat-title">
          <h2 id="models-chat-title">{t("models.group.chat")}</h2>
          <p className="muted small">{t("models.group.chatHint")}</p>
          <ul className={groupClass} aria-label={t("models.listLabel")} aria-busy={models.loading}>
            {chat.map(card)}
          </ul>
        </section>
      )}
      {search.length > 0 && (
        <section className="models-group" aria-labelledby="models-search-title">
          <h2 id="models-search-title">{t("models.group.search")}</h2>
          <p className="muted small">{t("models.group.searchHint")}</p>
          <ul className={groupClass} aria-label={t("models.group.search")} aria-busy={models.loading}>
            {search.map(card)}
          </ul>
        </section>
      )}
    </div>
  );
}

function ModelCard({
  model,
  role,
  name,
  best,
  hasBest,
  recommended,
  progress,
  error,
  fit,
  onInstall,
}: {
  model: ModelInfo;
  role: ModelRole;
  name: string;
  /** The one chat model to install: "Best for this computer", and a primary Install button. */
  best: boolean;
  /** Some chat model is the best pick, so other chat cards drop their "Recommended" badge. */
  hasBest: boolean;
  /** Recommended by the daemon; shown as such when there is no single best pick in this group. */
  recommended: boolean;
  progress?: ModelProgress;
  error?: string;
  fit?: FitReport | null;
  onInstall: () => void;
}) {
  const t = useT();
  const busy = progress && (progress.phase === "downloading" || progress.phase === "verifying");
  const failed = progress?.phase === "failed";
  const installed = model.installed || progress?.phase === "installed";
  const pct = progress ? percent(progress.downloaded, progress.total || model.size_bytes) : 0;
  const labelId = `model-${model.id.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const modelFit = fit?.models.find((f) => f.id === model.id);

  return (
    <li className="card model-card" aria-labelledby={labelId} data-model-id={model.id}>
      <div className="card-head">
        <span className="model-title">
          <span id={labelId} className="card-title">
            {name}
          </span>
          <bdi dir="ltr" className="model-id">
            <code>{model.id}</code>
          </bdi>
        </span>
        <span className="badges">
          {!installed && best && <Badge tone="accent">{t("models.best")}</Badge>}
          {!installed && !best && recommended && (role !== "chat" || !hasBest) && <Badge tone="accent">{t("models.recommended")}</Badge>}
          {installed && <Badge tone="good">{t("models.installed")}</Badge>}
        </span>
      </div>
      <p className="small">
        {t(ROLE_LABEL[role])} <span className="muted">· {formatBytes(model.size_bytes)}</span>
      </p>
      {modelFit && <FitNotes fit={modelFit} freeDisk={fit?.free_disk_bytes ?? null} installed={installed} why={false} />}
      {modelFit && modelFit.fits_ram && modelFit.min_ram_gb >= modelFit.machine_ram_gb && !installed && (
        <p className="note note-warn small">{t("models.tightFit", { ram: modelFit.machine_ram_gb })}</p>
      )}

      {busy && progress && (
        <div className="progress">
          <progress max={100} value={pct} aria-labelledby={labelId} aria-valuetext={t("models.progressValueText", { phase: phaseLabel(progress.phase), pct })} />
          <span className="small muted">
            {t("models.progress", {
              phase: phaseLabel(progress.phase),
              pct,
              done: formatBytes(progress.downloaded),
              total: formatBytes(progress.total || model.size_bytes),
            })}
          </span>
        </div>
      )}
      {(failed || error) && (
        <p className="note note-error" role="alert">
          {progress?.error || error || t("models.installFailed")}
        </p>
      )}

      {!installed && !busy && (
        <div className="card-actions">
          <button type="button" className={best ? "btn btn-primary" : "btn"} onClick={onInstall}>
            {failed || error ? t("models.retryInstall") : t("models.install")}
          </button>
        </div>
      )}
    </li>
  );
}
