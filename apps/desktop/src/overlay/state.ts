/**
 * Caret overlay logic, kept pure for tests. The overlay shows the shell's `OverlayView` and turns
 * keys into effects:
 *
 * - Preview: Enter accepts what is shown (also while streaming; the shell cancels the rest),
 *   Tab switches to editing, Esc cancels.
 * - Editing: a textarea with the draft. Ctrl/Cmd+Enter accepts the edited text (`null` when
 *   unchanged), Enter is a newline, Tab keeps its default, Esc cancels.
 * - Refused / error / inserted: Enter or Esc dismisses (a cancel).
 */
import type { OverlayPhase, OverlayView } from "../api";

export type Mode = "preview" | "edit";

export interface OverlayState {
  view: OverlayView;
  mode: Mode;
  /** Draft text as last shown when editing began (or kept in sync until the user types). */
  base: string;
  /** Editor contents. */
  draft: string;
  /** An accept was sent and the shell hasn't moved on yet: ignore repeats. */
  accepting: boolean;
}

export type Effect = { kind: "accept"; text: string | null } | { kind: "cancel" };

export interface KeyInput {
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
  /** IME composition in progress: never act on it. */
  isComposing?: boolean;
}

export interface Step {
  state: OverlayState;
  effect: Effect | null;
  /** Whether to stop the browser's default handling of the key. */
  preventDefault: boolean;
}

export const EMPTY_VIEW: OverlayView = {
  phase: "reading",
  text: "",
  provider: null,
  locality: null,
  citations: 0,
  excluded_sensitive: 0,
  message: null,
  verified: null,
  method: null,
};

export function initialState(view: OverlayView = EMPTY_VIEW): OverlayState {
  return { view, mode: "preview", base: view.text, draft: view.text, accepting: false };
}

const DRAFTING: readonly OverlayPhase[] = ["streaming", "done"];
const FINISHED: readonly OverlayPhase[] = ["refused", "error", "inserted"];

/** A draft is on screen and can be accepted or edited. */
export function canAccept(s: OverlayState): boolean {
  return !s.accepting && DRAFTING.includes(s.view.phase) && s.view.text.trim().length > 0;
}

export function isFinished(phase: OverlayPhase): boolean {
  return FINISHED.includes(phase);
}

export function edited(s: OverlayState): boolean {
  return s.draft !== s.base;
}

/** A new view from the shell. */
export function applyView(s: OverlayState, view: OverlayView): OverlayState {
  // A new draft (or a fresh read) starts over.
  if (view.phase === "reading") return initialState(view);
  const accepting = s.accepting && DRAFTING.includes(view.phase);
  if (s.mode === "edit") {
    if (!DRAFTING.includes(view.phase)) {
      // The shell moved on (inserting, refused, error…): leave the editor.
      return { view, mode: "preview", base: view.text, draft: view.text, accepting };
    }
    // Keep following the stream until the user changes something.
    if (!edited(s)) return { ...s, view, base: view.text, draft: view.text, accepting };
    return { ...s, view, accepting };
  }
  return { ...s, view, base: view.text, draft: view.text, accepting };
}

/** The user typed in the editor. */
export function setDraft(s: OverlayState, draft: string): OverlayState {
  return s.mode === "edit" ? { ...s, draft } : s;
}

export function startEdit(s: OverlayState): OverlayState {
  if (s.mode === "edit" || !canAccept(s)) return s;
  return { ...s, mode: "edit", base: s.view.text, draft: s.view.text };
}

/** Accept: unchanged (`null`) in preview, else the editor's text if it differs. */
export function accept(s: OverlayState): Step {
  if (!canAccept(s)) return { state: s, effect: null, preventDefault: true };
  let text: string | null = null;
  if (s.mode === "edit" && edited(s)) {
    if (!s.draft.trim()) return { state: s, effect: null, preventDefault: true };
    text = s.draft;
  }
  return { state: { ...s, accepting: true }, effect: { kind: "accept", text }, preventDefault: true };
}

export function cancel(s: OverlayState): Step {
  return { state: { ...s, accepting: false }, effect: { kind: "cancel" }, preventDefault: true };
}

const NOTHING = (s: OverlayState, preventDefault = false): Step => ({ state: s, effect: null, preventDefault });

export function handleKey(s: OverlayState, k: KeyInput): Step {
  if (k.isComposing) return NOTHING(s);
  if (k.key === "Escape") return cancel(s);

  if (s.mode === "edit") {
    if (k.key === "Enter" && (k.ctrlKey || k.metaKey)) return accept(s);
    // Enter is a newline, Tab moves focus as usual.
    return NOTHING(s);
  }

  if (k.key === "Enter") {
    if (isFinished(s.view.phase)) return cancel(s);
    return accept(s);
  }
  if (k.key === "Tab" && !k.shiftKey && !k.ctrlKey && !k.metaKey && !k.altKey) {
    if (!canAccept(s)) return NOTHING(s, true);
    return { state: startEdit(s), effect: null, preventDefault: true };
  }
  return NOTHING(s);
}
