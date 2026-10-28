// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { App, PAGES } from "../src/App";
import { t } from "../src/i18n";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../src/test/render";
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
    expect(navButtons().map((b) => b.textContent)).toEqual(PAGES.map((p) => t(p.label)));
    expect(navButtons()[0]!.getAttribute("aria-keyshortcuts")).toBe("Control+1");
    expect(navButtons()[1]!.title).toBe("Chat (Ctrl+2)");
    expect(navButtons()[0]!.getAttribute("aria-current")).toBe("page");
  });

  it("restores the last page and switches on click", async () => {
    finishedOnboarding("models");
    await render(<App />);
    expect(mainLabel()).toBe(t("app.nav.models"));
    await click(navButtons()[3]);
    expect(mainLabel()).toBe(t("app.nav.knowledge"));
    expect(localStorage.getItem("nugget.page")).toBe("knowledge");
  });

  it("Ctrl+1…n jumps to the nth page and moves focus into it", async () => {
    await render(<App />);
    for (let i = 0; i < PAGES.length; i++) {
      await key({ key: String(i + 1), code: `Digit${i + 1}`, ctrlKey: true }, document.body);
      await flush(3);
      expect(mainLabel()).toBe(t(PAGES[i]!.label));
    }
    expect(document.querySelector("main")!.contains(document.activeElement)).toBe(true);
  });

  it("Ctrl+, opens Status; Meta+digit is not the modifier off macOS", async () => {
    await render(<App />);
    await key({ key: ",", code: "Comma", ctrlKey: true }, document.body);
    expect(mainLabel()).toBe(t("app.nav.status"));
    await key({ key: "2", code: "Digit2", metaKey: true }, document.body);
    expect(mainLabel()).toBe(t("app.nav.status"));
  });

  it("ignores shortcuts during IME composition", async () => {
    await render(<App />);
    await key({ key: "2", code: "Digit2", ctrlKey: true, isComposing: true }, document.body);
    expect(mainLabel()).toBe(t("app.nav.today"));
  });

  it("arrow keys move focus between nav items", async () => {
    await render(<App />);
    navButtons()[0]!.focus();
    await key({ key: "ArrowDown" });
    expect(document.activeElement).toBe(navButtons()[1]);
    await key({ key: "ArrowUp" });
    await key({ key: "ArrowUp" });
    expect(document.activeElement).toBe(navButtons()[PAGES.length - 1]);
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

  it("opens with Ctrl+/", async () => {
    await render(<App />);
    await key({ key: "/", code: "Slash", ctrlKey: true }, document.body);
    expect(helpDialog()).toBeDefined();
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

  it("modifier shortcuts still work from a text field", async () => {
    await render(<App />);
    const ta = document.createElement("textarea");
    document.querySelector("main")!.appendChild(ta);
    ta.focus();
    await key({ key: "k", code: "KeyK", ctrlKey: true }, ta);
    expect(document.querySelector("dialog.palette[open]")).not.toBeNull();
  });
});
