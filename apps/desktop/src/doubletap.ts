/** Pure helpers for the Double Tap settings page and overlay (no React, no IPC). */
import type { DoubleTapModifier, DoubleTapSettings, TriggerSpec } from "./api";
import en from "./i18n/locales/en.json";
import { t, type MessageKey } from "./i18n";

export const WINDOW_MIN_MS = 150;
export const WINDOW_MAX_MS = 1000;
export const DEFAULT_WINDOW_MS = 350;

export function clampWindow(ms: number): number {
  if (!Number.isFinite(ms)) return DEFAULT_WINDOW_MS;
  return Math.min(WINDOW_MAX_MS, Math.max(WINDOW_MIN_MS, Math.round(ms)));
}

export function isMessageKey(s: string): s is MessageKey {
  return Object.prototype.hasOwnProperty.call(en, s);
}

/** A catalog key translated, or anything else (raw error text from the shell) as is. */
export function keyOrText(s: string): string {
  return isMessageKey(s) ? t(s) : s;
}

/** One row of the provider-per-app table. */
export interface ProviderRow {
  app: string;
  provider: string;
}

export function providerRows(map: Record<string, string>): ProviderRow[] {
  return Object.entries(map)
    .map(([app, provider]) => ({ app, provider }))
    .sort((a, b) => a.app.localeCompare(b.app));
}

/** Rows back to a map: trimmed, blank rows dropped, a later row for the same app wins. */
export function rowsToMap(rows: readonly ProviderRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of rows) {
    const app = r.app.trim();
    const provider = r.provider.trim();
    if (app && provider) out[app] = provider;
  }
  return out;
}

/** Add an app to a list, trimmed and without case-insensitive duplicates. */
export function addApp(list: readonly string[], app: string): string[] {
  const v = app.trim();
  if (!v || list.some((x) => x.toLowerCase() === v.toLowerCase())) return [...list];
  return [...list, v];
}

export function removeApp(list: readonly string[], app: string): string[] {
  return list.filter((x) => x !== app);
}

/** Why a chord can't be saved (checked again by the shell), or `null`. */
export function chordError(chord: string): MessageKey | null {
  const parts = chord.split("+").map((p) => p.trim());
  if (!chord.trim()) return "doubletap.trigger.chordEmpty";
  if (parts.some((p) => !p) || parts.length < 2) return "doubletap.trigger.chordInvalid";
  return null;
}

export function triggerError(trigger: TriggerSpec): MessageKey | null {
  return trigger.kind === "chord" ? chordError(trigger.chord) : null;
}

export function sameTrigger(a: TriggerSpec, b: TriggerSpec): boolean {
  if (a.kind === "double_tap" && b.kind === "double_tap") return a.modifier === b.modifier && a.window_ms === b.window_ms;
  if (a.kind === "chord" && b.kind === "chord") return a.chord.trim() === b.chord.trim();
  return false;
}

/** Settings as they will be sent: trimmed, normalised. */
export function normalizeSettings(s: DoubleTapSettings): DoubleTapSettings {
  const trigger: TriggerSpec =
    s.trigger.kind === "double_tap"
      ? { kind: "double_tap", modifier: s.trigger.modifier, window_ms: clampWindow(s.trigger.window_ms) }
      : { kind: "chord", chord: s.trigger.chord.trim() };
  return {
    enabled: s.enabled,
    trigger,
    app_providers: rowsToMap(providerRows(s.app_providers)),
    excluded_apps: s.excluded_apps.reduce<string[]>((acc, a) => addApp(acc, a), []),
    allow_terminals: s.allow_terminals ?? false,
  };
}

export function sameSettings(a: DoubleTapSettings, b: DoubleTapSettings): boolean {
  const x = normalizeSettings(a);
  const y = normalizeSettings(b);
  return (
    x.enabled === y.enabled &&
    x.allow_terminals === y.allow_terminals &&
    sameTrigger(x.trigger, y.trigger) &&
    JSON.stringify(providerRows(x.app_providers)) === JSON.stringify(providerRows(y.app_providers)) &&
    JSON.stringify(x.excluded_apps) === JSON.stringify(y.excluded_apps)
  );
}

/** Switch trigger kind, keeping a sensible value for the other half. */
export function switchKind(
  current: TriggerSpec,
  kind: TriggerSpec["kind"],
  memory: { modifier: DoubleTapModifier; window_ms: number; chord: string },
): TriggerSpec {
  if (current.kind === kind) return current;
  return kind === "double_tap"
    ? { kind: "double_tap", modifier: memory.modifier, window_ms: memory.window_ms }
    : { kind: "chord", chord: memory.chord };
}

export type Tone = "good" | "warn" | "bad" | "neutral";

export function permissionTone(state: string): Tone {
  switch (state) {
    case "granted":
      return "good";
    case "denied":
      return "bad";
    case "unknown":
      return "warn";
    default:
      return "neutral";
  }
}
