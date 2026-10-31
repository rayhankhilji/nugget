/**
 * Pure helpers for the Agent and Safety pages: the run timeline reducer over `agent.event`
 * notifications, outcome presentation, and policy-config editing. No React, no IPC.
 */
import {
  RISK_CLASSES,
  TASK_APPROVABLE_RISKS,
  type ActionDescriptor,
  type AgentBudget,
  type AgentEventNotice,
  type AgentOutcome,
  type AgentTaskDetail,
  type AgentVerificationCounts,
  type ConfirmAnswer,
  type ConfirmationRequest,
  type InjectionFinding,
  type PolicyConfig,
  type PolicyEffect,
  type PolicyRule,
  type RiskClass,
  type Verification,
} from "./api";
import { t, type MessageKey } from "./i18n";

// ---- timeline ----

export type ActionStatus = "proposed" | "awaiting" | "running" | "done" | "error" | "denied";

export interface ActionItem {
  kind: "action";
  step: number | null;
  actionId: string;
  tool: string;
  action: ActionDescriptor | null;
  status: ActionStatus;
  /** Result summary (done) or the reason (denied). */
  text: string | null;
  replayed: boolean;
  answer: ConfirmAnswer | null;
  verification: Verification | null;
  injection: InjectionFinding | null;
}

export type TimelineItem =
  | { kind: "step"; step: number }
  | { kind: "model"; step: number; text: string; toolCalls: string[] }
  | ActionItem
  | { kind: "question"; question: string; options: string[] }
  | { kind: "finished"; outcome: AgentOutcome; summary: string | null };

/** A pending confirmation, with when it arrived (for the countdown). */
export interface PendingConfirmation {
  request: ConfirmationRequest;
  receivedMs: number;
}

export interface RunView {
  taskId: string;
  goal: string;
  items: TimelineItem[];
  pending: PendingConfirmation[];
  /** Open `ask_user` question. */
  question: string | null;
  /** Candidate answers offered with the open question (untrusted text; empty: free text only). */
  options: string[];
  outcome: AgentOutcome | null;
  summary: string | null;
  verification: AgentVerificationCounts;
  injections: number;
}

export function emptyRun(taskId: string, goal = ""): RunView {
  return {
    taskId,
    goal,
    items: [],
    pending: [],
    question: null,
    options: [],
    outcome: null,
    summary: null,
    verification: { passed: 0, failed: 0, unverifiable: 0 },
    injections: 0,
  };
}

/** A run view for a task we did not watch start (from `agent.get`). */
export function runFromDetail(detail: AgentTaskDetail, nowMs: number): RunView {
  const run = emptyRun(detail.summary.task_id, detail.summary.goal);
  run.pending = (detail.pending ?? []).map((request) => ({ request, receivedMs: nowMs }));
  run.question = detail.summary.state === "needs_input" ? (detail.question ?? null) : null;
  run.options = run.question ? cleanOptions(detail.options) : [];
  run.verification = { ...detail.summary.verification };
  for (const p of run.pending) run.items.push(actionFromRequest(p.request));
  if (run.question) run.items.push({ kind: "question", question: run.question, options: run.options });
  return run;
}

/** Non-empty, distinct option strings (a newer or odd daemon may send anything). */
function cleanOptions(options: unknown): string[] {
  if (!Array.isArray(options)) return [];
  const out: string[] = [];
  for (const o of options) if (typeof o === "string" && o.trim() && !out.includes(o)) out.push(o);
  return out;
}

/** The run after the user answered its open question: question and options cleared. */
export function answered(run: RunView): RunView {
  return { ...run, question: null, options: [], outcome: null };
}

function newAction(actionId: string, tool: string): ActionItem {
  return {
    kind: "action",
    step: null,
    actionId,
    tool,
    action: null,
    status: "proposed",
    text: null,
    replayed: false,
    answer: null,
    verification: null,
    injection: null,
  };
}

function actionFromRequest(r: ConfirmationRequest): ActionItem {
  return { ...newAction(r.action_id, r.tool), action: r.action, status: "awaiting" };
}

