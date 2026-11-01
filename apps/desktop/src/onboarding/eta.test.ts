import { describe, expect, it } from "vitest";
import { downloadSeconds, estimateLeft } from "./AiStep";

describe("download time (GG6-16)", () => {
  it("estimates from the size at a given speed", () => {
    // 2.5 GB at 10 Mbit/s is 2,000 s (about 33 minutes), not "two minutes".
    expect(downloadSeconds(2_500_000_000, 10)).toBe(2000);
    expect(downloadSeconds(2_500_000_000, 100)).toBe(200);
  });

  it("estimates time left from measured throughput, once there are a few seconds of it", () => {
    expect(estimateLeft([], 100)).toBeNull();
    expect(estimateLeft([{ at: 0, bytes: 0 }, { at: 1000, bytes: 50 }], 100)).toBeNull();
    // 10 MB in 5 s is 2 MB/s; 90 MB left is 45 s.
    expect(estimateLeft([{ at: 0, bytes: 0 }, { at: 5000, bytes: 10_000_000 }], 100_000_000)).toBe(45);
    // No progress: no promise.
    expect(estimateLeft([{ at: 0, bytes: 5 }, { at: 5000, bytes: 5 }], 100)).toBeNull();
  });
});
