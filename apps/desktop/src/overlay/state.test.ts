import { describe, expect, it } from "vitest";
import type { OverlayView } from "../api";
import {
  applyView,
  EMPTY_VIEW,
  handleKey,
  initialState,
  setDraft,
  type OverlayState,
} from "./state";

const view = (v: Partial<OverlayView>): OverlayView => ({ ...EMPTY_VIEW, ...v });
const streaming = (text: string) => view({ phase: "streaming", text, provider: "local-chat", locality: "on_device" });

function at(v: OverlayView): OverlayState {
  return applyView(initialState(), v);
}

describe("overlay keys", () => {
  it("Enter accepts what is shown, even while streaming", () => {
    const step = handleKey(at(streaming("Hi Ada")), { key: "Enter" });
    expect(step.effect).toEqual({ kind: "accept", text: null });
    expect(step.preventDefault).toBe(true);
    expect(step.state.accepting).toBe(true);
    // A second Enter before the shell moves on does nothing.
    expect(handleKey(step.state, { key: "Enter" }).effect).toBeNull();
  });

  it("Tab enters edit mode with the draft; Enter is then a newline", () => {
    const s = handleKey(at(view({ phase: "done", text: "Draft" })), { key: "Tab" });
    expect(s.preventDefault).toBe(true);
    expect(s.state.mode).toBe("edit");
    expect(s.state.draft).toBe("Draft");
    const enter = handleKey(s.state, { key: "Enter" });
    expect(enter.effect).toBeNull();
    expect(enter.preventDefault).toBe(false);
    const tab = handleKey(s.state, { key: "Tab" });
    expect(tab.preventDefault).toBe(false);
  });

  it("Ctrl/Cmd+Enter accepts null when unchanged, else the edited text", () => {
    const editing = handleKey(at(view({ phase: "done", text: "Draft" })), { key: "Tab" }).state;
    expect(handleKey(editing, { key: "Enter", ctrlKey: true }).effect).toEqual({ kind: "accept", text: null });
    const changed = setDraft(editing, "Draft, edited");
    expect(handleKey(changed, { key: "Enter", metaKey: true }).effect).toEqual({ kind: "accept", text: "Draft, edited" });
    // Editing everything away doesn't insert an empty draft.
    expect(handleKey(setDraft(editing, "  "), { key: "Enter", ctrlKey: true }).effect).toBeNull();
  });

  it("ignores keys during IME composition", () => {
    expect(handleKey(at(streaming("x")), { key: "Enter", isComposing: true }).effect).toBeNull();
  });
});

describe("retry and refinements", () => {
  const done = (text = "Thanks Ada, Thursday works.") => at(view({ phase: "done", text, can_retry: true }));

  it("R retries while drafting and after a failed draft, once", () => {
    for (const phase of ["streaming", "done", "error"] as const) {
      const step = handleKey(at(view({ phase, text: "x", can_retry: true })), { key: "r" });
      expect(step.effect).toEqual({ kind: "retry" });
      expect(step.preventDefault).toBe(true);
      // A second R before the new draft starts does nothing; nor can it be accepted meanwhile.
      expect(handleKey(step.state, { key: "R" }).effect).toBeNull();
      if (phase !== "error") expect(handleKey(step.state, { key: "Enter" }).effect).toBeNull();
    }
  });

  it("S, L, F and C refine the text shown (base null)", () => {
    const keys = { s: "shorter", l: "longer", f: "formal", c: "casual" } as const;
    for (const [key, refinement] of Object.entries(keys)) {
      expect(handleKey(done(), { key }).effect).toEqual({ kind: "refine", refinement, base: null });
      expect(handleKey(done(), { key: key.toUpperCase(), shiftKey: true }).effect).toEqual({ kind: "refine", refinement, base: null });
    }
  });

  it("letters are text while editing, and never shortcuts with Ctrl, Cmd or Alt or during composition", () => {
    const editing = handleKey(done(), { key: "Tab" }).state;
    expect(handleKey(editing, { key: "r" }).effect).toBeNull();
    expect(handleKey(editing, { key: "s" }).preventDefault).toBe(false);
    expect(handleKey(done(), { key: "c", ctrlKey: true }).effect).toBeNull();
    expect(handleKey(done(), { key: "c", metaKey: true }).effect).toBeNull();
    expect(handleKey(done(), { key: "r", altKey: true }).effect).toBeNull();
    expect(handleKey(done(), { key: "r", isComposing: true }).effect).toBeNull();
  });
});

describe("previous drafts", () => {
  const done = at(view({ phase: "done", text: "Draft", can_retry: true }));

  it("H opens and closes the list only when there is one", () => {
    expect(handleKey(done, { key: "h" }).state.historyOpen).toBe(false);
    const open = handleKey(done, { key: "h" }, { history: true });
    expect(open.state.historyOpen).toBe(true);
    expect(open.effect).toBeNull();
    expect(handleKey(open.state, { key: "H" }, { history: true }).state.historyOpen).toBe(false);
  });
});
