import { useEffect, useRef, useState, type FormEvent } from "react";
import { cloudApi, deviceTimeZone, type CloudAction, type CloudActionKind } from "../../cloud";
import { errorMessage } from "../../api";
import { useAsync } from "../../hooks";
import { comboAria, comboLabel, type Combo } from "../../hooks.shortcuts";
import { formatRelative, formatTime } from "../../format";
import { AppLogo } from "../../components/AppLogo";
import { showToast } from "../../components/Toast";
import { ErrorNote, Spinner } from "../../components/ui";
import { useT, type MessageKey } from "../../i18n";
import { approvalsChanged, holdApproval, undoApproval, useHeldApprovals } from "./approvals";
import { parseSnooze, snoozePresets, type SnoozePreset } from "./snooze";

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
const EDIT: Combo = { key: "E" };
const SNOOZE: Combo = { key: "H" };

/** Labels for the fields an action's `editable` list may name (the API's own names). */
const FIELD: Record<string, MessageKey> = {
  to: "needs.edit.to",
  cc: "needs.edit.cc",
  bcc: "needs.edit.bcc",
  subject: "needs.edit.subject",
  body: "needs.edit.body",
  title: "needs.edit.title",
  start: "needs.edit.start",
  end: "needs.edit.end",
  attendees: "needs.edit.attendees",
  location: "needs.edit.location",
  description: "needs.edit.description",
  text: "needs.edit.text",
  markdown: "needs.edit.markdown",
};
/** Fields that take a list of addresses (comma-separated in the form). */
const LIST = new Set(["to", "cc", "bcc", "attendees"]);
/** Fields that take long text. */
const LONG = new Set(["body", "text", "markdown", "description"]);
/** Fields that take a local date and time (sent with `timezone`). */
const TIME = new Set(["start", "end"]);

/** The value an edit form starts from: the preview's matching field or body, else empty. */
function prefill(a: CloudAction, field: string): string {
  const norm = (s: string) => s.toLowerCase().replace(/[^a-z]/g, "");
  const f = a.preview.fields.find((x) => norm(x.label) === field);
  if (f) return f.value;
  if (LONG.has(field)) {
    const d = a.preview.diff.find((x) => norm(x.field) === field || ["body", "message", "text", "description", "content"].includes(norm(x.field)));
    if (d) return d.after ?? "";
  }
  return "";
}

export interface Approvals {
  pending: CloudAction[];
  appOf: (slug: string) => { name: string; logo: string | null };
  approve: (a: CloudAction) => void;
  reject: (a: CloudAction) => Promise<void>;
  busy: string | null;
  error: unknown;
  reload: () => void;
  /** Change fields of a pending action; resolves to an error to show, or null. */
  edit: (a: CloudAction, input: Record<string, unknown>) => Promise<unknown>;
  snooze: (a: CloudAction, until: Date) => Promise<void>;
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
      approvalsChanged();
    } catch (e) {
      setError(e);
      onDone(t("today.feedbackFailed", { error: errorMessage(e) }));
    } finally {
      setBusy(null);
    }
  };

  const edit = async (a: CloudAction, input: Record<string, unknown>) => {
    setBusy(a.id);
    try {
      const next = await cloudApi.updateAction(a.id, input);
      // A new digest: the row and its preview are re-read before anything can be approved.
      list.reload();
      if (next.decision === "deny") return new Error(t("needs.actions.denied", { reason: next.reason ?? "" }));
      showToast({ message: t("needs.edit.saved", { summary: next.summary }) });
      return null;
    } catch (e) {
      list.reload();
      return e;
    } finally {
      setBusy(null);
    }
  };

  const snooze = async (a: CloudAction, until: Date) => {
    setBusy(a.id);
    setError(null);
    try {
      await cloudApi.snoozeAction(a.id, until.toISOString());
      showToast({ message: t("today.snoozed", { title: a.summary, until: formatTime(until.toISOString()) }) });
      list.reload();
      approvalsChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };

  return { pending, appOf, approve, reject, busy, error: list.error ?? error, reload: list.reload, edit, snooze };
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
export function ActionPane({ a, approvals }: { a: CloudAction; approvals: Approvals }) {
  const t = useT();
  const appInfo = approvals.appOf(a.toolkit);
  const busy = approvals.busy;
  const onApprove = () => approvals.approve(a);
  const onReject = () => void approvals.reject(a);
  const denied = a.decision === "deny";
  const [editing, setEditing] = useState(false);
  const [snoozing, setSnoozing] = useState(false);
  const editable = (a.editable ?? []).filter((f) => FIELD[f]);
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
      {editing ? (
        <ActionEditor a={a} fields={editable} busy={busy !== null} onCancel={() => setEditing(false)} onSave={(input) => approvals.edit(a, input)} onSaved={() => setEditing(false)} />
      ) : (
        <>
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
        </>
      )}
      {snoozing && <SnoozeMenu a={a} onPick={(until) => void approvals.snooze(a, until).then(() => setSnoozing(false))} onClose={() => setSnoozing(false)} />}
      {editing ? null : denied ? (
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
          <span className="action-footer-gap" />
          {editable.length > 0 && (
            <>
              <button type="button" className="btn btn-ghost" disabled={busy !== null} onClick={() => setEditing(true)} data-shortcut="accept" aria-keyshortcuts={comboAria(EDIT)}>
                {t("needs.edit.button")}
              </button>
              <kbd className="keycap" aria-hidden="true" dir="ltr">
                {comboLabel(EDIT)}
              </kbd>
            </>
          )}
          <button
            type="button"
            className="btn btn-ghost"
            disabled={busy !== null}
            onClick={() => setSnoozing((v) => !v)}
            aria-expanded={snoozing}
            data-shortcut="snooze"
            aria-keyshortcuts={comboAria(SNOOZE)}
          >
            {t("today.snooze")}
          </button>
          <kbd className="keycap" aria-hidden="true" dir="ltr">
            {comboLabel(SNOOZE)}
          </kbd>
        </div>
      )}
    </article>
  );
}

