import { describe, expect, it } from "vitest";
import { baseName, formatDatesInText, formatLatency, formatRelative, splitList } from "./format";

describe("formatRelative", () => {
  const now = Date.parse("2026-10-28T12:00:00Z");
  const ago = (secs: number) => new Date(now - secs * 1000).toISOString();
  const rel = (ts: string | null | undefined) => formatRelative(ts, now, "en");

  it("handles missing and invalid timestamps", () => {
    expect(rel(null)).toBe("never");
    expect(rel(undefined)).toBe("never");
    expect(rel("")).toBe("never");
    expect(rel("yesterday-ish")).toBe("yesterday-ish");
  });

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

  it("formats future times (clock skew between daemon and app)", () => {
    expect(rel(ago(-10))).toBe("just now");
    expect(rel(ago(-3 * 3600))).toBe("in 3 hours");
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

describe("splitList", () => {
  it("splits on commas and newlines, trims and de-duplicates", () => {
    expect(splitList("INBOX, Archive\n  Sent ,,INBOX\n")).toEqual(["INBOX", "Archive", "Sent"]);
    expect(splitList("  ")).toEqual([]);
  });
});

describe("baseName", () => {
  it("returns the last path segment on any OS", () => {
    expect(baseName("/home/me/Notes/")).toBe("Notes");
    expect(baseName("C:\\Users\\me\\Vault")).toBe("Vault");
    expect(baseName("/")).toBe("/");
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

  it("follows the locale", () => {
    expect(fmt("am 2026-11-10", "de-DE")).toBe("am 10.11.2026");
  });

  it("leaves other numbers and impossible dates alone", () => {
    for (const s of ["v2026-11-10", "id 2026-11-10-abc", "2026-02-31", "2026-13-01", "call 555-1234", "12026-11-10", "no dates"]) {
      expect(fmt(s)).toBe(s);
    }
  });
});
