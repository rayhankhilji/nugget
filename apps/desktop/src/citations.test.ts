import { describe, expect, it } from "vitest";
import { splitCitations, withheldNotice } from "./citations";

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