/** Apply `patch` to the action with this id, creating it at the end if unseen. */
function withAction(items: TimelineItem[], actionId: string, tool: string, patch: (a: ActionItem) => ActionItem): TimelineItem[] {
  const i = items.findIndex((it) => it.kind === "action" && it.actionId === actionId);
  if (i < 0) return [...items, patch(newAction(actionId, tool))];
  const out = items.slice();
  out[i] = patch(items[i] as ActionItem);
  return out;
}

/** Fold one event into a run. Unknown events leave it unchanged. */
export function reduceRun(run: RunView, notice: AgentEventNotice, nowMs: number): RunView {
  const e = notice.event;
  switch (e.event) {
    case "run_started":
      return { ...run, goal: e.goal || run.goal };
    case "step_started":
      return { ...run, items: [...run.items, { kind: "step", step: e.step }] };
    case "model_replied":
      return { ...run, items: [...run.items, { kind: "model", step: e.step, text: e.text, toolCalls: e.tool_calls ?? [] }] };
    case "action_proposed":
      return {
        ...run,
        items: withAction(run.items, e.action_id, e.tool, (a) => ({ ...a, step: e.step, tool: e.tool, action: e.action })),
      };
    case "confirmation_needed": {
      const r = e.request;
      const pending = run.pending.filter((p) => p.request.request_id !== r.request_id);
      return {
        ...run,
        pending: [...pending, { request: r, receivedMs: nowMs }],
        items: withAction(run.items, r.action_id, r.tool, (a) => ({ ...a, action: a.action ?? r.action, status: "awaiting" })),
      };
    }
    case "confirmation_resolved": {
      const p = run.pending.find((x) => x.request.request_id === e.request_id);
      const pending = run.pending.filter((x) => x.request.request_id !== e.request_id);
      if (!p) return { ...run, pending };
      return {
        ...run,
        pending,
        items: withAction(run.items, p.request.action_id, p.request.tool, (a) => ({
          ...a,
          answer: e.answer,
          status: e.answer === "approved" ? "running" : a.status,
        })),
      };
    }
    case "action_denied":
      return {
        ...run,
        items: withAction(run.items, e.action_id, e.tool, (a) => ({ ...a, status: "denied", text: e.reason })),
      };
    case "action_done":
      return {
        ...run,
        items: withAction(run.items, e.action_id, e.tool, (a) => ({
          ...a,
          status: e.is_error ? "error" : "done",
          text: e.summary,
          replayed: e.replayed,
        })),
      };
    case "verification": {
      const v = e.verification;
      const key: keyof AgentVerificationCounts = v.status === "passed" ? "passed" : v.status === "failed" ? "failed" : "unverifiable";
      return {
        ...run,
        verification: { ...run.verification, [key]: run.verification[key] + 1 },
        items: withAction(run.items, e.action_id, e.tool, (a) => ({ ...a, verification: v })),
      };
    }
    case "injection_suspected":
      return {
        ...run,
        injections: run.injections + 1,
        items: withAction(run.items, e.action_id, e.tool, (a) => ({ ...a, injection: e.finding })),
      };
    case "suspended": {
      const options = cleanOptions(e.options);
      return { ...run, question: e.question, options, items: [...run.items, { kind: "question", question: e.question, options }] };
    }
    case "finished": {
      // `needs_input` ends the turn but the run waits for `agent.answer`: keep the question open.
      const asking = e.outcome.outcome === "needs_input";
      const question = asking ? (run.question ?? (typeof e.outcome.detail === "string" ? e.outcome.detail : null)) : null;
      return {
        ...run,
        pending: [],
        question,
        options: question ? run.options : [],
        outcome: e.outcome,
        summary: e.summary ?? null,
        items: [
          ...run.items.map((it) => (it.kind === "action" && it.status === "awaiting" ? { ...it, status: "denied" as const } : it)),
          ...(asking ? [] : [{ kind: "finished" as const, outcome: e.outcome, summary: e.summary ?? null }]),
        ],
      };
    }
    default:
      return run;
  }
}

