import { describe, expect, it } from "vitest";
import type { ActionDescriptor, AgentEvent, PolicyConfig, PolicyRule } from "./api";
import {
  addRule,
  emptyRun,
  isLive,
  nextRuleId,
  outcomeDetail,
  outcomeTone,
  reduceRun,
  removeRule,
  secondsLeft,
  setDefaultEnabled,
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
});
