import { describe, expect, it } from "vitest";
import type { GraphEdgeView, GraphNodeView, GraphView } from "./api";
import {
  MAX_RENDER_NODES,
  mulberry32,
  nearestInDirection,
  prepareGraph,
  runLayout,
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
});

describe("keyboard navigation", () => {
  const at = (id: string, x: number, y: number): SimNode => ({ id, kind: "person", name: id, depth: 1, degree: 1, r: 4, x, y });
  const o = at("o", 0, 0);
  const nodes = [o, at("r", 100, 10), at("far-r", 300, 0), at("l", -50, 0), at("u", 5, -80), at("d", 0, 60), at("dr", 40, 45)];

  it("moves to the nearest node in each direction", () => {
    expect(nearestInDirection(nodes, o, "right")?.id).toBe("r");
    expect(nearestInDirection(nodes, o, "left")?.id).toBe("l");
    expect(nearestInDirection(nodes, o, "up")?.id).toBe("u");
    expect(nearestInDirection(nodes, o, "down")?.id).toBe("d");
    expect(nearestInDirection([o], o, "down")).toBeNull();
  });
});
