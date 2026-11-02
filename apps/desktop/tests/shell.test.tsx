// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { App, PAGES } from "../src/App";
import { t } from "../src/i18n";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../src/test/render";
import { finishedOnboarding, key } from "./keys";

const navButtons = () => Array.from(document.querySelectorAll<HTMLButtonElement>("nav .nav-item"));
const mainLabel = () => document.querySelector("main")?.getAttribute("aria-label");
const helpDialog = () =>
  Array.from(document.querySelectorAll("dialog[open]")).find((d) => d.querySelector("h2")?.textContent === t("shortcuts.title"));

beforeEach(() => {
  installDomShims();
  mockDaemon();
  finishedOnboarding();
});
afterEach(cleanup);

describe("navigation", () => {
  it("renders the nav from the page registry, with shortcut hints", async () => {
    await render(<App />);
    const main = PAGES.filter((p) => p.place === "main");
    expect(navButtons().slice(0, main.length).map((b) => b.textContent)).toEqual(main.map((p) => t(p.label)));
    expect(navButtons()[0]!.getAttribute("aria-keyshortcuts")).toBe("Control+1");
    expect(navButtons()[1]!.title).toBe("Needs you (Ctrl+2)");
    expect(navButtons()[0]!.getAttribute("aria-current")).toBe("page");
  });

  it("Ctrl+1…n jumps to the nth page and moves focus into it", async () => {
    await render(<App />);
    for (let i = 0; i < Math.min(PAGES.length, 9); i++) {
      await key({ key: String(i + 1), code: `Digit${i + 1}`, ctrlKey: true }, document.body);
      await flush(3);
      expect(mainLabel()).toBe(t(PAGES[i]!.label));
    }
    expect(document.querySelector("main")!.contains(document.activeElement)).toBe(true);
  });

  it("ignores shortcuts during IME composition", async () => {
    await render(<App />);
    await key({ key: "2", code: "Digit2", ctrlKey: true, isComposing: true }, document.body);
    expect(mainLabel()).toBe(t("app.nav.today"));
  });
});

describe("shortcuts help", () => {
  it("opens with ? and closes with Escape", async () => {
    await render(<App />);
    navButtons()[0]!.focus();
    await key({ key: "?", shiftKey: true });
    const dlg = helpDialog();
    expect(dlg).toBeDefined();
    expect(dlg!.textContent).toContain("Ctrl+K");
    await key({ key: "Escape" });
    expect(helpDialog()).toBeUndefined();
  });

  it("does not open while typing in a text field", async () => {
    await render(<App />);
    const ta = document.createElement("textarea");
    document.querySelector("main")!.appendChild(ta);
    ta.focus();
    await key({ key: "?", shiftKey: true }, ta);
    expect(helpDialog()).toBeUndefined();
    const input = document.createElement("input");
    document.querySelector("main")!.appendChild(input);
    await key({ key: "?", shiftKey: true }, input);
    expect(helpDialog()).toBeUndefined();
  });
});
