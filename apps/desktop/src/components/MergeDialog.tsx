import { useId, useState, type FormEvent } from "react";
import { api, type EntityDetail, type EntitySummaryView } from "../api";
import { useAsync, useDebounced } from "../hooks";
import { formatNumber, Trans, useT } from "../i18n";
import { kindLabel } from "../knowledge";
import { Dialog } from "./Dialog";
import { ErrorNote, Spinner } from "./ui";

const RESULTS = 20;

/**
 * Something that tells two same-name entities apart: another alias, an identifier (an email),
 * else the end of the id. Only used when the names collide.
 */
export function distinguishing(name: string, id: string, d: Pick<EntityDetail, "aliases" | "identifiers"> | null): string {
  const alias = d?.aliases.find((a) => a.trim().toLocaleLowerCase() !== name.trim().toLocaleLowerCase());
  if (alias) return alias;
  const ident = d?.identifiers[0]?.[1];
  if (ident) return ident;
  return `#${id.slice(-6)}`;
}

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
  const sameName = picked !== null && picked.name.trim().toLocaleLowerCase() === target.name.trim().toLocaleLowerCase();
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
            placeholder={t("merge.findPlaceholder")}
            dir="auto"
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
                <span className="merge-option-text">
                  <bdi className="merge-name">{r.name}</bdi>
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
                <bdi>{target.name}</bdi>{" "}
                <span className="muted small">
                  {t("merge.keepMeta", { kind: kindLabel(target.kind), claims: target.claims.length })}
                  {sameName && (
                    <>
                      {" · "}
                      <bdi>{distinguishing(target.name, target.id, target)}</bdi>
                    </>
                  )}
                </span>
              </span>
            </label>
            <label className="check">
              <input type="radio" name="merge-keep" checked={keep === "other"} onChange={() => setKeep("other")} />
              <span>
                <bdi>{picked.name}</bdi>{" "}
                <span className="muted small">
                  {t("merge.keepMeta", { kind: kindLabel(picked.kind), claims: picked.claims })}
                  {sameName && (
                    <>
                      {" · "}
                      <bdi>{distinguishing(picked.name, picked.id, otherDetail)}</bdi>
                    </>
                  )}
                </span>
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
          <button type="submit" className="btn btn-danger" disabled={busy || !into || !from}>
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
        <Trans k="merge.summary" args={{ from: from.name, into: into.name }} tags={{ b: (c) => <b><bdi>{c}</bdi></b> }} />
      </p>
      <dl className="entity-props">
        <div>
          <dt>{t("merge.name")}</dt>
          <dd>
            <bdi>{into.name}</bdi> <span className="muted small">({kindLabel(into.kind)})</span>
          </dd>
        </div>
        <div>
          <dt>{t("merge.aliases")}</dt>
          <dd>{aliases.length > 0 ? <bdi>{aliases.join(t("common.listSeparator"))}</bdi> : <span className="muted">{t("merge.none")}</span>}</dd>
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
        <Trans k="merge.aliasNote" args={{ from: from.name, into: into.name }} tags={{ b: (c) => <b><bdi>{c}</bdi></b>, code: (c) => <bdi>{c}</bdi> }} />
      </p>
      {!sameKind && (
        <p className="note small" role="status">
          {t("merge.kindMismatch", { fromKind: kindLabel(from.kind), intoKind: kindLabel(into.kind) })}
        </p>
      )}
      <p className="small merge-warning">{t("merge.cantUndo")}</p>
    </>
  );
}
