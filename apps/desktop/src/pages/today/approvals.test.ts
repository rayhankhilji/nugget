import { describe, expect, it } from "vitest";
import { needsTotal } from "./approvals";

describe("needsTotal", () => {
  it("counts cards plus pending approvals, leaving out approvals held for Undo", () => {
    expect(needsTotal(5, [{ id: "a1" }, { id: "a2" }, { id: "a3" }], new Set())).toBe(8);
    expect(needsTotal(5, [{ id: "a1" }, { id: "a2" }, { id: "a3" }], new Set(["a2"]))).toBe(7);
    expect(needsTotal(0, [], new Set())).toBe(0);
  });
});
