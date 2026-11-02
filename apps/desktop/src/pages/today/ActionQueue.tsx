import { useEffect, useId, useState } from "react";
import { cloudApi, type CloudAction, type CloudActionKind } from "../../cloud";
import { errorMessage } from "../../api";
import { useAsync } from "../../hooks";
import { formatRelative } from "../../format";
import { AppLogo } from "../../components/AppLogo";
import { ErrorNote, Spinner } from "../../components/ui";
import { useT, type MessageKey } from "../../i18n";

const KIND: Record<CloudActionKind, MessageKey> = {
  send_mail: "needs.actions.kind.send_mail",
  reply_mail: "needs.actions.kind.reply_mail",
  create_event: "needs.actions.kind.create_event",
  update_event: "needs.actions.kind.update_event",
  post_message: "needs.actions.kind.post_message",
  create_page: "needs.actions.kind.create_page",
};

/**
 * Heed Cloud's approval queue: actions proposed from cards (a reply, an event) with what they will
 * send. Nothing runs until Approve, which echoes the digest of exactly what was shown; Reject
 * cancels it. `tick` changes when a new action was proposed.
 */
export function ActionQueue({ tick, onDone }: { tick: number; onDone: (status: string) => void }) {
  const t = useT();
  const headingId = useId();
  const list = useAsync(() => cloudApi.actions("pending"), [tick]);
  const apps = useAsync(() => cloudApi.apps().catch(() => []), []);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => setError(null), [tick]);

  const pending = list.data ?? [];
  if (!list.error && pending.length === 0) return null;
  const app = (slug: string) => apps.data?.find((a) => a.slug === slug) ?? { name: slug, logo: null };

  const decide = async (a: CloudAction, approve: boolean) => {
    setBusy(a.id);
    setError(null);
    try {
      if (approve) {
        const r = await cloudApi.approveAction(a.id, a.digest);
        onDone(
          r.status === "failed"
            ? t("needs.actions.failed", { summary: a.summary, error: r.error ?? "" })
            : r.status === "succeeded"
              ? t("needs.actions.succeeded", { summary: a.summary })
              : t("needs.actions.approved", { summary: a.summary }),
        );
      } else {
        await cloudApi.cancelAction(a.id);
        onDone(t("needs.actions.rejected", { summary: a.summary }));
      }
      list.reload();
    } catch (e) {
      setError(e);
      onDone(t("today.feedbackFailed", { error: errorMessage(e) }));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="action-queue" aria-labelledby={headingId}>
      <h2 id={headingId}>{t("needs.actions.title")}</h2>
      <ErrorNote error={list.error ?? error} onRetry={list.reload} />
      <ul className="action-list">
        {pending.map((a) => {
          const target = app(a.toolkit);
          const denied = a.decision === "deny";
          return (
            <li key={a.id} className="card action-item">
              <div className="action-head">
                <AppLogo app={target} />
                <div className="action-what">
                  <span className="action-meta">
                    {t(KIND[a.kind] ?? "needs.actions.kind.other")} · {t("needs.actions.in", { app: target.name })} · {t("needs.actions.expires", { when: formatRelative(a.expires_at, Date.now()) })}
                  </span>
                  <h3 className="action-summary" dir="auto">
                    {a.summary}
                  </h3>
                </div>
              </div>
              {!denied && (
                <ol className="action-steps" aria-label={t("needs.actions.steps")}>
                  <li data-state="done">
                    <StepDot state="done" />
                    {t("needs.actions.step.drafted")}
                  </li>
                  <li data-state="done">
                    <StepDot state="done" />
                    {t("needs.actions.step.preview")}
                  </li>
                  <li data-state="now" aria-current="step">
                    <StepDot state="now" />
                    {t("needs.actions.step.waiting")}
                  </li>
                </ol>
              )}
              {a.preview.fields.length > 0 && (
                <dl className="action-fields">
                  {a.preview.fields.map((f, i) => (
                    <div key={i}>
                      <dt>{f.label}</dt>
                      <dd dir="auto">{f.value}</dd>
                    </div>
                  ))}
                </dl>
              )}
              {a.preview.diff.length > 0 && (
                <ul className="today-diff" aria-label={t("today.action.diffLabel")} tabIndex={0}>
                  {a.preview.diff.map((d, i) => (
                    <li key={i}>
                      <span className="action-diff-field">{d.field}</span>
                      {d.before != null && (
                        <span className="today-diff-remove">
                          <span className="visually-hidden">{t("today.action.removed")} </span>
                          {d.before}
                        </span>
                      )}
                      {d.after != null && (
                        <span className="today-diff-add">
                          <span className="visually-hidden">{t("today.action.added")} </span>
                          {d.after}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              )}
              {denied ? (
                <p className="note note-warn">{t("needs.actions.denied", { reason: a.reason ?? "" })}</p>
              ) : (
                <div className="today-card-footer">
                  <button
                    type="button"
                    className="btn btn-small btn-primary"
                    disabled={busy !== null}
                    onClick={() => void decide(a, true)}
                    aria-label={t("needs.actions.approveLabel", { summary: a.summary })}
                  >
                    {busy === a.id ? <Spinner /> : t("needs.actions.approve")}
                  </button>
                  <button
                    type="button"
                    className="btn btn-small btn-ghost"
                    disabled={busy !== null}
                    onClick={() => void decide(a, false)}
                    aria-label={t("needs.actions.rejectLabel", { summary: a.summary })}
                  >
                    {t("needs.actions.reject")}
                  </button>
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/** A step marker: a drawn check when done, a turning ring while waiting. */
function StepDot({ state }: { state: "done" | "now" }) {
  return (
    <span className={`step-dot ${state}`} aria-hidden="true">
      {state === "done" && (
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
          <path d="M2 5.2 4.2 7.4 8 3" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      )}
    </span>
  );
}
