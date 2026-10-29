// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { act } from "react";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { Onboarding } from "./Onboarding";
import { loadState, STORAGE_KEY } from "./machine";
import { TRIGGER_KEY } from "./trigger";
import type { DoubleTapSettings } from "../api";

beforeEach(() => {
  installDomShims();
});
afterEach(cleanup);

const heading = (c: HTMLElement) => c.querySelector("h1")?.textContent;
const button = (c: HTMLElement, text: string | RegExp) => byText(c, "button", text);

describe("Onboarding", () => {
  it("walks every step, saving progress, and exits when finished", async () => {
    let dt: DoubleTapSettings = {
      enabled: true,
      trigger: { kind: "double_tap", modifier: "right_control", window_ms: 350 },
      app_providers: {},
      excluded_apps: [],
      allow_terminals: false,
    };
    mockDaemon({
      doubletap_settings_get: () => dt,
      doubletap_settings_set: (args: unknown) => (dt = (args as { settings: DoubleTapSettings }).settings),
    });
    const onExit = vi.fn();
    const { container } = await render(<Onboarding connected onExit={onExit} />);
    expect(heading(container)).toBe("Welcome to Nugget");

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Choose your AI");
    expect(document.activeElement).toBe(container.querySelector("h1"));
    expect(loadState().step).toBe("ai");

    // Local: recommended models with sizes, install with progress.
    const local = container.querySelector<HTMLInputElement>('input[type="radio"][value="local"]')!;
    await click(local);
    await flush();
    expect(container.textContent).toContain("qwen3-4b-q4");
    expect(container.textContent).toContain("2.5 GB download");
    expect(container.textContent).not.toContain("llama-8b-q4");
    await click(button(container, /Show all models/));
    expect(container.textContent).toContain("llama-8b-q4");
    await click(byText(container, "li.card button", "Install"));
    await act(() => emit("nugget:models-progress", { id: "qwen3-4b-q4", downloaded: 1_250_000_000, total: 2_500_000_000, phase: "downloading", error: null }));
    await flush();
    expect(container.querySelector("progress")?.getAttribute("value")).toBe("50");
    expect(container.textContent).toContain("Downloading · 50% · 1.3 GB of 2.5 GB");

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Add your first source");
    expect(loadState().ai).toBe("local");
    await click(button(container, "Add a source"));
    expect(document.querySelector("dialog")).not.toBeNull();
    await click(document.querySelector<HTMLElement>('dialog button[aria-label="Close"]'));
    expect(document.querySelector("dialog")).toBeNull();

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Permissions, explained");
    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Choose your Double Tap trigger");
    await click(container.querySelectorAll<HTMLInputElement>('input[type="radio"]')[1]!);
    // Saved where Double Tap reads it (and cached locally).
    expect(dt.trigger).toMatchObject({ kind: "chord" });
    expect(JSON.parse(localStorage.getItem(TRIGGER_KEY)!)).toMatchObject({ kind: "chord" });

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("You're all set");
    expect(container.textContent).toContain("Every step is done.");
    expect(onExit).not.toHaveBeenCalled();
    await click(button(container, "Start using Nugget"));
    expect(onExit).toHaveBeenCalled();
    expect(loadState().finished).toBe(true);
  });

  it("skips steps, lists them at the end and lets the user go back", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await click(button(container, "Continue"));
    for (let i = 0; i < 4; i++) await click(button(container, "Skip this step"));
    expect(heading(container)).toBe("You're all set");
    expect(container.textContent).toContain("You skipped 4 steps.");
    await click(byText(container, ".ob-pending button", "First source"));
    expect(heading(container)).toBe("Add your first source");
  });

  it("can be skipped entirely, and resumes where it was left", async () => {
    mockDaemon();
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ version: 1, step: "permissions", completed: ["welcome"], skipped: [], ai: "byok", finished: false }));
    const onExit = vi.fn();
    const { container } = await render(<Onboarding connected onExit={onExit} />);
    expect(heading(container)).toBe("Permissions, explained");
    await click(button(container, "Skip setup"));
    expect(onExit).toHaveBeenCalled();
    expect(loadState()).toMatchObject({ finished: true, skipped: ["ai", "source", "permissions", "trigger"] });
  });

  it("works without the background service", async () => {
    const { container } = await render(<Onboarding connected={false} onExit={() => {}} />);
    await click(button(container, "Continue"));
    await click(container.querySelector<HTMLInputElement>('input[value="local"]'));
    expect(container.textContent).toContain("isn't running");
    await click(button(container, "Continue"));
    expect(button(container, "Add a source")).toBeUndefined();
    expect(container.textContent).toContain("isn't running");
  });

  it("guides bring-your-own-key without ever asking for the key", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await click(button(container, "Continue"));
    await click(container.querySelector<HTMLInputElement>('input[value="byok"]'));
    expect(container.querySelector('input[type="password"], input[type="text"], textarea')).toBeNull();
    expect(container.textContent).toContain('api_key = "keychain:anthropic"');
    expect(container.textContent).toContain("dev.nugget.Nugget");
  });
});
