// @vitest-environment jsdom
/**
 * Right-to-left layout: the document flips to `dir="rtl"` for Arabic, user content keeps its own
 * direction (`dir="auto"`), directional icons mirror, and the stylesheets use logical properties.
 * jsdom does no layout, so this checks attributes, classes and declarations, never positions.
 * Structure only: no Arabic strings (the catalog is still being translated).
 */
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { App } from "../App";
import { ChatPage } from "../pages/Chat";
import { KnowledgePage } from "../pages/Knowledge";
import { entities } from "../test/fixtures";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { loadLocale } from ".";

const DONE = JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true });

/** Glyphs that point a way; drawn outside a translated string they must mirror in RTL. */
const DIRECTIONAL = /[←→‹›«»▸▹◂◃▶◀➜➔⟵⟶]/;

beforeEach(async () => {
  installDomShims();
  if (!Element.prototype.scrollIntoView) Element.prototype.scrollIntoView = () => {};
  await loadLocale("ar");
});
afterEach(async () => {
  cleanup();
  await loadLocale("en");
});

/** Elements in `root` that draw a directional glyph or icon without the mirroring class. */
function unmirroredIcons(root: ParentNode): Element[] {
  const marked = Array.from(root.querySelectorAll("[data-icon-dir], [aria-hidden=true]")).filter(
    (el) => el.hasAttribute("data-icon-dir") || (el.children.length === 0 && DIRECTIONAL.test(el.textContent ?? "")),
  );
  return marked.filter((el) => !el.closest(".flip-rtl"));
}

describe("document direction", () => {
  it("is rtl/ar for Arabic and back to ltr/en for English", async () => {
    expect(document.documentElement.dir).toBe("rtl");
    expect(document.documentElement.lang).toBe("ar");
    await loadLocale("en");
    expect(document.documentElement.dir).toBe("ltr");
    expect(document.documentElement.lang).toBe("en");
    await loadLocale("ar");
    expect(document.documentElement.dir).toBe("rtl");
  });
});

describe("sidebar in RTL", () => {
  it("renders the nav with its items, key combos kept LTR and directional icons mirrored", async () => {
    mockDaemon();
    localStorage.setItem("nugget.onboarding.v1", DONE);
    localStorage.setItem("nugget.page", "today");
    await render(<App />);
    await flush(10);
    expect(document.documentElement.dir).toBe("rtl");
    const sidebar = document.querySelector("aside.sidebar")!;
    const items = sidebar.querySelectorAll("nav button.nav-item");
    expect(items.length).toBeGreaterThanOrEqual(5);
    for (const b of Array.from(items)) expect(b.textContent?.trim()).not.toBe("");
    expect(sidebar.querySelector("nav [aria-current=page]")).not.toBeNull();
    // Nothing in the sidebar forces a physical direction on its own.
    expect(sidebar.querySelector("nav [dir=ltr]")).toBeNull();
    for (const kbd of Array.from(sidebar.querySelectorAll("kbd"))) expect(kbd.getAttribute("dir")).toBe("ltr");
    expect(unmirroredIcons(sidebar)).toEqual([]);
  });
});

describe("Chat in RTL", () => {
  it("gives the composer and every message dir=auto", async () => {
    mockDaemon({
      chat_history_load: [],
      chat_history_save: null,
      chat: () => ({ stream_id: "s-1", provider: "ollama", locality: "on_device" }),
      retrieve_ask: () => ({ stream_id: "s-1", provider: "ollama", locality: "on_device", citations: [], excluded_sensitive: 0 }),
    });
    const { container } = await render(<ChatPage />);
    const area = container.querySelector<HTMLTextAreaElement>("form.composer textarea")!;
    expect(area.getAttribute("dir")).toBe("auto");

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(area, "An English note");
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(container.querySelector("form.composer button[type=submit]"));
    await flush();
    await act(() => emit("nugget:chat-delta", { stream_id: "s-1", text: "A reply" }));
    await act(() => emit("nugget:chat-done", { stream_id: "s-1", input_tokens: 1, output_tokens: 2, error: null }));
    await flush();

    const bodies = Array.from(container.querySelectorAll(".msg .msg-body"));
    expect(bodies.length).toBe(2);
    for (const b of bodies) expect(b.getAttribute("dir")).toBe("auto");
    expect(bodies[0]!.textContent).toContain("An English note");
    expect(container.querySelector(".msg-user")).not.toBeNull();
    expect(unmirroredIcons(container.querySelector("form.composer")!)).toEqual([]);
  });
});

describe("Knowledge in RTL", () => {
  it("shows entity names in dir=auto elements and a dir=auto filter box", async () => {
    mockDaemon();
    const { container } = await render(<KnowledgePage />);
    await flush(10);
    expect(container.querySelector("#kb-search")?.getAttribute("dir")).toBe("auto");
    const rows = Array.from(container.querySelectorAll(".entity-row"));
    expect(rows.length).toBe(entities.length);
    for (const e of entities) {
      const el = Array.from(container.querySelectorAll(".entity-row [dir=auto]")).find((n) => n.textContent === e.name);
      expect(el, e.name).toBeDefined();
    }
  });
});

describe("stylesheets use logical properties", () => {
  const SRC = join(__dirname, "..");
  const FILES = (readdirSync(SRC, { recursive: true }) as string[]).filter((f) => f.endsWith(".css")).sort();

  it("finds the stylesheets", () => {
    expect(FILES).toEqual(expect.arrayContaining(["styles.css", "styles-chat.css", "styles-pages.css"]));
  });
  const PHYSICAL =
    /(^|[\s;{])(margin-left|margin-right|padding-left|padding-right|border-left|border-right|left|right)(-[a-z-]+)?\s*:|text-align\s*:\s*(left|right)\b|float\s*:\s*(left|right)\b/;

  it.each(FILES)("%s has no physical left/right declarations (unless marked rtl-ok)", (file) => {
    const lines = readFileSync(join(SRC, file), "utf8").split("\n");
    const bad = lines
      .map((line, i) => ({ line, n: i + 1 }))
      .filter(({ line }) => {
        const code = line.replace(/\/\*.*?\*\//g, "");
        return PHYSICAL.test(code) && !/\/\*\s*rtl-ok:[^*]+\*\//.test(line);
      })
      .map(({ line, n }) => `${file}:${n}: ${line.trim()}`);
    expect(bad).toEqual([]);
  });

  it("mirrors .flip-rtl icons and keeps code LTR", () => {
    const css = readFileSync(join(SRC, "styles.css"), "utf8");
    expect(css).toMatch(/\[dir="rtl"\]\s+\.flip-rtl\s*\{[^}]*transform:\s*scaleX\(-1\)/);
    expect(css).toMatch(/code,\s*pre,\s*kbd\s*\{[^}]*direction:\s*ltr/);
  });
});
