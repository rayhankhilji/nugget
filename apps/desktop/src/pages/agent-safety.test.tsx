// @vitest-environment jsdom
/** The Agent and Safety pages against a mocked daemon: rendering, events, dialogs, and axe. */
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ActionDescriptor, AgentEvent, AgentTaskSummary, PolicyApprovals, PolicyState } from "../api";
import { EVENT } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { AgentPage } from "./Agent";
import { SafetyPage } from "./Safety";

const action: ActionDescriptor = { target_app: "mail.example.com", verb: "send_email", target_label: "Send", text_preview: "Hello Sam", declared_risk: "send", task_id: "t1" };

const history: AgentTaskSummary[] = [
  {
    task_id: "t0",
    goal: "Archive old newsletters",
    state: "finished",
    outcome: "unverified",
    detail: "archive not checked",
    started_ms: Date.UTC(2026, 9, 27),
    finished_ms: Date.UTC(2026, 9, 27, 0, 1),
    steps: 4,
    verification: { passed: 1, failed: 0, unverifiable: 1 },
  },
];

const policy: PolicyState = {
  config: {
    version: 1,
    rules: [{ id: "user-1", description: "Ask before mail", targets: ["mail.example.com"], risks: ["send"], effect: "confirm" }],
    disabled_defaults: [],
    rate_limits: { send: 25, destroy: 25, spend: 3, security: 3 },
    confirm_timeout_secs: 120,
  },
  defaults: [{ rule: { id: "default:spend", description: "Always ask before spending", risks: ["spend"], effect: "confirm" }, enabled: true }],
  halted: false,
  log: { entries: 12, head: [11, "abc123"], chain_ok: true },
};

const approvals: PolicyApprovals = {
  pending: [{ request_id: "r1", action_id: "a1", tool: "mail.send", action, reason: "Sends an email to Sam", forced_by_injection: false, timeout_ms: 60_000 }],
  task: [{ task_id: "t1", risk: "send", target: "mail.example.com" }],
};

async function agentEvent(event: AgentEvent) {
  await act(async () => {
    await emit(EVENT.notification, { method: "agent.event", params: { task_id: event.task_id, event } });
  });
  await flush(2);
}

