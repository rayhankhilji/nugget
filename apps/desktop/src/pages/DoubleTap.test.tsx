// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DoubleTapSettings, DoubleTapStatus, ShortcutStatus, VoiceStatus } from "../api";
import { App } from "../App";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";

const settings: DoubleTapSettings = {
  enabled: true,
  trigger: { kind: "double_tap", modifier: "right_control", window_ms: 350 },
  app_providers: { "mail.google.com": "anthropic" },
  excluded_apps: ["1Password"],
  allow_terminals: false,
};

const status: DoubleTapStatus = {
  platform: "linux",
  session: "wayland",
  listening: false,
  trigger_error: "doubletap.hint.wayland_chord",
  accessibility: "denied",
  input_monitoring: "not_required",
  hint: "doubletap.hint.atspi",
  modifiers: [
    { id: "right_control", label: "Right Ctrl" },
    { id: "left_shift", label: "Left Shift" },
  ],
  recommended_trigger: { kind: "chord", chord: "Ctrl+Alt+Space" },
};

const voice: VoiceStatus = { samples: 1234, channels: ["email", "chat"], last_built: "2026-10-28T11:00:00Z" };

function setup(extra: Record<string, unknown> = {}) {
  const sent: unknown[] = [];
  const calls = mockDaemon({
    doubletap_settings_get: settings,
    doubletap_status: status,
    voice_status: voice,
    doubletap_default_excluded: ["1Password", "Bitwarden", "chase.com"],
    doubletap_settings_set: (args: unknown) => {
      const s = (args as { settings: DoubleTapSettings }).settings;
      sent.push(s);
      return s;
    },
    doubletap_request_permissions: status,
    doubletap_fire: null,
    ...extra,
  });
  localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true }));
  localStorage.setItem("nugget.page", "doubletap");
  return { calls, sent };
}

