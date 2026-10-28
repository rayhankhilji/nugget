/**
 * The Double Tap trigger preference. Mirrors `nugget_hotkey::Trigger`: a double tap of one
 * modifier, or a chord in the platform's accelerator syntax.
 *
 * TODO(wave 2): the daemon has no settings IPC yet, so the choice is kept in local settings
 * (`localStorage`) and nothing listens for it. When `settings.get`/`settings.set` (or a
 * `hotkey.set` method) lands, send it there and keep this as the offline cache.
 */
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
