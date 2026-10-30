import { describe, expect, it } from "vitest";
import type { OverlayView } from "../api";
import { applyView, EMPTY_VIEW, handleKey, initialState, setDraft, type OverlayState } from "./state";

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
