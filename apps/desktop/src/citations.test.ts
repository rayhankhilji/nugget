import { describe, expect, it } from "vitest";
import { splitCitations, stripCitationMarkers, withheldNotice } from "./citations";

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
