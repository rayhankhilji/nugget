import { useState } from "react";
import { api, errorMessage, events, type ModelInfo, type ModelProgress } from "../api";
import { useAsync, useEvent } from "../hooks";
import { formatBytes, percent } from "../format";
import { Badge, Empty, ErrorNote, PageHeader, Spinner } from "../components/ui";

const PHASE_LABEL: Record<string, string> = {
  downloading: "Downloading",
  verifying: "Verifying",
  installed: "Installed",
  failed: "Failed",
};

export function ModelsPage() {
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

  return (
    <div className="page">
      <PageHeader
        title="Models"
        description="Local models are downloaded from a signed manifest and verified before use."
        actions={
          <button type="button" className="btn btn-ghost" onClick={models.reload} disabled={models.loading}>
            Refresh
          </button>
        }
      />
      <ErrorNote error={models.error} onRetry={models.reload} />
      {models.loading && !models.data && <Spinner />}
      {models.data && list.length === 0 && <Empty title="No models available">The model manifest is empty.</Empty>}
      <ul className="cards">
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
          {model.recommended && <Badge tone="accent">Recommended</Badge>}
          {installed && <Badge tone="good">Installed</Badge>}
        </span>
      </div>
      <p className="muted small">{formatBytes(model.size_bytes)}</p>

      {busy && progress && (
        <div className="progress">
          <progress max={100} value={pct} aria-labelledby={labelId} aria-valuetext={`${PHASE_LABEL[progress.phase] ?? progress.phase} ${pct}%`} />
          <span className="small muted">
            {PHASE_LABEL[progress.phase] ?? progress.phase} · {pct}% · {formatBytes(progress.downloaded)} of{" "}
            {formatBytes(progress.total || model.size_bytes)}
          </span>
        </div>
      )}
      {(failed || error) && (
        <p className="note note-error" role="alert">
          {progress?.error || error || "Install failed."}
        </p>
      )}

      {!installed && !busy && (
        <div className="card-actions">
          <button type="button" className="btn" onClick={onInstall}>
            {failed || error ? "Retry install" : "Install"}
          </button>
        </div>
      )}
    </li>
  );
}
