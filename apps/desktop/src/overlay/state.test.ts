import { describe, expect, it } from "vitest";
import type { OverlayView } from "../api";
import {
  applyView,
  canInsertPrevious,
  EMPTY_VIEW,
  handleKey,
  initialState,
  insertPrevious,
  refine,
  retry,
  setDraft,
  settle,
  shortcutFor,
  wordCount,
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

  it("Enter does nothing while reading or with an empty draft", () => {
    expect(handleKey(at(view({ phase: "reading" })), { key: "Enter" }).effect).toBeNull();
    expect(handleKey(at(streaming("  ")), { key: "Enter" }).effect).toBeNull();
  });

  it("Esc cancels in any mode and phase", () => {
    for (const phase of ["reading", "streaming", "done", "refused", "error", "inserted"] as const) {
      expect(handleKey(at(view({ phase, text: "x" })), { key: "Escape" }).effect).toEqual({ kind: "cancel" });
    }
    const editing = handleKey(at(streaming("x")), { key: "Tab" }).state;
    expect(handleKey(editing, { key: "Escape" }).effect).toEqual({ kind: "cancel" });
  });

  it("Enter dismisses a refused, failed or finished overlay", () => {
    for (const phase of ["refused", "error", "inserted"] as const) {
      expect(handleKey(at(view({ phase })), { key: "Enter" }).effect).toEqual({ kind: "cancel" });
    }
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

  it("Tab is swallowed when there is nothing to edit", () => {
    const s = handleKey(at(view({ phase: "reading" })), { key: "Tab" });
    expect(s.state.mode).toBe("preview");
    expect(s.preventDefault).toBe(true);
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

describe("overlay views", () => {
  it("the editor follows the stream until the user types", () => {
    let s = handleKey(at(streaming("Hi")), { key: "Tab" }).state;
    s = applyView(s, streaming("Hi Ada"));
    expect(s.draft).toBe("Hi Ada");
    s = setDraft(s, "Hello Ada");
    s = applyView(s, streaming("Hi Ada, thanks"));
    expect(s.draft).toBe("Hello Ada");
    expect(s.base).toBe("Hi Ada");
    expect(s.view.text).toBe("Hi Ada, thanks");
  });

  it("leaves edit mode when the shell moves on", () => {
    const s = handleKey(at(streaming("Hi")), { key: "Tab" }).state;
    const next = applyView(s, view({ phase: "inserting", text: "Hi" }));
    expect(next.mode).toBe("preview");
  });

  it("a new reading phase starts over", () => {
    const accepted = handleKey(at(streaming("Hi")), { key: "Enter" }).state;
    const next = applyView(accepted, view({ phase: "reading" }));
    expect(next).toEqual(initialState(view({ phase: "reading" })));
  });

  it("setDraft is ignored outside edit mode", () => {
    const s = at(streaming("Hi"));
    expect(setDraft(s, "x")).toBe(s);
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

  it("does nothing without can_retry (an older shell) or while reading, refused or inserted", () => {
    expect(handleKey(at(view({ phase: "done", text: "x" })), { key: "r" }).effect).toBeNull();
    for (const phase of ["reading", "refused", "inserting", "inserted"] as const) {
      expect(handleKey(at(view({ phase, text: "x", can_retry: true })), { key: "r" }).effect).toBeNull();
    }
  });

  it("S, L, F and C refine the text shown (base null)", () => {
    const keys = { s: "shorter", l: "longer", f: "formal", c: "casual" } as const;
    for (const [key, refinement] of Object.entries(keys)) {
      expect(handleKey(done(), { key }).effect).toEqual({ kind: "refine", refinement, base: null });
      expect(handleKey(done(), { key: key.toUpperCase(), shiftKey: true }).effect).toEqual({ kind: "refine", refinement, base: null });
    }
  });

  it("refinements need a draft to rewrite", () => {
    expect(handleKey(done("  "), { key: "s" }).effect).toBeNull();
    expect(handleKey(at(view({ phase: "error", text: "", can_retry: true })), { key: "s" }).effect).toBeNull();
  });

  it("a refinement while editing rewrites the edited text", () => {
    const editing = handleKey(done(), { key: "Tab" }).state;
    expect(refine(editing, "shorter").effect).toEqual({ kind: "refine", refinement: "shorter", base: null });
    const changed = setDraft(editing, "Thursday works.");
    expect(refine(changed, "formal").effect).toEqual({ kind: "refine", refinement: "formal", base: "Thursday works." });
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

  it("matches the physical key on non-Latin layouts, the letter on Latin ones", () => {
    expect(shortcutFor({ key: "к", code: "KeyR" })).toBe("retry");
    expect(shortcutFor({ key: "ы", code: "KeyS" })).toBe("shorter");
    // Dvorak: the key labelled "p" sits where QWERTY has R. It is a "p", not a retry.
    expect(shortcutFor({ key: "p", code: "KeyR" })).toBeNull();
    expect(shortcutFor({ key: "Enter", code: "Enter" })).toBeNull();
  });

  it("Ctrl/Cmd+Shift+Enter inserts as is while editing", () => {
    const changed = setDraft(handleKey(done(), { key: "Tab" }).state, "Edited");
    expect(handleKey(changed, { key: "Enter", ctrlKey: true, shiftKey: true }).effect).toEqual({ kind: "accept", text: null });
    expect(handleKey(changed, { key: "Enter", metaKey: true }).effect).toEqual({ kind: "accept", text: "Edited" });
  });

  it("a failed request can be tried again; a new reading phase clears it", () => {
    const sent = retry(done()).state;
    expect(retry(sent).effect).toBeNull();
    expect(retry(settle(sent)).effect).toEqual({ kind: "retry" });
    expect(applyView(sent, view({ phase: "reading", refinement: "shorter" })).requested).toBe(false);
  });

  it("Tab moves on from a finished overlay, so Retry and Close can be reached", () => {
    expect(handleKey(at(view({ phase: "error", can_retry: true })), { key: "Tab" }).preventDefault).toBe(false);
    expect(handleKey(at(view({ phase: "reading" })), { key: "Tab" }).preventDefault).toBe(true);
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

  it("Esc closes the list first, then cancels", () => {
    const open = handleKey(done, { key: "h" }, { history: true }).state;
    const first = handleKey(open, { key: "Escape" });
    expect(first.effect).toBeNull();
    expect(first.preventDefault).toBe(true);
    expect(first.state.historyOpen).toBe(false);
    expect(handleKey(first.state, { key: "Escape" }).effect).toEqual({ kind: "cancel" });
  });

  it("picking one inserts it and closes the list", () => {
    const open = handleKey(done, { key: "h" }, { history: true }).state;
    const step = insertPrevious(open, 7);
    expect(step.effect).toEqual({ kind: "insertPrevious", id: 7 });
    expect(step.state.historyOpen).toBe(false);
    expect(step.state.accepting).toBe(true);
    expect(insertPrevious(step.state, 7).effect).toBeNull();
  });

  it("never into a refused field or while inserting", () => {
    expect(canInsertPrevious(at(view({ phase: "refused" })))).toBe(false);
    expect(canInsertPrevious(at(view({ phase: "inserting" })))).toBe(false);
    expect(canInsertPrevious(at(view({ phase: "error" })))).toBe(true);
  });

  it("the list closes when the shell moves on to inserting", () => {
    const open = handleKey(done, { key: "h" }, { history: true }).state;
    expect(applyView(open, view({ phase: "inserting", text: "Draft" })).historyOpen).toBe(false);
  });
});

describe("word count", () => {
  it("counts words, also without spaces", () => {
    expect(wordCount("Thanks Ada, Thursday works.")).toBe(4);
    expect(wordCount("")).toBe(0);
    expect(wordCount("木曜日で大丈夫です")).toBeGreaterThan(1);
  });
});
