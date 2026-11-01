import { describe, expect, it } from "vitest";
import { formatDatesInText, formatLatency, formatRelative } from "./format";

describe("formatRelative", () => {
  const now = Date.parse("2026-10-28T12:00:00Z");
  const ago = (secs: number) => new Date(now - secs * 1000).toISOString();
  const rel = (ts: string | null | undefined) => formatRelative(ts, now, "en");

  it("formats past times", () => {
    expect(rel(ago(0))).toBe("just now");
    expect(rel(ago(30))).toBe("just now");
    expect(rel(ago(50))).toBe("1 minute ago");
    expect(rel(ago(5 * 60))).toBe("5 minutes ago");
    expect(rel(ago(59 * 60))).toBe("59 minutes ago");
    expect(rel(ago(2 * 3600))).toBe("2 hours ago");
    expect(rel(ago(26 * 3600))).toBe("yesterday");
    expect(rel(ago(3 * 86400))).toBe("3 days ago");
    expect(rel(ago(14 * 86400))).toBe("2 weeks ago");
    expect(rel(ago(65 * 86400))).toBe("2 months ago");
    expect(rel(ago(800 * 86400))).toBe("2 years ago");
  });
});

describe("formatLatency", () => {
  it("picks a readable unit", () => {
    expect(formatLatency(null)).toBe("–");
    expect(formatLatency(undefined)).toBe("–");
    expect(formatLatency(-1)).toBe("–");
    expect(formatLatency(850)).toBe("850 ms");
    expect(formatLatency(4200)).toBe("4.2 s");
    expect(formatLatency(42_000)).toBe("42 s");
    expect(formatLatency(185_000)).toBe("3m 5s");
  });
});

describe("formatDatesInText", () => {
  const fmt = (text: string, locale = "en-US") => formatDatesInText(text, locale, "UTC");

  it("writes date-times as date and time, and midnight-UTC or bare dates as dates", () => {
    expect(fmt("attended Wren Mobile usability test on 2026-11-10T13:00:00Z")).toBe(
      "attended Wren Mobile usability test on Nov 10, 2026, 1:00 PM",
    );
    expect(fmt("due 2026-11-10T00:00:00Z.")).toBe("due Nov 10, 2026.");
    expect(fmt("born 1815-12-10, died 1852-11-27")).toBe("born Dec 10, 1815, died Nov 27, 1852");
    expect(fmt("at 2026-11-10T13:00:00.250+02:00")).toBe("at Nov 10, 2026, 11:00 AM");
  });

  it("takes a date before a colon, but not one glued to a time (LL2, GG7-11)", () => {
    expect(fmt("Due 2026-11-03: pay the invoice")).toBe("Due Nov 3, 2026: pay the invoice");
    expect(fmt("Due 2026-11-03:", "de")).toBe("Due 03.11.2026:");
    expect(fmt("log 2026-11-03:12 x")).toBe("log 2026-11-03:12 x");
  });
});
