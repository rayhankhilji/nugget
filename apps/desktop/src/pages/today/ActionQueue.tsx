import { useEffect, useState } from "react";
import { cloudApi, type CloudAction, type CloudActionKind } from "../../cloud";
import { errorMessage } from "../../api";
import { useAsync } from "../../hooks";
import { comboAria, comboLabel, type Combo } from "../../hooks.shortcuts";
import { formatRelative } from "../../format";
import { AppLogo } from "../../components/AppLogo";
import { showToast } from "../../components/Toast";
import { Spinner } from "../../components/ui";
import { useT, type MessageKey } from "../../i18n";
import { holdApproval, undoApproval, useHeldApprovals } from "./approvals";

const KIND: Record<CloudActionKind, MessageKey> = {
  send_mail: "needs.actions.kind.send_mail",
  reply_mail: "needs.actions.kind.reply_mail",
  create_event: "needs.actions.kind.create_event",
  update_event: "needs.actions.kind.update_event",
  post_message: "needs.actions.kind.post_message",
  create_page: "needs.actions.kind.create_page",
};

/** What the toast says once an action is approved (it reaches the cloud after the Undo hold). */
const DONE: Record<CloudActionKind, MessageKey> = {
  send_mail: "needs.actions.sent",
  reply_mail: "needs.actions.sent",
  post_message: "needs.actions.sent",
  create_event: "needs.actions.scheduled",
  update_event: "needs.actions.scheduled",
  create_page: "needs.actions.created",
};

const APPROVE: Combo = { mod: true, key: "Enter" };
const REJECT: Combo = { key: "Backspace" };

export interface Approvals {
  pending: CloudAction[];
  appOf: (slug: string) => { name: string; logo: string | null };
  approve: (a: CloudAction) => void;
  reject: (a: CloudAction) => Promise<void>;
  busy: string | null;
  error: unknown;
  reload: () => void;
}

/**
 * Heed Cloud's approval queue: actions proposed from cards (a reply, an event) with exactly what
 * they will send. Nothing runs until Approve, which echoes the digest of what was shown; then a
 * "Sent · Undo" toast holds it for a few seconds (see approvals.ts). Reject cancels it. `tick`
 * changes when a new action was proposed. Needs you lists these rows above its cards and shows the
 * selected one in its pane.
 */
export function useApprovals(enabled: boolean, tick: number, onDone: (status: string) => void): Approvals {
  const t = useT();
  const list = useAsync(() => (enabled ? cloudApi.actions("pending") : Promise.resolve([] as CloudAction[])), [enabled, tick]);
  const apps = useAsync(() => (enabled ? cloudApi.apps().catch(() => []) : Promise.resolve([])), [enabled]);
  const held = useHeldApprovals();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => setError(null), [tick]);

  const pending = (list.data ?? []).filter((a) => !held.has(a.id));
  const appOf = (slug: string) => apps.data?.find((a) => a.slug === slug) ?? { name: slug, logo: null };

  const approve = (a: CloudAction) => {
    setError(null);
    holdApproval(a, (r) => {
      if (r.ok) {
        onDone(
          r.action.status === "failed"
            ? t("needs.actions.failed", { summary: a.summary, error: r.action.error ?? "" })
            : r.action.status === "succeeded"
              ? t("needs.actions.succeeded", { summary: a.summary })
              : t("needs.actions.approved", { summary: a.summary }),
        );
        if (r.action.status === "failed") showToast({ message: t("needs.actions.failed", { summary: a.summary, error: r.action.error ?? "" }) });
      } else {
        showToast({ message: t("today.feedbackFailed", { error: errorMessage(r.error) }) });
        onDone(t("today.feedbackFailed", { error: errorMessage(r.error) }));
      }
      list.reload();
    });
    showToast({
      message: t(DONE[a.kind] ?? "needs.actions.sent", { summary: a.summary }),
      action: {
        label: t("toast.undo"),
        run: () => {
          if (undoApproval(a.id)) showToast({ message: t("needs.actions.undone") });
        },
      },
    });
  };

  const reject = async (a: CloudAction) => {
    setBusy(a.id);
    setError(null);
    try {
      await cloudApi.cancelAction(a.id);
      onDone(t("needs.actions.rejected", { summary: a.summary }));
      list.reload();
    } catch (e) {
      setError(e);
      onDone(t("today.feedbackFailed", { error: errorMessage(e) }));
    } finally {
      setBusy(null);
    }
  };

  return { pending, appOf, approve, reject, busy, error: list.error ?? error, reload: list.reload };
}

/** One row per pending action, for the Needs you queue. */
export function ApprovalRows({ approvals, selected, onSelect }: { approvals: Approvals; selected: string | null; onSelect: (id: string) => void }) {
  const t = useT();
  return (
    <>
      {approvals.pending.map((a) => {
        const target = approvals.appOf(a.toolkit);
        return (
          <li key={a.id}>
            <button type="button" className="needs-item action-row" data-action={a.id} aria-current={a.id === selected ? "true" : undefined} onClick={() => onSelect(a.id)}>
              <AppLogo app={target} />
              <span className="needs-item-kind">
                {t(KIND[a.kind] ?? "needs.actions.kind.other")} · {t("needs.actions.in", { app: target.name })}
              </span>
              <span className="needs-item-title" dir="auto">
                {a.summary}
              </span>
            </button>
          </li>
        );
      })}
    </>
  );
}

/** The selected action in the Needs you pane: what it will send, with Approve and Reject. */
export function ActionPane({
  a,
  appInfo,
  busy,
  onApprove,
  onReject,
}: {
  a: CloudAction;
  appInfo: { name: string; logo: string | null };
  busy: string | null;
  onApprove: () => void;
  onReject: () => void;
}) {
  const t = useT();
  const denied = a.decision === "deny";
  return (
    <article className="card action-item needs-pane">
      <div className="action-head">
        <AppLogo app={appInfo} />
        <div className="action-what">
          <span className="action-meta">
            {t(KIND[a.kind] ?? "needs.actions.kind.other")} · {t("needs.actions.in", { app: appInfo.name })} · {t("needs.actions.expires", { when: formatRelative(a.expires_at, Date.now()) })}
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
        <div className="today-card-footer action-footer">
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy !== null}
            onClick={onApprove}
            data-shortcut="primary"
            aria-label={t("needs.actions.approveLabel", { summary: a.summary })}
            aria-keyshortcuts={comboAria(APPROVE)}
          >
            {t("needs.actions.approve")}
          </button>
          <kbd className="keycap" aria-hidden="true" dir="ltr">
            {comboLabel({ ...APPROVE, key: "↵" })}
          </kbd>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={busy !== null}
            onClick={onReject}
            data-shortcut="reject"
            aria-label={t("needs.actions.rejectLabel", { summary: a.summary })}
            aria-keyshortcuts={comboAria(REJECT)}
          >
            {busy === a.id ? <Spinner /> : t("needs.actions.reject")}
          </button>
          <kbd className="keycap" aria-hidden="true" dir="ltr">
            ⌫
          </kbd>
        </div>
      )}
    </article>
  );
}

/** A step marker: a drawn check when done, a still ring while it waits on the person. */
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
