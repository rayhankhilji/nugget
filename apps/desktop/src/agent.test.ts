import { describe, expect, it } from "vitest";
import type { ActionDescriptor, AgentEvent, PolicyConfig, PolicyRule } from "./api";
import {
  addRule,
  ANY_TARGET,
  answered,
  canApproveForTask,
  emptyDraft,
  emptyRun,
  exportFileName,
  isLive,
  nextRuleId,
  outcomeDetail,
  outcomeTone,
  reduceRun,
  reduceRuns,
  removeRule,
  ruleFromDraft,
  rulesByTarget,
  runFromDetail,
  secondsLeft,
  setDefaultEnabled,
  setRateLimit,
  splitGlobs,
  type ActionItem,
  type RunView,
} from "./agent";

const T = "task-1";
const action: ActionDescriptor = { target_app: "mail.example.com", verb: "send_email", target_label: "Send", text_preview: "Hi", declared_risk: "send", task_id: T };

function play(events: AgentEvent[], start: RunView = emptyRun(T)): RunView {
  return events.reduce((run, event, i) => reduceRun(run, { task_id: T, event }, 1000 * i), start);
}

const actions = (run: RunView) => run.items.filter((i): i is ActionItem => i.kind === "action");

describe("timeline reducer", () => {
  it("follows a confirm → approve → verified finish", () => {
    const run = play([
      { event: "run_started", task_id: T, goal: "Reply to Sam" },
      { event: "step_started", task_id: T, step: 1 },
      { event: "model_replied", task_id: T, step: 1, text: "I'll send it.", tool_calls: ["mail.send"] },
      { event: "action_proposed", task_id: T, step: 1, action_id: "a1", tool: "mail.send", action },
      {
        event: "confirmation_needed",
        task_id: T,
        request: { request_id: "r1", action_id: "a1", tool: "mail.send", action, reason: "Sends an email", forced_by_injection: false, timeout_ms: 60_000 },
      },
    ]);
    expect(run.goal).toBe("Reply to Sam");
    expect(run.pending).toHaveLength(1);
    expect(actions(run)[0]!.status).toBe("awaiting");
    expect(secondsLeft(run.pending[0]!, run.pending[0]!.receivedMs + 1500)).toBe(59);
    expect(secondsLeft(run.pending[0]!, run.pending[0]!.receivedMs + 120_000)).toBe(0);

    const done = play(
      [
        { event: "confirmation_resolved", task_id: T, request_id: "r1", answer: "approved" },
        { event: "action_done", task_id: T, action_id: "a1", tool: "mail.send", is_error: false, summary: "sent", replayed: false },
        { event: "verification", task_id: T, action_id: "a1", tool: "mail.send", verification: { status: "passed", detail: "in Sent" } },
        { event: "finished", task_id: T, outcome: { outcome: "succeeded" }, summary: "Replied." },
      ],
      run,
    );
    expect(done.pending).toEqual([]);
    const a = actions(done)[0]!;
    expect(a).toMatchObject({ status: "done", answer: "approved", text: "sent", step: 1 });
    expect(a.verification).toEqual({ status: "passed", detail: "in Sent" });
    expect(actions(done)).toHaveLength(1);
    expect(done.verification).toEqual({ passed: 1, failed: 0, unverifiable: 0 });
    expect(done.outcome?.outcome).toBe("succeeded");
    expect(done.summary).toBe("Replied.");
    expect(isLive(done)).toBe(false);
    expect(done.items.at(-1)?.kind).toBe("finished");
  });

  it("records denials, injections and unverified outcomes", () => {
    const run = play([
      { event: "action_denied", task_id: T, action_id: "a2", tool: "files.delete", reason: "rejected by user" },
      { event: "injection_suspected", task_id: T, action_id: "a3", tool: "web.read", finding: { patterns: ["override_instructions"], excerpt: "ignore previous" } },
      { event: "verification", task_id: T, action_id: "a4", tool: "x", verification: { status: "unverifiable" } },
      { event: "finished", task_id: T, outcome: { outcome: "unverified", detail: "a4" }, summary: null },
    ]);
    const [denied, injected] = actions(run);
    expect(denied).toMatchObject({ status: "denied", text: "rejected by user", tool: "files.delete" });
    expect(injected!.injection?.patterns).toEqual(["override_instructions"]);
    expect(run.injections).toBe(1);
    expect(run.verification.unverifiable).toBe(1);
    expect(outcomeTone(run.outcome!.outcome)).toBe("warn");
    expect(outcomeDetail(run.outcome!)).toBe("a4");
  });

  it("marks unanswered confirmations as not done when the run ends", () => {
    const run = play([
      {
        event: "confirmation_needed",
        task_id: T,
        request: { request_id: "r9", action_id: "a9", tool: "pay", action: { ...action, declared_risk: "spend" }, reason: "", forced_by_injection: true, timeout_ms: 1000 },
      },
      { event: "finished", task_id: T, outcome: { outcome: "halted" }, summary: null },
    ]);
    expect(actions(run)[0]!.status).toBe("denied");
    expect(run.pending).toEqual([]);
    expect(outcomeTone("halted")).toBe("bad");
  });

  it("keeps a suspended run live with its question", () => {
    const run = play([
      { event: "suspended", task_id: T, question: "Which Sam?" },
      { event: "finished", task_id: T, outcome: { outcome: "needs_input", detail: "Which Sam?" }, summary: null },
    ]);
    expect(run.question).toBe("Which Sam?");
    expect(run.items.filter((i) => i.kind === "question")).toHaveLength(1);
    const suspended = play([{ event: "suspended", task_id: T, question: "Which Sam?" }]);
    expect(suspended.question).toBe("Which Sam?");
    expect(isLive(suspended)).toBe(true);
    expect(isLive(run)).toBe(true);
  });

  it("keeps the question's options until it is answered or the run ends", () => {
    const options = ["Sam Lee — Can I take Friday off?", "Sam Ortiz — Renew the contract at +20%?"];
    const run = play([
      { event: "suspended", task_id: T, question: "Which Sam?", options },
      { event: "finished", task_id: T, outcome: { outcome: "needs_input", detail: "Which Sam?" }, summary: null },
    ]);
    expect(run.options).toEqual(options);
    expect(run.items.find((i) => i.kind === "question")).toEqual({ kind: "question", question: "Which Sam?", options });
    const after = answered(run);
    expect(after.question).toBeNull();
    expect(after.options).toEqual([]);
    expect(after.outcome).toBeNull();
    // Without options the question is free text.
    expect(play([{ event: "suspended", task_id: T, question: "Which Sam?" }]).options).toEqual([]);
    // A run that ends (cancelled while waiting) drops the question and its options.
    const ended = reduceRun(run, { task_id: T, event: { event: "finished", task_id: T, outcome: { outcome: "cancelled" }, summary: null } }, 0);
    expect(ended.question).toBeNull();
    expect(ended.options).toEqual([]);
  });

  it("formats budget outcomes and ignores unknown events", () => {
    expect(outcomeDetail({ outcome: "budget_exhausted", detail: { budget: "steps", limit: 30 } })).toContain("30");
    const run = emptyRun(T);
    expect(reduceRun(run, { task_id: T, event: { event: "something_new" } as unknown as AgentEvent }, 0)).toBe(run);
  });

  it("keys runs by task and starts unseen ones", () => {
    const runs = reduceRuns({}, { task_id: "b", event: { event: "step_started", task_id: "b", step: 1 } }, 0);
    expect(Object.keys(runs)).toEqual(["b"]);
    expect(runs.b!.items).toEqual([{ kind: "step", step: 1 }]);
  });

  it("seeds a run from task detail", () => {
    const run = runFromDetail(
      {
        summary: { task_id: T, goal: "g", state: "needs_input", started_ms: 0, steps: 2, verification: { passed: 1, failed: 0, unverifiable: 0 } },
        entries: [],
        explain: "",
        pending: [],
        question: "Which?",
        options: ["Sam Lee", "", "Sam Ortiz", "Sam Lee"],
      },
      0,
    );
    expect(run.question).toBe("Which?");
    expect(run.options).toEqual(["Sam Lee", "Sam Ortiz"]);
    expect(run.verification.passed).toBe(1);
  });

  it("only offers task approval for send and destroy", () => {
    expect(canApproveForTask("send")).toBe(true);
    expect(canApproveForTask("destroy")).toBe(true);
    expect(canApproveForTask("spend")).toBe(false);
    expect(canApproveForTask("security")).toBe(false);
  });
});

