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
  /**
   * What the AI step actually set up, and on which path: a local model installed (or installing),
   * or a provider whose key was saved. Absent until something is set up.
   */
  aiSetup?: { ai: AiChoice; what: string };
  /** Names of the sources there are (added in setup, or already there). Absent: none known. */
  sources?: string[];
  /** The flow is over (finished or skipped as a whole); don't show it on launch. */
  finished: boolean;
}

export type Action =
  | { type: "next" }
  /** The footer's Continue: `next` when the step did what it is for, else `skip`. */
  | { type: "continue" }
  | { type: "aiSetUp"; ai: AiChoice; what: string }
  | { type: "sourcesFound"; names: string[] }
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

/**
 * Whether a step's goal is met, so Continue can mark it done. The AI step needs a model installed
 * (or installing) or a key saved on the chosen path; the source step needs a source. The other
 * steps are done once seen.
 */
export function stepGoalMet(state: OnboardingState, step: StepId = state.step): boolean {
  if (step === "ai") return (state.ai === "local" || state.ai === "byok") && (state.aiSetup?.ai === state.ai || legacyDone(state, "ai"));
  if (step === "source") return (state.sources?.length ?? 0) > 0 || legacyDone(state, "source");
  return true;
}

/**
 * Progress saved before setups were recorded (no `aiSetup` / `sources`): a step completed then
 * was done, so it still counts.
 */
export function legacyDone(state: OnboardingState, step: "ai" | "source"): boolean {
  const recorded = step === "ai" ? state.aiSetup !== undefined : state.sources !== undefined;
  return !recorded && state.completed.includes(step);
}

export function reduce(state: OnboardingState, action: Action): OnboardingState {
  switch (action.type) {
    case "continue":
      return reduce(state, { type: stepGoalMet(state) ? "next" : "skip" });
    case "aiSetUp":
      return state.aiSetup?.ai === action.ai && state.aiSetup.what === action.what
        ? state
        : { ...state, aiSetup: { ai: action.ai, what: action.what } };
    case "sourcesFound": {
      const names = [...new Set([...(state.sources ?? []), ...action.names])];
      return names.length === (state.sources?.length ?? 0) ? state : { ...state, sources: names };
    }
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
  const isAi = (x: unknown): x is AiChoice => x === "local" || x === "byok" || x === "later";
  const ai = isAi(o.ai) ? o.ai : null;
  const out: OnboardingState = {
    version: 1,
    step: o.step,
    completed: steps(o.completed),
    skipped: steps(o.skipped),
    ai,
    finished: o.finished === true,
  };
  const setup = o.aiSetup as Record<string, unknown> | null | undefined;
  if (setup && typeof setup === "object" && isAi(setup.ai) && typeof setup.what === "string") {
    out.aiSetup = { ai: setup.ai, what: setup.what };
  }
  if (Array.isArray(o.sources)) {
    const names = o.sources.filter((x): x is string => typeof x === "string");
    if (names.length) out.sources = names;
  }
  return out;
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
