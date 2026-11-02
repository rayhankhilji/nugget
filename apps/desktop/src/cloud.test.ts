// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cloudApi, deviceTimeZone, type CloudBrief, type CloudSearchResponse } from "./cloud";
import { cleanup, mockDaemon } from "./test/render";

afterEach(cleanup);

const searchResponse: CloudSearchResponse = {
  mode: "hybrid",
  query: "contract",
  dates: { from: null, to: null, phrase: null },
  results: [
    { item_id: "i1", source: "gmail", kind: "email", title: "Signed contract", author: "Dana", url: null, occurred_at: null, snippet: "…", score: 0.9 },
  ],
  total: 1,
  facets: { sources: [{ value: "gmail", count: 1 }], people: [] },
  timeline: { bucket: "month", points: [{ start: "2026-10-01T00:00:00Z", count: 1 }] },
};

describe("cloud search, cards and brief wrappers", () => {
  it("send the device zone and the typed arguments to the shell", async () => {
    const seen: Record<string, unknown> = {};
    const brief: CloudBrief = {
      day: "2026-11-02",
      timezone: "Europe/London",
      prose: "Dana waits [1].",
      citations: [],
      brief: { date: "2026-11-02", generated_at: "2026-11-02T07:00:00Z", top: [], meetings: [], overdue: [], renewals: [], open_cards: 0 },
      created_at: "2026-11-02T07:00:01Z",
    };
    mockDaemon({
      cloud_search: (a: unknown) => ((seen.search = a), searchResponse),
      cloud_cards: (a: unknown) => ((seen.cards = a), { cards: [], last_refresh: null }),
      cloud_card_action: (a: unknown) => ((seen.action = a), null),
      cloud_brief: (a: unknown) => ((seen.brief = a), brief),
    });
    const tz = deviceTimeZone();
    expect((await cloudApi.search({ q: "contract", sources: ["gmail"] })).results[0]?.title).toBe("Signed contract");
    expect(seen.search).toEqual({ request: { q: "contract", sources: ["gmail"], tz } });
    expect((await cloudApi.cards()).cards).toEqual([]);
    expect(seen.cards).toEqual({ tz });
    await cloudApi.dismissCard("abc", "already_handled");
    expect(seen.action).toEqual({ id: "abc", action: "dismiss", reason: "already_handled", note: null });
    await cloudApi.snoozeCard("abc", "2026-11-04T09:00:00Z");
    expect(seen.action).toEqual({ id: "abc", action: "snooze", until: "2026-11-04T09:00:00Z" });
    expect((await cloudApi.brief())?.prose).toContain("[1]");
    expect(seen.brief).toEqual({ tz });
  });
});
