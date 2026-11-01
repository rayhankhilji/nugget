// @vitest-environment jsdom
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { QuickAskSettings as Settings, QuickAskView } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { QuickAskSettings } from "./QuickAskSettings";

const listening: QuickAskView = {
  settings: { enabled: true, shortcut: "Ctrl+Shift+Space" },
  status: { listening: true, error: null, wayland_fallback: false, state: "listening" },
};

function setup(initial: QuickAskView = listening, set?: (s: Settings) => QuickAskView) {
  const saved: Settings[] = [];
  mockDaemon({
    quickask_settings_get: initial,
    quickask_settings_set: (a: unknown) => {
      const s = (a as { settings: Settings }).settings;
      saved.push(s);
      return set ? set(s) : { settings: s, status: { ...initial.status, listening: s.enabled, state: s.enabled ? "listening" : "off" } };
    },
  });
  return saved;
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("quick ask settings", () => {
  it("shows the shortcut it listens for and saves a new one", async () => {
    const saved = setup();
    const { container } = await render(<QuickAskSettings />);
    expect(container.textContent).toContain("Listening for Ctrl+Shift+Space.");
    const input = container.querySelector<HTMLInputElement>('input[type="text"]')!;
    expect(input.value).toBe("Ctrl+Shift+Space");
    expect(container.querySelector(`label[for="${input.id}"]`)?.textContent).toBe("Shortcut");
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, " Ctrl+Alt+K ");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(container, "button", "Save"));
    expect(saved).toEqual([{ enabled: true, shortcut: "Ctrl+Alt+K" }]);
    expect(container.textContent).toContain("Listening for Ctrl+Alt+K.");
    expect(container.textContent).toContain("Saved.");
  });

  it("turns off with the switch", async () => {
    const saved = setup();
    const { container } = await render(<QuickAskSettings />);
    const sw = container.querySelector<HTMLInputElement>('input[role="switch"]')!;
    expect(sw.checked).toBe(true);
    await click(sw);
    expect(saved).toEqual([{ enabled: false, shortcut: "Ctrl+Shift+Space" }]);
    expect(container.textContent).toContain("Quick ask is off.");
  });

  it("shows a rejected shortcut and a listener error", async () => {
    mockDaemon({
      quickask_settings_get: { ...listening, status: { listening: false, error: "taken by another app", wayland_fallback: false, state: "error" } },
      quickask_settings_set: () => {
        throw { kind: "other", message: "Ctrl+Alt+Space is already used by Nugget" };
      },
    });
    const { container } = await render(<QuickAskSettings />);
    expect(container.textContent).toContain("The shortcut isn't working: taken by another app");
    const input = container.querySelector<HTMLInputElement>('input[type="text"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "Ctrl+Alt+Space");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(byText(container, "button", "Save"));
    await flush(2);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("already used");
  });

  it("explains the Wayland fallback command", async () => {
    setup({ ...listening, status: { listening: false, error: null, wayland_fallback: true, state: "unsupported_wayland" } });
    const { container } = await render(<QuickAskSettings />);
    expect(container.querySelector("code")?.textContent).toBe("nugget-desktop --quick-ask");
    expect(container.textContent).toContain("On Wayland");
    expect(container.querySelector('input[type="text"]')).toBeNull();
  });
});
