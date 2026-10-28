// @vitest-environment jsdom
/**
 * Accessibility: render every page and every onboarding step against mocked daemon data and run
 * axe-core over the result. jsdom can't compute layout, so colour contrast is checked by eye,
 * not here; everything else axe knows (names, roles, labels, landmarks, ARIA) is.
 */
import axe from "axe-core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { PageId } from "./api";
import { App } from "./App";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "./test/render";
import { Onboarding } from "./onboarding/Onboarding";
import { STEPS, type OnboardingState } from "./onboarding/machine";
import { loadLocale } from "./i18n";

/** Each page, and text from the mocked daemon that proves it rendered real content. */
const PAGES: [PageId, string][] = [
  ["chat", "Light"],
  ["search", "Search"],
  ["knowledge", "Ada Lovelace"],
  ["sources", "Work mail"],
  ["apps", "Claude Desktop"],
  ["plugins", "RSS feeds"],
  ["models", "qwen3-4b-q4"],
  ["privacy", "api.anthropic.com"],
  ["status", "local-chat"],
];

async function violations(root: Element = document.body): Promise<string[]> {
  const result = await axe.run(root, { rules: { "color-contrast": { enabled: false } } });
  return result.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

beforeEach(() => {
  installDomShims();
  mockDaemon();
});
afterEach(async () => {
  cleanup();
  await loadLocale("en");
});

describe("axe", () => {
  it.each(PAGES)("page %s has no violations", async (page, expected) => {
    localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true }));
    localStorage.setItem("nugget.page", page);
    await render(<App />);
    await flush(10);
    expect(document.querySelector("main")?.textContent).toContain(expected);
    expect(await violations()).toEqual([]);
  });

  it.each(STEPS.map((s) => [s]))("onboarding step %s has no violations", async (step) => {
    const state: OnboardingState = { version: 1, step, completed: [], skipped: [], ai: "local", finished: false };
    await render(<Onboarding connected initial={state} onExit={() => {}} />);
    await flush(5);
    expect(await violations()).toEqual([]);
  });

  it("bring-your-own-key guidance has no violations", async () => {
    const state: OnboardingState = { version: 1, step: "ai", completed: [], skipped: [], ai: "byok", finished: false };
    await render(<Onboarding connected initial={state} onExit={() => {}} />);
    expect(await violations()).toEqual([]);
  });

  it("the add-source wizard opened from onboarding has no violations", async () => {
    const state: OnboardingState = { version: 1, step: "source", completed: [], skipped: [], ai: null, finished: false };
    const { container } = await render(<Onboarding connected initial={state} onExit={() => {}} />);
    await click(container.querySelector(".ob-main .btn-primary"));
    expect(document.querySelector("dialog")).not.toBeNull();
    expect(await violations()).toEqual([]);
  });

  it("right-to-left layout sets dir and lang, and stays clean", async () => {
    await loadLocale("ar");
    expect(document.documentElement.dir).toBe("rtl");
    expect(document.documentElement.lang).toBe("ar");
    const state: OnboardingState = { version: 1, step: "welcome", completed: [], skipped: [], ai: null, finished: false };
    const { container } = await render(<Onboarding connected initial={state} onExit={() => {}} />);
    expect(container.querySelector("h1")?.textContent).toBe("مرحبًا بك في Nugget");
    expect(await violations()).toEqual([]);
  });
});
