/**
 * The Double Tap trigger preference. Mirrors `nugget_hotkey::Trigger`: a double tap of one
 * modifier, or a chord in the platform's accelerator syntax.
 *
 * The source of truth is the shell's Double Tap settings (`doubletap.json`, the file that
 * configures the running hotkey; the Double Tap settings page edits the same file).
 * Onboarding reads and writes through it with {@link readTrigger} / {@link writeTrigger}.
 * `localStorage` only keeps a copy for the first render and for running outside the app, and
 * once migrates a choice that older versions of onboarding kept there and never applied.
 */
import { doubletapApi, inTauri, type DoubleTapSettings, type TriggerSpec } from "../api";
import { normalizeSettings, sameTrigger } from "../doubletap";
import type { Os } from "./platform";

export type Modifier =
  | "left_command"
  | "right_command"
  | "left_control"
  | "right_control"
  | "left_option"
  | "right_option"
  | "left_shift"
  | "right_shift";

export const MODIFIERS: readonly Modifier[] = [
  "right_command",
  "left_command",
  "right_control",
  "left_control",
  "right_option",
  "left_option",
  "right_shift",
  "left_shift",
];

export type TriggerPref =
  | { kind: "double_tap"; modifier: Modifier; window_ms: number }
  | { kind: "chord"; accelerator: string };

export const DEFAULT_WINDOW_MS = 350;
export const TRIGGER_KEY = "nugget.settings.trigger";
/** Set once the locally kept choice has been offered to the shell's settings. */
export const MIGRATED_KEY = "nugget.settings.trigger.migrated";

/** Modifiers that exist on this OS's keyboards (⌘ only on a Mac). */
export function modifiersFor(os: Os): Modifier[] {
  return os === "macos" ? [...MODIFIERS] : MODIFIERS.filter((m) => !m.endsWith("_command"));
}

/** Suggested chords; the accelerator syntax is Tauri's (`CmdOrCtrl` is ⌘ on a Mac, Ctrl elsewhere). */
export const CHORDS = ["Alt+Space", "CmdOrCtrl+Shift+Space", "CmdOrCtrl+Alt+K"] as const;

/**
 * Default: a double tap of the right-hand modifier least used on its own on that OS. Never a
 * key every keyboard lacks (no notch, no Apple-only key on Windows or Linux).
 */
export function defaultTrigger(os: Os): TriggerPref {
  return { kind: "double_tap", modifier: os === "macos" ? "right_command" : "right_control", window_ms: DEFAULT_WINDOW_MS };
}

function isModifier(v: unknown): v is Modifier {
  return typeof v === "string" && (MODIFIERS as readonly string[]).includes(v);
}

export function parseTrigger(raw: string | null): TriggerPref | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as Record<string, unknown>;
    if (v.kind === "double_tap" && isModifier(v.modifier)) {
      const w = typeof v.window_ms === "number" && v.window_ms >= 150 && v.window_ms <= 1000 ? v.window_ms : DEFAULT_WINDOW_MS;
      return { kind: "double_tap", modifier: v.modifier, window_ms: w };
    }
    if (v.kind === "chord" && typeof v.accelerator === "string" && v.accelerator.trim()) {
      return { kind: "chord", accelerator: v.accelerator.trim() };
    }
  } catch {
    /* fall through */
  }
  return null;
}

export function loadTrigger(os: Os): TriggerPref {
  try {
    return parseTrigger(localStorage.getItem(TRIGGER_KEY)) ?? defaultTrigger(os);
  } catch {
    return defaultTrigger(os);
  }
}

export function saveTrigger(pref: TriggerPref): void {
  try {
    localStorage.setItem(TRIGGER_KEY, JSON.stringify(pref));
  } catch {
    /* unavailable: kept for this session only */
  }
}

/** To the shell's settings shape. */
export function toSpec(pref: TriggerPref): TriggerSpec {
  return pref.kind === "double_tap"
    ? { kind: "double_tap", modifier: pref.modifier, window_ms: pref.window_ms }
    : { kind: "chord", chord: pref.accelerator };
}

/** From the shell's settings shape. */
export function fromSpec(spec: TriggerSpec): TriggerPref {
  return spec.kind === "double_tap"
    ? { kind: "double_tap", modifier: spec.modifier, window_ms: spec.window_ms }
    : { kind: "chord", accelerator: spec.chord };
}

function flagged(key: string): boolean {
  try {
    return localStorage.getItem(key) !== null;
  } catch {
    return false;
  }
}

function flag(key: string): void {
  try {
    localStorage.setItem(key, "1");
  } catch {
    /* unavailable: the migration is offered again next time, and stays a no-op */
  }
}

/**
 * Once: a choice that an older onboarding kept only in `localStorage` replaces the shell's
 * trigger, if that is still the recommended default (nobody chose one in Double Tap settings).
 */
async function migrate(settings: DoubleTapSettings): Promise<DoubleTapSettings> {
  if (flagged(MIGRATED_KEY)) return settings;
  let local: TriggerPref | null = null;
  try {
    local = parseTrigger(localStorage.getItem(TRIGGER_KEY));
  } catch {
    /* nothing to migrate */
  }
  if (!local || sameTrigger(toSpec(local), settings.trigger)) {
    flag(MIGRATED_KEY);
    return settings;
  }
  const status = await doubletapApi.status().catch(() => null);
  if (!status) return settings; // try again next time
  let out = settings;
  if (sameTrigger(settings.trigger, status.recommended_trigger)) {
    try {
      out = await doubletapApi.settingsSet(normalizeSettings({ ...settings, trigger: toSpec(local) }));
    } catch {
      /* not valid here (e.g. a chord this session can't use): keep the shell's trigger */
    }
  }
  flag(MIGRATED_KEY);
  return out;
}

/**
 * The trigger Double Tap runs with: the shell's settings (migrating an older local choice
 * once), cached locally. Outside the app, the local copy or the default.
 */
export async function readTrigger(os: Os): Promise<TriggerPref> {
  if (!inTauri()) return loadTrigger(os);
  const settings = await migrate(await doubletapApi.settingsGet());
  const pref = fromSpec(settings.trigger);
  saveTrigger(pref);
  return pref;
}

/**
 * Save the trigger in the shell's settings (which restarts the hotkey and tells the settings
 * page) and return what was saved. Throws when the shell refuses it; the local copy then keeps
 * the previous trigger. Outside the app, only the local copy is kept.
 */
export async function writeTrigger(pref: TriggerPref): Promise<TriggerPref> {
  if (!inTauri()) {
    saveTrigger(pref);
    return pref;
  }
  const current = await doubletapApi.settingsGet();
  const saved = await doubletapApi.settingsSet(normalizeSettings({ ...current, trigger: toSpec(pref) }));
  const out = fromSpec(saved.trigger);
  saveTrigger(out);
  flag(MIGRATED_KEY);
  return out;
}
