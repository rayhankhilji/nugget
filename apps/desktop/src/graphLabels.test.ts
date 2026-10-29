import { describe, expect, it } from "vitest";
import { mulberry32 } from "./graph";
import {
  edgeAlpha,
  LABEL_MAX_MIN_ZOOM,
  LabelPlacer,
  labelEligible,
  labelMinZoom,
  labelPriority,
  newCandidate,
  Side,
  SMALL_GRAPH_LABELS,
  WidthCache,
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
  it("places a lone label below its node, centred", () => {
    const c = cand(0, 100, 100);
    expect(new LabelPlacer().place([c], 1, 800, 600)).toBe(1);
    expect(c.placed).toBe(true);
    expect(c.side).toBe(Side.Below);
    expect(c.left).toBeCloseTo(100 - 30);
    expect(c.top).toBeGreaterThan(100 + c.r);
  });

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

  it("moves a label to another side rather than dropping it", () => {
    // Two nodes stacked closely: the first (higher priority) takes "below", which covers the
    // second's "below" slot, so the second goes above instead.
    const a = cand(0, 100, 100, { priority: 10 });
    const b = cand(1, 100, 108, { priority: 5 });
    new LabelPlacer().place([a, b], 2, 800, 600);
    expect(a.placed && b.placed).toBe(true);
    expect(a.side).toBe(Side.Below);
    expect(b.side).not.toBe(Side.Below);
    assertNoOverlaps([a, b]);
  });

  it("respects priority: the higher priority label wins a contested spot", () => {
    // Identical positions: only a few labels fit around one spot; they must be the top ones.
    const cs = Array.from({ length: 8 }, (_, i) => cand(i, 200, 200, { priority: (i * 3) % 8 }));
    new LabelPlacer().place(cs, cs.length, 800, 600);
    const placed = cs.filter((c) => c.placed).map((c) => c.priority).sort((a, b) => b - a);
    expect(placed.length).toBeGreaterThan(0);
    expect(placed.length).toBeLessThan(8);
    expect(placed).toEqual([7, 6, 5, 4, 3, 2, 1, 0].slice(0, placed.length));
    // The best one gets the preferred side.
    expect(cs.find((c) => c.priority === 7)!.side).toBe(Side.Below);
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

  it("places a pinned label even when every side is blocked", () => {
    const pins = Array.from({ length: 6 }, (_, i) => cand(i, 100, 100, { pinned: true }));
    expect(new LabelPlacer().place(pins, pins.length, 800, 600)).toBe(6);
    expect(pins.every((c) => c.placed)).toBe(true);
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

  it("only considers the first `count` candidates and is reusable across frames", () => {
    const placer = new LabelPlacer();
    const cs = scatter(100, 800, 600, 3);
    placer.place(cs, 100, 800, 600);
    const first = cs.map((c) => [c.placed, c.left, c.top]);
    placer.place(cs, 100, 800, 600);
    expect(cs.map((c) => [c.placed, c.left, c.top])).toEqual(first);
    // A smaller frame: candidates past `count` are untouched by placement but reset is per-count.
    cs.forEach((c) => (c.placed = true));
    placer.place(cs, 10, 800, 600);
    expect(cs.slice(10).every((c) => c.placed)).toBe(true);
    assertNoOverlaps(cs.slice(0, 10));
  });

  it("handles 2,000 labels quickly", () => {
    const placer = new LabelPlacer();
    const cs = scatter(2000, 1600, 1000, 5);
    placer.place(cs, cs.length, 1600, 1000); // warm-up
    const t0 = performance.now();
    for (let i = 0; i < 5; i++) placer.place(cs, cs.length, 1600, 1000, 400);
    // Generous bound for slow CI; locally well under a millisecond or two.
    expect((performance.now() - t0) / 5).toBeLessThan(40);
  });
});

describe("label thresholds", () => {
  it("shows everything in small graphs", () => {
    expect(labelMinZoom(0, 5, SMALL_GRAPH_LABELS)).toBe(0);
  });

  it("lets better connected nodes show at lower zoom", () => {
    const max = 40;
    const zs = [0, 1, 3, 10, 40].map((d) => labelMinZoom(d, max, 500));
    for (let i = 1; i < zs.length; i++) expect(zs[i]!).toBeLessThan(zs[i - 1]!);
    expect(zs[0]).toBe(LABEL_MAX_MIN_ZOOM);
    expect(zs[zs.length - 1]).toBe(0);
    expect(labelMinZoom(0, 0, 500)).toBe(LABEL_MAX_MIN_ZOOM);
  });

  it("is eligible by zoom, or always when pinned", () => {
    const z = labelMinZoom(2, 40, 500);
    expect(labelEligible(z - 0.01, z, false)).toBe(false);
    expect(labelEligible(z, z, false)).toBe(true);
    expect(labelEligible(0.01, z, true)).toBe(true);
  });

  it("ranks highlight neighbours, then degree, then depth", () => {
    expect(labelPriority(0, 3, true)).toBeGreaterThan(labelPriority(500, 0, false));
    expect(labelPriority(5, 2, false)).toBeGreaterThan(labelPriority(4, 0, false));
    expect(labelPriority(5, 1, false)).toBeGreaterThan(labelPriority(5, 2, false));
  });
});

describe("edge alpha", () => {
  it("fades with density, zoom-out and highlight, within bounds", () => {
    expect(edgeAlpha(100, 1, false)).toBe(1);
    expect(edgeAlpha(3000, 1, false)).toBeLessThan(edgeAlpha(300, 1, false));
    expect(edgeAlpha(300, 0.1, false)).toBeLessThan(edgeAlpha(300, 1, false));
    expect(edgeAlpha(300, 1, true)).toBeLessThan(edgeAlpha(300, 1, false));
    for (const n of [0, 10, 1e6]) for (const k of [0.05, 1, 8]) {
      const a = edgeAlpha(n, k, false);
      expect(a).toBeGreaterThan(0);
      expect(a).toBeLessThanOrEqual(1);
    }
  });
});

describe("WidthCache", () => {
  it("measures each string once per font", () => {
    const cache = new WidthCache();
    let calls = 0;
    const measure = (s: string) => {
      calls++;
      return s.length * 7;
    };
    expect(cache.get("Ada", "12px a", measure)).toBe(21);
    expect(cache.get("Ada", "12px a", measure)).toBe(21);
    expect(calls).toBe(1);
    cache.get("Ada", "bold 12px a", measure);
    expect(calls).toBe(2);
    expect(cache.size).toBe(1);
  });
});
