// @vitest-environment jsdom
/**
 * KK1 (GG6-25): the Status page says plainly when the data disk is full or nearly full, and says
 * nothing when there is room (or the daemon is too old to report it).
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { StatusResponse } from "../api";
import { status as baseStatus } from "../test/fixtures";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { StatusPage } from "./Status";

beforeEach(installDomShims);
afterEach(cleanup);

async function page(status: StatusResponse) {
  mockDaemon({ status });
  const r = await render(<StatusPage />);
  await flush();
  return r.container;
}

describe("Status: disk space (KK1)", () => {
  it("alerts when the disk is full", async () => {
    const c = await page({ ...baseStatus, disk: { free_bytes: 120_000_000, level: "full", recent_full_error: true } });
    const alert = Array.from(c.querySelectorAll('[role="alert"]')).find((e) => e.textContent?.includes("disk"));
    expect(alert?.textContent).toContain("Your disk is almost full (120 MB free).");
    expect(alert?.textContent).toContain("Nothing is lost");
  });

  it("says the disk is full when free space is unknown", async () => {
    const c = await page({ ...baseStatus, disk: { free_bytes: null, level: "full", recent_full_error: true } });
    expect(c.textContent).toContain("Your disk is full.");
  });

  it("notes a low disk politely", async () => {
    const c = await page({ ...baseStatus, disk: { free_bytes: 800_000_000, level: "low" } });
    const note = Array.from(c.querySelectorAll('[role="status"]')).find((e) => e.textContent?.includes("disk"));
    expect(note?.textContent).toBe("Your disk is getting full (800 MB free). Free up some space so Nugget can keep indexing.");
  });

  it("stays quiet when there is room or no report", async () => {
    for (const disk of [{ free_bytes: 50e9, level: "ok" as const }, undefined]) {
      const c = await page({ ...baseStatus, disk });
      expect(c.textContent).not.toContain("Your disk");
      cleanup();
    }
  });
});