/** Edit what an action will send: only the fields its `editable` list names. */
function ActionEditor({
  a,
  fields,
  busy,
  onSave,
  onSaved,
  onCancel,
}: {
  a: CloudAction;
  fields: string[];
  busy: boolean;
  onSave: (input: Record<string, unknown>) => Promise<unknown>;
  onSaved: () => void;
  onCancel: () => void;
}) {
  const t = useT();
  const initial = useRef(Object.fromEntries(fields.map((f) => [f, prefill(a, f)])));
  const [values, setValues] = useState<Record<string, string>>(initial.current);
  const [error, setError] = useState<unknown>(null);
  const form = useRef<HTMLFormElement>(null);
  useEffect(() => form.current?.querySelector<HTMLElement>("input, textarea")?.focus(), []);
  const save = async (e: FormEvent) => {
    e.preventDefault();
    const input: Record<string, unknown> = {};
    for (const f of fields) {
      const v = (values[f] ?? "").trim();
      if (v === (initial.current[f] ?? "").trim()) continue;
      input[f] = LIST.has(f) ? v.split(/[,;\s]+/).filter(Boolean) : v;
    }
    if (Object.keys(input).some((f) => TIME.has(f)) && fields.includes("timezone")) input.timezone = deviceTimeZone() ?? "UTC";
    if (Object.keys(input).length === 0) return onCancel();
    const err = await onSave(input);
    if (err) setError(err);
    else onSaved();
  };
  return (
    <form ref={form} className="action-editor" onSubmit={(e) => void save(e)} onKeyDown={(e) => e.key === "Escape" && (e.stopPropagation(), onCancel())}>
      {fields.map((f) => {
        const id = `edit-${a.id}-${f}`;
        const set = (v: string) => setValues((x) => ({ ...x, [f]: v }));
        return (
          <div key={f} className="field">
            <label htmlFor={id}>{t(FIELD[f]!)}</label>
            {LONG.has(f) ? (
              <textarea id={id} className="input" rows={f === "markdown" ? 8 : 5} dir="auto" value={values[f] ?? ""} onChange={(e) => set(e.target.value)} />
            ) : (
              <input id={id} className="input" type={TIME.has(f) ? "datetime-local" : "text"} dir="auto" value={values[f] ?? ""} onChange={(e) => set(e.target.value)} />
            )}
          </div>
        );
      })}
      <ErrorNote error={error} />
      <div className="today-card-footer action-footer">
        <button type="submit" className="btn btn-primary" disabled={busy}>
          {busy ? <Spinner /> : t("needs.edit.save")}
        </button>
        <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={busy}>
          {t("common.cancel")}
        </button>
      </div>
    </form>
  );
}

const PRESET_LABEL: Record<SnoozePreset, MessageKey> = {
  hour: "needs.snooze.hour",
  afternoon: "needs.snooze.afternoon",
  evening: "needs.snooze.evening",
};

/** Snooze an action until a time before it expires: presets that still fit, or a typed time. */
function SnoozeMenu({ a, onPick, onClose }: { a: CloudAction; onPick: (until: Date) => void; onClose: () => void }) {
  const t = useT();
  const now = new Date();
  const expires = new Date(a.expires_at);
  const [text, setText] = useState("");
  const parsed = parseSnooze(text, now, expires);
  return (
    <div className="snooze-menu" role="group" aria-label={t("today.snoozeUntil")} onKeyDown={(e) => e.key === "Escape" && (e.stopPropagation(), onClose())}>
      <p className="snooze-menu-title">{t("today.snoozeUntil")}</p>
      <div className="snooze-presets">
        {snoozePresets(now, expires).map((p) => (
          <button key={p.id} type="button" className="btn btn-small" onClick={() => onPick(p.until)}>
            {t(PRESET_LABEL[p.id])} <span className="snooze-time">{formatTime(p.until.toISOString())}</span>
          </button>
        ))}
      </div>
      <form
        className="snooze-custom"
        onSubmit={(e) => {
          e.preventDefault();
          if (parsed) onPick(parsed);
        }}
      >
        <input
          className="input"
          aria-label={t("needs.snooze.custom")}
          placeholder={t("needs.snooze.placeholder")}
          value={text}
          onChange={(e) => setText(e.target.value)}
          autoFocus
        />
        <button type="submit" className="btn btn-small btn-primary" disabled={!parsed}>
          {t("today.snooze")}
        </button>
      </form>
      <p className="small muted" aria-live="polite">
        {text.trim() ? (parsed ? formatTime(parsed.toISOString()) : t("needs.snooze.invalid", { when: formatTime(a.expires_at) })) : ""}
      </p>
    </div>
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
