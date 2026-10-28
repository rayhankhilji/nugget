// @vitest-environment jsdom
import axe from "axe-core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { App } from "../src/App";
import { filterItems } from "../src/components/CommandPalette";
import { requestSearch, usePendingSearch } from "../src/hooks.nav";
import { t } from "../src/i18n";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../src/test/render";
import { finishedOnboarding, key, type } from "./keys";

const palette = () => document.querySelector<HTMLDialogElement>("dialog.palette[open]");
const input = () => palette()!.querySelector<HTMLInputElement>("input[role=combobox]")!;
const options = () => Array.from(palette()!.querySelectorAll<HTMLElement>("[role=option]"));
const labels = () => options().map((o) => o.querySelector(".palette-label")!.textContent);
const mainLabel = () => document.querySelector("main")?.getAttribute("aria-label");

async function open(target: EventTarget = document.body) {
  await key({ key: "k", code: "KeyK", ctrlKey: true }, target);
  expect(palette()).not.toBeNull();
}

beforeEach(() => {
  installDomShims();
  mockDaemon();
  finishedOnboarding();
});
afterEach(cleanup);

describe("command palette", () => {
  it("opens with Ctrl+K as an accessible combobox", async () => {
    await render(<App />);
    await open();
    const dlg = palette()!;
    expect(dlg.getAttribute("aria-modal")).toBe("true");
    expect(document.activeElement).toBe(input());
    const list = document.getElementById(input().getAttribute("aria-controls")!)!;
    expect(list.getAttribute("role")).toBe("listbox");
    expect(input().getAttribute("aria-activedescendant")).toBe(options()[0]!.id);
    expect(options()[0]!.getAttribute("aria-selected")).toBe("true");
    // Ctrl+K again toggles it shut.
    await key({ key: "k", code: "KeyK", ctrlKey: true });
    expect(palette()).toBeNull();
  });

  it("the sidebar button opens it too", async () => {
    await render(<App />);
    await click(document.querySelector(".palette-trigger"));
    expect(palette()).not.toBeNull();
  });

  it("filters by label and keywords, and Enter navigates", async () => {
    await render(<App />);
    await open();
    await type(input(), "know");
    expect(labels()[0]).toBe(t("app.nav.knowledge"));
    await type(input(), "egress");
    expect(labels()[0]).toBe(t("app.nav.privacy"));
    await key({ key: "Enter" });
    expect(palette()).toBeNull();
    expect(mainLabel()).toBe(t("app.nav.privacy"));
    expect(localStorage.getItem("nugget.page")).toBe("privacy");
  });

  it("arrow keys move the active option and wrap", async () => {
    await render(<App />);
    await open();
    await key({ key: "ArrowDown" });
    expect(input().getAttribute("aria-activedescendant")).toBe(options()[1]!.id);
    await key({ key: "ArrowUp" });
    await key({ key: "ArrowUp" });
    expect(input().getAttribute("aria-activedescendant")).toBe(options().at(-1)!.id);
  });

  it("Escape closes it and returns focus to where it was", async () => {
    await render(<App />);
    const nav = document.querySelectorAll<HTMLButtonElement>("nav .nav-item")[3]!;
    nav.focus();
    await open(nav);
    expect(document.activeElement).toBe(input());
    await key({ key: "Escape" });
    expect(palette()).toBeNull();
    expect(document.activeElement).toBe(nav);
  });

  it("shows No matches but still offers to search", async () => {
    await render(<App />);
    await open();
    await type(input(), "zzqx");
    expect(palette()!.textContent).toContain(t("palette.empty"));
    expect(labels()).toEqual([t("palette.action.search", { query: "zzqx" })]);
  });

  it("the search action runs the query on the Search page", async () => {
    const calls = mockDaemon();
    await render(<App />);
    await open();
    await type(input(), "lovelace notes");
    const opt = byText(palette()!, ".palette-label", t("palette.action.search", { query: "lovelace notes" }));
    await click(opt);
    await flush(5);
    expect(palette()).toBeNull();
    expect(mainLabel()).toBe(t("app.nav.search"));
    expect(document.querySelector<HTMLInputElement>("#search-q")?.value).toBe("lovelace notes");
    expect(calls).toContain("search");
  });

  it("opens the shortcuts list from an action", async () => {
    await render(<App />);
    await open();
    await type(input(), "shortcuts");
    await key({ key: "Enter" });
    expect(palette()).toBeNull();
    expect(document.querySelector("dialog[open] h2")?.textContent).toBe(t("shortcuts.title"));
  });

  it("has no axe violations while open", async () => {
    await render(<App />);
    await open();
    await type(input(), "s");
    const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
  });
});

describe("filterItems", () => {
  const items = [
    { label: "Chat", keywords: "ask, conversation" },
    { label: "Search", keywords: "find" },
    { label: "Résumé", keywords: "" },
    { label: "Connected apps", keywords: "mcp" },
  ];
  it("ranks label prefixes above keyword hits, folds accents, needs every word", () => {
    expect(filterItems(items, "").length).toBe(4);
    expect(filterItems(items, "ch").map((i) => i.label)).toEqual(["Chat", "Search"]);
    expect(filterItems(items, "resume").map((i) => i.label)).toEqual(["Résumé"]);
    expect(filterItems(items, "apps conn").map((i) => i.label)).toEqual(["Connected apps"]);
    expect(filterItems(items, "mcp").map((i) => i.label)).toEqual(["Connected apps"]);
    expect(filterItems(items, "nothing")).toEqual([]);
  });
});

describe("nav requests", () => {
  it("keeps a search requested before the page mounts", async () => {
    requestSearch("early");
    const got: string[] = [];
    function Probe() {
      usePendingSearch((q) => got.push(q));
      return null;
    }
    await render(<Probe />);
    requestSearch("later");
    expect(got).toEqual(["early", "later"]);
  });
});
