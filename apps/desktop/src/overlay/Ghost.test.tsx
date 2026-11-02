// @vitest-environment jsdom
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { GHOST_EVENT, type GhostPaint } from "../api";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { Ghost } from "./Ghost";
import { FALLBACK_COLOR, GHOST_OPACITY, ghostStyle, ghostText, safeColor } from "./ghost";

const paint: GhostPaint = {
  visible: true,
  text: " the deck by Friday.",
  streaming: true,
  rtl: false,
  words: true,
  font_px: 13,
  line_px: 18,
  indent_px: 101,
  color: "#1d1c1d",
};

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("ghost text rendering", () => {
  it("matches the field's size and colour at ghost opacity, indented up to the caret", () => {
    const s = ghostStyle(paint);
    expect(s).toMatchObject({ fontSize: "13px", lineHeight: "18px", textIndent: "101px", color: "#1d1c1d", opacity: GHOST_OPACITY, direction: "ltr" });
    expect(GHOST_OPACITY).toBeCloseTo(0.45);
    expect(ghostStyle({ ...paint, rtl: true }).direction).toBe("rtl");
  });

  it("never trusts odd values from the shell", () => {
    const s = ghostStyle({ ...paint, font_px: Number.NaN, line_px: 2, indent_px: -5, color: "red; background: url(x)" });
    expect(s.fontSize).toBe("13px");
    expect(s.lineHeight).toBe("13px");
    expect(s.textIndent).toBe("0px");
    expect(s.color).toBe(FALLBACK_COLOR);
    expect(safeColor("#ABCDEF80")).toBe("#ABCDEF80");
    expect(safeColor(null)).toBe(FALLBACK_COLOR);
    expect(ghostStyle({ ...paint, font_px: 500 }).fontSize).toBe("96px");
  });

  it("draws the rest of the suggestion, without trailing spaces, and nothing when hidden", () => {
    expect(ghostText({ ...paint, text: " the deck  " })).toBe(" the deck");
    expect(ghostText({ ...paint, text: "\nSecond paragraph" })).toBe("\nSecond paragraph");
    expect(ghostText({ ...paint, visible: false })).toBe("");
  });

  it("follows the shell's paints and stays out of the accessibility tree", async () => {
    mockDaemon({});
    await render(<Ghost />);
    await flush(5);
    const el = document.querySelector<HTMLElement>(".ghost")!;
    /** The suggestion as drawn (plain part and streaming tail), without the Tab hint. */
    const shown = () => Array.from(el.querySelectorAll(".ghost-text, .ghost-tail"), (n) => n.textContent).join("");
    expect(el.getAttribute("aria-hidden")).toBe("true");
    expect(el.textContent).toBe("");
    await act(async () => {
      await emit(GHOST_EVENT, paint);
    });
    await flush(5);
    expect(shown()).toBe(" the deck by Friday.");
    expect(el.querySelector<HTMLElement>(".ghost-text")!.style.opacity).toBe("0.45");
    // No Tab hint while it is still streaming.
    expect(el.querySelector(".ghost-chip")).toBeNull();
    expect(el.style.textIndent).toBe("101px");
    expect(el.dataset.streaming).toBe("true");
    // Typing consumed part of it; then it was accepted and hidden.
    await act(async () => {
      await emit(GHOST_EVENT, { ...paint, text: " by Friday.", streaming: false });
    });
    await flush(5);
    expect(shown()).toBe(" by Friday.");
    expect(el.querySelector(".ghost-chip")?.textContent).toContain("Tab");
    expect(el.dataset.streaming).toBeUndefined();
    await act(async () => {
      await emit(GHOST_EVENT, { ...paint, visible: false, text: "" });
    });
    await flush(5);
    expect(el.textContent).toBe("");
  });

  it("teaches the keys on the first 3 suggestions only, then shows the dot alone", async () => {
    localStorage.removeItem("heed.ghost.hintsShown");
    mockDaemon({});
    await render(<Ghost />);
    await flush(5);
    const chip = () => document.querySelector(".ghost-chip");
    for (const [i, text] of [" one.", " two.", " three.", " four."].entries()) {
      await act(async () => {
        await emit(GHOST_EVENT, { ...paint, text, streaming: false });
      });
      await flush(5);
      expect(chip()).not.toBeNull();
      expect(chip()!.textContent?.includes("Tab")).toBe(i < 3);
    }
    localStorage.removeItem("heed.ghost.hintsShown");
  });
});
