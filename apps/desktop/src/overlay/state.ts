/**
 * Caret overlay logic, kept pure for tests. The overlay shows the shell's `OverlayView` and turns
 * keys into effects:
 *
 * - Preview: Enter accepts what is shown (also while streaming; the shell cancels the rest),
 *   Tab switches to editing, Esc cancels. R retries, S / L / F / C ask for a shorter, longer,
 *   more formal or more casual version, H opens the previous drafts (Esc closes them first).
 * - Editing: a textarea with the draft. Ctrl/Cmd+Enter accepts the edited text (`null` when
 *   unchanged), Ctrl/Cmd+Shift+Enter inserts the draft as shown ("Insert as is"), Enter is a
 *   newline, Tab keeps its default, Esc cancels. Letters are text here, never shortcuts.
 * - Refused / error / inserted: Enter or Esc dismisses (a cancel); R retries a failed draft
 *   when the shell can.
 */
import type { OverlayPhase, OverlayView, Refinement } from "../api";

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
  /** A retry or refinement was sent and no new draft has started yet: ignore repeats. */
  requested: boolean;
  /** The previous drafts list is open. */
  historyOpen: boolean;
}

export type Effect =
  | { kind: "accept"; text: string | null }
  | { kind: "cancel" }
  | { kind: "retry" }
  | { kind: "refine"; refinement: Refinement; base: string | null }
  | { kind: "insertPrevious"; id: number };

export interface KeyInput {
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
  /** IME composition in progress: never act on it. */
  isComposing?: boolean;
  /** The physical key (`KeyR`), for layouts whose letters aren't Latin. */
  code?: string;
}

/** What the overlay knows besides its state when a key comes in. */
export interface KeyContext {
  /** Previous drafts can be listed (some exist, and this isn't a practice session). */
  history?: boolean;
}

/** Letter shortcuts in preview mode. */
export const SHORTCUTS = { retry: "R", shorter: "S", longer: "L", formal: "F", casual: "C", history: "H" } as const;
type Shortcut = keyof typeof SHORTCUTS;

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
  return { view, mode: "preview", base: view.text, draft: view.text, accepting: false, requested: false, historyOpen: false };
}

const DRAFTING: readonly OverlayPhase[] = ["streaming", "done"];
const FINISHED: readonly OverlayPhase[] = ["refused", "error", "inserted"];

/** A draft is on screen and can be accepted or edited. */
export function canAccept(s: OverlayState): boolean {
  return !s.accepting && !s.requested && DRAFTING.includes(s.view.phase) && s.view.text.trim().length > 0;
}

/** Retry: a new draft from the same context (while drafting, and after a failed draft). */
export function canRetry(s: OverlayState): boolean {
  return s.view.can_retry === true && !s.accepting && !s.requested && (DRAFTING.includes(s.view.phase) || s.view.phase === "error");
}

/** A refinement needs a draft to rewrite. */
export function canRefine(s: OverlayState): boolean {
  return canRetry(s) && DRAFTING.includes(s.view.phase) && s.view.text.trim().length > 0;
}

