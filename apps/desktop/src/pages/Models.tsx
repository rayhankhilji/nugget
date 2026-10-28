import { useState } from "react";
import { api, errorMessage, events, type ModelInfo, type ModelProgress } from "../api";
import { useAsync, useEvent } from "../hooks";
import { formatBytes, percent } from "../format";
import { Badge, EmptyState, ErrorNote, Loading, PageHeader } from "../components/ui";
import { t as translate, useT, type MessageKey } from "../i18n";

const PHASES = new Set(["downloading", "verifying", "installed", "failed"]);

/** Label for an install phase; unknown phases (a newer daemon) show as sent. */
export function phaseLabel(phase: string): string {
  return PHASES.has(phase) ? translate(`models.phase.${phase}` as MessageKey) : phase;
}

export function ModelsPage() {
  const t = useT();
  const models = useAsync(api.modelsList);
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

  const anyInstalled = list.some((m) => m.installed || progress[m.id]?.phase === "installed");

  return (
    <div className="page">
      <PageHeader
        title={t("models.title")}
        description={t("models.description")}
        actions={
          <button type="button" className="btn btn-ghost" onClick={models.reload}>
            {t("common.refresh")}
          </button>
        }
      />
      <ErrorNote error={models.error} onRetry={models.reload} />
      {models.loading && !models.data && <Loading label={t("models.loading")} />}
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
      {list.length > 0 && !anyInstalled && (
        <EmptyState title={t("models.noneInstalledTitle")}>{t("models.noneInstalledBody")}</EmptyState>
      )}
      {list.length > 0 && (
        <ul className={models.loading ? "cards pg-stale" : "cards"} aria-label={t("models.listLabel")} aria-busy={models.loading}>
          {list.map((m) => (
            <ModelCard
              key={m.id}
              model={m}
              progress={progress[m.id]}
              error={requestError[m.id]}
              onInstall={() => void install(m.id)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function ModelCard({
  model,
  progress,
  error,
  onInstall,
}: {
  model: ModelInfo;
  progress?: ModelProgress;
  error?: string;
  onInstall: () => void;
}) {
  const t = useT();
  const busy = progress && (progress.phase === "downloading" || progress.phase === "verifying");
  const failed = progress?.phase === "failed";
  const installed = model.installed || progress?.phase === "installed";
  const pct = progress ? percent(progress.downloaded, progress.total || model.size_bytes) : 0;
  const labelId = `model-${model.id.replace(/[^a-zA-Z0-9_-]/g, "_")}`;

  return (
    <li className="card" aria-labelledby={labelId}>
      <div className="card-head">
        <span id={labelId} className="card-title">
          {model.id}
        </span>
        <span className="badges">
          {model.recommended && <Badge tone="accent">{t("models.recommended")}</Badge>}
          {installed && <Badge tone="good">{t("models.installed")}</Badge>}
        </span>
      </div>
      <p className="muted small">{formatBytes(model.size_bytes)}</p>

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
          <button type="button" className="btn" onClick={onInstall}>
            {failed || error ? t("models.retryInstall") : t("models.install")}
          </button>
        </div>
      )}
    </li>
  );
}
