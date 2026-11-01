import { describe, expect, it } from "vitest";
import { triageLine } from "./triageLine";
import { t } from "./i18n";

describe("AI sorting on Status (JJ2)", () => {
  it("says plainly when sorting is off and what it means for privacy", () => {
    const off = triageLine(null, { state: "off", pending: 93, done: 0 });
    expect(off).toMatchObject({ on: false, key: "status.triageOffPending", args: { n: 93 } });
    expect(t(off.key, off.args)).toBe("No local model: 93 items stay private until sorted. They are searchable.");
    expect(triageLine(null, null).key).toBe("status.triageOffNone");
    // A provider but triage turned off is off too.
    expect(triageLine("local-chat", { state: "off", pending: 1, done: 0 }).on).toBe(false);
  });

  it("shows progress while running, waiting or paused", () => {
    expect(triageLine("local-chat", { state: "running", pending: 5, done: 1 }).key).toBe("status.triageRunning");
    expect(triageLine("local-chat", { state: "yielding", pending: 5, done: 1 }).key).toBe("status.triageYielding");
    expect(triageLine("local-chat", { state: "paused", pending: 5, done: 1 }).key).toBe("status.triagePaused");
    expect(triageLine("local-chat", { state: "idle", pending: 0, done: 9 }).key).toBe("status.triageIdle");
    const l = triageLine("local-chat", { state: "running", pending: 1, done: 0, failed: 2 });
    expect(t(l.key, l.args)).toBe("Sorting: 1 item left. Already searchable; private until sorted.");
    expect(t("status.triageFailed", { n: l.failed })).toBe("2 items could not be sorted and stay private.");
  });
});
