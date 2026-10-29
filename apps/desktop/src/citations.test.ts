import { describe, expect, it } from "vitest";
import { citedNumbers, groupSources, splitCitations, stripCitationMarkers, withheldNotice } from "./citations";

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

  it("leaves unknown numbers and other brackets as text", () => {
    expect(splitCitations("see [9] and [a] or [12345]", valid)).toEqual([{ kind: "text", text: "see [9] and [a] or [12345]" }]);
    expect(splitCitations("", valid)).toEqual([]);
  });
});

describe("withheldNotice", () => {
  it("says nothing when no sensitive item was withheld", () => {
    expect(withheldNotice(0, "claude")).toBeNull();
    expect(withheldNotice(undefined, "claude")).toBeNull();
    expect(withheldNotice(null, "claude")).toBeNull();
  });

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

  it("leaves other brackets, links and code alone", () => {
    const keep = [
      "- [x] done and [ ] not",
      "see [the docs](https://example.com) or [1](https://example.com/1)",
      "a [text][1] reference\n\n[1]: https://example.com",
      "footnote[^1] and arr[1] and f(x)[2]",
      "`list[0] [1]` and\n```\nxs [2]\n```",
      "",
    ];
    for (const s of keep) expect(stripCitationMarkers(s)).toBe(s);
  });
});

describe("citedNumbers", () => {
  const valid = new Set([1, 2, 3, 4]);
  it("finds the markers the text cites, ignoring code, links and unknown numbers", () => {
    expect([...citedNumbers("A [1], B [2][4]. `x[3]` [3](https://x) [9]", valid)].sort()).toEqual([1, 2, 4]);
    expect(citedNumbers("```\nlist[1]\n```\nnothing cited", valid).size).toBe(0);
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
  it("tells items apart by source as well as key", () => {
    const g = groupSources([{ n: 1, item: { source: "x", key: "k" } }, { n: 2, item: { source: "y", key: "k" } }], new Set([1, 2]));
    expect(g.cited).toHaveLength(2);
  });
});
