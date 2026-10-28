// @vitest-environment jsdom
/** The chat Markdown renderer: structure, streaming tolerance and safety (no HTML, no unsafe links). */
import { createElement } from "react";
import { describe, expect, it, vi } from "vitest";
import { Markdown, parseBlocks, safeHref, type CitationLinks } from "../src/components/Markdown";
import { click, render, setupHarness } from "./harness";

setupHarness();

async function md(text: string, props: { onOpenLink?: (url: string, label: string) => void; citations?: CitationLinks } = {}) {
  const r = await render(createElement(Markdown, { text, ...props }));
  return r.container.querySelector(".md") as HTMLElement;
}

describe("parseBlocks", () => {
  it("splits headings, paragraphs, lists, quotes, fences and tables", () => {
    const blocks = parseBlocks(
      ["# Title", "", "Some *text*", "more", "", "- a", "- b", "  - nested", "", "1. one", "2. two", "", "> quote", "", "```js", "let x = 1;", "```", "", "| a | b |", "|---|:-:|", "| 1 | 2 |", "", "---"].join("\n"),
    );
    expect(blocks.map((b) => b.t)).toEqual(["h", "p", "list", "list", "quote", "code", "table", "hr"]);
    const ul = blocks[2];
    expect(ul?.t === "list" && ul.items.length).toBe(2);
    expect(ul?.t === "list" && ul.items[1]?.some((b) => b.t === "list")).toBe(true);
    const code = blocks[5];
    expect(code).toEqual({ t: "code", lang: "js", code: "let x = 1;", open: false });
    const table = blocks[6];
    expect(table?.t === "table" && table.align).toEqual([null, "center"]);
  });

  it("keeps an unterminated fence open (streaming)", () => {
    expect(parseBlocks("Here:\n```py\nprint(1)\n")).toEqual([
      { t: "p", text: "Here:" },
      { t: "code", lang: "py", code: "print(1)\n", open: true },
    ]);
  });

  it("bounds nesting", () => {
    expect(() => parseBlocks(">".repeat(5000) + " deep")).not.toThrow();
  });
});

describe("Markdown", () => {
  it("renders inline formatting and blocks", async () => {
    const el = await md("## Head\n\n**bold** and *it* and `code` and ~~gone~~\n\n1. x\n2. y\n\n> q");
    expect(el.querySelector("h4")?.textContent).toBe("Head");
    expect(el.querySelector("strong")?.textContent).toBe("bold");
    expect(el.querySelector("em")?.textContent).toBe("it");
    expect(el.querySelector("code.md-inline-code")?.textContent).toBe("code");
    expect(el.querySelector("del")?.textContent).toBe("gone");
    expect(el.querySelectorAll("ol > li")).toHaveLength(2);
    expect(el.querySelector("blockquote")?.textContent).toBe("q");
  });

  it("leaves unmatched markers and arithmetic as text", async () => {
    const el = await md("2 * 3 * 4 and **open");
    expect(el.textContent).toBe("2 * 3 * 4 and **open");
    expect(el.querySelector("em, strong")).toBeNull();
  });

  it("renders code blocks with a copy button, even while open", async () => {
    const el = await md("```sh\necho hi");
    expect(el.querySelector("pre code")?.textContent).toBe("echo hi");
    expect(el.querySelector(".md-code-lang")?.textContent).toBe("sh");
    expect(el.querySelector(".md-code button")?.getAttribute("aria-label")).toBe("Copy code");
  });

  it("shows raw HTML as text", async () => {
    const el = await md('<script>alert(1)</script>\n\n<img src=x onerror="alert(2)">\n\n**<b>x</b>**');
    expect(el.querySelector("script, img, b")).toBeNull();
    expect(el.textContent).toContain("<script>alert(1)</script>");
    expect(el.textContent).toContain('<img src=x onerror="alert(2)">');
    expect(el.querySelector("strong")?.textContent).toBe("<b>x</b>");
  });

  it("opens https links through the callback, never by navigating", async () => {
    const onOpenLink = vi.fn();
    const el = await md("See [the docs](https://example.com/a?b=1) or https://example.org/x.", { onOpenLink });
    const links = el.querySelectorAll("a");
    expect(links).toHaveLength(2);
    expect(links[1]?.getAttribute("href")).toBe("https://example.org/x");
    const ev = new MouseEvent("click", { bubbles: true, cancelable: true });
    links[0]!.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    expect(onOpenLink).toHaveBeenCalledWith("https://example.com/a?b=1", "the docs");
  });

  it("never makes javascript:, data: or other schemes clickable", async () => {
    const onOpenLink = vi.fn();
    const el = await md(
      "[a](javascript:alert(1)) [b](data:text/html,<script>x</script>) [c](JaVaScRiPt:alert(1)) [d](vbscript:x) [e](file:///etc/passwd) [f](http://plain.example) [g](mailto:me@example.com) <javascript:alert(1)>",
      { onOpenLink },
    );
    expect(el.querySelector("a")).toBeNull();
    expect(el.innerHTML).not.toMatch(/href/i);
    // http and mailto are shown with their address so they can be copied.
    expect(el.textContent).toContain("f (http://plain.example)");
    expect(el.textContent).toContain("g (mailto:me@example.com)");
    expect(safeHref("javascript:alert(1)")).toBeNull();
    expect(safeHref("https://ok.example/")).toBe("https://ok.example/");
    expect(safeHref("https://bad example/")).toBeNull();
  });

  it("renders links as text without an opener", async () => {
    const el = await md("[x](https://example.com)");
    expect(el.querySelector("a")).toBeNull();
    expect(el.textContent).toBe("x (https://example.com)");
  });

  it("makes known [n] citations clickable", async () => {
    const onCite = vi.fn();
    const el = await md("Ada wrote it [1][3].", {
      citations: { valid: new Set([1]), label: (n) => `Source ${n}`, onCite },
    });
    const buttons = el.querySelectorAll("button.cite");
    expect(buttons).toHaveLength(1);
    expect(buttons[0]?.getAttribute("aria-label")).toBe("Source 1");
    expect(el.textContent).toContain("[3]");
    await click(buttons[0]);
    expect(onCite).toHaveBeenCalledWith(1);
  });
});
