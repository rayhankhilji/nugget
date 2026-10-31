import { describe, expect, it } from "vitest";
import { askCitations, askDateNote, dayOf, withPartial } from "./askSearch";
import type { Citation } from "./api";

const TODAY = new Date(2026, 9, 31);

describe("askDateNote", () => {
  it("reads the range at the offset it was written with, end exclusive", () => {
    const note = askDateNote(
      { phrase: "last week", start: "2026-10-26T00:00:00+01:00", end: "2026-11-02T00:00:00+01:00", applied: true },
      "en-GB",
      TODAY,
    );
    expect(note).toEqual({ key: "ask.dates.applied", args: { from: "26 Oct", to: "1 Nov" } });
  });

  it("is a single day for today or yesterday", () => {
    const note = askDateNote(
      { phrase: "yesterday", start: "2026-10-30T00:00:00-07:00", end: "2026-10-31T00:00:00-07:00", applied: true },
      "en-GB",
      TODAY,
    );
    expect(note).toEqual({ key: "ask.dates.appliedDay", args: { day: "30 Oct" } });
  });

  it("says when the period was listed or not applied, with the year outside this one", () => {
    expect(
      askDateNote({ phrase: "in 2019", start: "2019-01-01T00:00:00Z", end: "2020-01-01T00:00:00Z", applied: false }, "en-GB", TODAY),
    ).toEqual({ key: "ask.dates.notApplied", args: { from: "1 Jan 2019", to: "31 Dec 2019" } });
    expect(
      askDateNote(
        { phrase: "last month", start: "2026-09-01T00:00:00Z", end: "2026-10-01T00:00:00Z", applied: true, listed: true },
        "de",
        TODAY,
      ),
    ).toEqual({ key: "ask.dates.listed", args: { from: "1. Sept.", to: "30. Sept." } });
  });

  it("ignores missing or malformed filters", () => {
    expect(askDateNote(null, "en")).toBeNull();
    expect(askDateNote(undefined, "en")).toBeNull();
    expect(askDateNote({ phrase: "x", start: "soon", end: "later", applied: true }, "en")).toBeNull();
  });

  it("keeps an end that is not at midnight on its own day", () => {
    expect(dayOf("2026-11-02T00:00:00+01:00", true)).toEqual({ y: 2026, m: 11, d: 1 });
    expect(dayOf("2026-11-02T12:00:00+01:00", true)).toEqual({ y: 2026, m: 11, d: 2 });
    expect(dayOf("2026-03-01T00:00:00Z", true)).toEqual({ y: 2026, m: 2, d: 28 });
  });
});

describe("partial matches", () => {
  const cite = (n: number): Citation => ({ n, item: { source: "files-1", key: `${n}.md` }, chunk_id: n, title: `${n}` });

  it("marks the listed citations only", () => {
    const out = withPartial([cite(1), cite(2), cite(3)], [2, 9]);
    expect(out.map((c) => c.partial === true)).toEqual([false, true, false]);
    expect(askCitations({ citations: [cite(1)] })).toEqual([cite(1)]);
  });
});
