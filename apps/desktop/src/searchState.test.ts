import { describe, expect, it } from "vitest";
import { decode, EMPTY_STATE, encode, hasFilters, monthOf, monthRange, normalize, toFilters } from "./searchState";

describe("searchState", () => {
  it("round-trips a full state in a stable order", () => {
    const s = { q: "budget  plan", sources: ["mail-1", "files-1"], kinds: ["mail", "event"] as const, people: ["ent_b", "ent_a"], from: "2024-01-01", to: "2024-12-31", timeline: "ent_a" };
    const e = encode({ ...s, kinds: [...s.kinds] });
    expect(e).toBe("q=budget++plan&source=files-1&source=mail-1&kind=event&kind=mail&person=ent_a&person=ent_b&from=2024-01-01&to=2024-12-31&timeline=ent_a");
    expect(decode(e)).toEqual({ ...s, kinds: ["event", "mail"], sources: ["files-1", "mail-1"], people: ["ent_a", "ent_b"] });
    // Equal states encode equally whatever the order they were built in.
    expect(encode(decode(e))).toBe(e);
  });

  it("encodes the empty state as an empty string", () => {
    expect(encode(EMPTY_STATE)).toBe("");
    expect(decode("")).toEqual(EMPTY_STATE);
  });

  it("tolerates junk", () => {
    expect(decode(null)).toEqual(EMPTY_STATE);
    expect(decode("%%%&&=")).toEqual(EMPTY_STATE);
    expect(decode("?q=x&kind=fax&kind=mail&kind=mail&from=2024-13-01&to=nope&source=&person=%00bad&zzz=1")).toEqual({
      ...EMPTY_STATE,
      q: "x",
      kinds: ["mail"],
    });
    expect(decode(`q=${"a".repeat(5000)}`).q).toHaveLength(1000);
  });

  it("swaps a reversed date range", () => {
    expect(normalize({ from: "2024-05-01", to: "2024-01-01" })).toMatchObject({ from: "2024-01-01", to: "2024-05-01" });
  });

  it("keeps unicode queries", () => {
    const e = encode({ q: "رسالة & 東京" });
    expect(decode(e).q).toBe("رسالة & 東京");
  });

  it("knows when filters are set and builds the daemon's filters", () => {
    expect(hasFilters(decode("q=x"))).toBe(false);
    expect(hasFilters(decode("q=x&timeline=e1"))).toBe(false);
    expect(hasFilters(decode("q=x&from=2024-01-01"))).toBe(true);
    expect(toFilters(decode("q=x&kind=chat&person=p"))).toEqual({ sources: [], kinds: ["chat"], people: ["p"], from: null, to: null });
  });

  it("computes month ranges and months", () => {
    expect(monthRange("2024-02")).toEqual({ from: "2024-02-01", to: "2024-02-29" });
    expect(monthRange("2023-12")).toEqual({ from: "2023-12-01", to: "2023-12-31" });
    expect(monthRange("2024-13")).toBeNull();
    expect(monthOf("2024-03-02")).toBe("2024-03");
    expect(monthOf(new Date(2024, 2, 15, 12).toISOString())).toBe("2024-03");
    expect(monthOf(null)).toBeNull();
    expect(monthOf("garbage")).toBeNull();
  });
});
