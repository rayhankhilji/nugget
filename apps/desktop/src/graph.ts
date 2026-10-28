/**
 * Graph view model: turning `knowledge.graph` results into a capped, de-duplicated node/link set,
 * a seeded force layout (d3-force), and the view maths (fit, hit testing) the canvas uses.
 * Nothing here touches the DOM, so it is unit-tested directly.
 */
import {
  forceCollide,
  forceLink,
  forceManyBody,
  forceSimulation,
  forceX,
  forceY,
  type Simulation,
  type SimulationLinkDatum,
  type SimulationNodeDatum,
} from "d3-force";
import type { GraphView } from "./api";

/** The canvas draws at most this many nodes; the rest are dropped and "truncated" is shown. */
export const MAX_RENDER_NODES = 2000;
/** Above this many nodes, labels only show when zoomed in and collision is skipped. */
export const LARGE_GRAPH = 600;

export interface SimNode extends SimulationNodeDatum {
  id: string;
  kind: string;
  name: string;
  depth: number;
  degree: number;
  /** Radius in world units. */
  r: number;
  x: number;
  y: number;
}

export interface SimLink extends SimulationLinkDatum<SimNode> {
  source: SimNode | string;
  target: SimNode | string;
  /** Distinct predicates of the claims between the two entities. */
  predicates: string[];
  claims: number[];
}

export interface PreparedGraph {
  nodes: SimNode[];
  links: SimLink[];
  /** The daemon truncated its answer or the canvas cap dropped nodes. */
  truncated: boolean;
  /** Nodes dropped by the canvas cap. */
  omitted: number;
  center: string | null;
}

/** Small, fast seeded PRNG (mulberry32): same seed, same sequence, values in [0, 1). */
export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Stable 32-bit hash of a string (FNV-1a), for seeding a layout from e.g. the center id. */
export function hashSeed(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

export function nodeRadius(degree: number, isCenter: boolean): number {
  return (isCenter ? 9 : 4) + Math.min(8, Math.sqrt(degree) * 1.5);
}

function pairKey(a: string, b: string): string {
  return a < b ? `${a}\u0000${b}` : `${b}\u0000${a}`;
}

/**
 * De-duplicate nodes, merge parallel edges, drop dangling edges and self-loops, and cap the node
 * count. When capping, the center is kept first, then nodes by depth (nearest first) and degree
 * (most connected first). Nodes come back in that order, which is also the keyboard order.
 */
export function prepareGraph(
  view: GraphView,
  opts: { maxNodes?: number; center?: string | null } = {},
): PreparedGraph {
  const maxNodes = Math.max(1, opts.maxNodes ?? MAX_RENDER_NODES);
  const center = opts.center ?? null;

  const byId = new Map<string, { id: string; kind: string; name: string; depth: number }>();
  for (const n of view.nodes) if (!byId.has(n.id)) byId.set(n.id, n);

  const degree = new Map<string, number>();
  const linkMap = new Map<string, { a: string; b: string; predicates: Set<string>; claims: number[] }>();
  for (const e of view.edges) {
    if (e.from === e.to || !byId.has(e.from) || !byId.has(e.to)) continue;
    const key = pairKey(e.from, e.to);
    let l = linkMap.get(key);
    if (!l) {
      l = { a: e.from, b: e.to, predicates: new Set(), claims: [] };
      linkMap.set(key, l);
      degree.set(e.from, (degree.get(e.from) ?? 0) + 1);
      degree.set(e.to, (degree.get(e.to) ?? 0) + 1);
    }
    l.predicates.add(e.predicate);
    l.claims.push(e.claim);
  }

  const ordered = [...byId.values()].sort(
    (a, b) =>
      Number(b.id === center) - Number(a.id === center) ||
      a.depth - b.depth ||
      (degree.get(b.id) ?? 0) - (degree.get(a.id) ?? 0) ||
      a.name.localeCompare(b.name) ||
      a.id.localeCompare(b.id),
  );
  const kept = ordered.slice(0, maxNodes);
  const keptIds = new Set(kept.map((n) => n.id));
  const omitted = ordered.length - kept.length;

  const nodes: SimNode[] = kept.map((n) => {
    const d = degree.get(n.id) ?? 0;
    return { id: n.id, kind: n.kind, name: n.name, depth: n.depth, degree: d, r: nodeRadius(d, n.id === center), x: 0, y: 0 };
  });
  const links: SimLink[] = [];
  for (const l of linkMap.values()) {
    if (!keptIds.has(l.a) || !keptIds.has(l.b)) continue;
    links.push({ source: l.a, target: l.b, predicates: [...l.predicates].sort(), claims: l.claims });
  }
  return { nodes, links, truncated: view.truncated || omitted > 0, omitted, center: center && keptIds.has(center) ? center : null };
}

/** Deterministic starting positions: the center pinned at the origin, others in rings by depth. */
export function seedPositions(nodes: SimNode[], rng: () => number, center: string | null): void {
  const spread = Math.sqrt(nodes.length) * 6;
  for (const n of nodes) {
    if (n.id === center) {
      n.x = 0;
      n.y = 0;
      n.fx = 0;
      n.fy = 0;
      continue;
    }
    const angle = rng() * Math.PI * 2;
    const ring = n.depth > 0 ? n.depth * 80 : 20;
    const radius = ring + rng() * (40 + spread);
    n.x = Math.cos(angle) * radius;
    n.y = Math.sin(angle) * radius;
    n.vx = 0;
    n.vy = 0;
  }
}

/** A stopped simulation: the caller drives it with `tick()` (one or two per animation frame). */
export function createSimulation(graph: PreparedGraph, seed: number): Simulation<SimNode, SimLink> {
  const rng = mulberry32(seed);
  seedPositions(graph.nodes, rng, graph.center);
  const large = graph.nodes.length > LARGE_GRAPH;
  const sim = forceSimulation<SimNode, SimLink>(graph.nodes)
    .randomSource(rng)
    // Large graphs settle in ~130 ticks instead of ~300.
    .alphaDecay(large ? 0.05 : 0.0228)
    .velocityDecay(0.4)
    .force(
      "link",
      forceLink<SimNode, SimLink>(graph.links)
        .id((d) => d.id)
        .distance((l) => 30 + ((l.source as SimNode).r ?? 4) + ((l.target as SimNode).r ?? 4))
        .iterations(1),
    )
    .force(
      "charge",
      forceManyBody<SimNode>()
        .strength(large ? -35 : -140)
        // Coarser Barnes–Hut approximation for big graphs: about half the cost per tick.
        .theta(large ? 1.2 : 0.9)
        .distanceMax(large ? 350 : 900),
    )
    .force("x", forceX<SimNode>(0).strength(large ? 0.06 : 0.04))
    .force("y", forceY<SimNode>(0).strength(large ? 0.06 : 0.04))
    .stop();
  if (!large) sim.force("collide", forceCollide<SimNode>((d) => d.r + 3).iterations(1));
  return sim;
}

/** Run a layout to completion (or `ticks` steps) and return positions by id. For tests and previews. */
export function runLayout(graph: PreparedGraph, seed: number, ticks = 300): Map<string, { x: number; y: number }> {
  const sim = createSimulation(graph, seed);
  sim.tick(ticks);
  return new Map(graph.nodes.map((n) => [n.id, { x: n.x, y: n.y }]));
}

// ---- view maths ----

/** Screen = world × k + (x, y). */
export interface ViewTransform {
  k: number;
  x: number;
  y: number;
}

export const MIN_ZOOM = 0.05;
export const MAX_ZOOM = 8;

export function clampZoom(k: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, k));
}

