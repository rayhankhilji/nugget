import { describe, expect, it } from "vitest";
import { mulberry32 } from "./graph";
import {
  LabelPlacer,
  labelEligible,
  labelMinZoom,
  newCandidate,
  type LabelCandidate,
} from "./graphLabels";

function cand(node: number, x: number, y: number, opts: Partial<LabelCandidate> = {}): LabelCandidate {
  return { ...newCandidate(), node, x, y, r: 4, w: 60, h: 14, priority: 0, ...opts };
}

const overlaps = (a: LabelCandidate, b: LabelCandidate) =>
  a.left < b.left + b.w && b.left < a.left + a.w && a.top < b.top + b.h && b.top < a.top + a.h;

function assertNoOverlaps(cs: readonly LabelCandidate[], skipPinned = false) {
  const placed = cs.filter((c) => c.placed && !(skipPinned && c.pinned));
  for (let i = 0; i < placed.length; i++) {
    for (let j = i + 1; j < placed.length; j++) {
      expect(overlaps(placed[i]!, placed[j]!), `labels ${placed[i]!.node} and ${placed[j]!.node} overlap`).toBe(false);
    }
  }
}

/** `n` random labels in a `w` × `h` viewport. */
function scatter(n: number, w: number, h: number, seed = 1): LabelCandidate[] {
  const rng = mulberry32(seed);
  return Array.from({ length: n }, (_, i) =>
    cand(i, rng() * w, rng() * h, { w: 20 + rng() * 120, priority: Math.floor(rng() * 50), r: 2 + rng() * 8 }),
  );
}

describe("label placement", () => {
  it("never overlaps placed labels, at any density", () => {
    const placer = new LabelPlacer();
    for (const n of [10, 70, 300, 2000]) {
      const cs = scatter(n, 800, 600, n);
      const placed = placer.place(cs, cs.length, 800, 600);
      expect(placed).toBe(cs.filter((c) => c.placed).length);
      expect(placed).toBeGreaterThan(0);
      assertNoOverlaps(cs);
    }
  });

  it("always places pinned (hovered or selected) labels, first", () => {
    const cs = scatter(500, 400, 300, 9).map((c) => ({ ...c, priority: 1000 }));
    const hovered = cand(500, 200, 150, { pinned: true, priority: 0 });
    const selected = cand(501, 202, 152, { pinned: true, priority: -5 });
    const all = [...cs, hovered, selected];
    new LabelPlacer().place(all, all.length, 400, 300, 50);
    expect(hovered.placed).toBe(true);
    expect(selected.placed).toBe(true);
    // Pinned labels are placed before anything else, so nothing else overlaps them.
    for (const c of cs) {
      if (!c.placed) continue;
      expect(overlaps(c, hovered)).toBe(false);
      expect(overlaps(c, selected)).toBe(false);
    }
    // And they avoid each other when there is room.
    expect(overlaps(hovered, selected)).toBe(false);
  });

  it("skips labels outside the viewport and caps unpinned labels", () => {
    const out = cand(0, -500, -500, { priority: 100 });
    const inside = Array.from({ length: 20 }, (_, i) => cand(i + 1, 50 + (i % 5) * 150, 40 + Math.floor(i / 5) * 80, { priority: i }));
    const pinned = cand(99, 700, 500, { pinned: true });
    const all = [out, ...inside, pinned];
    const n = new LabelPlacer().place(all, all.length, 800, 600, 5);
    expect(out.placed).toBe(false);
    expect(pinned.placed).toBe(true);
    expect(n).toBe(6);
    // The five placed are the five highest priorities.
    expect(inside.filter((c) => c.placed).map((c) => c.priority).sort((a, b) => a - b)).toEqual([15, 16, 17, 18, 19]);
  });
});

describe("label thresholds", () => {
  it("is eligible by zoom, or always when pinned", () => {
    const z = labelMinZoom(2, 40, 500);
    expect(labelEligible(z - 0.01, z, false)).toBe(false);
    expect(labelEligible(z, z, false)).toBe(true);
    expect(labelEligible(0.01, z, true)).toBe(true);
  });
});
