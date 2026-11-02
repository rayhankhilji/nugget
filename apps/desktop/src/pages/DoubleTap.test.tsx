// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
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


beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("Double Tap settings page", () => {
  it("GG6-19: on macOS with Accessibility denied, explains how to turn it back on and opens the pane", async () => {
    const panes: string[] = [];
    const mac: DoubleTapStatus = { ...status, platform: "macos", session: "macos", trigger_error: null, input_monitoring: "unknown", hint: "doubletap.hint.accessibility" };
    setup({
      doubletap_status: mac,
      doubletap_request_permissions: mac,
      doubletap_open_privacy_settings: (args: unknown) => (panes.push((args as { pane: string }).pane), true),
    });
    await render(<App />);
    await flush(10);
    const box = document.querySelector<HTMLElement>(".dt-recover")!;
    expect(box).not.toBeNull();
    expect(box.textContent).toContain("Turn Accessibility back on");
    expect(box.textContent).toContain("System Settings → Privacy & Security → Accessibility");
    await click(byText(box, "button", "Open Accessibility settings"));
    await click(byText(box, "button", "Open Input Monitoring settings"));
    expect(panes).toEqual(["accessibility", "input_monitoring"]);
    expect(box.querySelector('[role="alert"]')).toBeNull();
    const results = await axe.run(box);
    expect(results.violations).toEqual([]);
  });

  it("GG6-19: says how to get there by hand when System Settings can't be opened", async () => {
    const mac: DoubleTapStatus = { ...status, platform: "macos", session: "macos", trigger_error: null, input_monitoring: "unknown", hint: null };
    setup({ doubletap_status: mac, doubletap_open_privacy_settings: false });
    await render(<App />);
    await flush(10);
    const box = document.querySelector<HTMLElement>(".dt-recover")!;
    await click(byText(box, "button", "Open Accessibility settings"));
    await flush();
    expect(box.querySelector('[role="alert"]')?.textContent).toContain("Open it from the Apple menu");
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

  it("turns suggestions as you type off, and says when inline suggestions can't run", async () => {
    const { sent } = setup({ doubletap_status: { ...status, platform: "macos", ghost: { available: false, error: "unsupported" } } });
    await render(<App />);
    await flush(10);
    const label = byText(document, "label", "Suggest as you type");
    const toggle = label?.querySelector<HTMLInputElement>('input[role="switch"]');
    expect(toggle?.checked).toBe(true);
    const hint = document.getElementById(toggle!.getAttribute("aria-describedby")!);
    expect(hint?.textContent).toContain("Press Tab to accept it, ⌥→ to accept one word");
    expect(hint?.textContent).toContain("Never in password, one-time code or card fields");
    expect(document.querySelector("main")!.textContent).toContain("Double Tap opens its panel instead");
    await click(toggle);
    await click(byText(document, "button", "Save"));
    expect((sent[0] as DoubleTapSettings).auto_suggest).toBe(false);
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
});