async function change(el: Element | null | undefined, value: string) {
  if (!el) throw new Error("change: element not found");
  const input = el as HTMLInputElement;
  const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  await act(async () => {
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event(el instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
  await flush(2);
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("Double Tap settings page", () => {
  it("renders settings, status and voice, with no axe violations", async () => {
    setup();
    await render(<App />);
    await flush(10);
    const main = document.querySelector("main")!;
    expect(main.textContent).toContain("mail.google.com");
    expect(main.textContent).toContain("1Password");
    expect(main.textContent).toContain("AT-SPI");
    expect(main.textContent).toContain("Wayland");
    expect(main.textContent).toContain("1,234");
    expect(main.textContent).toContain("email");
    const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
  });

  it("edits and saves normalised settings", async () => {
    const { sent } = setup();
    await render(<App />);
    await flush(10);
    const save = byText(document, "button", "Save") as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    await click(byText(document, "button", "Use recommended"));
    expect((document.querySelector('input[placeholder^="e.g. Ctrl"]') as HTMLInputElement).value).toBe("Ctrl+Alt+Space");

    await click(byText(document, "button", "Restore defaults"));
    expect(document.querySelector("main")!.textContent).toContain("chase.com");

    await click(document.querySelector('button[aria-label="Remove mail.google.com"]'));
    expect(save.disabled).toBe(false);
    await click(save);
    expect(sent).toEqual([
      {
        enabled: true,
        trigger: { kind: "chord", chord: "Ctrl+Alt+Space" },
        app_providers: {},
        excluded_apps: ["1Password", "Bitwarden", "chase.com"],
        allow_terminals: false,
      },
    ]);
    expect(document.querySelector("main")!.textContent).toContain("Saved.");
  });

  it("blocks saving an invalid chord", async () => {
    setup();
    await render(<App />);
    await flush(10);
    const chordRadio = Array.from(document.querySelectorAll<HTMLInputElement>('input[type="radio"]'))[1];
    await click(chordRadio);
    const chord = document.querySelector('input[placeholder^="e.g. Ctrl"]');
    await change(chord, "Space");
    expect(chord?.getAttribute("aria-invalid")).toBe("true");
    expect((byText(document, "button", "Save") as HTMLButtonElement).disabled).toBe(true);
  });

  it("follows a trigger saved in onboarding, but keeps unsaved edits", async () => {
    let current = settings;
    setup({ doubletap_settings_get: () => current });
    await render(<App />);
    await flush(10);
    const chordInput = () => document.querySelector<HTMLInputElement>('input[placeholder^="e.g. Ctrl"]');
    expect(chordInput()).toBeNull();

    current = { ...settings, trigger: { kind: "chord", chord: "Alt+Space" } };
    await act(() => emit("doubletap:settings", current));
    await flush(5);
    expect(chordInput()?.value).toBe("Alt+Space");
    expect((byText(document, "button", "Save") as HTMLButtonElement).disabled).toBe(true);

    // With unsaved edits, the form is left alone (the saved copy still updates underneath).
    await change(chordInput(), "Ctrl+Alt+K");
    current = { ...settings, trigger: { kind: "chord", chord: "CmdOrCtrl+Shift+Space" } };
    await act(() => emit("doubletap:settings", current));
    await flush(5);
    expect(chordInput()?.value).toBe("Ctrl+Alt+K");
  });

  it("turns read-ahead off and explains what it reads", async () => {
    const { sent } = setup();
    await render(<App />);
    await flush(10);
    const label = byText(document, "label", "Read ahead while you reply");
    const toggle = label?.querySelector<HTMLInputElement>('input[role="switch"]');
    // On when the shell doesn't say (an older shell), and described for screen readers.
    expect(toggle?.checked).toBe(true);
    const hint = document.getElementById(toggle!.getAttribute("aria-describedby")!);
    expect(hint?.textContent).toContain("Local models only");
    expect(hint?.textContent).toContain("Nothing is sent to a cloud provider");

    await click(toggle);
    expect(toggle?.checked).toBe(false);
    await click(byText(document, "button", "Save"));
    expect((sent[0] as DoubleTapSettings).read_ahead).toBe(false);
  });

  it("shows read-ahead as saved by the shell", async () => {
    setup({ doubletap_settings_get: { ...settings, read_ahead: false } });
    await render(<App />);
    await flush(10);
    const label = byText(document, "label", "Read ahead while you reply");
    expect(label?.querySelector<HTMLInputElement>('input[role="switch"]')?.checked).toBe(false);
    expect((byText(document, "button", "Save") as HTMLButtonElement).disabled).toBe(true);
  });

  it("adds a provider row and fires a test draft", async () => {
    const { calls, sent } = setup();
    await render(<App />);
    await flush(10);
    await change(document.querySelector('input[placeholder^="e.g. com.tinyspeck"]'), "slack");
    await click(byText(document, "button", "Add"));
    await click(byText(document, "button", "Save"));
    expect((sent[0] as DoubleTapSettings).app_providers).toEqual({ "mail.google.com": "anthropic", slack: "local-chat" });
    await click(byText(document, "button", "Try it"));
    expect(calls).toContain("doubletap_fire");
  });

  it("shows the desktop's shortcut consent and checks back until it is approved", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    try {
      let shortcut: ShortcutStatus = { state: "awaiting_consent", trigger: null, error: null };
      const { calls } = setup({ doubletap_status: () => ({ ...status, listening: true, trigger_error: null, shortcut }) });
      const polls = () => calls.filter((c) => c === "doubletap_status").length;
      await render(<App />);
      await flush(10);
      const note = () => document.querySelector("main")!.textContent;
      expect(note()).toContain("Approve it in the dialog");

      shortcut = { state: "active", trigger: "Super+D", error: null };
      await act(async () => {
        vi.advanceTimersByTime(2000);
      });
      await flush(5);
      expect(note()).toContain("Your desktop starts Double Tap with Super+D.");

      // Active: no more checking back.
      const settled = polls();
      await act(async () => {
        vi.advanceTimersByTime(6000);
      });
      await flush(3);
      expect(polls()).toBe(settled);
    } finally {
      vi.useRealTimers();
    }
  });

  it("alerts when the desktop shortcut fails", async () => {
    setup({
      doubletap_status: { ...status, listening: true, trigger_error: null, shortcut: { state: "failed", trigger: null, error: "portal gone" } },
    });
    await render(<App />);
    await flush(10);
    const alerts = Array.from(document.querySelectorAll('[role="alert"]')).map((a) => a.textContent);
    expect(alerts).toContain("The desktop shortcut stopped working: portal gone");
  });
});