/** A previous draft can go in instead (not into a refused field, nor while inserting). */
export function canInsertPrevious(s: OverlayState): boolean {
  return !s.accepting && !s.requested && (DRAFTING.includes(s.view.phase) || s.view.phase === "error");
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
  const drafting = DRAFTING.includes(view.phase);
  const accepting = s.accepting && drafting;
  const requested = s.requested && drafting;
  const historyOpen = s.historyOpen && view.phase !== "inserting" && view.phase !== "inserted" && view.phase !== "refused";
  if (s.mode === "edit") {
    if (!drafting) {
      // The shell moved on (inserting, refused, error…): leave the editor.
      return { view, mode: "preview", base: view.text, draft: view.text, accepting, requested, historyOpen };
    }
    // Keep following the stream until the user changes something.
    if (!edited(s)) return { ...s, view, base: view.text, draft: view.text, accepting, requested, historyOpen };
    return { ...s, view, accepting, requested, historyOpen };
  }
  return { ...s, view, base: view.text, draft: view.text, accepting, requested, historyOpen };
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

/** Insert the draft as shown, ignoring the edits ("Insert as is"). */
export function acceptAsIs(s: OverlayState): Step {
  if (!canAccept(s)) return { state: s, effect: null, preventDefault: true };
  return { state: { ...s, accepting: true }, effect: { kind: "accept", text: null }, preventDefault: true };
}

export function cancel(s: OverlayState): Step {
  return { state: { ...s, accepting: false }, effect: { kind: "cancel" }, preventDefault: true };
}

export function retry(s: OverlayState): Step {
  if (!canRetry(s)) return { state: s, effect: null, preventDefault: true };
  return { state: { ...s, requested: true, historyOpen: false }, effect: { kind: "retry" }, preventDefault: true };
}

/** The base a refinement rewrites: the edited text while editing (when changed), else `null` (the text shown). */
export function refineBase(s: OverlayState): string | null {
  return s.mode === "edit" && edited(s) && s.draft.trim() ? s.draft : null;
}

export function refine(s: OverlayState, refinement: Refinement): Step {
  if (!canRefine(s)) return { state: s, effect: null, preventDefault: true };
  return {
    state: { ...s, requested: true, historyOpen: false },
    effect: { kind: "refine", refinement, base: refineBase(s) },
    preventDefault: true,
  };
}

export function insertPrevious(s: OverlayState, id: number): Step {
  if (!canInsertPrevious(s)) return { state: s, effect: null, preventDefault: true };
  return { state: { ...s, accepting: true, historyOpen: false }, effect: { kind: "insertPrevious", id }, preventDefault: true };
}

export function setHistoryOpen(s: OverlayState, open: boolean): OverlayState {
  return s.historyOpen === open ? s : { ...s, historyOpen: open };
}

/** The request failed: let the user try again. */
export function settle(s: OverlayState): OverlayState {
  return { ...s, accepting: false, requested: false };
}

/** Which letter shortcut a key is: by its letter, or by its physical key when the letter isn't Latin. */
export function shortcutFor(k: KeyInput): Shortcut | null {
  if (k.ctrlKey || k.metaKey || k.altKey) return null;
  const letter = /^[a-z]$/i.test(k.key) ? k.key.toUpperCase() : k.code && /^Key[A-Z]$/.test(k.code) && k.key.length === 1 ? k.code.slice(3) : null;
  if (!letter) return null;
  const hit = (Object.keys(SHORTCUTS) as Shortcut[]).find((name) => SHORTCUTS[name] === letter);
  return hit ?? null;
}

const NOTHING = (s: OverlayState, preventDefault = false): Step => ({ state: s, effect: null, preventDefault });

export function handleKey(s: OverlayState, k: KeyInput, ctx: KeyContext = {}): Step {
  if (k.isComposing) return NOTHING(s);
  if (k.key === "Escape") {
    // The list closes first; a second Esc cancels.
    if (s.historyOpen) return { state: { ...s, historyOpen: false }, effect: null, preventDefault: true };
    return cancel(s);
  }

  if (s.mode === "edit") {
    if (k.key === "Enter" && (k.ctrlKey || k.metaKey)) return k.shiftKey ? acceptAsIs(s) : accept(s);
    // Enter is a newline, Tab moves focus as usual, letters are text.
    return NOTHING(s);
  }

  const shortcut = shortcutFor(k);
  if (shortcut === "retry") return canRetry(s) ? retry(s) : NOTHING(s);
  if (shortcut === "history") {
    if (!ctx.history || (!s.historyOpen && !canInsertPrevious(s))) return NOTHING(s);
    return { state: setHistoryOpen(s, !s.historyOpen), effect: null, preventDefault: true };
  }
  if (shortcut) return canRefine(s) ? refine(s, shortcut) : NOTHING(s);

  if (k.key === "Enter") {
    if (isFinished(s.view.phase)) return cancel(s);
    return accept(s);
  }
  if (k.key === "Tab" && !k.shiftKey && !k.ctrlKey && !k.metaKey && !k.altKey) {
    // With nothing to edit, Tab moves between the buttons once the shell is finished (Retry,
    // Close); while it's still reading there's nothing to move to.
    if (!canAccept(s)) return NOTHING(s, !isFinished(s.view.phase));
    return { state: startEdit(s), effect: null, preventDefault: true };
  }
  return NOTHING(s);
}

/** Words in a draft, for the "draft ready" announcement (by word segments, so CJK counts too). */
export function wordCount(text: string): number {
  const Seg = (Intl as unknown as { Segmenter?: typeof Intl.Segmenter }).Segmenter;
  if (Seg) {
    let n = 0;
    for (const part of new Seg(undefined, { granularity: "word" }).segment(text)) if (part.isWordLike) n++;
    return n;
  }
  return text.split(/\s+/).filter(Boolean).length;
}