/** Fold an event into the runs map (keyed by task id); events for unseen tasks start a run. */
export function reduceRuns(runs: Record<string, RunView>, notice: AgentEventNotice, nowMs: number): Record<string, RunView> {
  const id = notice.task_id;
  const run = runs[id] ?? emptyRun(id);
  return { ...runs, [id]: reduceRun(run, notice, nowMs) };
}

/** Whether a run is still going (not finished, or suspended waiting for an answer). */
export function isLive(run: RunView): boolean {
  return run.outcome === null || run.outcome.outcome === "needs_input";
}

/** Seconds left before a confirmation times out (never negative). */
export function secondsLeft(p: PendingConfirmation, nowMs: number): number {
  return Math.max(0, Math.ceil((p.receivedMs + p.request.timeout_ms - nowMs) / 1000));
}

/** Only Send and Destroy can be approved for the rest of a task. */
export function canApproveForTask(risk: RiskClass | string): boolean {
  return (TASK_APPROVABLE_RISKS as readonly string[]).includes(risk);
}

// ---- labels and tones ----

export type Tone = "neutral" | "accent" | "good" | "warn" | "bad";

/** Label for a known enum value, else the raw value (a newer daemon may send new ones). */
export function enumLabel(prefix: string, value: string): string {
  const key = `${prefix}.${value}` as MessageKey;
  const out = t(key);
  return out === key ? value : out;
}

export const riskLabel = (r: string) => enumLabel("agent.risk", r);
export const effectLabel = (e: string) => enumLabel("safety.effect", e);
export const outcomeLabel = (o: string) => enumLabel("agent.outcome", o);
export const taskStateLabel = (s: string) => enumLabel("agent.state", s);
export const verificationLabel = (v: string) => enumLabel("agent.verification", v);

export function riskTone(r: string): Tone {
  switch (r) {
    case "observe":
      return "neutral";
    case "edit":
      return "accent";
    case "send":
    case "destroy":
      return "warn";
    default:
      return "bad";
  }
}

/** Only `succeeded` is good: an unverified claim of success is a warning, not a pass. */
export function outcomeTone(label: string): Tone {
  switch (label) {
    case "succeeded":
      return "good";
    case "unverified":
    case "partial":
    case "budget_exhausted":
    case "refused":
      return "warn";
    case "needs_input":
      return "accent";
    case "cancelled":
      return "neutral";
    default:
      return "bad";
  }
}

export function verificationTone(status: string): Tone {
  return status === "passed" ? "good" : status === "failed" ? "bad" : "warn";
}

export function budgetText(b: AgentBudget): string {
  switch (b.budget) {
    case "steps":
      return t("agent.budget.steps", { limit: b.limit });
    case "deadline":
      return t("agent.budget.deadline", { secs: Math.round(b.limit_ms / 1000) });
    case "tokens":
      return t("agent.budget.tokens", { limit: b.limit });
    case "tool_rate":
      return t("agent.budget.toolRate", { tool: b.tool, max: b.max_calls });
    default:
      return JSON.stringify(b);
  }
}

/** The outcome's detail as text, if it has one. */
export function outcomeDetail(o: AgentOutcome): string | null {
  if (o.detail === undefined || o.detail === null) return null;
  return typeof o.detail === "string" ? o.detail : budgetText(o.detail);
}

// ---- policy editing ----

/** What the add-rule form holds: comma/newline separated lists as typed. */
export interface RuleDraft {
  id: string;
  description: string;
  targets: string;
  verbs: string;
  labels: string;
  risks: RiskClass[];
  effect: PolicyEffect;
}

export function emptyDraft(existing: readonly PolicyRule[]): RuleDraft {
  return { id: nextRuleId(existing), description: "", targets: "", verbs: "", labels: "", risks: [], effect: "confirm" };
}

