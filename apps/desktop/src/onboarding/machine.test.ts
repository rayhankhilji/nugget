import { describe, expect, it } from "vitest";
import {
  INITIAL,
  loadState,
  parseState,
  pendingSteps,
  reduce,
  stepGoalMet,
  STEPS,
  STORAGE_KEY,
  type Action,
  type OnboardingState,
} from "./machine";

const run = (...actions: Action[]): OnboardingState => actions.reduce(reduce, INITIAL);

function memoryStorage(init: Record<string, string> = {}) {
  const data = { ...init };
  return {
    data,
    getItem: (k: string) => (k in data ? data[k]! : null),
    setItem: (k: string, v: string) => {
      data[k] = v;
    },
  };
}

describe("onboarding state machine", () => {
  it("walks the steps in order", () => {
    let s = INITIAL;
    const seen = [s.step];
    for (let i = 0; i < STEPS.length - 1; i++) {
      s = reduce(s, { type: "next" });
      seen.push(s.step);
    }
    expect(seen).toEqual([...STEPS]);
    expect(s.completed).toEqual(STEPS.slice(0, -1));
    expect(s.finished).toBe(false);
    s = reduce(s, { type: "next" });
    expect(s.finished).toBe(true);
  });

  it("reads as sign in, connect apps, then the setup steps and the first answer", () => {
    expect([...STEPS]).toEqual(["welcome", "source", "ai", "permissions", "trigger", "done"]);
  });

  it("skips a step, recording it, and completing it later clears the skip", () => {
    let s = run({ type: "next" }, { type: "skip" });
    expect(s.step).toBe("ai");
    expect(s.skipped).toEqual(["source"]);
    expect(pendingSteps(s)).toEqual(["source", "ai", "permissions", "trigger"]);
    s = reduce(s, { type: "chooseAi", ai: "local" });
    s = reduce(s, { type: "goto", step: "source" });
    s = reduce(s, { type: "next" });
    expect(s.skipped).toEqual([]);
    expect(s.completed).toContain("source");
    expect(s.step).toBe("ai");
    expect(s.ai).toBe("local");
  });

  it("Continue marks a step done only when it set something up, else skips it", () => {
    let s = run({ type: "continue" }, { type: "continue" });
    expect(s.step).toBe("ai");
    expect(s.skipped).toEqual(["source"]);
    s = reduce(s, { type: "continue" });
    expect(s.skipped).toEqual(["source", "ai"]);
    expect(s.completed).toEqual(["welcome"]);

    s = run({ type: "next" }, { type: "sourcesFound", names: ["Notes"] });
    // GG6-2: a source with nothing indexed yet doesn't finish the step.
    expect(stepGoalMet(s, "source")).toBe(false);
    s = reduce(s, { type: "sourcesIndexed" });
    s = reduce(s, { type: "continue" });
    expect(s.completed).toEqual(["welcome", "source"]);
    expect(s.sources).toEqual(["Notes"]);
    expect(s.indexed).toBe(true);

    s = run({ type: "next" }, { type: "skip" }, { type: "chooseAi", ai: "local" }, { type: "aiSetUp", ai: "local", what: "qwen3-4b" }, { type: "continue" });
    expect(s.completed).toEqual(["welcome", "ai"]);
    expect(s.step).toBe("permissions");
    // A setup on another path doesn't count for the current choice.
    expect(stepGoalMet({ ...s, ai: "byok" }, "ai")).toBe(false);
  });

  it("a connected cloud app finishes the connect step", () => {
    let s = run({ type: "next" });
    expect(stepGoalMet(s)).toBe(false);
    s = reduce(s, { type: "appsConnected", names: ["Gmail"] });
    s = reduce(s, {
      type: "appsConnected",
      names: ["Gmail", "Google Calendar"],
    });
    expect(s.cloudApps).toEqual(["Gmail", "Google Calendar"]);
    // The same apps again change nothing (no re-render loop).
    expect(reduce(s, { type: "appsConnected", names: ["Gmail"] })).toBe(s);
    s = reduce(s, { type: "continue" });
    expect(s.completed).toEqual(["welcome", "source"]);
    expect(s.step).toBe("ai");
  });

  it("GG6-21: Decide later is a decision, not a skipped step", () => {
    const s = run({ type: "next" }, { type: "skip" }, { type: "chooseAi", ai: "later" }, { type: "continue" });
    expect(s.step).toBe("permissions");
    expect(s.skipped).toEqual(["source"]);
    expect(s.completed).toEqual(["welcome", "ai"]);
    expect(pendingSteps(s)).not.toContain("ai");
  });

  it("GG6-2: Continue with a source that indexed nothing counts as continuing without one", () => {
    let s = run({ type: "next" }, { type: "sourcesFound", names: ["Empty"] });
    s = reduce(s, { type: "continue" });
    expect(s.skipped).toEqual(["source"]);
    expect(pendingSteps(s)).toContain("source");
  });
});

describe("onboarding persistence", () => {
  it("ignores corrupt or foreign saves", () => {
    expect(parseState("{")).toBeNull();
    expect(parseState(JSON.stringify({ version: 2, step: "ai" }))).toBeNull();
    expect(parseState(JSON.stringify({ version: 1, step: "nope" }))).toBeNull();
    expect(parseState(JSON.stringify({ version: 1, step: "ai", completed: ["welcome", "x"], ai: "cloud" }))).toEqual({
      ...INITIAL,
      step: "ai",
      completed: ["welcome"],
    });
    expect(loadState(memoryStorage({ [STORAGE_KEY]: "garbage" }))).toEqual(INITIAL);
    expect(
      parseState(JSON.stringify({ version: 1, step: "done", aiSetup: { ai: "byok", what: "anthropic" }, sources: ["Notes", 3] })),
    ).toEqual({ ...INITIAL, step: "done", aiSetup: { ai: "byok", what: "anthropic" }, sources: ["Notes"] });
    expect(parseState(JSON.stringify({ version: 1, step: "done", aiSetup: { ai: "cloud", what: 1 } }))).toEqual({ ...INITIAL, step: "done" });
    expect(parseState(JSON.stringify({ version: 1, step: "source", cloudApps: ["Gmail", 2] }))).toEqual({ ...INITIAL, step: "source", cloudApps: ["Gmail"] });
  });
});
