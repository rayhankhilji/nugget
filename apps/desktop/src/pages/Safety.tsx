import { useEffect, useId, useState, type FormEvent } from "react";
import {
  agentApi,
  agentEvents,
  isMethodNotFound,
  POLICY_EFFECTS,
  RISK_CLASSES,
  type PolicyConfig,
  type PolicyRule,
  type PolicyState,
  type RiskClass,
} from "../api";
import {
  addRule,
  ANY_TARGET,
  effectLabel,
  emptyDraft,
  exportFileName,
  removeRule,
  riskLabel,
  riskTone,
  ruleFromDraft,
  rulesByTarget,
  setDefaultEnabled,
  setRateLimit,
  type RuleDraft,
  type RuleErrors,
} from "../agent";
import { useAsync, useEvent } from "../hooks";
import { Dialog } from "../components/Dialog";
import { Badge, EmptyState, ErrorNote, Loading, NotAvailable, PageHeader } from "../components/ui";
import { formatNumber, useT } from "../i18n";

const EFFECT_TONE = { allow: "good", confirm: "warn", deny: "bad" } as const;

/** What the agent may do: rules per app and site, limits, approvals, the kill switch, the log. */
export function SafetyPage() {
  const t = useT();
  const loaded = useAsync(agentApi.policyGet);
  const [state, setState] = useState<PolicyState | null>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);
  const [adding, setAdding] = useState(false);
  const [announce, setAnnounce] = useState("");

  useEffect(() => {
    if (loaded.data) setState(loaded.data);
  }, [loaded.data]);

  useEvent(agentEvents.onKillChanged, (k) => setState((s) => (s ? { ...s, halted: k.halted } : s)));

  const save = async (config: PolicyConfig, message: string): Promise<boolean> => {
    setSaving(true);
    setSaveError(null);
    try {
      setState(await agentApi.policySet(config));
      setAnnounce(message);
      return true;
    } catch (e) {
      setSaveError(e);
      return false;
    } finally {
      setSaving(false);
    }
  };

  if (isMethodNotFound(loaded.error)) {
    return (
      <div className="page">
        <PageHeader title={t("safety.title")} description={t("safety.description")} />
        <NotAvailable what={t("safety.title")} />
      </div>
    );
  }

  const config = state?.config;

  return (
    <div className="page">
      <PageHeader
        title={t("safety.title")}
        description={t("safety.description")}
        actions={
          <button type="button" className="btn btn-ghost" onClick={loaded.reload} disabled={loaded.loading}>
            {t("common.refresh")}
          </button>
        }
      />
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>
      <ErrorNote error={loaded.error} onRetry={loaded.reload} />
      {loaded.loading && !state && <Loading />}

      {state && config && (
        <>
          <KillSwitch halted={state.halted} onChange={(halted) => setState((s) => (s ? { ...s, halted } : s))} />
          <Approvals />

          <section className="safety-section" aria-labelledby="safety-rules-title">
            <div className="agent-run-head">
              <div>
                <h2 id="safety-rules-title">{t("safety.rules.title")}</h2>
                <p className="muted small">{t("safety.rules.description")}</p>
              </div>
              <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
                {t("safety.rules.add")}
              </button>
            </div>
            <ErrorNote error={saveError} />
            <UserRules
              rules={config.rules ?? []}
              busy={saving}
              onRemove={(id) => void save(removeRule(config, id), t("safety.announce.removed", { id }))}
            />
            <h3>{t("safety.defaults.title")}</h3>
            <p className="muted small">{t("safety.defaults.description")}</p>
            <DefaultRules
              state={state}
              busy={saving}
              onToggle={(id, enabled) =>
                void save(setDefaultEnabled(config, id, enabled), t(enabled ? "safety.announce.enabled" : "safety.announce.disabled", { id }))
              }
            />
          </section>

          <Limits config={config} busy={saving} onSave={(c) => save(c, t("safety.announce.limitsSaved"))} />
          <LogStatus state={state} />
          <Browser />
        </>
      )}

      {adding && config && (
        <AddRuleDialog
          rules={config.rules ?? []}
          onClose={() => setAdding(false)}
          onAdd={async (rule) => {
            const ok = await save(addRule(config, rule), t("safety.announce.added", { id: rule.id }));
            if (ok) setAdding(false);
            return ok;
          }}
        />
      )}
    </div>
  );
}

