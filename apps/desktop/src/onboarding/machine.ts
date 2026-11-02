/**
 * First-run onboarding as a pure state machine, persisted after every change so quitting
 * mid-way resumes where the user left off. Every step can be skipped, and so can the whole flow.
 *
 * The first step is the fork: Heed Cloud or run it yourself (`path`). On the cloud path the flow
 * reads sign in → connect apps → AI → …; running it yourself goes to the AI choice first, then a
 * local source. `done` asks the first question. The ids are kept from the earlier order so saved
 * progress stays valid.
 */

export const STEPS = ["welcome", "source", "ai", "permissions", "trigger", "done"] as const;
export type StepId = (typeof STEPS)[number];

/** Running it yourself: the AI choice comes straight after the fork, then a local source. */
export const LOCAL_STEPS: readonly StepId[] = ["welcome", "ai", "source", "permissions", "trigger", "done"];

/** The first-run fork: Heed Cloud (sign in) or run it yourself (open source, your own AI). */
export type RunPath = "cloud" | "local";

/**
 * How the user wants Heed to think: on this device, with their own API key (or server), with
 * their ChatGPT plan, or not decided.
 */
export type AiChoice = "local" | "byok" | "chatgpt" | "later";

/** One row of "Choose your AI": every way in, each its own option. */
export const AI_OPTIONS = ["chatgpt", "local", "ollama", "anthropic", "openai", "openrouter", "compatible", "later"] as const;
export type AiOption = (typeof AI_OPTIONS)[number];

/** The AI path a row sets up. */
export function aiChoiceOf(option: AiOption): AiChoice {
  return option === "chatgpt" || option === "local" || option === "later" ? option : "byok";
}

/** The row to show for a choice saved before rows were recorded. */
export function defaultOption(ai: AiChoice | null): AiOption | null {
  if (ai === null) return null;
  return ai === "byok" ? "anthropic" : ai;
}

export interface OnboardingState {
  version: 1;
  step: StepId;
  /** Steps the user finished (pressed Continue on). */
  completed: StepId[];
  /** Steps the user skipped. A step later completed is removed from here. */
  skipped: StepId[];
  ai: AiChoice | null;
  /** The fork's answer. Absent until chosen (and on progress saved before the fork). */
  path?: RunPath;
  /** The "Choose your AI" row picked (which provider, for a key). Absent: follows `ai`. */
  aiOption?: AiOption;
  /**
   * What the AI step actually set up, and on which path: a local model installed (or installing),
   * or a provider whose key was saved. Absent until something is set up.
   */
  aiSetup?: { ai: AiChoice; what: string };
  /** Names of the sources there are (added in setup, or already there). Absent: none known. */
  sources?: string[];
  /**
   * A source has something searchable (at least one item indexed). Adding a folder that holds
   * nothing Heed can read does not finish the source step.
   */
  indexed?: boolean;
  /** Names of the cloud apps connected (Composio, through a Heed account). Absent: none known. */
  cloudApps?: string[];
  /** The flow is over (finished or skipped as a whole); don't show it on launch. */
  finished: boolean;
}

export type Action =
  | { type: "next" }
  /** The footer's Continue: `next` when the step did what it is for, else `skip`. */
  | { type: "continue" }
  | { type: "aiSetUp"; ai: AiChoice; what: string }
  | { type: "sourcesFound"; names: string[] }
  /** A source has at least one item indexed. */
  | { type: "sourcesIndexed" }
  /** Cloud apps with an active connection (their display names). */
  | { type: "appsConnected"; names: string[] }
  | { type: "back" }
  | { type: "skip" }
  | { type: "goto"; step: StepId }
  | { type: "chooseAi"; ai: AiChoice }
  /** A "Choose your AI" row: sets `ai` to its path. */
  | { type: "chooseAiOption"; option: AiOption }
  /**
   * The fork. `cloud` stays on the first step (the sign-in); `local` moves on to the AI choice;
   * `null` shows the fork again.
   */
  | { type: "choosePath"; path: RunPath | null }
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

/** The order of the steps on the path chosen at the fork. */
export function stepOrder(state?: Pick<OnboardingState, "path">): readonly StepId[] {
  return state?.path === "local" ? LOCAL_STEPS : STEPS;
}

