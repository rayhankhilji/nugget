// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import type { DoubleTapSettings, DoubleTapStatus, TriggerSpec } from "../api";
import { byText, cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { TriggerStep } from "./TriggerStep";
import { MIGRATED_KEY, migrateTriggerOnce, readTrigger, TRIGGER_KEY, writeTrigger, type TriggerPref } from "./trigger";

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
  it("writes through the shell's settings, keeping the rest of them", async () => {
    const { state } = shell(settings());
    const saved = await writeTrigger({ kind: "double_tap", modifier: "left_shift", window_ms: 500 });
    expect(saved).toEqual({ kind: "double_tap", modifier: "left_shift", window_ms: 500 });
    expect(state.sets).toEqual([{ ...settings(), trigger: { kind: "double_tap", modifier: "left_shift", window_ms: 500 } }]);
    expect(JSON.parse(localStorage.getItem(TRIGGER_KEY)!)).toEqual(saved);
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
});

describe("migration at start", () => {
  it("runs once, before and without the trigger step", async () => {
    const { state, calls } = shell(settings());
    local({ kind: "chord", accelerator: "Alt+Space" });
    await Promise.all([migrateTriggerOnce(), migrateTriggerOnce()]);
    expect(state.sets.map((s) => s.trigger)).toEqual([{ kind: "chord", chord: "Alt+Space" }]);
    expect(localStorage.getItem(MIGRATED_KEY)).not.toBeNull();
    // Done: neither a later start nor onboarding's step migrate again.
    const before = calls.length;
    await migrateTriggerOnce();
    expect(calls.length).toBe(before);
    expect(await readTrigger("linux")).toEqual({ kind: "chord", accelerator: "Alt+Space" });
    expect(state.sets).toHaveLength(1);
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
});
