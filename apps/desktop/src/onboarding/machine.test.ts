import { describe, expect, it } from "vitest";
import {
  INITIAL,
  loadState,
  parseState,
  pendingSteps,
  reduce,
  restartOnboarding,
  saveState,
  shouldShowOnboarding,
  stepGoalMet,
  STEPS,
  STORAGE_KEY,
  type Action,
  type OnboardingState,
} from "./machine";
import { defaultTrigger, modifiersFor, parseTrigger } from "./trigger";
import { detectOs } from "./platform";

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

  it("skips a step, recording it, and completing it later clears the skip", () => {
    let s = run({ type: "next" }, { type: "skip" });
    expect(s.step).toBe("source");
    expect(s.skipped).toEqual(["ai"]);
    expect(pendingSteps(s)).toEqual(["ai", "source", "permissions", "trigger"]);
    s = reduce(s, { type: "goto", step: "ai" });
    s = reduce(s, { type: "chooseAi", ai: "local" });
    s = reduce(s, { type: "next" });
    expect(s.skipped).toEqual([]);
    expect(s.completed).toContain("ai");
    expect(s.ai).toBe("local");
  });

  it("skipping a completed step doesn't mark it skipped", () => {
    const s = run({ type: "next" }, { type: "back" }, { type: "skip" });
    expect(s.step).toBe("ai");
    expect(s.skipped).toEqual([]);
    expect(s.completed).toEqual(["welcome"]);
  });

  it("goes back but not before the first step", () => {
    expect(run({ type: "back" }).step).toBe("welcome");
    expect(run({ type: "next" }, { type: "next" }, { type: "back" }).step).toBe("ai");
  });

  it("skip all finishes and marks the rest skipped", () => {
    const s = run({ type: "next" }, { type: "skipAll" });
    expect(s.finished).toBe(true);
    expect(s.skipped).toEqual(["ai", "source", "permissions", "trigger"]);
  });

  it("skipping the last step finishes", () => {
    const s = reduce({ ...INITIAL, step: "done" }, { type: "skip" });
    expect(s.finished).toBe(true);
  });

  it("Continue marks a step done only when it set something up, else skips it", () => {
    let s = run({ type: "continue" }, { type: "chooseAi", ai: "later" }, { type: "continue" });
    expect(s.step).toBe("source");
    expect(s.skipped).toEqual(["ai"]);
    s = reduce(s, { type: "continue" });
    expect(s.skipped).toEqual(["ai", "source"]);
    expect(s.completed).toEqual(["welcome"]);

    s = run({ type: "next" }, { type: "chooseAi", ai: "local" }, { type: "aiSetUp", ai: "local", what: "qwen3-4b" }, { type: "continue" });
    expect(s.completed).toEqual(["welcome", "ai"]);
    // A setup on another path doesn't count for the current choice.
    expect(stepGoalMet({ ...s, ai: "byok" }, "ai")).toBe(false);
    s = reduce(s, { type: "sourcesFound", names: ["Notes"] });
    s = reduce(s, { type: "continue" });
    expect(s.completed).toEqual(["welcome", "ai", "source"]);
    expect(s.sources).toEqual(["Notes"]);
  });

  it("restart keeps the AI choice", () => {
    const s = run({ type: "chooseAi", ai: "byok" }, { type: "skipAll" }, { type: "restart" });
    expect(s).toEqual({ ...INITIAL, ai: "byok" });
  });
});

describe("onboarding persistence", () => {
  it("round-trips through storage and resumes", () => {
    const store = memoryStorage();
    expect(shouldShowOnboarding(store)).toBe(true);
    const s = run({ type: "next" }, { type: "chooseAi", ai: "later" }, { type: "next" });
    saveState(s, store);
    expect(loadState(store)).toEqual(s);
    expect(shouldShowOnboarding(store)).toBe(true);
    saveState(reduce(s, { type: "skipAll" }), store);
    expect(shouldShowOnboarding(store)).toBe(false);
    expect(restartOnboarding(store).step).toBe("welcome");
    expect(shouldShowOnboarding(store)).toBe(true);
  });

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
  });

  it("survives storage that throws", () => {
    const broken = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("full");
      },
    };
    expect(loadState(broken)).toEqual(INITIAL);
    expect(() => saveState(INITIAL, broken)).not.toThrow();
  });
});

describe("Double Tap trigger preference", () => {
  it("defaults to a key every keyboard on that OS has", () => {
    expect(defaultTrigger("macos")).toMatchObject({ kind: "double_tap", modifier: "right_command" });
    expect(defaultTrigger("windows")).toMatchObject({ kind: "double_tap", modifier: "right_control" });
    expect(modifiersFor("linux").some((m) => m.includes("command"))).toBe(false);
  });

  it("parses saved preferences defensively", () => {
    expect(parseTrigger(JSON.stringify({ kind: "chord", accelerator: " Alt+Space " }))).toEqual({
      kind: "chord",
      accelerator: "Alt+Space",
    });
    expect(parseTrigger(JSON.stringify({ kind: "double_tap", modifier: "left_shift", window_ms: 5 }))).toEqual({
      kind: "double_tap",
      modifier: "left_shift",
      window_ms: 350,
    });
    expect(parseTrigger(JSON.stringify({ kind: "double_tap", modifier: "fn" }))).toBeNull();
    expect(parseTrigger("nope")).toBeNull();
  });

  it("detects the OS from the user agent", () => {
    expect(detectOs("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15")).toBe("macos");
    expect(detectOs("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/130")).toBe("windows");
    expect(detectOs("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15")).toBe("linux");
  });
});
