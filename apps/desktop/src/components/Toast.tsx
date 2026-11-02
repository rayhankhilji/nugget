import { useEffect, useSyncExternalStore } from "react";
import { comboLabel, hasMod, isTypingTarget, type Combo } from "../hooks.shortcuts";
import { useT } from "../i18n";
import { Icon } from "./Icon";

/**
 * Toasts: one ink pill at the bottom of the window, for a moment, with at most one action
 * (usually Undo, which ⌘Z / Ctrl+Z also runs while the toast shows). They are a polite status
 * region, so screen readers hear the message without losing their place. One at a time: a new
 * toast replaces the last.
 */
export interface Toast {
  id: number;
  message: string;
  action?: { label: string; run: () => void };
  /** Milliseconds on screen (default 6000). */
  duration?: number;
}

let current: Toast | null = null;
let timer: ReturnType<typeof setTimeout> | undefined;
let next = 1;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

/** Show a toast; returns its id (for `dismissToast`). */
export function showToast(t: Omit<Toast, "id">): number {
  const toast = { ...t, id: next++ };
  current = toast;
  clearTimeout(timer);
  timer = setTimeout(() => dismissToast(toast.id), toast.duration ?? 6000);
  emit();
  return toast.id;
}

/** Hide the toast with this id, if it is still the one showing. */
export function dismissToast(id: number): void {
  if (current?.id !== id) return;
  current = null;
  clearTimeout(timer);
  emit();
}

const UNDO: Combo = { mod: true, key: "Z" };

/** The toast region; mount once, at the root of a window. */
export function Toaster() {
  const t = useT();
  const toast = useSyncExternalStore(
    (l) => (listeners.add(l), () => listeners.delete(l)),
    () => current,
    () => current,
  );
  useEffect(() => {
    if (!toast?.action) return;
    const onKey = (e: KeyboardEvent) => {
      if (!hasMod(e) || e.shiftKey || e.key.toLowerCase() !== "z" || isTypingTarget(e.target)) return;
      e.preventDefault();
      toast.action!.run();
      dismissToast(toast.id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toast]);
  return (
    <div className="toaster" role="status" aria-live="polite">
      {toast && (
        <div className="toast" key={toast.id}>
          <span className="toast-message" dir="auto">
            {toast.message}
          </span>
          {toast.action && (
            <button
              type="button"
              className="toast-action"
              aria-keyshortcuts="Meta+Z Control+Z"
              onClick={() => {
                toast.action!.run();
                dismissToast(toast.id);
              }}
            >
              {toast.action.label}
              <kbd aria-hidden="true" dir="ltr">
                {comboLabel(UNDO)}
              </kbd>
            </button>
          )}
          <button type="button" className="toast-close" aria-label={t("toast.close")} onClick={() => dismissToast(toast.id)}>
            <Icon name="close" size={14} />
          </button>
        </div>
      )}
    </div>
  );
}
