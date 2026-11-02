import { describe, expect, it } from "vitest";
import { applyPaletteFilters, parsePaletteQuery, type PaletteItem } from "./CommandPalette";

const item = (id: string, author: string | null, source: string, sourceName: string): PaletteItem => ({ id, label: id, group: "Mail", author, source, sourceName, run: () => {} });

describe("⌘K from: and in:", () => {
  it("splits the filters from the words", () => {
    expect(parsePaletteQuery("retainer from:mei in:gmail q4")).toEqual({ text: "retainer q4", from: "mei", in: "gmail" });
    expect(parsePaletteQuery("  launch  ")).toEqual({ text: "launch", from: null, in: null });
  });
  it("keeps results by author and by app, case- and accent-insensitively", () => {
    const items = [item("a", "Mei Tanaka", "gmail", "Gmail"), item("b", "Marco", "slack", "Slack"), item("c", null, "googledrive", "Google Drive")];
    expect(applyPaletteFilters(items, parsePaletteQuery("x from:MEI")).map((i) => i.id)).toEqual(["a"]);
    expect(applyPaletteFilters(items, parsePaletteQuery("x in:drive")).map((i) => i.id)).toEqual(["c"]);
    expect(applyPaletteFilters(items, parsePaletteQuery("x")).map((i) => i.id)).toEqual(["a", "b", "c"]);
  });
});
