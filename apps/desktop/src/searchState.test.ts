import { describe, expect, it } from "vitest";
import { decode, EMPTY_STATE, encode, hasFilters, toFilters } from "./searchState";

describe("searchState", () => {
  it("round-trips a full state in a stable order", () => {
    const s = { q: "budget  plan", sources: ["mail-1", "files-1"], kinds: ["mail", "event"] as const, people: ["ent_b", "ent_a"], from: "2024-01-01", to: "2024-12-31", timeline: "ent_a" };
    const e = encode({ ...s, kinds: [...s.kinds] });
    expect(e).toBe("q=budget++plan&source=files-1&source=mail-1&kind=event&kind=mail&person=ent_a&person=ent_b&from=2024-01-01&to=2024-12-31&timeline=ent_a");
    expect(decode(e)).toEqual({ ...s, kinds: ["event", "mail"], sources: ["files-1", "mail-1"], people: ["ent_a", "ent_b"] });
    // Equal states encode equally whatever the order they were built in.
    expect(encode(decode(e))).toBe(e);
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

  it("knows when filters are set and builds the daemon's filters", () => {
    expect(hasFilters(decode("q=x"))).toBe(false);
    expect(hasFilters(decode("q=x&timeline=e1"))).toBe(false);
    expect(hasFilters(decode("q=x&from=2024-01-01"))).toBe(true);
    expect(toFilters(decode("q=x&kind=chat&person=p"))).toEqual({ sources: [], kinds: ["chat"], people: ["p"], from: null, to: null });
  });
});
