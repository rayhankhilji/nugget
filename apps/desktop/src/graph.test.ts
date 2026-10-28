import { describe, expect, it } from "vitest";
import type { GraphEdgeView, GraphNodeView, GraphView } from "./api";
import {
  createSimulation,
  fitTransform,
  hashSeed,
  MAX_RENDER_NODES,
  mulberry32,
  neighbours,
  nodeAt,
  prepareGraph,
  runLayout,
  toWorld,
  zoomAt,
  type SimNode,
} from "./graph";

const node = (id: string, depth = 1, kind: GraphNodeView["kind"] = "person"): GraphNodeView => ({ id, kind, name: id.toUpperCase(), depth });
const edge = (claim: number, from: string, to: string, predicate = "knows"): GraphEdgeView => ({ claim, from, to, predicate });

/** A star around `c` plus a chain, enough structure for the layout to mean something. */
function sample(): GraphView {
  return {
    nodes: [node("c", 0), node("a"), node("b"), node("d"), node("e", 2), node("f", 2)],
    edges: [edge(1, "c", "a"), edge(2, "c", "b"), edge(3, "c", "d"), edge(4, "a", "e"), edge(5, "b", "f")],
    truncated: false,
  };
}

/** A random-ish graph of `n` nodes with ~`n * 1.5` edges, from a seed. */
function big(n: number, seed = 7): GraphView {
  const rng = mulberry32(seed);
  const nodes = Array.from({ length: n }, (_, i) => node(`n${i}`, i === 0 ? 0 : 1 + Math.floor(rng() * 3), i % 3 === 0 ? "project" : "person"));
  const edges: GraphEdgeView[] = [];
  for (let i = 1; i < n; i++) edges.push(edge(i, `n${Math.floor(rng() * i)}`, `n${i}`));
  for (let i = 0; i < n / 2; i++) edges.push(edge(n + i, `n${Math.floor(rng() * n)}`, `n${Math.floor(rng() * n)}`));
  return { nodes, edges, truncated: false };
}

describe("mulberry32", () => {
  it("is deterministic per seed and stays in [0, 1)", () => {
    const a = mulberry32(42);
    const b = mulberry32(42);
    const c = mulberry32(43);
    const xs = Array.from({ length: 1000 }, () => a());
    expect(Array.from({ length: 1000 }, () => b())).toEqual(xs);
    expect(Array.from({ length: 5 }, () => c())).not.toEqual(xs.slice(0, 5));
    expect(xs.every((x) => x >= 0 && x < 1)).toBe(true);
    // Roughly uniform.
    const mean = xs.reduce((s, x) => s + x, 0) / xs.length;
    expect(mean).toBeGreaterThan(0.45);
    expect(mean).toBeLessThan(0.55);
  });

  it("hashSeed is stable", () => {
    expect(hashSeed("ent_1")).toBe(hashSeed("ent_1"));
    expect(hashSeed("ent_1")).not.toBe(hashSeed("ent_2"));
  });
});

describe("prepareGraph", () => {
  it("drops duplicate nodes, dangling edges and self-loops, and merges parallel edges", () => {
    const g = prepareGraph(
      {
        nodes: [node("a"), node("b"), node("a")],
        edges: [edge(1, "a", "b", "works_with"), edge(2, "b", "a", "knows"), edge(3, "a", "a"), edge(4, "a", "zzz")],
        truncated: false,
      },
      { center: "a" },
    );
    expect(g.nodes.map((n) => n.id)).toEqual(["a", "b"]);
    expect(g.links).toHaveLength(1);
    expect(g.links[0]!.predicates).toEqual(["knows", "works_with"]);
    expect(g.links[0]!.claims).toEqual([1, 2]);
    expect(g.nodes.find((n) => n.id === "a")!.degree).toBe(1);
    expect(g.truncated).toBe(false);
    expect(g.center).toBe("a");
  });

  it("orders center first, then by depth and degree", () => {
    const g = prepareGraph(sample(), { center: "c" });
    expect(g.nodes[0]!.id).toBe("c");
    const depths = g.nodes.map((n) => n.depth);
    expect(depths).toEqual([...depths].sort((x, y) => x - y));
    // Center is drawn larger.
    expect(g.nodes[0]!.r).toBeGreaterThan(g.nodes[1]!.r);
  });

  it("caps rendering and reports truncation", () => {
    const g = prepareGraph(big(2600), { center: "n0" });
    expect(g.nodes).toHaveLength(MAX_RENDER_NODES);
    expect(g.omitted).toBe(600);
    expect(g.truncated).toBe(true);
    expect(g.nodes[0]!.id).toBe("n0");
    const ids = new Set(g.nodes.map((n) => n.id));
    for (const l of g.links) {
      expect(ids.has(l.source as string)).toBe(true);
      expect(ids.has(l.target as string)).toBe(true);
    }
    // The daemon's own truncation flag is passed on.
    expect(prepareGraph({ ...sample(), truncated: true }).truncated).toBe(true);
    // A center outside the kept set is dropped.
    expect(prepareGraph(sample(), { center: "missing" }).center).toBeNull();
  });
});

