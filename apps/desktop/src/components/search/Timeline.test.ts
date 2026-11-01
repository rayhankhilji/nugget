import { describe, expect, it } from "vitest";
import type { TimelineEvent } from "../../api";
import { groupByMonth } from "./Timeline";

const item = (at: string | null, title: string): TimelineEvent => ({
  type: "item", at, date_source: "extracted", item: { source: "s", key: title }, title, snippet: "", kind: "file", url: null, sensitive: false,
});

describe("groupByMonth", () => {
  it("groups ascending by month, undated last, keeping order within a month", () => {
    const g = groupByMonth([item("2024-03-01", "c"), item(null, "u"), item("2024-01-05", "a"), item("2024-01-20", "b")]);
    expect(g.map((x) => x.month)).toEqual(["2024-01", "2024-03", "undated"]);
    expect(g[0]!.events.map((e) => (e.type === "item" ? e.title : ""))).toEqual(["a", "b"]);
  });
  it("is empty for no events", () => {
    expect(groupByMonth([])).toEqual([]);
  });
});
