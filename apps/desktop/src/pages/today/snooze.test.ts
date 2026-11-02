import { describe, expect, it } from "vitest";
import { parseSnooze, snoozePresets } from "./snooze";

const now = new Date(2026, 10, 3, 8, 14);
const expires = new Date(2026, 10, 4, 6, 0);

describe("snooze", () => {
  it("offers only presets before the action expires", () => {
    expect(snoozePresets(now, expires).map((p) => p.id)).toEqual(["hour", "afternoon", "evening"]);
    expect(snoozePresets(now, new Date(2026, 10, 3, 15, 0)).map((p) => p.id)).toEqual(["hour", "afternoon"]);
    expect(snoozePresets(new Date(2026, 10, 3, 19, 0), expires).map((p) => p.id)).toEqual(["hour"]);
  });
  it("reads what people type, within (now, expiry]", () => {
    expect(parseSnooze("in 2 hours", now, expires)).toEqual(new Date(2026, 10, 3, 10, 14));
    expect(parseSnooze("in 30 min", now, expires)).toEqual(new Date(2026, 10, 3, 8, 44));
    expect(parseSnooze("at 4pm", now, expires)).toEqual(new Date(2026, 10, 3, 16, 0));
    expect(parseSnooze("16:30", now, expires)).toEqual(new Date(2026, 10, 3, 16, 30));
    expect(parseSnooze("tonight", now, expires)).toEqual(new Date(2026, 10, 3, 18, 0));
    expect(parseSnooze("tomorrow 5am", now, expires)).toEqual(new Date(2026, 10, 4, 5, 0));
    expect(parseSnooze("tomorrow 9am", now, expires)).toBeNull(); // after it expires
    expect(parseSnooze("7am", now, expires)).toBeNull(); // already past
    expect(parseSnooze("whenever", now, expires)).toBeNull();
  });
});