/** `user-N` with the smallest N not taken. */
export function nextRuleId(existing: readonly PolicyRule[]): string {
  const taken = new Set(existing.map((r) => r.id));
  let n = 1;
  while (taken.has(`user-${n}`)) n++;
  return `user-${n}`;
}

/** Split a comma/newline separated list, trimmed, without empties or duplicates. */
export function splitGlobs(text: string): string[] {
  const out: string[] = [];
  for (const part of text.split(/[,\n]/)) {
    const s = part.trim();
    if (s && !out.includes(s)) out.push(s);
  }
  return out;
}

export type RuleErrors = Partial<Record<"id" | "conditions", string>>;

/** Validate a draft against the existing user rules; a rule or the errors. */
export function ruleFromDraft(draft: RuleDraft, existing: readonly PolicyRule[]): { rule: PolicyRule } | { errors: RuleErrors } {
  const errors: RuleErrors = {};
  const id = draft.id.trim();
  if (!id) errors.id = t("safety.rule.err.idRequired");
  else if (id.startsWith("default:")) errors.id = t("safety.rule.err.idReserved");
  else if (existing.some((r) => r.id === id)) errors.id = t("safety.rule.err.idTaken");
  const targets = splitGlobs(draft.targets);
  const verbs = splitGlobs(draft.verbs);
  const labels = splitGlobs(draft.labels);
  const risks = RISK_CLASSES.filter((r) => draft.risks.includes(r));
  // A rule with no condition matches every action; refuse it rather than allow or deny everything.
  if (!targets.length && !verbs.length && !labels.length && !risks.length) errors.conditions = t("safety.rule.err.noConditions");
  if (Object.keys(errors).length) return { errors };
  const rule: PolicyRule = { id, effect: draft.effect };
  const description = draft.description.trim();
  if (description) rule.description = description;
  if (targets.length) rule.targets = targets;
  if (verbs.length) rule.verbs = verbs;
  if (risks.length) rule.risks = risks;
  if (labels.length) rule.labels = labels;
  return { rule };
}

export function addRule(config: PolicyConfig, rule: PolicyRule): PolicyConfig {
  return { ...config, rules: [...(config.rules ?? []).filter((r) => r.id !== rule.id), rule] };
}

export function removeRule(config: PolicyConfig, id: string): PolicyConfig {
  return { ...config, rules: (config.rules ?? []).filter((r) => r.id !== id) };
}

export function setDefaultEnabled(config: PolicyConfig, id: string, enabled: boolean): PolicyConfig {
  const rest = (config.disabled_defaults ?? []).filter((d) => d !== id);
  return { ...config, disabled_defaults: enabled ? rest : [...rest, id] };
}

/** `null` (or a non-positive/invalid number) removes the limit: unlimited. */
export function setRateLimit(config: PolicyConfig, risk: RiskClass, limit: number | null): PolicyConfig {
  const { [risk]: _, ...rest } = config.rate_limits ?? {};
  if (limit === null || !Number.isFinite(limit) || limit < 1) return { ...config, rate_limits: rest };
  return { ...config, rate_limits: { ...rest, [risk]: Math.floor(limit) } };
}

/** Group key for rules with no target: they apply to every app and site. */
export const ANY_TARGET = "*";

/**
 * Rules per app and site: each target glob with the rules naming it, sorted by target; rules
 * without targets under `ANY_TARGET`, first. A rule with several targets appears under each.
 */
export function rulesByTarget(rules: readonly PolicyRule[]): [string, PolicyRule[]][] {
  const groups = new Map<string, PolicyRule[]>();
  for (const r of rules) {
    const targets = r.targets?.length ? r.targets : [ANY_TARGET];
    for (const target of targets) {
      const list = groups.get(target) ?? [];
      list.push(r);
      groups.set(target, list);
    }
  }
  return [...groups.entries()].sort(([a], [b]) => (a === ANY_TARGET ? -1 : b === ANY_TARGET ? 1 : a.localeCompare(b)));
}

/** File name for an exported action log. */
export function exportFileName(nowMs: number): string {
  const d = new Date(nowMs);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `nugget-action-log-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}.jsonl`;
}
