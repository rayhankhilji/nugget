// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { addRecent, clearRecent, deleteSaved, listRecent, listSaved, MAX_RECENT, RECENT_KEY, renameSaved, SAVED_KEY, saveSearch } from "./savedSearches";

afterEach(() => {
  vi.restoreAllMocks();
  localStorage.clear();
});

describe("saved searches", () => {
  it("saves, lists, renames and deletes", () => {
    const a = saveSearch("  Budget mail ", "kind=mail&q=budget");
    expect(a.name).toBe("Budget mail");
    // Stored in its stable encoding.
    expect(listSaved()).toEqual([{ id: a.id, name: "Budget mail", state: "q=budget&kind=mail" }]);
    const b = saveSearch("Ada", "q=ada");
    expect(listSaved().map((s) => s.name)).toEqual(["Budget mail", "Ada"]);
    // Same name (any case) replaces.
    saveSearch("budget MAIL", "q=budget2");
    expect(listSaved()).toHaveLength(2);
    expect(listSaved()[0]).toMatchObject({ id: a.id, state: "q=budget2" });
    renameSaved(b.id, "Lovelace");
    expect(listSaved()[1]!.name).toBe("Lovelace");
    deleteSaved(a.id);
    expect(listSaved().map((s) => s.id)).toEqual([b.id]);
    expect(JSON.parse(localStorage.getItem(SAVED_KEY)!)).toHaveLength(1);
  });

  it("drops junk entries and survives corrupt storage", () => {
    localStorage.setItem(SAVED_KEY, JSON.stringify([null, 3, { id: "x" }, { id: "ok", name: "Fine", state: "q=a" }]));
    expect(listSaved()).toEqual([{ id: "ok", name: "Fine", state: "q=a" }]);
    localStorage.setItem(SAVED_KEY, "{not json");
    expect(listSaved()).toEqual([]);
  });

  it("keeps working when storage throws", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "removeItem").mockImplementation(() => {
      throw new Error("denied");
    });
    expect(listSaved()).toEqual([]);
    expect(() => saveSearch("A", "q=a")).not.toThrow();
    expect(() => addRecent("q=a")).not.toThrow();
    expect(() => clearRecent()).not.toThrow();
    expect(listRecent()).toEqual([]);
  });
});

describe("recent searches", () => {
  it("keeps the last 10 distinct, newest first, without timelines or empty queries", () => {
    for (let i = 0; i < 12; i++) addRecent(`q=q${i}`);
    addRecent("q=q5");
    addRecent("");
    addRecent("timeline=e1");
    addRecent("q=q11&timeline=e1");
    const r = listRecent();
    expect(r).toHaveLength(MAX_RECENT);
    expect(r[0]).toBe("q=q11");
    expect(r[1]).toBe("q=q5");
    expect(new Set(r).size).toBe(r.length);
  });

  it("clears", () => {
    addRecent("q=a");
    expect(listRecent()).toEqual(["q=a"]);
    clearRecent();
    expect(listRecent()).toEqual([]);
    expect(localStorage.getItem(RECENT_KEY)).toBeNull();
  });
});
