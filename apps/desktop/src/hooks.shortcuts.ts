import { useEffect, useRef } from "react";
import { t } from "./i18n";

/**
 * Keyboard shortcuts for the shell. The "mod" key is ⌘ on macOS and Ctrl elsewhere, both when
 * matching a key press and when showing a hint, so the label always names the key that works.
 */

interface NavigatorUAData {
  platform?: string;
}

/** True on macOS (and iPadOS with a keyboard), where shortcuts use ⌘ instead of Ctrl. */
export function isMac(): boolean {
  if (typeof navigator === "undefined") return false;
  const nav = navigator as Navigator & { userAgentData?: NavigatorUAData };
  const platform = nav.userAgentData?.platform || nav.platform || "";
  return /mac|iphone|ipad|ipod/i.test(platform);
}

/** A key combination: `key` is the main key as shown (`K`, `1`, `,`, `/`, `?`, `Esc`). */
export interface Combo {
  mod?: boolean;
  key: string;
}

/** Visible label, e.g. `⌘K` on macOS or `Ctrl+K` elsewhere. */
export function comboLabel(c: Combo): string {
  const key = c.key === "Escape" ? t("shortcuts.key.esc") : c.key.toUpperCase();
  if (!c.mod) return key;
  return isMac() ? `⌘${key}` : `${t("shortcuts.key.ctrl")}+${key}`;
}

/** Value for `aria-keyshortcuts` (ARIA names keys in English, independent of the UI language). */
export function comboAria(c: Combo): string {
  const key = c.key === "?" ? "Shift+?" : c.key;
  return c.mod ? `${isMac() ? "Meta" : "Control"}+${key}` : key;
}

/** The platform modifier is held (and only it, apart from Shift for shifted keys). */
export function hasMod(e: KeyboardEvent): boolean {
  return isMac() ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

/** An IME is composing text: the key belongs to the input method, not to us. */
export function isComposing(e: KeyboardEvent): boolean {
  return e.isComposing || e.keyCode === 229;
}

/** Focus is somewhere that takes text, so plain keys like `?` must reach it. */
export function isTypingTarget(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  if (el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement) return true;
  if (el instanceof HTMLInputElement) {
    return !["button", "checkbox", "radio", "range", "submit", "reset", "color", "file", "image"].includes(el.type);
  }
  const role = el.getAttribute("role");
  return role === "textbox" || role === "combobox" || role === "searchbox";
}

/** The digit 1–9 of a number-row key, by physical key so it works on AZERTY and friends. */
export function digitOf(e: KeyboardEvent): number | null {
  const m = /^Digit([1-9])$/.exec(e.code) ?? /^([1-9])$/.exec(e.key);
  return m ? Number(m[1]) : null;
}

/** The letter of a key, lower-case, by character (falls back to the physical key). */
export function letterOf(e: KeyboardEvent): string {
  if (e.key.length === 1) return e.key.toLowerCase();
  const m = /^Key([A-Z])$/.exec(e.code);
  return m ? m[1]!.toLowerCase() : e.key;
}

/** Listen for keydown on the window for the component's lifetime; `handler` may change freely. */
export function useKeydown(handler: (e: KeyboardEvent) => void): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    const l = (e: KeyboardEvent) => ref.current(e);
    window.addEventListener("keydown", l);
    return () => window.removeEventListener("keydown", l);
  }, []);
}
