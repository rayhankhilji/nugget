import { useEffect, useId, useState, type FormEvent } from "react";
import {
  agentApi,
  agentEvents,
  api,
  isMethodNotFound,
  type AgentOutcome,
  type AgentTaskSummary,
  type AgentVerificationCounts,
  type ConfirmChoice,
} from "../api";
import {
  canApproveForTask,
  emptyRun,
  isLive,
  outcomeDetail,
  outcomeLabel,
  outcomeTone,
  reduceRuns,
  riskLabel,
  riskTone,
  runFromDetail,
  secondsLeft,
  taskStateLabel,
  verificationLabel,
  verificationTone,
  type ActionItem,
  type PendingConfirmation,
  type RunView,
  type TimelineItem,
} from "../agent";
import { useAsync, useEvent, useNow } from "../hooks";
import { formatRelative, formatTime } from "../format";
import { Dialog } from "../components/Dialog";
import { GoToPage } from "../components/GoToPage";
import { Badge, EmptyState, ErrorNote, Loading, NotAvailable, PageHeader, Spinner } from "../components/ui";
import { formatNumber, useT, type MessageKey } from "../i18n";

const LIVE_STATES = new Set(["running", "confirming", "needs_input"]);
const HISTORY_LIMIT = 50;

/** Run a goal with the agent, follow it step by step, answer its confirmations, review history. */
export function AgentPage() {
  const t = useT();
  const history = useAsync(() => agentApi.agentList(HISTORY_LIMIT));
  const policy = useAsync(agentApi.policyGet);
  // Only to explain a run that can't start: an older daemon or a failed call shows nothing here.
  const providers = useAsync(api.providersList);
  const noModel = providers.data !== undefined && providers.data.filter((p) => !p.error).length === 0;
  const [runs, setRuns] = useState<Record<string, RunView>>({});
  const [activeId, setActiveId] = useState<string | null>(null);
  const [halted, setHalted] = useState(false);
  const [later, setLater] = useState<string[]>([]);
  const [detailOf, setDetailOf] = useState<string | null>(null);
  const [announce, setAnnounce] = useState("");
  const [actionError, setActionError] = useState<unknown>(null);

  useEffect(() => {
    if (policy.data) setHalted(policy.data.halted);
  }, [policy.data]);

  const { reload: reloadHistory } = history;
  useEvent(agentEvents.onAgentEvent, (n) => {
    setRuns((r) => reduceRuns(r, n, Date.now()));
    const e = n.event;
    if (n.task_id !== activeId) {
      if (e.event === "finished") reloadHistory();
      return;
    }
    if (e.event === "confirmation_needed") setAnnounce(t("agent.announce.confirm", { tool: e.request.tool }));
    else if (e.event === "suspended") setAnnounce(t("agent.announce.question"));
    else if (e.event === "finished") {
      setAnnounce(t("agent.announce.finished", { outcome: outcomeLabel(e.outcome.outcome) }));
      reloadHistory();
    }
  });
  useEvent(agentEvents.onKillChanged, (k) => {
    setHalted(k.halted);
    setAnnounce(k.halted ? t("agent.halted.title") : t("agent.announce.resumed"));
  });

  // Coming back to the page: follow the newest task still running, so its confirmations show.
  useEffect(() => {
    if (activeId || !history.data) return;
    const live = history.data.find((s) => LIVE_STATES.has(s.state));
    if (!live) return;
    let alive = true;
    agentApi
      .agentGet(live.task_id)
      .then((d) => {
        if (!alive) return;
        setRuns((r) => (r[live.task_id] ? r : { ...r, [live.task_id]: runFromDetail(d, Date.now()) }));
        setActiveId((cur) => cur ?? live.task_id);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [history.data, activeId]);

  const run = activeId ? runs[activeId] : undefined;
  const live = run ? isLive(run) : false;
  const shown = run?.pending.find((p) => !later.includes(p.request.request_id));

  const started = (taskId: string, goal: string) => {
    setRuns((r) => (r[taskId] ? { ...r, [taskId]: { ...r[taskId]!, goal: r[taskId]!.goal || goal } } : { ...r, [taskId]: emptyRun(taskId, goal) }));
    setActiveId(taskId);
    setLater([]);
    setAnnounce(t("agent.announce.started"));
    reloadHistory();
  };

  const act = async (fn: () => Promise<unknown>) => {
    setActionError(null);
    try {
      await fn();
    } catch (e) {
      setActionError(e);
    }
  };

  const cancel = () => run && void act(() => agentApi.agentCancel(run.taskId));

  const resume = () =>
    void act(async () => {
      const k = await agentApi.policyResume();
      setHalted(k.halted);
    });

  if (isMethodNotFound(history.error)) {
    return (
      <div className="page">
        <PageHeader title={t("agent.title")} description={t("agent.description")} />
        <NotAvailable what={t("agent.title")} />
      </div>
    );
  }

  return (
    <div className="page">
      <PageHeader title={t("agent.title")} description={t("agent.description")} />
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>

      {halted && (
        <div className="note note-warn agent-halted" role="alert">
          <span>
            <strong>{t("agent.halted.title")}</strong> {t("agent.halted.body")}
          </span>
          <button type="button" className="btn btn-small" onClick={resume}>
            {t("agent.halted.resume")}
          </button>
        </div>
      )}
      <ErrorNote error={actionError} />

      {run ? (
        <section className="agent-run" aria-labelledby="agent-run-title">
          <div className="agent-run-head">
            <div>
              <h2 id="agent-run-title">{t("agent.run.title")}</h2>
              <p className="agent-goal">{run.goal || <span className="muted">{t("agent.run.noGoal")}</span>}</p>
            </div>
            <div className="page-actions">
              {live ? (
                <button type="button" className="btn btn-danger" onClick={cancel}>
                  {t("agent.run.cancel")}
                </button>
              ) : (
                <button type="button" className="btn" onClick={() => setActiveId(null)}>
                  {t("agent.run.new")}
                </button>
              )}
            </div>
          </div>
          {live && !run.question && !run.pending.length && (
            <p className="muted small agent-working">
              <Spinner label={t("agent.run.working")} /> {t("agent.run.working")}
            </p>
          )}
          <Timeline run={run} onReview={(id) => setLater((l) => l.filter((x) => x !== id))} />
          {run.question && <AnswerForm taskId={run.taskId} question={run.question} onAnswered={() => setRuns((r) => (r[run.taskId] ? { ...r, [run.taskId]: { ...r[run.taskId]!, question: null, outcome: null } } : r))} />}
          {run.outcome && run.outcome.outcome !== "needs_input" && <OutcomePanel outcome={run.outcome} summary={run.summary} counts={run.verification} injections={run.injections} />}
        </section>
      ) : (
        <>
          {noModel && (
            <p className="note" role="note">
              <span>
                <strong>{t("agent.noModel.title")}</strong> {t("agent.noModel.body")}
              </span>{" "}
              <GoToPage page="models" className="btn btn-ghost btn-small" />
            </p>
          )}
          <RunForm halted={halted} onStarted={started} />
        </>
      )}

      <History history={history} activeId={activeId} onDetails={setDetailOf} />

      {shown && run && (
        <ConfirmDialog
          key={shown.request.request_id}
          pending={shown}
          taskId={run.taskId}
          onLater={() => setLater((l) => [...l, shown.request.request_id])}
        />
      )}
      {detailOf && <DetailDialog taskId={detailOf} onClose={() => setDetailOf(null)} />}
    </div>
  );
}

function RunForm({ halted, onStarted }: { halted: boolean; onStarted: (taskId: string, goal: string) => void }) {
  const t = useT();
  const ids = { goal: useId(), context: useId(), hint: useId() };
  const [goal, setGoal] = useState("");
  const [context, setContext] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const g = goal.trim();
    if (!g || busy) return;
    setBusy(true);
    setError(null);
    try {
      const s = await agentApi.agentRun({ goal: g, context: context.trim() || null });
      onStarted(s.task_id, g);
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  return (
    <form className="card agent-form" onSubmit={(e) => void submit(e)} aria-label={t("agent.form.label")}>
      <div className="field">
        <label htmlFor={ids.goal}>{t("agent.form.goal")}</label>
        <textarea
          id={ids.goal}
          rows={3}
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
          placeholder={t("agent.form.goalPlaceholder")}
          aria-describedby={ids.hint}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void submit(e);
          }}
        />
        <p id={ids.hint} className="field-hint muted small">
          {t("agent.form.goalHint")}
        </p>
      </div>
      <details className="help">
        <summary>{t("agent.form.contextSummary")}</summary>
        <div className="field">
          <label htmlFor={ids.context}>{t("agent.form.context")}</label>
          <textarea id={ids.context} rows={3} value={context} onChange={(e) => setContext(e.target.value)} />
        </div>
      </details>
      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="submit" className="btn btn-primary" disabled={!goal.trim() || busy || halted}>
          {busy ? <Spinner label={t("agent.form.starting")} /> : t("agent.form.run")}
        </button>
      </div>
      {halted && <p className="muted small">{t("agent.form.haltedHint")}</p>}
    </form>
  );
}

function Timeline({ run, onReview }: { run: RunView; onReview: (requestId: string) => void }) {
  const t = useT();
  if (!run.items.length) return null;
  return (
    <ol className="timeline" aria-label={t("agent.timeline.label")}>
      {run.items.map((it, i) => (
        <li key={i} className={`tl tl-${it.kind}`}>
          <TimelineEntry item={it} run={run} onReview={onReview} />
        </li>
      ))}
    </ol>
  );
}

function TimelineEntry({ item, run, onReview }: { item: TimelineItem; run: RunView; onReview: (requestId: string) => void }) {
  const t = useT();
  switch (item.kind) {
    case "step":
      return <span className="tl-step">{t("agent.timeline.step", { n: item.step })}</span>;
    case "model":
      return (
        <div>
          <span className="tl-label">{t("agent.timeline.model")}</span>
          {item.text && <p className="tl-text">{item.text}</p>}
          {item.toolCalls.length > 0 && (
            <p className="muted small">
              {t("agent.timeline.calls")}{" "}
              {item.toolCalls.map((c, i) => (
                <code key={i} className="tl-call">
                  {c}
                </code>
              ))}
            </p>
          )}
        </div>
      );
    case "action":
      return <ActionEntry item={item} pending={run.pending.find((p) => p.request.action_id === item.actionId)} onReview={onReview} />;
    case "question":
      return (
        <div>
          <span className="tl-label">{t("agent.timeline.question")}</span>
          <p className="tl-text">{item.question}</p>
        </div>
      );
    case "finished":
      return (
        <div>
          <span className="tl-label">{t("agent.timeline.finished")}</span> <Badge tone={outcomeTone(item.outcome.outcome)}>{outcomeLabel(item.outcome.outcome)}</Badge>
        </div>
      );
  }
}

const STATUS_TONE: Record<ActionItem["status"], "neutral" | "accent" | "good" | "warn" | "bad"> = {
  proposed: "neutral",
  awaiting: "accent",
  running: "accent",
  done: "good",
  error: "bad",
  denied: "bad",
};

function ActionEntry({ item, pending, onReview }: { item: ActionItem; pending: PendingConfirmation | undefined; onReview: (requestId: string) => void }) {
  const t = useT();
  const a = item.action;
  return (
    <div className="tl-act">
      <div className="tl-action-head">
        <span className="tl-label">{t("agent.timeline.action")}</span>
        <code>{item.tool}</code>
        {a && <Badge tone={riskTone(a.declared_risk)}>{riskLabel(a.declared_risk)}</Badge>}
        <Badge tone={STATUS_TONE[item.status]}>{t(`agent.status.${item.status}` as MessageKey)}</Badge>
        {item.replayed && <Badge>{t("agent.status.replayed")}</Badge>}
        {item.verification && (
          <Badge tone={verificationTone(item.verification.status)}>
            {t("agent.timeline.verified", { status: verificationLabel(item.verification.status) })}
          </Badge>
        )}
      </div>
      {a && (
        <p className="small">
          <span className="muted">{t("agent.field.target")}</span> {a.target_app || "—"} · <span className="muted">{t("agent.field.verb")}</span> <code>{a.verb}</code>
          {a.target_label && (
            <>
              {" · "}
              <span className="muted">{t("agent.field.label")}</span> {a.target_label}
            </>
          )}
        </p>
      )}
      {item.text && <p className="tl-text small">{item.text}</p>}
      {item.answer && <p className="muted small">{t(`agent.answer.${item.answer}` as MessageKey)}</p>}
      {item.verification?.detail && <p className="muted small">{item.verification.detail}</p>}
      {item.injection && (
        <div className="note note-warn small" role="note">
          <span>
            <strong>{t("agent.injection.title")}</strong> {t("agent.injection.body", { patterns: item.injection.patterns.join(", ") })}
          </span>
          {item.injection.excerpt && <q className="tl-excerpt">{item.injection.excerpt}</q>}
        </div>
      )}
      {pending && (
        <button type="button" className="btn btn-small btn-primary" onClick={() => onReview(pending.request.request_id)}>
          {t("agent.confirm.review")}
        </button>
      )}
    </div>
  );
}

function AnswerForm({ taskId, question, onAnswered }: { taskId: string; question: string; onAnswered: () => void }) {
  const t = useT();
  const id = useId();
  const [answer, setAnswer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!answer.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await agentApi.agentAnswer(taskId, answer.trim());
      onAnswered();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="card agent-answer" onSubmit={(e) => void submit(e)}>
      <div className="field">
        <label htmlFor={id}>{t("agent.question.label")}</label>
        <p className="tl-text">{question}</p>
        <textarea id={id} rows={2} value={answer} onChange={(e) => setAnswer(e.target.value)} />
      </div>
      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="submit" className="btn btn-primary" disabled={!answer.trim() || busy}>
          {t("agent.question.send")}
        </button>
      </div>
    </form>
  );
}

function OutcomePanel({ outcome, summary, counts, injections }: { outcome: AgentOutcome; summary: string | null; counts: AgentVerificationCounts; injections: number }) {
  const t = useT();
  const detail = outcomeDetail(outcome);
  return (
    <section className="card agent-outcome" aria-labelledby="agent-outcome-title">
      <div className="card-head">
        <h3 id="agent-outcome-title" className="card-title">
          {t("agent.outcomeTitle")}
        </h3>
        <Badge tone={outcomeTone(outcome.outcome)}>{outcomeLabel(outcome.outcome)}</Badge>
      </div>
      <p>{t(`agent.outcomeHelp.${outcome.outcome}` as MessageKey)}</p>
      {detail && <p className="muted small">{detail}</p>}
      {summary && <p className="tl-text">{summary}</p>}
      <VerificationStats counts={counts} injections={injections} />
    </section>
  );
}

function VerificationStats({ counts, injections }: { counts: AgentVerificationCounts; injections?: number }) {
  const t = useT();
  return (
    <dl className="stats agent-stats">
      <div>
        <dt>{t("agent.verification.passed")}</dt>
        <dd>{formatNumber(counts.passed)}</dd>
      </div>
      <div>
        <dt>{t("agent.verification.failed")}</dt>
        <dd>{formatNumber(counts.failed)}</dd>
      </div>
      <div>
        <dt>{t("agent.verification.unverifiable")}</dt>
        <dd>{formatNumber(counts.unverifiable)}</dd>
      </div>
      {injections !== undefined && (
        <div>
          <dt>{t("agent.stat.injections")}</dt>
          <dd>{formatNumber(injections)}</dd>
        </div>
      )}
    </dl>
  );
}

function ConfirmDialog({ pending, taskId, onLater }: { pending: PendingConfirmation; taskId: string; onLater: () => void }) {
  const t = useT();
  const now = useNow(1000);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const r = pending.request;
  const a = r.action;
  const left = secondsLeft(pending, now);

  const choose = async (choice: ConfirmChoice) => {
    setBusy(true);
    setError(null);
    try {
      await agentApi.agentConfirm(taskId, r.request_id, choice);
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };

  return (
    <Dialog title={t("agent.confirm.title")} description={r.reason} onClose={onLater} wide>
      {r.forced_by_injection && (
        <div className="note note-warn" role="note">
          <span>
            <strong>{t("agent.confirm.injectionTitle")}</strong> {t("agent.confirm.injectionBody")}
          </span>
        </div>
      )}
      <dl className="confirm-facts">
        <div>
          <dt>{t("agent.field.risk")}</dt>
          <dd>
            <Badge tone={riskTone(a.declared_risk)}>{riskLabel(a.declared_risk)}</Badge>
          </dd>
        </div>
        <div>
          <dt>{t("agent.field.target")}</dt>
          <dd>{a.target_app || "—"}</dd>
        </div>
        <div>
          <dt>{t("agent.field.verb")}</dt>
          <dd>
            <code>{a.verb}</code> <span className="muted small">({r.tool})</span>
          </dd>
        </div>
        {a.target_label && (
          <div>
            <dt>{t("agent.field.label")}</dt>
            <dd>{a.target_label}</dd>
          </div>
        )}
        {a.text_preview && (
          <div>
            <dt>{t("agent.field.preview")}</dt>
            <dd>
              <pre className="confirm-preview">{a.text_preview}</pre>
            </dd>
          </div>
        )}
      </dl>
      <p className="small muted" role="timer" aria-live="off">
        {left > 0 ? t("agent.confirm.countdown", { secs: left }) : t("agent.confirm.expired")}
      </p>
      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="button" className="btn btn-ghost" onClick={onLater} disabled={busy}>
          {t("agent.confirm.later")}
        </button>
        <button type="button" className="btn btn-danger" onClick={() => void choose("deny")} disabled={busy || left === 0} data-autofocus="">
          {t("agent.confirm.deny")}
        </button>
        {canApproveForTask(a.declared_risk) && (
          <button type="button" className="btn" onClick={() => void choose("task")} disabled={busy || left === 0}>
            {t("agent.confirm.approveTask")}
          </button>
        )}
        <button type="button" className="btn btn-primary" onClick={() => void choose("once")} disabled={busy || left === 0}>
          {t("agent.confirm.approveOnce")}
        </button>
      </div>
    </Dialog>
  );
}

function History({
  history,
  activeId,
  onDetails,
}: {
  history: { data: AgentTaskSummary[] | undefined; error: unknown; loading: boolean; reload: () => void };
  activeId: string | null;
  onDetails: (taskId: string) => void;
}) {
  const t = useT();
  const now = useNow();
  const rows = history.data ?? [];
  return (
    <section className="agent-history" aria-labelledby="agent-history-title">
      <div className="agent-run-head">
        <h2 id="agent-history-title">{t("agent.history.title")}</h2>
        <button type="button" className="btn btn-ghost btn-small" onClick={history.reload} disabled={history.loading}>
          {t("common.refresh")}
        </button>
      </div>
      <ErrorNote error={history.error} onRetry={history.reload} />
      {history.loading && !history.data && <Loading label={t("agent.history.loading")} />}
      {history.data && rows.length === 0 && <EmptyState title={t("agent.history.emptyTitle")}>{t("agent.history.emptyBody")}</EmptyState>}
      {rows.length > 0 && (
        <div className="table-wrap" tabIndex={0} role="region" aria-label={t("agent.history.label")}>
          <table>
            <thead>
              <tr>
                <th scope="col">{t("agent.history.col.goal")}</th>
                <th scope="col">{t("agent.history.col.result")}</th>
                <th scope="col">{t("agent.history.col.started")}</th>
                <th scope="col" className="num">
                  {t("agent.history.col.steps")}
                </th>
                <th scope="col">{t("agent.history.col.verification")}</th>
                <th scope="col">
                  <span className="visually-hidden">{t("agent.history.col.actions")}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((s) => {
                const at = new Date(s.started_ms).toISOString();
                return (
                  <tr key={s.task_id} aria-current={s.task_id === activeId ? "true" : undefined}>
                    <th scope="row" className="agent-history-goal">
                      {s.goal}
                    </th>
                    <td>
                      {s.outcome ? (
                        <Badge tone={outcomeTone(s.outcome)}>{outcomeLabel(s.outcome)}</Badge>
                      ) : (
                        <Badge tone={s.state === "interrupted" ? "warn" : "accent"}>{taskStateLabel(s.state)}</Badge>
                      )}
                    </td>
                    <td className="nowrap" title={formatTime(at)}>
                      {formatRelative(at, now)}
                    </td>
                    <td className="num">{formatNumber(s.steps)}</td>
                    <td className="small">
                      {t("agent.history.counts", { passed: s.verification.passed, failed: s.verification.failed, unverifiable: s.verification.unverifiable })}
                    </td>
                    <td>
                      <button type="button" className="btn btn-small btn-ghost" onClick={() => onDetails(s.task_id)} aria-label={t("agent.history.detailsLabel", { goal: s.goal })}>
                        {t("common.details")}
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function DetailDialog({ taskId, onClose }: { taskId: string; onClose: () => void }) {
  const t = useT();
  const detail = useAsync(() => agentApi.agentGet(taskId), [taskId]);
  const d = detail.data;
  const s = d?.summary;
  return (
    <Dialog title={t("agent.detail.title")} description={s?.goal} onClose={onClose} wide>
      <ErrorNote error={detail.error} onRetry={detail.reload} />
      {detail.loading && !d && <Loading label={t("agent.detail.loading")} />}
      {d && s && (
        <>
          <p>
            {s.outcome ? <Badge tone={outcomeTone(s.outcome)}>{outcomeLabel(s.outcome)}</Badge> : <Badge tone="accent">{taskStateLabel(s.state)}</Badge>}{" "}
            <span className="muted small">
              {formatTime(new Date(s.started_ms).toISOString())} · {t("agent.detail.steps", { n: s.steps })}
            </span>
          </p>
          {s.detail && <p className="muted small">{s.detail}</p>}
          <VerificationStats counts={s.verification} />
          {d.question && (
            <p>
              <span className="tl-label">{t("agent.timeline.question")}</span> {d.question}
            </p>
          )}
          {d.pending.length > 0 && <p className="note">{t("agent.detail.pending", { n: d.pending.length })}</p>}
          <h3>{t("agent.detail.explain")}</h3>
          <pre className="explain" tabIndex={0} aria-label={t("agent.detail.explain")}>
            {d.explain}
          </pre>
          <p className="muted small">
            <code>{s.task_id}</code>
          </p>
        </>
      )}
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose} data-autofocus="">
          {t("common.close")}
        </button>
      </div>
    </Dialog>
  );
}