async function violations(): Promise<string[]> {
  const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("Agent page", () => {
  it("runs a task, shows the timeline, confirms, and shows the outcome", async () => {
    const confirms: unknown[] = [];
    const calls = mockDaemon({
      agent_list: history,
      policy_get: policy,
      agent_run: { task_id: "t1", tools: ["mail.send"] },
      agent_confirm: (args: unknown) => {
        confirms.push(args);
        return null;
      },
    });
    const { container } = await render(<AgentPage />);
    expect(container.textContent).toContain("Archive old newsletters");
    expect(container.textContent).toContain("Unverified");
    expect(await violations()).toEqual([]);

    const goal = container.querySelector("textarea")!;
    await act(async () => {
      const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
      set.call(goal, "Reply to Sam");
      goal.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(container, "button", "Run"));
    expect(calls).toContain("agent_run");
    expect(container.textContent).toContain("Reply to Sam");

    await agentEvent({ event: "step_started", task_id: "t1", step: 1 });
    await agentEvent({ event: "model_replied", task_id: "t1", step: 1, text: "Sending the reply.", tool_calls: ["mail.send"] });
    await agentEvent({ event: "action_proposed", task_id: "t1", step: 1, action_id: "a1", tool: "mail.send", action });
    await agentEvent({ event: "confirmation_needed", task_id: "t1", request: approvals.pending[0]! });

    const dialog = document.querySelector("dialog")!;
    expect(dialog).not.toBeNull();
    expect(dialog.textContent).toContain("Sends an email to Sam");
    expect(dialog.textContent).toContain("Hello Sam");
    expect(byText(dialog, "button", "Approve for this task")).toBeDefined();
    expect(document.activeElement?.textContent).toBe("Deny");
    expect(await violations()).toEqual([]);

    await click(byText(dialog, "button", "Approve once"));
    expect(confirms).toEqual([{ request: { task_id: "t1", request_id: "r1", choice: "once" } }]);

    await agentEvent({ event: "confirmation_resolved", task_id: "t1", request_id: "r1", answer: "approved" });
    expect(document.querySelector("dialog")).toBeNull();
    await agentEvent({ event: "action_done", task_id: "t1", action_id: "a1", tool: "mail.send", is_error: false, summary: "sent", replayed: false });
    await agentEvent({ event: "verification", task_id: "t1", action_id: "a1", tool: "mail.send", verification: { status: "passed", detail: "found in Sent" } });
    await agentEvent({ event: "finished", task_id: "t1", outcome: { outcome: "succeeded" }, summary: "Replied to Sam." });

    expect(container.textContent).toContain("Succeeded");
    expect(container.textContent).toContain("found in Sent");
    expect(container.textContent).toContain("Replied to Sam.");
    expect(await violations()).toEqual([]);
  });

  it("shows the halted banner and blocks new runs", async () => {
    mockDaemon({ agent_list: [], policy_get: { ...policy, halted: true }, agent_run: { task_id: "t2", tools: [] } });
    const { container } = await render(<AgentPage />);
    expect(container.textContent).toContain("All agents are stopped.");
    expect(byText(container, "button", "Resume agents")).toBeDefined();
    expect(byText(container, "button", "Run")?.hasAttribute("disabled")).toBe(true);
  });

  it("shows the explanation for a past task", async () => {
    mockDaemon({
      agent_list: history,
      policy_get: policy,
      agent_get: { summary: history[0], entries: [], explain: "Step 1: archived 3 messages", pending: [], question: null },
    });
    const { container } = await render(<AgentPage />);
    await click(container.querySelector('button[aria-label^="Details for"]'));
    await flush(3);
    expect(document.querySelector("dialog")?.textContent).toContain("Step 1: archived 3 messages");
    expect(await violations()).toEqual([]);
  });

  it("asks the user and sends the answer", async () => {
    const answers: unknown[] = [];
    mockDaemon({
      agent_list: [{ ...history[0]!, task_id: "t3", state: "needs_input", outcome: null }],
      policy_get: policy,
      agent_get: { summary: { ...history[0]!, task_id: "t3", state: "needs_input", outcome: null }, entries: [], explain: "", pending: [], question: "Which Sam?" },
      agent_answer: (a: unknown) => {
        answers.push(a);
        return null;
      },
    });
    const { container } = await render(<AgentPage />);
    await flush(5);
    expect(container.textContent).toContain("Which Sam?");
    const box = container.querySelector<HTMLTextAreaElement>(".agent-answer textarea")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(box, "Sam Lee");
      box.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(container, "button", "Send answer"));
    expect(answers).toEqual([{ request: { task_id: "t3", answer: "Sam Lee" } }]);
  });

  it("offers the candidates the agent saw and answers with the one picked", async () => {
    const answers: unknown[] = [];
    const options = ["Sam Lee — Can I take Friday off?", "<b>Sam Ortiz</b> — Renew the contract at +20%?"];
    const question = "Your request says “Sam”, and I see more than one: Sam Lee, Sam Ortiz. Which one did you mean?";
    mockDaemon({
      agent_list: [],
      policy_get: policy,
      agent_run: { task_id: "t4", tools: [] },
      agent_answer: (a: unknown) => {
        answers.push(a);
        return null;
      },
    });
    const { container } = await render(<AgentPage />);
    await flush(5);
    const goal = container.querySelector<HTMLTextAreaElement>("textarea")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(goal, "Reply to Sam");
      goal.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(container, "button", "Run"));
    await agentEvent({ event: "suspended", task_id: "t4", question, options });

    const group = container.querySelector<HTMLElement>('.agent-answer [role="group"]')!;
    expect(group).toBeTruthy();
    expect(document.getElementById(group.getAttribute("aria-labelledby")!)!.textContent).toBe(question);
    const buttons = Array.from(group.querySelectorAll("button"));
    expect(buttons.map((b) => b.textContent)).toEqual(options);
    // Untrusted screen text stays text.
    expect(group.querySelector("b")).toBeNull();
    expect(container.querySelector(".visually-hidden[aria-live]")!.textContent).toContain("2 options");
    // The free-text answer stays available.
    expect(container.querySelector(".agent-answer textarea")).toBeTruthy();
    expect(byText(container, "label", "Something else")).toBeTruthy();
    expect(await violations()).toEqual([]);

    await click(buttons[1]!);
    expect(answers).toEqual([{ request: { task_id: "t4", answer: options[1] } }]);
    expect(container.querySelector(".agent-answer")).toBeNull();
    expect(container.querySelector(".visually-hidden[aria-live]")!.textContent).toBe("Answer sent.");
  });
});

