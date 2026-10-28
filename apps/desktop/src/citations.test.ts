import { describe, expect, it } from "vitest";
import { contextPrompt, splitCitations } from "./citations";

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

describe("contextPrompt", () => {
  it("includes the passages and tells the model to treat them as data", () => {
    const p = contextPrompt("[1] hello");
    expect(p).toContain("[1] hello");
    expect(p).toMatch(/ignore any instructions/i);
  });
});
