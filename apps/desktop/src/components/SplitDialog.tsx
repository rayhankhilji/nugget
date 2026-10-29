import { useId, useState, type FormEvent } from "react";
import { ENTITY_KINDS, knowledgeSplit, type EntityDetail, type EntityKind } from "../api";
import { useT } from "../i18n";
import { claimText, kindLabel, splitProblem, splitRequest } from "../knowledge";
import { claimTime } from "../recency";
import { Dialog } from "./Dialog";
import { RelativeTime } from "./RelativeTime";
import { ErrorNote, Spinner } from "./ui";

/**
 * Split an entity that conflates two things: pick the facts (and any aliases or identifiers)
 * that belong to the other one and name it. The facts move to a new entity.
 */
export function SplitDialog({
  target,
  onClose,
  onSplit,
}: {
  target: EntityDetail;
  onClose: () => void;
  /** The new entity's id. */
  onSplit: (id: string) => void;
}) {
  const t = useT();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<EntityKind>(target.kind);
  const [claims, setClaims] = useState<number[]>([]);
  const [aliases, setAliases] = useState<string[]>([]);
  const [idents, setIdents] = useState<[string, string][]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [tried, setTried] = useState(false);
  const nameId = useId();
  const kindId = useId();
  const problemId = useId();

  const draft = { name, claims, aliases, identifiers: idents };
  const problem = splitProblem(target, draft);
  const otherAliases = target.aliases.filter((a) => a !== target.name);
  const toggle = <T,>(list: T[], x: T, eq: (a: T, b: T) => boolean = (a, b) => a === b): T[] =>
    list.some((y) => eq(x, y)) ? list.filter((y) => !eq(x, y)) : [...list, x];
  const sameIdent = (a: [string, string], b: [string, string]) => a[0] === b[0] && a[1] === b[1];

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setTried(true);
    if (problem) return;
    setBusy(true);
    setError(null);
    try {
      const id = await knowledgeSplit({ ...splitRequest(target, draft), kind });
      onSplit(id);
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  return (
    <Dialog title={t("split.title", { name: target.name })} description={t("split.description")} onClose={onClose} wide>
      <form onSubmit={submit} noValidate>
        <div className="field">
          <label htmlFor={nameId}>{t("split.name")}</label>
          <input
            id={nameId}
            value={name}
            onChange={(e) => setName(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            data-autofocus=""
            aria-describedby={tried && problem ? problemId : undefined}
          />
        </div>
        <div className="field">
          <label htmlFor={kindId}>{t("split.kind")}</label>
          <select id={kindId} value={kind} onChange={(e) => setKind(e.target.value as EntityKind)}>
            {ENTITY_KINDS.map((k) => (
              <option key={k} value={k}>
                {kindLabel(k)}
              </option>
            ))}
          </select>
        </div>
        <fieldset className="field">
          <legend>{t("split.facts")}</legend>
          {target.claims.length === 0 && <p className="muted small">{t("entity.noFacts", { name: target.name })}</p>}
          {target.claims.map((c) => {
            const seen = claimTime(c);
            return (
              <label key={c.id} className="check">
                <input type="checkbox" checked={claims.includes(c.id)} onChange={() => setClaims((l) => toggle(l, c.id))} />
                <span>
                  {claimText(c)}
                  {/* When each fact was seen helps tell two conflated things apart. */}
                  {seen && (
                    <>
                      {" "}
                      <RelativeTime ts={seen} className="muted small" />
                    </>
                  )}
                </span>
              </label>
            );
          })}
        </fieldset>
        {(otherAliases.length > 0 || target.identifiers.length > 0) && (
          <fieldset className="field">
            <legend>{t("split.details")}</legend>
            {otherAliases.map((a) => (
              <label key={`a\u0000${a}`} className="check">
                <input type="checkbox" checked={aliases.includes(a)} onChange={() => setAliases((l) => toggle(l, a))} />
                <span>{a}</span>
              </label>
            ))}
            {target.identifiers.map((i) => (
              <label key={`i\u0000${i[0]}\u0000${i[1]}`} className="check">
                <input
                  type="checkbox"
                  checked={idents.some((x) => sameIdent(x, i))}
                  onChange={() => setIdents((l) => toggle(l, i, sameIdent))}
                />
                <span>
                  {i[0]}: <code>{i[1]}</code>
                </span>
              </label>
            ))}
          </fieldset>
        )}
        {tried && problem && (
          <p id={problemId} className="note note-error small" role="alert">
            {t(problem)}
          </p>
        )}
        <ErrorNote error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {busy ? <Spinner label={t("split.splitting")} /> : t("split.split")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
