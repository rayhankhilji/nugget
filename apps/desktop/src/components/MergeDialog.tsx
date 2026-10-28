import { useId, useState, type FormEvent } from "react";
import { api, type EntityDetail, type EntitySummaryView } from "../api";
import { useAsync, useDebounced } from "../hooks";
import { kindLabel } from "../knowledge";
import { Dialog } from "./Dialog";
import { ErrorNote, Spinner } from "./ui";

const RESULTS = 20;

/**
 * Merge another entity with `target`: pick it, choose which one stays, see what the result
 * will look like, then confirm. The merged-away id keeps resolving (links never break).
 */
export function MergeDialog({
  target,
  onClose,
  onMerged,
}: {
  target: EntityDetail;
  onClose: () => void;
  onMerged: (into: string) => void;
}) {
  const [text, setText] = useState(target.name);
  const query = useDebounced(text.trim(), 250);
  const [picked, setPicked] = useState<EntitySummaryView | null>(null);
  const [keep, setKeep] = useState<"target" | "other">("target");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const searchId = useId();
  const listId = useId();

  const results = useAsync(
    () => api.knowledgeEntities({ text: query || null, limit: RESULTS + 1 }).then((r) => r.filter((e) => e.id !== target.id).slice(0, RESULTS)),
    [query, target.id],
  );
  const other = useAsync(() => (picked ? api.knowledgeEntity(picked.id) : Promise.resolve(null)), [picked?.id]);
  const otherDetail = other.data && other.data.id === picked?.id ? other.data : null;

  const into = keep === "target" ? target : otherDetail;
  const from = keep === "target" ? otherDetail : target;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!into || !from) return;
    setBusy(true);
    setError(null);
    try {
      await api.knowledgeMerge({ into: into.id, from: from.id });
      onMerged(into.id);
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={`Merge with ${target.name}`}
      description="Use this when two entries are the same person, project or thing. Facts, mentions and aliases are combined."
      onClose={onClose}
      wide
    >
      <form onSubmit={submit}>
        <div className="field">
          <label htmlFor={searchId}>Find the duplicate</label>
          <input
            id={searchId}
            type="search"
            value={text}
            onChange={(e) => setText(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            data-autofocus=""
            aria-controls={listId}
          />
        </div>
        <fieldset className="field">
          <legend>Entity to merge</legend>
          <div id={listId} className="merge-results" aria-busy={results.loading || undefined}>
            <ErrorNote error={results.error} onRetry={results.reload} />
            {results.loading && !results.data && <Spinner />}
            {results.data && results.data.length === 0 && <p className="muted small">No other entities match “{query}”.</p>}
            {results.data?.map((r) => (
              <label key={r.id} className="check merge-option">
                <input type="radio" name="merge-other" checked={picked?.id === r.id} onChange={() => setPicked(r)} />
                <span>
                  <span className="merge-name">{r.name}</span>{" "}
                  <span className="muted small">
                    {kindLabel(r.kind)} · {r.claims.toLocaleString()} facts · {r.mentions.toLocaleString()} mentions
                  </span>
                </span>
              </label>
            ))}
          </div>
        </fieldset>

        {picked && (
          <fieldset className="field">
            <legend>Keep</legend>
            <label className="check">
              <input type="radio" name="merge-keep" checked={keep === "target"} onChange={() => setKeep("target")} />
              <span>
                {target.name} <span className="muted small">({target.id})</span>
              </span>
            </label>
            <label className="check">
              <input type="radio" name="merge-keep" checked={keep === "other"} onChange={() => setKeep("other")} />
              <span>
                {picked.name} <span className="muted small">({picked.id})</span>
              </span>
            </label>
          </fieldset>
        )}

        {picked && (
          <section className="merge-preview" aria-live="polite" aria-label="Merge preview">
            {other.loading && !otherDetail && <Spinner label="Loading preview" />}
            <ErrorNote error={other.error} onRetry={other.reload} />
            {into && from && <MergePreview into={into} from={from} />}
          </section>
        )}

        <ErrorNote error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy || !into || !from}>
            {busy ? <Spinner label="Merging" /> : "Merge"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

function MergePreview({ into, from }: { into: EntityDetail; from: EntityDetail }) {
  const aliases = [...new Set([...into.aliases, from.name, ...from.aliases])].filter((a) => a !== into.name);
  const sameKind = into.kind === from.kind;
  return (
    <>
      <p>
        <b>{from.name}</b> will be merged into <b>{into.name}</b>.
      </p>
      <dl className="entity-props">
        <div>
          <dt>Name</dt>
          <dd>
            {into.name} <span className="muted small">({kindLabel(into.kind)})</span>
          </dd>
        </div>
        <div>
          <dt>Aliases</dt>
          <dd>{aliases.length > 0 ? aliases.join(", ") : <span className="muted">None</span>}</dd>
        </div>
        <div>
          <dt>Facts</dt>
          <dd>
            up to {(into.claims.length + from.claims.length).toLocaleString()}{" "}
            <span className="muted small">
              ({into.claims.length.toLocaleString()} + {from.claims.length.toLocaleString()}; duplicates are combined)
            </span>
          </dd>
        </div>
        <div>
          <dt>Mentions</dt>
          <dd>{(into.mentions.length + from.mentions.length).toLocaleString()}</dd>
        </div>
      </dl>
      <p className="muted small">
        Links and apps that refer to <code>{from.id}</code> keep working: it becomes an alias of <code>{into.id}</code>.
      </p>
      {!sameKind && (
        <p className="note small" role="status">
          These are different kinds ({kindLabel(from.kind)} and {kindLabel(into.kind)}). The result is a {kindLabel(into.kind)}.
        </p>
      )}
    </>
  );
}
