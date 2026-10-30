/**
 * The action flow for one card action: dry-run preview (diff, risk, target) → Approve →
 * execute bound to the preview's digest → confirm when the policy asks → finish the artifact
 * (copy a draft / open it in the mail app, open a file, show a note, confirm a reminder).
 */
import { useEffect, useRef, useState } from "react";
import {
  api,
  errorMessage,
  isMailtoUrl,
  proactiveApi,
  type ActionArtifact,
  type CardActionView,
  type CardExecuteResult,
  type CardPreview,
  type CardView,
} from "../../api";
import { Dialog } from "../../components/Dialog";
import { CopyButton } from "../../components/CopyButton";
import { Badge, ErrorNote, Spinner } from "../../components/ui";
import { fileUrl } from "../../knowledge";
import { formatTime } from "../../format";
import { useT } from "../../i18n";
import { diffMarker, riskLabel, riskTone, targetLabel } from "./model";

type Phase =
  | { kind: "previewing" }
  | { kind: "preview"; preview: CardPreview }
  | { kind: "executing"; preview: CardPreview }
  | { kind: "result"; preview: CardPreview; result: CardExecuteResult };

export function ActionDialog({ card, action, onClose }: { card: CardView; action: CardActionView; onClose: (executed: boolean) => void }) {
  const t = useT();
  const [phase, setPhase] = useState<Phase>({ kind: "previewing" });
  const [error, setError] = useState<unknown>(null);
  const executed = phase.kind === "result" && phase.result.outcome === "executed";

  const loadPreview = async () => {
    setError(null);
    setPhase({ kind: "previewing" });
    try {
      const preview = await proactiveApi.cardsPreview({ card_id: card.id, action: action.index });
      setPhase({ kind: "preview", preview });
    } catch (e) {
      setError(e);
    }
  };

  useEffect(() => {
    void loadPreview();
    // Once per dialog.
  }, []);

  const execute = async (preview: CardPreview, confirmed: boolean) => {
    setError(null);
    setPhase({ kind: "executing", preview });
    try {
      const result = await proactiveApi.cardsExecute({ card_id: card.id, action: preview.action, digest: preview.digest, confirmed });
      setPhase({ kind: "result", preview, result });
    } catch (e) {
      setError(e);
      setPhase({ kind: "preview", preview });
    }
  };

  const preview = phase.kind === "previewing" ? null : phase.preview;

  // Each phase swaps the buttons: when the focused one goes (Approve while it runs, Confirm once
  // confirmed), focus would drop to the page body. Move it to the phase's main button instead.
  const actionsRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const dialog = actionsRef.current?.closest("dialog");
    if (!dialog) return;
    const active = document.activeElement;
    if (active instanceof HTMLElement && active !== document.body && dialog.contains(active) && !isDisabled(active)) return;
    dialog.querySelector<HTMLElement>("[data-autofocus]:not(:disabled)")?.focus();
  }, [phase]);

  return (
    <Dialog title={action.label || t("today.action.title")} description={card.title} onClose={() => onClose(executed)} wide>
      {phase.kind === "previewing" && !error && <Spinner label={t("today.action.previewing")} />}
      <ErrorNote error={error} onRetry={preview ? undefined : () => void loadPreview()} />

      {preview && (
        <section aria-label={t("today.action.previewLabel")} className="today-preview">
          <p className="today-preview-summary">{preview.summary}</p>
          <div className="badges">
            <Badge tone={riskTone(preview.risk)}>{t("today.action.risk", { risk: riskLabel(preview.risk) })}</Badge>
            <Badge>{t("today.action.target", { target: targetLabel(preview.target) })}</Badge>
          </div>
          <p className="muted small">{t("today.action.dryRun")}</p>
          {preview.diff.length > 0 ? (
            <ul className="today-diff" aria-label={t("today.action.diffLabel")}>
              {preview.diff.map((line, i) => (
                <li key={i} className={`today-diff-${line.op === "add" || line.op === "remove" ? line.op : "context"}`}>
                  <span aria-hidden="true" className="today-diff-marker">
                    {diffMarker(line.op)}
                  </span>
                  {line.op === "add" && <span className="visually-hidden">{t("today.action.added")} </span>}
                  {line.op === "remove" && <span className="visually-hidden">{t("today.action.removed")} </span>}
                  <span className="today-diff-text">{line.text}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="muted small">{t("today.action.noDiff")}</p>
          )}
        </section>
      )}

      {phase.kind === "result" && <Outcome result={phase.result} onConfirm={() => void execute(phase.preview, true)} />}

      <div className="dialog-actions" ref={actionsRef}>
        {phase.kind === "result" && phase.result.outcome !== "needs_confirmation" ? (
          <button type="button" className="btn btn-primary" onClick={() => onClose(executed)} data-autofocus>
            {t("common.done")}
          </button>
        ) : (
          <>
            <button type="button" className="btn btn-ghost" onClick={() => onClose(executed)}>
              {t("common.cancel")}
            </button>
            {(phase.kind === "preview" || phase.kind === "executing") && (
              <button
                type="button"
                className="btn btn-primary"
                disabled={phase.kind === "executing"}
                onClick={() => void execute(phase.preview, false)}
                data-autofocus
              >
                {phase.kind === "executing" ? <Spinner label={t("today.action.running")} /> : t("today.action.approve")}
              </button>
            )}
          </>
        )}
      </div>
    </Dialog>
  );
}

const isDisabled = (el: HTMLElement) => el.matches(":disabled");

function Outcome({ result, onConfirm }: { result: CardExecuteResult; onConfirm: () => void }) {
  const t = useT();
  switch (result.outcome) {
    case "executed":
      return result.artifact ? <ArtifactView artifact={result.artifact} /> : <p role="status">{t("today.artifact.done")}</p>;
    case "needs_confirmation":
      return (
        <div className="note note-warn" role="alert">
          <span>{result.detail ? t("today.action.confirmReason", { reason: result.detail }) : t("today.action.confirmGeneric")}</span>
          <button type="button" className="btn btn-primary btn-small" onClick={onConfirm} data-autofocus>
            {t("today.action.confirm")}
          </button>
        </div>
      );
    case "denied":
      return (
        <div className="note note-error" role="alert">
          {result.detail ? t("today.action.deniedReason", { reason: result.detail }) : t("today.action.denied")}
        </div>
      );
    case "halted":
      return (
        <div className="note note-error" role="alert">
          {result.detail ? t("today.action.haltedReason", { reason: result.detail }) : t("today.action.halted")}
        </div>
      );
    default:
      return (
        <div className="note" role="status">
          {t("today.action.unknownOutcome", { outcome: String(result.outcome) })}
        </div>
      );
  }
}

/** Finish what the action produced. Text is untrusted and shown as plain text only. */
export function ArtifactView({ artifact }: { artifact: ActionArtifact }) {
  const t = useT();
  const [status, setStatus] = useState<{ text: string; error: boolean } | null>(null);
  const autoOpened = useRef(false);

  const openPath = async (path: string) => {
    const url = fileUrl(path);
    if (!url) {
      setStatus({ text: t("today.artifact.cantOpen"), error: true });
      return;
    }
    try {
      const r = await api.openUrl(url);
      setStatus({ text: r === "revealed" ? t("today.artifact.revealed") : t("today.artifact.opened"), error: false });
    } catch (e) {
      setStatus({ text: t("today.artifact.openFailed", { error: errorMessage(e) }), error: true });
    }
  };

  const openMail = async (url: string) => {
    try {
      await proactiveApi.openMailto(url);
      setStatus({ text: t("today.artifact.mailOpened"), error: false });
    } catch (e) {
      setStatus({ text: t("today.artifact.openFailed", { error: errorMessage(e) }), error: true });
    }
  };

  // An executed `file` artifact (e.g. an .ics) is handed to the OS once, as the user approved.
  useEffect(() => {
    if (artifact.kind === "file" && artifact.path && !autoOpened.current) {
      autoOpened.current = true;
      void openPath(artifact.path);
    }
  }, [artifact]);

  const live = (
    <p className={status?.error ? "field-error" : "muted small"} role="status" aria-live="polite">
      {status?.text ?? ""}
    </p>
  );

  switch (artifact.kind) {
    case "draft":
      return (
        <div className="today-artifact">
          <p>{t("today.artifact.draftReady")}</p>
          {artifact.text && (
            <pre className="today-draft" tabIndex={0} aria-label={t("today.artifact.draftLabel")}>
              {artifact.text}
            </pre>
          )}
          <div className="row today-row-start">
            {artifact.text && <CopyButton text={artifact.text} what={t("today.artifact.draftWhat")} className="btn" />}
            {isMailtoUrl(artifact.url) && (
              <button type="button" className="btn" onClick={() => void openMail(artifact.url!)}>
                {t("today.artifact.openMail")}
              </button>
            )}
          </div>
          {live}
        </div>
      );
    case "file":
      return (
        <div className="today-artifact">
          <p>{t("today.artifact.fileReady")}</p>
          {artifact.path && (
            <>
              <code className="block selectable">{artifact.path}</code>
              <div className="row today-row-start">
                <button type="button" className="btn" onClick={() => void openPath(artifact.path!)}>
                  {t("today.artifact.openFile")}
                </button>
              </div>
            </>
          )}
          {live}
        </div>
      );
    case "note":
      return (
        <div className="today-artifact">
          <p>{t("today.artifact.noteReady")}</p>
          {artifact.path && (
            <>
              <code className="block selectable">{artifact.path}</code>
              <div className="row today-row-start">
                <button type="button" className="btn" onClick={() => void openPath(artifact.path!)}>
                  {t("today.artifact.openNote")}
                </button>
              </div>
            </>
          )}
          {live}
        </div>
      );
    case "reminder":
      return (
        <p role="status">{artifact.at ? t("today.artifact.reminderAt", { at: formatTime(artifact.at) }) : t("today.artifact.reminder")}</p>
      );
    case "event":
      return <p role="status">{t("today.artifact.event")}</p>;
    case "archived":
      return <p role="status">{t("today.artifact.archived")}</p>;
    default:
      return <p role="status">{t("today.artifact.done")}</p>;
  }
}
