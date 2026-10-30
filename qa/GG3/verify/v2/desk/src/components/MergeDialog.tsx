import { useId, useState, type FormEvent } from "react";
import { api, type EntityDetail, type EntitySummaryView } from "../api";
import { useAsync, useDebounced } from "../hooks";
import { formatNumber, Trans, useT } from "../i18n";
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
  const t = useT();
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
      title={t("merge.title", { name: target.name })}
      description={t("merge.description")}
      onClose={onClose}
      wide
    >
      <form onSubmit={submit}>
        <div className="field">
          <label htmlFor={searchId}>{t("merge.find")}</label>
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
          <legend>{t("merge.entityToMerge")}</legend>
          <div id={listId} className="merge-results" aria-busy={results.loading || undefined}>
            <ErrorNote error={results.error} onRetry={results.reload} />
            {results.loading && !results.data && <Spinner />}
            {results.data && results.data.length === 0 && <p className="muted small">{t("merge.noMatches", { query })}</p>}
            {results.data?.map((r) => (
              <label key={r.id} className="check merge-option">
                <input type="radio" name="merge-other" checked={picked?.id === r.id} onChange={() => setPicked(r)} />
                <span>
                  <span className="merge-name">{r.name}</span>{" "}
                  <span className="muted small">
                    {t("merge.optionMeta", { kind: kindLabel(r.kind), claims: r.claims, mentions: r.mentions })}
                  </span>
                </span>
              </label>
            ))}
          </div>
        </fieldset>

        {picked && (
          <fieldset className="field">
            <legend>{t("merge.keep")}</legend>
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
          <section className="merge-preview" aria-live="polite" aria-label={t("merge.previewLabel")}>
            {other.loading && !otherDetail && <Spinner label={t("merge.loadingPreview")} />}
            <ErrorNote error={other.error} onRetry={other.reload} />
            {into && from && <MergePreview into={into} from={from} />}
          </section>
        )}

        <ErrorNote error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy || !into || !from}>
            {busy ? <Spinner label={t("merge.merging")} /> : t("merge.merge")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

function MergePreview({ into, from }: { into: EntityDetail; from: EntityDetail }) {
  const t = useT();
  const aliases = [...new Set([...into.aliases, from.name, ...from.aliases])].filter((a) => a !== into.name);
  const sameKind = into.kind === from.kind;
  return (
    <>
      <p>
        <Trans k="merge.summary" args={{ from: from.name, into: into.name }} tags={{ b: (c) => <b>{c}</b> }} />
      </p>
      <dl className="entity-props">
        <div>
          <dt>{t("merge.name")}</dt>
          <dd>
            {into.name} <span className="muted small">({kindLabel(into.kind)})</span>
          </dd>
        </div>
        <div>
          <dt>{t("merge.aliases")}</dt>
          <dd>{aliases.length > 0 ? aliases.join(t("common.listSeparator")) : <span className="muted">{t("merge.none")}</span>}</dd>
        </div>
        <div>
          <dt>{t("merge.facts")}</dt>
          <dd>
            <Trans
              k="merge.factsValue"
              args={{ total: into.claims.length + from.claims.length, a: into.claims.length, b: from.claims.length }}
              tags={{ note: (c) => <span className="muted small">{c}</span> }}
            />
          </dd>
        </div>
        <div>
          <dt>{t("merge.mentions")}</dt>
          <dd>{formatNumber(into.mentions.length + from.mentions.length)}</dd>
        </div>
      </dl>
      <p className="muted small">
        <Trans k="merge.aliasNote" args={{ from: from.id, into: into.id }} tags={{ code: (c) => <code>{c}</code> }} />
      </p>
      {!sameKind && (
        <p className="note small" role="status">
          {t("merge.kindMismatch", { fromKind: kindLabel(from.kind), intoKind: kindLabel(into.kind) })}
        </p>
      )}
    </>
  );
}
