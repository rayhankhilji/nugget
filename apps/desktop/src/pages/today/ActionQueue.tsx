import { useEffect, useId, useState, type KeyboardEvent } from "react";
import { cloudApi, type CloudAction, type CloudActionKind } from "../../cloud";
import { errorMessage } from "../../api";
import { useAsync } from "../../hooks";
import { comboAria, comboLabel, hasMod, isComposing, isTypingTarget, type Combo } from "../../hooks.shortcuts";
import { formatRelative } from "../../format";
import { AppLogo } from "../../components/AppLogo";
import { showToast } from "../../components/Toast";
import { ErrorNote, Spinner } from "../../components/ui";
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

/**
 * Heed Cloud's approval queue, as a split: one line per proposed action on the left, exactly
 * what it will send on the right with Approve and Reject pinned under it (⌘↵ / Ctrl+↵ and ⌫).
 * Nothing runs until Approve, which echoes the digest of what was shown; then a "Sent · Undo"
 * toast holds it for a few seconds (see approvals.ts). Reject cancels it. `tick` changes when a
 * new action was proposed.
 */
export function ActionQueue({ tick, onDone }: { tick: number; onDone: (status: string) => void }) {
  const t = useT();
  const headingId = useId();
  const list = useAsync(() => cloudApi.actions("pending"), [tick]);
  const apps = useAsync(() => cloudApi.apps().catch(() => []), []);
  const held = useHeldApprovals();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  useEffect(() => setError(null), [tick]);

  const pending = (list.data ?? []).filter((a) => !held.has(a.id));
  const selected = pending.find((a) => a.id === selectedId) ?? pending[0];
  if (!list.error && pending.length === 0) return null;
  const app = (slug: string) => apps.data?.find((a) => a.slug === slug) ?? { name: slug, logo: null };

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

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const k = e.nativeEvent;
    if (e.defaultPrevented || isComposing(k) || isTypingTarget(e.target) || !selected || selected.decision === "deny" || busy) return;
    if (hasMod(k) && e.key === "Enter") {
      e.preventDefault();
      approve(selected);
    } else if (!hasMod(k) && !e.altKey && (e.key === "Backspace" || e.key === "Delete")) {
      e.preventDefault();
      void reject(selected);
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (!(e.target as HTMLElement).closest(".needs-list")) return;
      e.preventDefault();
      const i = pending.findIndex((a) => a.id === selected.id);
      const next = pending[(i + (e.key === "ArrowDown" ? 1 : pending.length - 1)) % pending.length];
      if (!next) return;
      setSelectedId(next.id);
      e.currentTarget.querySelector<HTMLButtonElement>(`[data-action="${CSS.escape(next.id)}"]`)?.focus();
    }
  };

  return (
    <section className="action-queue" aria-labelledby={headingId}>
      <h2 id={headingId}>{t("needs.actions.title")}</h2>
      <ErrorNote error={list.error ?? error} onRetry={list.reload} />
      {selected && (
        <div className="needs-split action-split" onKeyDown={onKey}>
          <ul className="needs-list" aria-label={t("needs.actions.listLabel")}>
            {pending.map((a) => {
              const target = app(a.toolkit);
              return (
                <li key={a.id}>
                  <button type="button" className="needs-item action-row" data-action={a.id} aria-current={a.id === selected.id ? "true" : undefined} onClick={() => setSelectedId(a.id)}>
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
          </ul>
          <ActionPane
            key={selected.id}
            a={selected}
            appInfo={app(selected.toolkit)}
            busy={busy}
            onApprove={() => approve(selected)}
            onReject={() => void reject(selected)}
          />
        </div>
      )}
    </section>
  );
}

function ActionPane({
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
