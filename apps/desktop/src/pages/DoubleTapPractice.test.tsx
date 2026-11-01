// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { DOUBLETAP_PRACTICE_EVENT, type DoubleTapSettings, type DoubleTapStatus, type PracticeField } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { DoubleTapPractice } from "./DoubleTapPractice";

const settings: DoubleTapSettings = {
  enabled: true,
  trigger: { kind: "chord", chord: "Ctrl+Alt+Space" },
  app_providers: {},
  excluded_apps: [],
  allow_terminals: false,
};

const status: DoubleTapStatus = {
  platform: "linux",
  session: "x11",
  listening: true,
  trigger_error: null,
  accessibility: "not_required",
  input_monitoring: "not_required",
  hint: null,
  modifiers: [],
  recommended_trigger: { kind: "chord", chord: "Ctrl+Alt+Space" },
};

function setup() {
  const fields: (PracticeField | null)[] = [];
  mockDaemon({
    doubletap_practice: (args: unknown) => {
      fields.push((args as { field: PracticeField | null }).field);
      return null;
    },
  });
  return { fields };
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("Try your shortcut", () => {
  it("explains the trigger and labels the field, with no axe violations", async () => {
    setup();
    const { container } = await render(<DoubleTapPractice settings={settings} status={status} />);
    expect(container.querySelector("h2")?.textContent).toBe("Try your shortcut");
    expect(container.textContent).toContain("(Ctrl+Alt+Space)");
    expect(container.textContent).toContain("nothing is sent to any other app");
    expect(container.querySelector("blockquote")?.textContent).toContain("Friday morning");
    const area = container.querySelector("textarea")!;
    expect(container.querySelector(`label[for="${area.id}"]`)?.textContent).toBe("Your reply (practice field)");
    expect(area.getAttribute("dir")).toBe("auto");
    expect(container.querySelector(".note")).toBeNull();
    const result = await axe.run(container, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => v.id)).toEqual([]);
  });

  it("tells the shell about the field while it has focus, and forgets it on blur and unmount", async () => {
    const { fields } = setup();
    const { container, unmount } = await render(<DoubleTapPractice settings={settings} status={status} />);
    const area = container.querySelector("textarea")!;
    await act(async () => area.focus());
    await flush(2);
    const f = fields.at(-1)!;
    expect(f).not.toBeNull();
    expect(f.value).toBe("");
    expect(f.selection).toEqual([0, 0]);
    expect(f.rtl).toBe(false);
    expect(f.sample).toContain("Friday morning");
    expect(Object.keys(f.rect).sort()).toEqual(["height", "width", "x", "y"]);
    await act(async () => area.blur());
    await flush(2);
    expect(fields.at(-1)).toBeNull();
    await act(async () => area.focus());
    await flush(2);
    unmount();
    await flush(2);
    expect(fields.at(-1)).toBeNull();
  });

  it("inserts a draft at the selection and says so; focus brings the field back", async () => {
    setup();
    const { container } = await render(<DoubleTapPractice settings={settings} status={status} />);
    const area = container.querySelector("textarea")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(area, "Hi Sam, XX");
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
    area.setSelectionRange(8, 10);
    await act(() => emit(DOUBLETAP_PRACTICE_EVENT, { kind: "insert", text: "Friday works." }));
    await flush(2);
    expect(area.value).toBe("Hi Sam, Friday works.");
    expect(document.activeElement).toBe(area);
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Draft inserted in the practice field");
    area.blur();
    await act(() => emit(DOUBLETAP_PRACTICE_EVENT, { kind: "focus" }));
    await flush(2);
    expect(document.activeElement).toBe(area);
    await click(byText(container, "button", "Clear"));
    expect(area.value).toBe("");
    expect(container.querySelector('[role="status"]')?.textContent).toBe("");
  });

  it("points to the trigger settings when Double Tap is off or not listening", async () => {
    setup();
    const { container, unmount } = await render(<DoubleTapPractice settings={{ ...settings, enabled: false }} status={status} />);
    expect(container.querySelector(".note")?.textContent).toContain("Double Tap is off");
    unmount();
    const r = await render(<DoubleTapPractice settings={settings} status={{ ...status, listening: false }} />);
    expect(r.container.querySelector(".note")?.textContent).toContain("isn't listening");
    expect(byText(r.container, "button", "Go to trigger settings")).toBeDefined();
  });

  it("ignores an older shell without the practice command", async () => {
    mockDaemon({});
    const { container } = await render(<DoubleTapPractice settings={settings} status={undefined} />);
    const area = container.querySelector("textarea")!;
    await act(async () => area.focus());
    await flush(2);
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });
});
