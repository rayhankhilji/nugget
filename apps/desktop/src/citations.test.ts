import { describe, expect, it } from "vitest";
import { groupSources, splitCitations, stripCitationMarkers, withheldNotice } from "./citations";

describe("splitCitations", () => {
  const valid = new Set([1, 2, 3]);

  it("turns known [n] markers into citations", () => {
    expect(splitCitations("[1] Alpha. [2][3] Beta", valid)).toEqual([
      { kind: "cite", n: 1, raw: "[1]" },
      { kind: "text", text: " Alpha. " },
      { kind: "cite", n: 2, raw: "[2]" },
      { kind: "cite", n: 3, raw: "[3]" },
      { kind: "text", text: " Beta" },
    ]);
  });
});

describe("withheldNotice", () => {
  it("counts withheld items and names the cloud model", () => {
    expect(withheldNotice(1, "claude")).toMatch(/^1 sensitive item was left out because claude is a cloud model/);
    expect(withheldNotice(3, "gpt")).toMatch(/^3 sensitive items were left out because gpt/);
  });
});

describe("stripCitationMarkers", () => {
  it("drops [n] markers and the blank before them", () => {
    expect(stripCitationMarkers("By the river [1].")).toBe("By the river.");
    expect(stripCitationMarkers("Herons [2][3], then egrets [12] and more.[4]")).toBe("Herons, then egrets and more.");
    expect(stripCitationMarkers("[1] Alpha.\n[2][3]  Beta")).toBe("Alpha.\nBeta");
    expect(stripCitationMarkers("- one [1]\n- two [2]")).toBe("- one\n- two");
  });
});

describe("groupSources", () => {
  const c = (n: number, key: string) => ({ n, item: { source: "s", key } });
  it("puts cited items first, once each with their cited markers, and the rest after", () => {
    const g = groupSources([c(1, "a"), c(2, "b"), c(3, "c"), c(4, "a"), c(5, "d"), c(6, "d"), c(7, "b")], new Set([4, 2, 1]));
    expect(g.cited.map((e) => [e.key.split("\u0000")[1], e.ns])).toEqual([
      ["a", [1, 4]],
      ["b", [2]],
    ]);
    expect(g.cited[1]!.citations.map((x) => x.n)).toEqual([2, 7]);
    expect(g.uncited.map((e) => [e.key.split("\u0000")[1], e.ns])).toEqual([
      ["c", [3]],
      ["d", [5, 6]],
    ]);
  });
});
