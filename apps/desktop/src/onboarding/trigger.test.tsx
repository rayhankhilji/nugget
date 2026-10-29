// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import type { DoubleTapSettings, DoubleTapStatus, TriggerSpec } from "../api";
import { byText, cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { TriggerStep } from "./TriggerStep";
import { MIGRATED_KEY, readTrigger, TRIGGER_KEY, writeTrigger, type TriggerPref } from "./trigger";

const recommended: TriggerSpec = { kind: "double_tap", modifier: "right_control", window_ms: 350 };

function settings(trigger: TriggerSpec = recommended): DoubleTapSettings {
  return { enabled: true, trigger, app_providers: { "mail.google.com": "local" }, excluded_apps: ["1Password"], allow_terminals: true };
}

/** The shell's Double Tap settings, mocked: `get`/`set` keep one value, like `doubletap.json`. */
function shell(initial: DoubleTapSettings, opts: { refuse?: boolean } = {}) {
  const state = { saved: initial, sets: [] as DoubleTapSettings[] };
  const calls = mockDaemon({
    doubletap_settings_get: () => state.saved,
    doubletap_settings_set: (args: unknown) => {
      const s = (args as { settings: DoubleTapSettings }).settings;
      if (opts.refuse) throw { kind: "other", message: "enter a shortcut such as Ctrl+Alt+Space" };
      state.sets.push(s);
      state.saved = s;
      return s;
    },
    doubletap_status: { recommended_trigger: recommended } as DoubleTapStatus,
  });
  return { state, calls };
}

const local = (p: TriggerPref) => localStorage.setItem(TRIGGER_KEY, JSON.stringify(p));

beforeEach(installDomShims);
afterEach(cleanup);

describe("trigger source of truth", () => {
  it("reads what Double Tap runs with and caches it", async () => {
    shell(settings({ kind: "chord", chord: "Ctrl+Alt+J" }));
    expect(await readTrigger("linux")).toEqual({ kind: "chord", accelerator: "Ctrl+Alt+J" });
    expect(JSON.parse(localStorage.getItem(TRIGGER_KEY)!)).toEqual({ kind: "chord", accelerator: "Ctrl+Alt+J" });
  });

  it("writes through the shell's settings, keeping the rest of them", async () => {
    const { state } = shell(settings());
    const saved = await writeTrigger({ kind: "double_tap", modifier: "left_shift", window_ms: 500 });
    expect(saved).toEqual({ kind: "double_tap", modifier: "left_shift", window_ms: 500 });
    expect(state.sets).toEqual([{ ...settings(), trigger: { kind: "double_tap", modifier: "left_shift", window_ms: 500 } }]);
    expect(JSON.parse(localStorage.getItem(TRIGGER_KEY)!)).toEqual(saved);
  });

  it("keeps the previous local copy when the shell refuses", async () => {
    shell(settings(), { refuse: true });
    local({ kind: "double_tap", modifier: "right_control", window_ms: 350 });
    await expect(writeTrigger({ kind: "chord", accelerator: "Nope" })).rejects.toThrow(/shortcut/);
    expect(JSON.parse(localStorage.getItem(TRIGGER_KEY)!)).toMatchObject({ kind: "double_tap" });
  });

  it("migrates an older onboarding choice once, onto the default trigger", async () => {
    const { state } = shell(settings());
    local({ kind: "chord", accelerator: "Alt+Space" });
    expect(await readTrigger("linux")).toEqual({ kind: "chord", accelerator: "Alt+Space" });
    expect(state.sets.map((s) => s.trigger)).toEqual([{ kind: "chord", chord: "Alt+Space" }]);
    expect(localStorage.getItem(MIGRATED_KEY)).not.toBeNull();
    // Changed later in Double Tap settings: the old choice doesn't come back.
    state.saved = settings();
    expect(await readTrigger("linux")).toEqual({ kind: "double_tap", modifier: "right_control", window_ms: 350 });
    expect(state.sets).toHaveLength(1);
  });

  it("doesn't migrate over a trigger chosen in Double Tap settings", async () => {
    const chosen: TriggerSpec = { kind: "double_tap", modifier: "left_shift", window_ms: 400 };
    const { state } = shell(settings(chosen));
    local({ kind: "chord", accelerator: "Alt+Space" });
    expect(await readTrigger("linux")).toEqual({ kind: "double_tap", modifier: "left_shift", window_ms: 400 });
    expect(state.sets).toEqual([]);
    expect(localStorage.getItem(MIGRATED_KEY)).not.toBeNull();
  });

  it("outside the app, keeps the choice locally", async () => {
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    expect(await readTrigger("macos")).toEqual({ kind: "double_tap", modifier: "right_command", window_ms: 350 });
    await writeTrigger({ kind: "chord", accelerator: "Alt+Space" });
    expect(await readTrigger("macos")).toEqual({ kind: "chord", accelerator: "Alt+Space" });
  });
});

async function change(el: Element | null | undefined, value: string) {
  if (!el) throw new Error("change: element not found");
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await flush();
}

const step = () => <TriggerStep state={{} as never} dispatch={() => {}} connected headingRef={{ current: null }} />;
const selects = (c: HTMLElement) => Array.from(c.querySelectorAll("select"));

describe("TriggerStep", () => {
  it("shows the shell's trigger, including values onboarding doesn't offer", async () => {
    shell(settings({ kind: "chord", chord: "Ctrl+Alt+J" }));
    const { container } = await render(step());
    const radios = container.querySelectorAll<HTMLInputElement>('input[type="radio"]');
    expect(radios[1]!.checked).toBe(true);
    expect(selects(container)[0]!.value).toBe("Ctrl+Alt+J");
  });

  it("saves each change to the shell and follows changes made in Double Tap settings", async () => {
    const { state } = shell(settings());
    const { container } = await render(step());
    expect(selects(container)[0]!.value).toBe("right_control");
    await change(selects(container)[0], "left_shift");
    await change(selects(container)[1], "500");
    expect(state.saved.trigger).toEqual({ kind: "double_tap", modifier: "left_shift", window_ms: 500 });
    expect(state.sets).toHaveLength(2);

    // Saved on the Double Tap settings page: the shell announces it.
    await act(() => emit("doubletap:settings", settings({ kind: "double_tap", modifier: "right_shift", window_ms: 420 })));
    await flush();
    expect(selects(container)[0]!.value).toBe("right_shift");
    expect(selects(container)[1]!.value).toBe("420");
    expect(byText(container, "option", "420 ms")).toBeDefined();
  });

  it("reports a refused save and shows the trigger that still applies", async () => {
    shell(settings(), { refuse: true });
    const { container } = await render(step());
    await change(selects(container)[0], "left_shift");
    expect(container.querySelector('[role="alert"]')?.textContent).toMatch(/Couldn't save: .*shortcut/);
    expect(selects(container)[0]!.value).toBe("right_control");
  });
});