/** Transform that fits every node into a `width` × `height` viewport with `padding` px around. */
export function fitTransform(nodes: readonly SimNode[], width: number, height: number, padding = 32): ViewTransform {
  if (nodes.length === 0 || width <= 0 || height <= 0) return { k: 1, x: width / 2, y: height / 2 };
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const n of nodes) {
    minX = Math.min(minX, n.x - n.r);
    minY = Math.min(minY, n.y - n.r);
    maxX = Math.max(maxX, n.x + n.r);
    maxY = Math.max(maxY, n.y + n.r);
  }
  const w = Math.max(1, maxX - minX);
  const h = Math.max(1, maxY - minY);
  const k = clampZoom(Math.min(2, (width - padding * 2) / w, (height - padding * 2) / h));
  return { k, x: width / 2 - ((minX + maxX) / 2) * k, y: height / 2 - ((minY + maxY) / 2) * k };
}

/** Zoom by `factor` keeping the screen point (`px`, `py`) fixed. */
export function zoomAt(t: ViewTransform, factor: number, px: number, py: number): ViewTransform {
  const k = clampZoom(t.k * factor);
  const wx = (px - t.x) / t.k;
  const wy = (py - t.y) / t.k;
  return { k, x: px - wx * k, y: py - wy * k };
}

export function toWorld(t: ViewTransform, sx: number, sy: number): { x: number; y: number } {
  return { x: (sx - t.x) / t.k, y: (sy - t.y) / t.k };
}

/**
 * The node under screen point (`sx`, `sy`), if any: the nearest one whose disc (plus a few
 * pixels of slop, so small nodes stay clickable when zoomed out) contains the point.
 */
export function nodeAt(nodes: readonly SimNode[], t: ViewTransform, sx: number, sy: number, slopPx = 4): SimNode | null {
  const p = toWorld(t, sx, sy);
  let best: SimNode | null = null;
  let bestD = Infinity;
  for (const n of nodes) {
    const dx = n.x - p.x;
    const dy = n.y - p.y;
    const d2 = dx * dx + dy * dy;
    const reach = n.r + slopPx / t.k;
    if (d2 <= reach * reach && d2 < bestD) {
      best = n;
      bestD = d2;
    }
  }
  return best;
}

/** Ids of the nodes directly linked to `id`. */
export function neighbours(links: readonly SimLink[], id: string): Set<string> {
  const out = new Set<string>();
  for (const l of links) {
    const a = typeof l.source === "string" ? l.source : l.source.id;
    const b = typeof l.target === "string" ? l.target : l.target.id;
    if (a === id) out.add(b);
    else if (b === id) out.add(a);
  }
  return out;
}
