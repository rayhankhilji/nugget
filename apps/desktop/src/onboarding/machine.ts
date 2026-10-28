/**
 * First-run onboarding as a pure state machine, persisted after every change so quitting
 * mid-way resumes where the user left off. Every step can be skipped, and so can the whole flow.
 */

export const STEPS = ["welcome", "ai", "source", "permissions", "trigger", "done"] as const;
export type StepId = (typeof STEPS)[number];

/** How the user wants Nugget to think: on this device, with their own API key, or not decided. */
export type AiChoice = "local" | "byok" | "later";

export interface OnboardingState {
  version: 1;
  step: StepId;
  /** Steps the user finished (pressed Continue on). */
  completed: StepId[];
  /** Steps the user skipped. A step later completed is removed from here. */
  skipped: StepId[];
  ai: AiChoice | null;
  /** The flow is over (finished or skipped as a whole); don't show it on launch. */
  finished: boolean;
}

export type Action =
  | { type: "next" }
  | { type: "back" }
  | { type: "skip" }
  | { type: "goto"; step: StepId }
  | { type: "chooseAi"; ai: AiChoice }
  | { type: "skipAll" }
  | { type: "finish" }
  | { type: "restart" };

export const INITIAL: OnboardingState = {
  version: 1,
  step: "welcome",
  completed: [],
  skipped: [],
  ai: null,
  finished: false,
};

export function stepIndex(step: StepId): number {
  return STEPS.indexOf(step);
}

function without(list: StepId[], step: StepId): StepId[] {
  return list.filter((s) => s !== step);
}

function withStep(list: StepId[], step: StepId): StepId[] {
  return list.includes(step) ? list : [...list, step];
}

function advance(step: StepId): StepId {
  return STEPS[Math.min(stepIndex(step) + 1, STEPS.length - 1)]!;
}

export function reduce(state: OnboardingState, action: Action): OnboardingState {
  switch (action.type) {
    case "next": {
      if (state.step === "done") return { ...state, finished: true, completed: withStep(state.completed, "done") };
      return {
        ...state,
        completed: withStep(state.completed, state.step),
        skipped: without(state.skipped, state.step),
        step: advance(state.step),
      };
    }
    case "back": {
      const i = stepIndex(state.step);
      return i > 0 ? { ...state, step: STEPS[i - 1]! } : state;
    }
    case "skip": {
      if (state.step === "done") return { ...state, finished: true };
      return {
        ...state,
        skipped: state.completed.includes(state.step) ? state.skipped : withStep(state.skipped, state.step),
        step: advance(state.step),
      };
    }
    case "goto":
      return { ...state, step: action.step };
    case "chooseAi":
      return { ...state, ai: action.ai };
    case "skipAll": {
      // Everything not yet completed counts as skipped, so a later "set up again" knows what's left.
      const rest = STEPS.filter((s) => s !== "done" && !state.completed.includes(s));
      return { ...state, skipped: rest, finished: true };
    }
    case "finish":
      return { ...state, finished: true, completed: withStep(state.completed, "done"), step: "done" };
    case "restart":
      return { ...INITIAL, ai: state.ai };
  }
}

/** Steps left undone, for the summary on the last step. */
export function pendingSteps(state: OnboardingState): StepId[] {
  return STEPS.filter((s) => s !== "welcome" && s !== "done" && !state.completed.includes(s));
}

// ---- persistence ----

export const STORAGE_KEY = "nugget.onboarding.v1";

function isStep(v: unknown): v is StepId {
  return typeof v === "string" && (STEPS as readonly string[]).includes(v);
}

/** Parse saved state, ignoring anything malformed (a corrupt save restarts the flow, never crashes it). */
export function parseState(raw: string | null): OnboardingState | null {
  if (!raw) return null;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  if (o.version !== 1 || !isStep(o.step)) return null;
  const steps = (x: unknown) => (Array.isArray(x) ? x.filter(isStep) : []);
  const ai = o.ai === "local" || o.ai === "byok" || o.ai === "later" ? o.ai : null;
  return {
    version: 1,
    step: o.step,
    completed: steps(o.completed),
    skipped: steps(o.skipped),
    ai,
    finished: o.finished === true,
  };
}

export function loadState(storage: Pick<Storage, "getItem"> | null = safeStorage()): OnboardingState {
  try {
    return parseState(storage?.getItem(STORAGE_KEY) ?? null) ?? INITIAL;
  } catch {
    return INITIAL;
  }
}

export function saveState(state: OnboardingState, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    /* storage full or unavailable: the flow still works for this session */
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** Show onboarding on launch unless it was finished or skipped before. */
export function shouldShowOnboarding(storage: Pick<Storage, "getItem"> | null = safeStorage()): boolean {
  return !loadState(storage).finished;
}

/** Reset saved progress so the flow shows again from the start (keeps the AI choice). */
export function restartOnboarding(storage: Pick<Storage, "getItem" | "setItem"> | null = safeStorage()): OnboardingState {
  const next = reduce(loadState(storage), { type: "restart" });
  saveState(next, storage);
  return next;
}
