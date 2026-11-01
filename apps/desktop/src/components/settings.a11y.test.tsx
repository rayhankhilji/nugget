// @vitest-environment jsdom
/**
 * LL2: the tray and quick ask settings (Status → Updates and app) keep keyboard focus on the
 * control that was used while it saves (aria-disabled instead of disabled, which drops focus to
 * the page), ignore presses until the save is done, and have no axe violations in any state.
 */
import axe from "axe-core";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { QuickAskSettings as Settings, QuickAskView } from "../api";
import { byText, cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { AppSettings } from "./AppSettings";
import { QuickAskSettings } from "./QuickAskSettings";

beforeEach(installDomShims);
afterEach(cleanup);

const listening: QuickAskView = {
  settings: { enabled: true, shortcut: "Ctrl+Shift+Space" },
  status: { listening: true, error: null, wayland_fallback: false, state: "listening" },
};

async function violations(root: Element): Promise<string[]> {
  const r = await axe.run(root, { rules: { "color-contrast": { enabled: false }, region: { enabled: false } } });
  return r.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

/** A held answer the test settles. */
function deferred<T>() {
  let resolve: (v: T) => void = () => {};
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

describe("settings keep focus while saving (LL2)", () => {
  it("open at login: the switch keeps focus and ignores a second press while saving", async () => {
    const held = deferred<{ supported: boolean; enabled: boolean }>();
    const sent: unknown[] = [];
    mockDaemon({
      autostart_get: { supported: true, enabled: false },
      autostart_set: (a: unknown) => {
        sent.push(a);
        return held.promise;
      },
      platform_settings_get: () => new Promise(() => {}),
      quickask_settings_get: listening,
    });
    const { container } = await render(<AppSettings />);
    await flush();
    const box = container.querySelector<HTMLInputElement>('input[role="switch"]')!;
    box.focus();
    await act(async () => box.click());
    expect(box.disabled).toBe(false);
    expect(box.getAttribute("aria-disabled")).toBe("true");
    expect(document.activeElement).toBe(box);
    await act(async () => box.click());
    expect(sent).toHaveLength(1);
    expect(await violations(container)).toEqual([]);
    await act(async () => held.resolve({ supported: true, enabled: true }));
    await flush();
    expect(box.checked).toBe(true);
    expect(box.getAttribute("aria-disabled")).toBeNull();
    expect(document.activeElement).toBe(box);
  });

  it("quick ask: Save keeps focus after saving (the shortcut is then unchanged) and a press does nothing", async () => {
    const saved: Settings[] = [];
    mockDaemon({
      quickask_settings_get: listening,
      quickask_settings_set: (a: unknown) => {
        const s = (a as { settings: Settings }).settings;
        saved.push(s);
        return { settings: s, status: listening.status };
      },
    });
    const { container } = await render(<QuickAskSettings />);
    await flush();
    const input = container.querySelector<HTMLInputElement>('input[type="text"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "Ctrl+Alt+K");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const save = byText(container, "button", "Save") as HTMLButtonElement;
    save.focus();
    await act(async () => save.click());
    await flush();
    expect(saved).toEqual([{ enabled: true, shortcut: "Ctrl+Alt+K" }]);
    expect(save.disabled).toBe(false);
    expect(save.getAttribute("aria-disabled")).toBe("true");
    expect(document.activeElement).toBe(save);
    await act(async () => save.click());
    expect(saved).toHaveLength(1);
    expect(await violations(container)).toEqual([]);
  });

  it("quick ask: a failed load is an alert, not a blank section", async () => {
    mockDaemon({
      quickask_settings_get: () => {
        throw { kind: "other", message: "The quick ask settings file could not be read." };
      },
    });
    const { container } = await render(<QuickAskSettings />);
    await flush();
    expect(container.querySelector('[role="alert"]')?.textContent).toBe("The quick ask settings file could not be read.");
    expect(await violations(container)).toEqual([]);
  });
});