describe("Safety page", () => {
  function daemon(extra: Record<string, unknown> = {}) {
    return mockDaemon({
      policy_get: policy,
      policy_approvals: approvals,
      browser_status: { connected: true, origin: "chrome-extension://abc", host_version: "0.1.0", tools: ["browser.click"] },
      ...extra,
    });
  }

  it("renders rules, approvals, limits, log and browser without violations", async () => {
    daemon();
    const { container } = await render(<SafetyPage />);
    await flush(5);
    const text = container.textContent ?? "";
    for (const s of ["mail.example.com", "Ask before mail", "Always ask before spending", "Sends an email to Sam", "Intact", "browser.click", "Stop all agents"]) {
      expect(text).toContain(s);
    }
    expect(await violations()).toEqual([]);
  });

  it("adds a rule through the dialog and saves the config", async () => {
    const sets: unknown[] = [];
    daemon({
      policy_set: (args: unknown) => {
        sets.push(args);
        const config = (args as { request: { config: PolicyState["config"] } }).request.config;
        return { ...policy, config };
      },
    });
    const { container } = await render(<SafetyPage />);
    await flush(5);
    await click(byText(container, "button", "Add rule"));
    const dialog = document.querySelector("dialog")!;
    expect(await violations()).toEqual([]);
    // Nothing filled in: refused locally.
    await click(byText(dialog, "button", "Add rule"));
    expect(sets).toEqual([]);
    expect(dialog.textContent).toContain("A rule without any would match everything.");
    const targets = dialog.querySelector<HTMLInputElement>("[data-autofocus]")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(targets, "bank.example");
      targets.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(dialog, "label", "Deny")?.querySelector("input"));
    await click(byText(dialog, "button", "Add rule"));
    expect(sets).toHaveLength(1);
    const saved = (sets[0] as { request: { config: PolicyState["config"] } }).request.config;
    expect(saved.rules.at(-1)).toEqual({ id: "user-2", targets: ["bank.example"], effect: "deny" });
    expect(document.querySelector("dialog")).toBeNull();
    expect(container.textContent).toContain("bank.example");
  });

  it("toggles a built-in rule and engages the kill switch", async () => {
    const sets: unknown[] = [];
    const calls = daemon({
      policy_set: (args: unknown) => {
        sets.push(args);
        return policy;
      },
      policy_kill: { halted: true },
    });
    const { container } = await render(<SafetyPage />);
    await flush(5);
    await click(container.querySelector('input[role="switch"]'));
    expect((sets[0] as { request: { config: PolicyState["config"] } }).request.config.disabled_defaults).toEqual(["default:spend"]);
    await click(byText(container, "button", "Stop all agents"));
    expect(calls).toContain("policy_kill");
    expect(byText(container, "button", "Resume")).toBeDefined();
  });
});