describe("rule editing", () => {
  const base: PolicyConfig = {
    version: 1,
    rules: [{ id: "user-1", targets: ["mail.example.com"], effect: "confirm" }],
    disabled_defaults: [],
    rate_limits: { send: 25, spend: 3 },
    confirm_timeout_secs: 120,
    future_field: true,
  };

  it("splits glob lists", () => {
    expect(splitGlobs(" a.com, b.com\nc.com,,a.com ")).toEqual(["a.com", "b.com", "c.com"]);
  });

  it("validates drafts", () => {
    const draft = emptyDraft(base.rules);
    expect(draft.id).toBe("user-2");
    expect(ruleFromDraft(draft, base.rules)).toHaveProperty("errors.conditions");
    expect(ruleFromDraft({ ...draft, id: "user-1", targets: "x" }, base.rules)).toHaveProperty("errors.id");
    expect(ruleFromDraft({ ...draft, id: "default:send", targets: "x" }, base.rules)).toHaveProperty("errors.id");
    expect(ruleFromDraft({ ...draft, id: " ", targets: "x" }, base.rules)).toHaveProperty("errors.id");
    const ok = ruleFromDraft({ ...draft, targets: "bank.example, *.bank.example", risks: ["spend", "send"], effect: "deny", description: " no banks " }, base.rules);
    expect(ok).toEqual({
      rule: { id: "user-2", description: "no banks", targets: ["bank.example", "*.bank.example"], risks: ["send", "spend"], effect: "deny" },
    });
  });

  it("adds, removes and toggles while keeping unknown fields", () => {
    const rule: PolicyRule = { id: "user-2", verbs: ["click"], effect: "allow" };
    const added = addRule(base, rule);
    expect(added.rules.map((r) => r.id)).toEqual(["user-1", "user-2"]);
    expect(added.future_field).toBe(true);
    expect(removeRule(added, "user-1").rules.map((r) => r.id)).toEqual(["user-2"]);
    const off = setDefaultEnabled(base, "default:x", false);
    expect(off.disabled_defaults).toEqual(["default:x"]);
    expect(setDefaultEnabled(off, "default:x", false).disabled_defaults).toEqual(["default:x"]);
    expect(setDefaultEnabled(off, "default:x", true).disabled_defaults).toEqual([]);
    expect(nextRuleId([{ id: "user-1", effect: "allow" }, { id: "user-3", effect: "allow" }])).toBe("user-2");
  });

  it("sets and clears rate limits", () => {
    expect(setRateLimit(base, "destroy", 10).rate_limits).toEqual({ send: 25, spend: 3, destroy: 10 });
    expect(setRateLimit(base, "send", null).rate_limits).toEqual({ spend: 3 });
    expect(setRateLimit(base, "send", 0).rate_limits).toEqual({ spend: 3 });
    expect(setRateLimit(base, "send", Number.NaN).rate_limits).toEqual({ spend: 3 });
    expect(setRateLimit(base, "send", 4.7).rate_limits.send).toBe(4);
  });

  it("groups rules per app and site", () => {
    const groups = rulesByTarget([
      { id: "a", targets: ["z.com", "a.com"], effect: "deny" },
      { id: "b", effect: "confirm", risks: ["send"] },
      { id: "c", targets: ["a.com"], effect: "allow" },
    ]);
    expect(groups.map(([t, rs]) => [t, rs.map((r) => r.id)])).toEqual([
      [ANY_TARGET, ["b"]],
      ["a.com", ["a", "c"]],
      ["z.com", ["a"]],
    ]);
  });

  it("names exported logs", () => {
    expect(exportFileName(new Date(2026, 9, 28, 9, 5).getTime())).toBe("nugget-action-log-20261028-0905.jsonl");
  });
});
