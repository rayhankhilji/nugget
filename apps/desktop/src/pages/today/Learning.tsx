/**
 * What the user's own feedback taught the card ranking (BB2): the per-card "why" lines and the
 * "What Nugget has learned" panel with Forget / Reset all. Learning only changes ranking and
 * suppression; resetting it is audited by the daemon, which then emits `cards.changed`.
 *
 * Keys and labels come from indexed mail (sender addresses, thread titles): plain text only.
 */
import { useId, useRef, useState } from "react";
import {
  cardLearned,
  errorMessage,
  isMethodNotFound,
  type CardLearned,
  type CardView,
  type LearnedView,
  type ProactiveLearning,
  type ResetLearningRequest,
  type ResetLearningResult,
} from "../../api";
import { Dialog } from "../../components/Dialog";
import { Badge, ErrorNote, Loading, Spinner } from "../../components/ui";
import { formatRelative, formatTime } from "../../format";
import { useNow } from "../../hooks";
import { useT } from "../../i18n";
import { learnedDirectionLabel, learnedLine, learnedName, learnedViewDirection, learningScopeLabel } from "./model";

/** Most learned effects a card's "why" lists. */
export const MAX_LEARNED_LINES = 2;

export type Forget = (request: ResetLearningRequest) => Promise<ResetLearningResult>;