function KillSwitch({ halted, onChange }: { halted: boolean; onChange: (halted: boolean) => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const flip = async () => {
    setBusy(true);
    setError(null);
    try {
      const k = halted ? await agentApi.policyResume() : await agentApi.policyKill();
      onChange(k.halted);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={`card safety-kill${halted ? " safety-kill-on" : ""}`} aria-labelledby="safety-kill-title">
      <div className="card-head">
        <div>
          <h2 id="safety-kill-title" className="card-title">
            {t("safety.kill.title")}
          </h2>
          <p className="muted small">{t("safety.kill.description")}</p>
        </div>
        <Badge tone={halted ? "bad" : "good"}>{halted ? t("safety.kill.halted") : t("safety.kill.running")}</Badge>
      </div>
      <ErrorNote error={error} />
      <div className="card-actions">
        <button type="button" className={halted ? "btn btn-primary" : "btn btn-danger"} onClick={() => void flip()} disabled={busy}>
          {halted ? t("safety.kill.resume") : t("safety.kill.stop")}
        </button>
      </div>
    </section>
  );
}

function Approvals() {
  const t = useT();
  const approvals = useAsync(agentApi.policyApprovals);
  const [error, setError] = useState<unknown>(null);
  const { reload } = approvals;
  useEvent(agentEvents.onAgentEvent, (n) => {
    const e = n.event.event;
    if (e === "confirmation_needed" || e === "confirmation_resolved" || e === "finished") reload();
  });
  const answer = async (taskId: string, requestId: string, choice: "once" | "deny") => {
    setError(null);
    try {
      await agentApi.agentConfirm(taskId, requestId, choice);
      reload();
    } catch (e) {
      setError(e);
    }
  };
  const data = approvals.data;
  return (
    <section className="safety-section" aria-labelledby="safety-approvals-title">
      <h2 id="safety-approvals-title">{t("safety.approvals.title")}</h2>
      <ErrorNote error={approvals.error ?? error} onRetry={reload} />
      {approvals.loading && !data && <Loading />}
      {data && (
        <>
          <h3>{t("safety.approvals.pending")}</h3>
          {data.pending.length === 0 ? (
            <p className="muted small">{t("safety.approvals.noPending")}</p>
          ) : (
            <ul className="safety-list">
              {data.pending.map((p) => (
                <li key={p.request_id} className="card">
                  <div className="card-head">
                    <span className="card-title">
                      <code>{p.tool}</code> {p.action.target_app}
                    </span>
                    <Badge tone={riskTone(p.action.declared_risk)}>{riskLabel(p.action.declared_risk)}</Badge>
                  </div>
                  <p className="small">{p.reason}</p>
                  <div className="card-actions inline-row">
                    <button type="button" className="btn btn-small btn-danger" onClick={() => void answer(p.action.task_id, p.request_id, "deny")}>
                      {t("agent.confirm.deny")}
                    </button>
                    <button type="button" className="btn btn-small btn-primary" onClick={() => void answer(p.action.task_id, p.request_id, "once")}>
                      {t("agent.confirm.approveOnce")}
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
          <h3>{t("safety.approvals.task")}</h3>
          {data.task.length === 0 ? (
            <p className="muted small">{t("safety.approvals.noTask")}</p>
          ) : (
            <ul className="safety-list-plain">
              {data.task.map((a, i) => (
                <li key={`${a.task_id}-${a.risk}-${i}`}>
                  <Badge tone={riskTone(a.risk)}>{riskLabel(a.risk)}</Badge> {a.target ?? t("safety.anyTarget")}{" "}
                  <span className="muted small">
                    <code>{a.task_id}</code>
                  </span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </section>
  );
}

function RuleConditions({ rule }: { rule: PolicyRule }) {
  const t = useT();
  return (
    <span className="small">
      {rule.verbs?.length ? (
        <span className="rule-cond">
          <span className="muted">{t("safety.rule.verbs")}</span> {rule.verbs.join(", ")}
        </span>
      ) : null}
      {rule.risks?.length ? (
        <span className="rule-cond">
          <span className="muted">{t("safety.rule.risks")}</span> {rule.risks.map((r) => riskLabel(r)).join(", ")}
        </span>
      ) : null}
      {rule.labels?.length ? (
        <span className="rule-cond">
          <span className="muted">{t("safety.rule.labels")}</span> {rule.labels.join(", ")}
        </span>
      ) : null}
      {!rule.verbs?.length && !rule.risks?.length && !rule.labels?.length && <span className="muted">{t("safety.rule.anyAction")}</span>}
    </span>
  );
}

function UserRules({ rules, busy, onRemove }: { rules: PolicyRule[]; busy: boolean; onRemove: (id: string) => void }) {
  const t = useT();
  if (!rules.length) return <EmptyState title={t("safety.rules.emptyTitle")}>{t("safety.rules.emptyBody")}</EmptyState>;
  return (
    <div className="rule-groups">
      {rulesByTarget(rules).map(([target, list]) => (
        <section key={target} className="card" aria-label={target === ANY_TARGET ? t("safety.anyTarget") : target}>
          <h3 className="card-title">{target === ANY_TARGET ? t("safety.anyTarget") : <code>{target}</code>}</h3>
          <ul className="safety-list-plain">
            {list.map((r) => (
              <li key={r.id} className="rule-row">
                <Badge tone={EFFECT_TONE[r.effect] ?? "neutral"}>{effectLabel(r.effect)}</Badge>
                <span className="rule-main">
                  <code>{r.id}</code> {r.description && <span>{r.description}</span>}
                  <RuleConditions rule={r} />
                </span>
                <button
                  type="button"
                  className="btn btn-small btn-danger-ghost"
                  onClick={() => onRemove(r.id)}
                  disabled={busy}
                  aria-label={t("safety.rules.removeLabel", { id: r.id })}
                >
                  {t("safety.rules.remove")}
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}

function DefaultRules({ state, busy, onToggle }: { state: PolicyState; busy: boolean; onToggle: (id: string, enabled: boolean) => void }) {
  const t = useT();
  if (!state.defaults.length) return <p className="muted small">{t("safety.defaults.none")}</p>;
  return (
    <div className="table-wrap" tabIndex={0} role="region" aria-label={t("safety.defaults.title")}>
      <table>
        <thead>
          <tr>
            <th scope="col">{t("safety.defaults.col.rule")}</th>
            <th scope="col">{t("safety.defaults.col.applies")}</th>
            <th scope="col">{t("safety.defaults.col.effect")}</th>
            <th scope="col">{t("safety.defaults.col.enabled")}</th>
          </tr>
        </thead>
        <tbody>
          {state.defaults.map(({ rule, enabled }) => (
            <tr key={rule.id}>
              <th scope="row">
                {rule.description ?? rule.id}
                <div className="muted small">
                  <code>{rule.id}</code>
                </div>
              </th>
              <td>
                <span className="small">{rule.targets?.length ? rule.targets.join(", ") : t("safety.anyTarget")}</span>{" "}
                <RuleConditions rule={rule} />
              </td>
              <td>
                <Badge tone={EFFECT_TONE[rule.effect] ?? "neutral"}>{effectLabel(rule.effect)}</Badge>
              </td>
              <td>
                <label className="switch">
                  <input
                    type="checkbox"
                    role="switch"
                    checked={enabled}
                    disabled={busy}
                    onChange={(e) => onToggle(rule.id, e.target.checked)}
                    aria-label={t("safety.defaults.toggleLabel", { id: rule.id })}
                  />
                  <span className="switch-track" aria-hidden="true" />
                </label>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function AddRuleDialog({ rules, onClose, onAdd }: { rules: PolicyRule[]; onClose: () => void; onAdd: (rule: PolicyRule) => Promise<boolean> }) {
  const t = useT();
  const ids = { id: useId(), desc: useId(), targets: useId(), verbs: useId(), labels: useId(), idErr: useId(), condErr: useId(), targetsHint: useId() };
  const [draft, setDraft] = useState<RuleDraft>(() => emptyDraft(rules));
  const [errors, setErrors] = useState<RuleErrors>({});
  const [busy, setBusy] = useState(false);
  const set = <K extends keyof RuleDraft>(k: K, v: RuleDraft[K]) => setDraft((d) => ({ ...d, [k]: v }));

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const r = ruleFromDraft(draft, rules);
    if ("errors" in r) {
      setErrors(r.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    const ok = await onAdd(r.rule);
    if (!ok) setBusy(false);
  };

  return (
    <Dialog title={t("safety.rule.addTitle")} description={t("safety.rule.addDescription")} onClose={onClose} wide>
      <form onSubmit={(e) => void submit(e)} noValidate>
        <div className="field-grid">
          <div className="field">
            <label htmlFor={ids.id}>{t("safety.rule.id")}</label>
            <input
              id={ids.id}
              value={draft.id}
              onChange={(e) => set("id", e.target.value)}
              aria-invalid={errors.id ? true : undefined}
              aria-describedby={errors.id ? ids.idErr : undefined}
            />
            {errors.id && (
              <p id={ids.idErr} className="field-error small">
                {errors.id}
              </p>
            )}
          </div>
          <div className="field">
            <label htmlFor={ids.desc}>{t("safety.rule.description")}</label>
            <input id={ids.desc} value={draft.description} onChange={(e) => set("description", e.target.value)} />
          </div>
        </div>
        <fieldset className="field">
          <legend>{t("safety.rule.effect")}</legend>
          <div className="inline-row">
            {POLICY_EFFECTS.map((eff) => (
              <label key={eff} className="check">
                <input type="radio" name="rule-effect" checked={draft.effect === eff} onChange={() => set("effect", eff)} />
                <span>{effectLabel(eff)}</span>
              </label>
            ))}
          </div>
        </fieldset>
        <div className="field">
          <label htmlFor={ids.targets}>{t("safety.rule.targets")}</label>
          <input
            id={ids.targets}
            value={draft.targets}
            onChange={(e) => set("targets", e.target.value)}
            placeholder={t("safety.rule.targetsPlaceholder")}
            aria-describedby={ids.targetsHint}
            data-autofocus=""
          />
          <p id={ids.targetsHint} className="field-hint muted small">
            {t("safety.rule.targetsHint")}
          </p>
        </div>
        <div className="field-grid">
          <div className="field">
            <label htmlFor={ids.verbs}>{t("safety.rule.verbs")}</label>
            <input id={ids.verbs} value={draft.verbs} onChange={(e) => set("verbs", e.target.value)} placeholder={t("safety.rule.verbsPlaceholder")} />
          </div>
          <div className="field">
            <label htmlFor={ids.labels}>{t("safety.rule.labels")}</label>
            <input id={ids.labels} value={draft.labels} onChange={(e) => set("labels", e.target.value)} />
          </div>
        </div>
        <fieldset className="field" aria-describedby={errors.conditions ? ids.condErr : undefined}>
          <legend>{t("safety.rule.risks")}</legend>
          <div className="inline-row">
            {RISK_CLASSES.map((r) => (
              <label key={r} className="check">
                <input
                  type="checkbox"
                  checked={draft.risks.includes(r)}
                  onChange={(e) => set("risks", e.target.checked ? [...draft.risks, r] : draft.risks.filter((x) => x !== r))}
                />
                <span>{riskLabel(r)}</span>
              </label>
            ))}
          </div>
          {draft.effect === "allow" && draft.risks.length === 0 && <p className="muted small">{t("safety.rule.allowHint")}</p>}
          {errors.conditions && (
            <p id={ids.condErr} className="field-error small">
              {errors.conditions}
            </p>
          )}
        </fieldset>
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {t("safety.rule.save")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

function Limits({ config, busy, onSave }: { config: PolicyConfig; busy: boolean; onSave: (c: PolicyConfig) => Promise<boolean> }) {
  const t = useT();
  const timeoutId = useId();
  const baseId = useId();
  const initial = () => ({
    limits: Object.fromEntries(RISK_CLASSES.map((r) => [r, config.rate_limits?.[r]?.toString() ?? ""])) as Record<RiskClass, string>,
    timeout: String(config.confirm_timeout_secs ?? ""),
  });
  const [form, setForm] = useState(initial);
  const [invalid, setInvalid] = useState(false);
  useEffect(() => setForm(initial()), [config]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const timeout = Number(form.timeout);
    if (!Number.isInteger(timeout) || timeout < 5 || timeout > 3600) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    let next: PolicyConfig = { ...config, confirm_timeout_secs: timeout };
    for (const r of RISK_CLASSES) {
      const v = form.limits[r].trim();
      next = setRateLimit(next, r, v === "" ? null : Number(v));
    }
    await onSave(next);
  };

  return (
    <section className="safety-section" aria-labelledby="safety-limits-title">
      <h2 id="safety-limits-title">{t("safety.limits.title")}</h2>
      <p className="muted small">{t("safety.limits.description")}</p>
      <form onSubmit={(e) => void submit(e)} className="card" noValidate>
        <div className="limits-grid">
          {RISK_CLASSES.map((r) => (
            <div key={r} className="field">
              <label htmlFor={`${baseId}-${r}`}>{riskLabel(r)}</label>
              <input
                id={`${baseId}-${r}`}
                type="number"
                min={1}
                inputMode="numeric"
                value={form.limits[r]}
                placeholder={t("safety.limits.unlimited")}
                onChange={(e) => setForm((f) => ({ ...f, limits: { ...f.limits, [r]: e.target.value } }))}
              />
            </div>
          ))}
        </div>
        <div className="field">
          <label htmlFor={timeoutId}>{t("safety.limits.timeout")}</label>
          <input
            id={timeoutId}
            type="number"
            min={5}
            max={3600}
            value={form.timeout}
            onChange={(e) => setForm((f) => ({ ...f, timeout: e.target.value }))}
            aria-invalid={invalid ? true : undefined}
            aria-describedby={`${timeoutId}-hint`}
          />
          <p id={`${timeoutId}-hint`} className={invalid ? "field-error small" : "field-hint muted small"}>
            {t("safety.limits.timeoutHint")}
          </p>
        </div>
        <div className="card-actions">
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {t("safety.limits.save")}
          </button>
        </div>
      </form>
    </section>
  );
}

function LogStatus({ state }: { state: PolicyState }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [done, setDone] = useState("");
  const log = state.log;

  const exportLog = async () => {
    setBusy(true);
    setError(null);
    setDone("");
    try {
      const out = await agentApi.policyLogExport();
      const name = exportFileName(Date.now());
      download(out.jsonl, name);
      setDone(t("safety.log.exported", { name, n: out.status.entries }));
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="safety-section" aria-labelledby="safety-log-title">
      <h2 id="safety-log-title">{t("safety.log.title")}</h2>
      <p className="muted small">{t("safety.log.description")}</p>
      <dl className="stats">
        <div>
          <dt>{t("safety.log.entries")}</dt>
          <dd>{formatNumber(log.entries)}</dd>
        </div>
        <div>
          <dt>{t("safety.log.chain")}</dt>
          <dd>
            <Badge tone={log.chain_ok ? "good" : "bad"}>{log.chain_ok ? t("safety.log.chainOk") : t("safety.log.chainBroken")}</Badge>
          </dd>
        </div>
      </dl>
      {log.head && (
        <p className="muted small">
          {t("safety.log.head", { seq: log.head[0] })} <code className="log-hash">{log.head[1]}</code>
        </p>
      )}
      {log.error && (
        <p className="note note-error" role="alert">
          {log.error}
        </p>
      )}
      <ErrorNote error={error} />
      <div className="inline-row">
        <button type="button" className="btn" onClick={() => void exportLog()} disabled={busy}>
          {t("safety.log.export")}
        </button>
        <span role="status" className="muted small">
          {done}
        </span>
      </div>
    </section>
  );
}

/** Save `text` as a file through a temporary Blob link. */
function download(text: string, name: string) {
  const blob = new Blob([text], { type: "application/x-ndjson" });
  if (typeof URL.createObjectURL !== "function") return;
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function Browser() {
  const t = useT();
  const status = useAsync(agentApi.browserStatus);
  const s = status.data;
  return (
    <section className="safety-section" aria-labelledby="safety-browser-title">
      <h2 id="safety-browser-title">{t("safety.browser.title")}</h2>
      {isMethodNotFound(status.error) ? <NotAvailable what={t("safety.browser.title")} /> : <ErrorNote error={status.error} onRetry={status.reload} />}
      {status.loading && !s && <Loading />}
      {s && (
        <div className="card">
          <div className="card-head">
            <span className="card-title">{s.connected ? t("safety.browser.connected") : t("safety.browser.disconnected")}</span>
            <Badge tone={s.connected ? "good" : "neutral"}>{s.connected ? t("safety.browser.on") : t("safety.browser.off")}</Badge>
          </div>
          {!s.connected && <p className="muted small">{t("safety.browser.help")}</p>}
          {s.origin && (
            <p className="small">
              <span className="muted">{t("safety.browser.origin")}</span> <code>{s.origin}</code>
            </p>
          )}
          {s.host_version && (
            <p className="small">
              <span className="muted">{t("safety.browser.version")}</span> {s.host_version}
            </p>
          )}
          <p className="small">
            <span className="muted">{t("safety.browser.tools")}</span>{" "}
            {s.tools.length ? s.tools.map((tool) => <code key={tool} className="tl-call">{tool}</code>) : t("safety.browser.noTools")}
          </p>
          <div className="card-actions">
            <button type="button" className="btn btn-ghost btn-small" onClick={status.reload}>
              {t("common.refresh")}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