describe("force layout", () => {
  it("is deterministic for a seed", () => {
    const a = runLayout(prepareGraph(sample(), { center: "c" }), 1234);
    const b = runLayout(prepareGraph(sample(), { center: "c" }), 1234);
    expect([...b.entries()]).toEqual([...a.entries()]);
    const c = runLayout(prepareGraph(sample(), { center: "c" }), 99);
    expect([...c.values()]).not.toEqual([...a.values()]);
  });

  it("pins the center and keeps linked nodes close", () => {
    const g = prepareGraph(sample(), { center: "c" });
    const pos = runLayout(g, 5);
    expect(pos.get("c")).toEqual({ x: 0, y: 0 });
    for (const p of pos.values()) {
      expect(Number.isFinite(p.x) && Number.isFinite(p.y)).toBe(true);
    }
    const dist = (a: string, b: string) => Math.hypot(pos.get(a)!.x - pos.get(b)!.x, pos.get(a)!.y - pos.get(b)!.y);
    // Linked pairs end up closer than the farthest unlinked pair.
    expect(dist("c", "a")).toBeLessThan(dist("e", "f"));
    expect(dist("a", "e")).toBeLessThan(dist("e", "f"));
  });

  it("ticks 2,000 nodes fast enough to animate", () => {
    const g = prepareGraph(big(MAX_RENDER_NODES), { center: "n0" });
    const sim = createSimulation(g, 3);
    sim.tick(2); // warm-up (quadtree allocation, JIT)
    const t0 = performance.now();
    sim.tick(10);
    const perTick = (performance.now() - t0) / 10;
    // Generous bound so slow CI machines pass; locally this is a few ms.
    expect(perTick).toBeLessThan(60);
    expect(g.nodes.every((n) => Number.isFinite(n.x) && Number.isFinite(n.y))).toBe(true);
  });
});

describe("view maths", () => {
  const nodes: SimNode[] = [
    { id: "a", kind: "person", name: "A", depth: 0, degree: 1, r: 5, x: -100, y: 0 },
    { id: "b", kind: "person", name: "B", depth: 1, degree: 1, r: 5, x: 100, y: 50 },
  ];

  it("fits all nodes in the viewport", () => {
    const t = fitTransform(nodes, 800, 600, 20);
    for (const n of nodes) {
      const sx = n.x * t.k + t.x;
      const sy = n.y * t.k + t.y;
      expect(sx).toBeGreaterThanOrEqual(20 - 1e-6);
      expect(sx).toBeLessThanOrEqual(780 + 1e-6);
      expect(sy).toBeGreaterThanOrEqual(20 - 1e-6);
      expect(sy).toBeLessThanOrEqual(580 + 1e-6);
    }
    expect(fitTransform([], 800, 600)).toEqual({ k: 1, x: 400, y: 300 });
  });

  it("zooms around the pointer", () => {
    const t = { k: 1, x: 10, y: 20 };
    const before = toWorld(t, 300, 200);
    const z = zoomAt(t, 2, 300, 200);
    expect(z.k).toBe(2);
    const after = toWorld(z, 300, 200);
    expect(after.x).toBeCloseTo(before.x);
    expect(after.y).toBeCloseTo(before.y);
    expect(zoomAt(t, 1e9, 0, 0).k).toBe(8);
  });

  it("hit-tests nodes in screen space", () => {
    const t = { k: 2, x: 400, y: 300 };
    expect(nodeAt(nodes, t, -100 * 2 + 400, 300)?.id).toBe("a");
    expect(nodeAt(nodes, t, 100 * 2 + 400 + 3, 50 * 2 + 300)?.id).toBe("b");
    expect(nodeAt(nodes, t, 400, 300)).toBeNull();
  });

  it("finds neighbours on resolved and unresolved links", () => {
    const g = prepareGraph(sample(), { center: "c" });
    expect([...neighbours(g.links, "c")].sort()).toEqual(["a", "b", "d"]);
    createSimulation(g, 1); // resolves link endpoints to node objects
    expect([...neighbours(g.links, "a")].sort()).toEqual(["c", "e"]);
  });
});