/** How feedback moved this card, with a "Forget this" per effect. Nothing when it did not. */
export function LearnedLines({ card, onForget }: { card: CardView; onForget: Forget }) {
  const t = useT();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const lines = cardLearned(card)
    .map((l) => ({ l, text: learnedLine(l) }))
    .filter((x): x is { l: CardLearned; text: string } => x.text !== null)
    .slice(0, MAX_LEARNED_LINES);
  if (lines.length === 0) return null;

  const forget = async (l: CardLearned) => {
    const id = `${l.scope}\u0000${l.key}`;
    setBusy(id);
    setError(null);
    try {
      await onForget({ scope: l.scope as ResetLearningRequest["scope"], key: l.key });
    } catch (e) {
      setError(t("today.learning.failed", { error: errorMessage(e) }));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="today-learned">
      <ul aria-label={t("today.learned.label")}>
        {lines.map(({ l, text }) => {
          const id = `${l.scope}\u0000${l.key}`;
          return (
            <li key={id} className={`today-learned-${l.direction}`}>
              <span className="small">{text}</span>
              <button
                type="button"
                className="btn btn-ghost btn-small"
                disabled={busy !== null}
                aria-label={t("today.learning.forgetLabel", { name: learnedName(l.scope, l.key), scope: learningScopeLabel(l.scope) })}
                onClick={() => void forget(l)}
              >
                {busy === id ? <Spinner label={t("today.learning.forgetting")} /> : t("today.learned.forget")}
              </button>
            </li>
          );
        })}
      </ul>
      {error && (
        <p className="field-error small" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/**
 * "What Nugget has learned": collapsed by default; lists each learned adjustment with a Forget
 * button, and Reset all behind a confirmation dialog. Hidden for daemons without the method.
 */
export function LearningPanel({
  learning,
  error,
  loading,
  onRetry,
  onForget,
}: {
  learning: ProactiveLearning | undefined;
  error: unknown;
  loading: boolean;
  onRetry: () => void;
  onForget: Forget;
}) {
  const t = useT();
  const headingId = useId();
  const listId = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [status, setStatus] = useState("");
  const [confirming, setConfirming] = useState(false);

  if (!learning && isMethodNotFound(error)) return null;
  const entries = learning?.entries ?? [];

  const forget = async (request: ResetLearningRequest, id: string): Promise<boolean> => {
    setBusy(id);
    setFailure(null);
    try {
      const r = await onForget(request);
      setStatus(t("today.learning.forgot", { n: r?.cleared ?? 0 }));
      headingRef.current?.focus();
      return true;
    } catch (e) {
      setFailure(e);
      return false;
    } finally {
      setBusy(null);
    }
  };

  return (
    <section aria-labelledby={headingId} className="today-learning">
      <h2 id={headingId} ref={headingRef} tabIndex={-1}>
        {t("today.learning.title")}
      </h2>
      <p className="muted small">{t("today.learning.description")}</p>
      <p className="visually-hidden" role="status" aria-live="polite">
        {status}
      </p>
      {!learning && <ErrorNote error={error} onRetry={onRetry} />}
      {!learning && loading && <Loading label={t("today.learning.loading")} lines={2} />}
      {learning && entries.length === 0 && <p className="muted small">{t("today.learning.empty")}</p>}
      {entries.length > 0 && (
        <>
          <div className="row today-row-start">
            <button
              type="button"
              className="btn btn-ghost btn-small"
              aria-expanded={open}
              aria-controls={listId}
              onClick={() => setOpen((o) => !o)}
            >
              {open ? t("today.learning.hide") : t("today.learning.show", { n: entries.length })}
            </button>
            <button type="button" className="btn btn-small btn-danger-ghost" disabled={busy !== null} onClick={() => setConfirming(true)}>
              {t("today.learning.resetAll")}
            </button>
          </div>
          {!confirming && <ErrorNote error={failure} />}
          <div id={listId} hidden={!open}>
            {open && (
              <ul className="today-learning-list" aria-label={t("today.learning.listLabel")}>
                {entries.map((e) => {
                  const id = `${e.scope}\u0000${e.key}`;
                  return (
                    <LearnedEntry
                      key={id}
                      entry={e}
                      busy={busy === id}
                      disabled={busy !== null}
                      onForget={() => void forget({ scope: e.scope as ResetLearningRequest["scope"], key: e.key }, id)}
                    />
                  );
                })}
              </ul>
            )}
          </div>
        </>
      )}

      {confirming && (
        <Dialog
          title={t("today.learning.resetTitle")}
          description={t("today.learning.resetBody")}
          onClose={() => {
            if (busy === null) setConfirming(false);
          }}
        >
          <ErrorNote error={failure} />
          <div className="dialog-actions">
            <button type="button" className="btn btn-ghost" disabled={busy !== null} onClick={() => setConfirming(false)} data-autofocus>
              {t("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn-danger"
              disabled={busy !== null}
              onClick={async () => {
                if (await forget({ scope: "all" }, "\u0000all")) setConfirming(false);
              }}
            >
              {busy === "\u0000all" ? <Spinner label={t("today.learning.forgetting")} /> : t("today.learning.resetConfirm")}
            </button>
          </div>
        </Dialog>
      )}
    </section>
  );
}

function LearnedEntry({ entry: e, busy, disabled, onForget }: { entry: LearnedView; busy: boolean; disabled: boolean; onForget: () => void }) {
  const t = useT();
  const now = useNow();
  const direction = learnedViewDirection(e);
  const name = learnedName(e.scope, e.key, e.label);
  const scope = learningScopeLabel(e.scope);
  const tone = direction === "more" ? "good" : direction === "hidden" ? "bad" : "warn";
  return (
    <li className="today-learning-entry">
      <div className="today-learning-what">
        <span className="small muted">{scope}</span>
        <span className="today-learning-name" dir="auto">
          {name}
        </span>
        {e.scope !== "kind" && e.label && e.scope !== "thread" && <span className="muted small">{e.key}</span>}
      </div>
      <span className="badges">
        <Badge tone={tone}>{learnedDirectionLabel(direction)}</Badge>
      </span>
      <span className="muted small">
        {t("today.learning.counts", { accepted: Math.round(e.accepted), dismissed: Math.round(e.dismissed) })}
        {e.last_at && (
          <>
            {" · "}
            <span title={formatTime(e.last_at)}>{t("today.learning.lastAt", { when: formatRelative(e.last_at, now) })}</span>
          </>
        )}
      </span>
      <button
        type="button"
        className="btn btn-small"
        disabled={disabled}
        aria-label={t("today.learning.forgetLabel", { name, scope })}
        onClick={onForget}
      >
        {busy ? <Spinner label={t("today.learning.forgetting")} /> : t("today.learning.forget")}
      </button>
    </li>
  );
}
