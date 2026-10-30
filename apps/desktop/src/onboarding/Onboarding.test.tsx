// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { act } from "react";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { Onboarding } from "./Onboarding";
import { INITIAL, loadState, STORAGE_KEY } from "./machine";
import { indexPhase } from "./steps";
import { TRIGGER_KEY } from "./trigger";
import type { DoubleTapSettings, SourceInfo } from "../api";

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
    // A download in progress doesn't set the step up yet (it can still fail): Continue would skip it.
    const hint = () => container.querySelector("#ob-continue-hint");
    expect(hint()?.textContent).toBe("Nothing set up here yet: Continue skips this step.");
    expect(button(container, "Continue")!.getAttribute("aria-describedby")).toBe("ob-continue-hint");
    await act(() => emit("nugget:models-progress", { id: "qwen3-4b-q4", downloaded: 2_500_000_000, total: 2_500_000_000, phase: "installed", error: null }));
    await flush();
    expect(hint()).toBeNull();
    expect(button(container, "Continue")!.hasAttribute("aria-describedby")).toBe(false);

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Add your first source");
    expect(loadState().ai).toBe("local");
    // "Add a folder" opens the folder form directly, skipping the kind picker.
    await click(button(container, "Add a folder"));
    expect(document.querySelector("dialog")).not.toBeNull();
    expect(byText(document.body, "dialog button", "Choose folders…")).toBeDefined();
    await click(document.querySelector<HTMLElement>('dialog button[aria-label="Close"]'));
    expect(document.querySelector("dialog")).toBeNull();
    // The sources the daemon already has count: wait until the step knows about them.
    await flush();
    expect(hint()).toBeNull();

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("What Double Tap needs");
    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Choose your Double Tap trigger");
    await click(container.querySelectorAll<HTMLInputElement>('input[type="radio"]')[1]!);
    // Saved where Double Tap reads it (and cached locally).
    expect(dt.trigger).toMatchObject({ kind: "chord" });
    expect(JSON.parse(localStorage.getItem(TRIGGER_KEY)!)).toMatchObject({ kind: "chord" });

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("You're all set");
    expect(container.textContent).toContain("Every step is done.");
    // The summary says what was set up.
    const summary = [...container.querySelectorAll(".ob-summary > div")].map((d) => d.textContent);
    expect(summary[0]).toBe("AI modelOn this device (qwen3-4b-q4)");
    expect(summary[1]).toBe("SourcesDocuments and Work mail");
    expect(container.querySelectorAll(".ob-summary kbd").length).toBeGreaterThan(0);
    // "Skip setup" keeps its slot on the last step but is hidden.
    expect(button(container, "Skip setup")!.classList.contains("ob-invisible")).toBe(true);
    expect(onExit).not.toHaveBeenCalled();
    // The mocked daemon has an indexed source, so the last step points at Search.
    expect(container.textContent).toContain("1,234 items are indexed and searchable.");
    await click(button(container, "Search your sources"));
    expect(onExit).toHaveBeenCalledWith("search");
    expect(loadState().finished).toBe(true);
  });

  it("shows indexing progress for a new folder and says when search works", async () => {
    const folder: SourceInfo = { id: "files-9", kind: "files", name: "notes", enabled: true, state: "syncing", items: 0, chunks: 0 };
    let listed: SourceInfo[] = [];
    mockDaemon({
      pick_folders: () => ["/home/me/notes"],
      sources_add: () => ((listed = [folder]), folder),
      sources_list: () => listed,
    });
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "source", completed: ["welcome", "ai"] }} />);
    await click(button(container, "Add a folder"));
    await click(byText(document.body, "dialog button", "Choose folders…"));
    expect(document.querySelector("dialog code")?.textContent).toBe("/home/me/notes");
    await click(byText(document.body, "dialog button", "Add folder"));
    await flush();
    expect(document.querySelector("dialog")).toBeNull();
    expect(container.querySelector(".ob-index li")?.textContent).toContain("notes: getting ready to index…");

    listed = [{ ...folder, items: 5 }];
    await act(() => emit("nugget:sources-changed", {}));
    await flush();
    expect(container.querySelector(".ob-index li")?.textContent).toContain("notes: indexing, 5 items so far…");

    listed = [{ ...folder, state: "watching", items: 93, chunks: 120 }];
    await act(() => emit("nugget:sources-changed", {}));
    await flush();
    const row = container.querySelector<HTMLElement>(".ob-index li")!;
    expect(row.dataset.phase).toBe("ready");
    expect(row.textContent).toContain("notes: 93 items indexed. Search works now.");
  });

  it("has nothing to skip on the welcome step", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    expect(button(container, "Skip this step")).toBeUndefined();
    expect(container.querySelector("#ob-continue-hint")).toBeNull();
  });

  it("summarises progress saved before setups were recorded without contradicting it", async () => {
    mockDaemon({ sources_list: [], models_list: [], providers_list: [] });
    const { container } = await render(
      <Onboarding
        connected
        onExit={() => {}}
        initial={{ ...INITIAL, step: "done", ai: "local", completed: ["welcome", "ai", "source", "permissions", "trigger"] }}
      />,
    );
    await flush();
    const summary = [...container.querySelectorAll(".ob-summary > div")].map((d) => d.textContent);
    expect(summary[0]).toBe("AI modelOn this device");
    expect(summary[1]).toBe("SourcesAdded");
    expect(container.textContent).toContain("Every step is done.");
  });

  it("finds a key saved before when setup runs again", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", ai: "byok" }} />);
    await flush();
    // The mocked daemon has an "anthropic" provider in its config.
    expect(container.querySelector("#ob-continue-hint")).toBeNull();
    await click(button(container, "Continue"));
    expect(loadState().completed).toContain("ai");
    expect(loadState().aiSetup).toEqual({ ai: "byok", what: "anthropic" });
  });

  it("offers one language picker on the welcome step, labelled, in the header", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    expect(heading(container)).toBe("Welcome to Nugget");
    const selects = [...container.querySelectorAll("select")].filter((s) => s.labels?.[0]?.textContent === "Language");
    expect(selects).toHaveLength(1);
    expect(selects[0]!.closest("header.ob-top")).not.toBeNull();
  });

  it("finishes to the default page when nothing is searchable yet", async () => {
    mockDaemon({ sources_list: [] });
    const onExit = vi.fn();
    const { container } = await render(<Onboarding connected onExit={onExit} initial={{ ...INITIAL, step: "done" }} />);
    await flush();
    expect(container.textContent).toContain("chat and drafting start once you choose a model");
    expect(button(container, "Search your sources")).toBeUndefined();
    await click(button(container, "Start using Nugget"));
    expect(onExit).toHaveBeenCalledWith(undefined);
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
    expect(heading(container)).toBe("What Double Tap needs");
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

  it("takes a key in a masked field only, and still shows the manual setup", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    await click(button(container, "Continue"));
    await click(container.querySelector<HTMLInputElement>('input[value="byok"]'));
    // The key goes in a password field that the browser won't remember.
    const key = container.querySelectorAll<HTMLInputElement>('input[type="password"]');
    expect(key).toHaveLength(1);
    expect(key[0]!.autocomplete).toBe("off");
    expect(container.querySelector("textarea")).toBeNull();
    expect(container.textContent).toContain('api_key = "keychain:anthropic"');
    expect(container.textContent).toContain("dev.nugget.Nugget");
  });
});

describe("indexPhase", () => {
  it("tells waiting, indexing, searchable and failed apart", () => {
    expect(indexPhase({ state: "syncing", items: 0 })).toBe("waiting");
    expect(indexPhase({ state: "idle", items: 0 })).toBe("waiting");
    expect(indexPhase({ state: "syncing", items: 3 })).toBe("indexing");
    expect(indexPhase({ state: "watching", items: 3 })).toBe("ready");
    expect(indexPhase({ state: "idle", items: 3 })).toBe("ready");
    expect(indexPhase({ state: "error", items: 3, last_error: "denied" })).toBe("failed");
    expect(indexPhase({ state: "idle", items: 0, last_error: "no such folder" })).toBe("failed");
  });
});