export function stepIndex(step: StepId, state?: Pick<OnboardingState, "path">): number {
  return stepOrder(state).indexOf(step);
}

function without(list: StepId[], step: StepId): StepId[] {
  return list.filter((s) => s !== step);
}

function withStep(list: StepId[], step: StepId): StepId[] {
  return list.includes(step) ? list : [...list, step];
}

function advance(state: OnboardingState): StepId {
  const order = stepOrder(state);
  return order[Math.min(order.indexOf(state.step) + 1, order.length - 1)]!;
}

/**
 * Whether a step's goal is met, so Continue can mark it done. The AI step needs a model installed
 * (or installing) or a key saved on the chosen path, or the choice to decide later (a decision,
 * not a skipped step); the source step needs a source with something indexed or a cloud app
 * connected. The other steps
 * are done once seen.
 */
export function stepGoalMet(state: OnboardingState, step: StepId = state.step): boolean {
  if (step === "ai") {
    if (state.ai === "later") return true;
    return (state.ai === "local" || state.ai === "byok" || state.ai === "chatgpt") && (state.aiSetup?.ai === state.ai || legacyDone(state, "ai"));
  }
  if (step === "source") return state.indexed === true || (state.cloudApps?.length ?? 0) > 0 || legacyDone(state, "source");
  return true;
}

/**
 * Progress saved before setups were recorded (no `aiSetup` / `sources`): a step completed then
 * was done, so it still counts.
 */
export function legacyDone(state: OnboardingState, step: "ai" | "source"): boolean {
  const recorded = step === "ai" ? state.aiSetup !== undefined : state.sources !== undefined || state.cloudApps !== undefined;
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
    case "sourcesIndexed":
      return state.indexed ? state : { ...state, indexed: true };
    case "appsConnected": {
      const names = [...new Set([...(state.cloudApps ?? []), ...action.names])];
      return names.length === (state.cloudApps?.length ?? 0) ? state : { ...state, cloudApps: names };
    }
    case "next": {
      if (state.step === "done") return { ...state, finished: true, completed: withStep(state.completed, "done") };
      return {
        ...state,
        completed: withStep(state.completed, state.step),
        skipped: without(state.skipped, state.step),
        step: advance(state),
      };
    }
    case "back": {
      const order = stepOrder(state);
      const i = order.indexOf(state.step);
      return i > 0 ? { ...state, step: order[i - 1]! } : state;
    }
    case "skip": {
      if (state.step === "done") return { ...state, finished: true };
      return {
        ...state,
        skipped: state.completed.includes(state.step) ? state.skipped : withStep(state.skipped, state.step),
        step: advance(state),
      };
    }
    case "goto":
      return { ...state, step: action.step };
    case "chooseAi": {
      const { aiOption: _, ...rest } = state;
      return { ...rest, ai: action.ai };
    }
    case "chooseAiOption":
      return { ...state, ai: aiChoiceOf(action.option), aiOption: action.option };
    case "choosePath": {
      if (action.path === null) {
        const { path: _, ...rest } = state;
        return { ...rest, step: "welcome" };
      }
      if (action.path === "cloud") return { ...state, path: "cloud", step: "welcome" };
      // Running it yourself: the fork is answered, and the AI choice comes next.
      return {
        ...state,
        path: "local",
        completed: withStep(state.completed, "welcome"),
        skipped: without(state.skipped, "welcome"),
        step: "ai",
      };
    }
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
  const isAi = (x: unknown): x is AiChoice => x === "local" || x === "byok" || x === "chatgpt" || x === "later";
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
  if (o.indexed === true) out.indexed = true;
  if (o.path === "cloud" || o.path === "local") out.path = o.path;
  if (typeof o.aiOption === "string" && (AI_OPTIONS as readonly string[]).includes(o.aiOption)) {
    const option = o.aiOption as AiOption;
    // Only kept when it still matches the saved choice.
    if (ai !== null && aiChoiceOf(option) === ai) out.aiOption = option;
  }
  if (Array.isArray(o.cloudApps)) {
    const names = o.cloudApps.filter((x): x is string => typeof x === "string");
    if (names.length) out.cloudApps = names;
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
