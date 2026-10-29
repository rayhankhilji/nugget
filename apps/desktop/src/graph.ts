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

/**
 * Deterministic starting positions. The center is pinned at the origin. Nodes are visited in their
 * prepared order (center, then by depth and degree); a node with already placed neighbours starts
 * near their mean, others on a golden-angle spiral. Starting close to the final shape lets the
 * force layout settle in far fewer ticks than random positions.
 */
export function seedPositions(nodes: SimNode[], rng: () => number, center: string | null, links: readonly SimLink[] = []): void {
  const index = new Map<string, number>();
  nodes.forEach((n, i) => index.set(n.id, i));
  // Adjacency as CSR arrays: no per-node allocations.
  const deg = new Int32Array(nodes.length + 1);
  const ends: [number, number][] = [];
  for (const l of links) {
    const a = index.get(typeof l.source === "string" ? l.source : l.source.id);
    const b = index.get(typeof l.target === "string" ? l.target : l.target.id);
    if (a === undefined || b === undefined) continue;
    ends.push([a, b]);
    deg[a + 1]!++;
    deg[b + 1]!++;
  }
  for (let i = 0; i < nodes.length; i++) deg[i + 1]! += deg[i]!;
  const adj = new Int32Array(deg[nodes.length]!);
  const fill = deg.slice(0, nodes.length);
  for (const [a, b] of ends) {
    adj[fill[a]!++] = b;
    adj[fill[b]!++] = a;
  }

  const placed = new Uint8Array(nodes.length);
  const golden = Math.PI * (3 - Math.sqrt(5));
  let spiral = 0;
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i]!;
    n.vx = 0;
    n.vy = 0;
    if (n.id === center) {
      n.x = 0;
      n.y = 0;
      n.fx = 0;
      n.fy = 0;
      placed[i] = 1;
      continue;
    }
    let sx = 0;
    let sy = 0;
    let k = 0;
    for (let j = deg[i]!; j < deg[i + 1]!; j++) {
      const m = adj[j]!;
      if (!placed[m]) continue;
      sx += nodes[m]!.x;
      sy += nodes[m]!.y;
      k++;
    }
    const angle = rng() * Math.PI * 2;
    if (k > 0) {
      const radius = 25 + rng() * 35;
      n.x = sx / k + Math.cos(angle) * radius;
      n.y = sy / k + Math.sin(angle) * radius;
    } else {
      // Unconnected to anything placed: next spiral slot, nudged so ties don't line up.
      const s = spiral++ + (center ? 1 : 0);
      const radius = 40 * Math.sqrt(s + 0.5);
      const a = s * golden + (rng() - 0.5) * 0.2;
      n.x = Math.cos(a) * radius;
      n.y = Math.sin(a) * radius;
    }
    placed[i] = 1;
  }
}

/**
 * Ticks to run before the first paint (off-screen), so the user never sees the initial explosion.
 * Bounded by count, not time, so the result is deterministic.
 */
export function preTicks(nodeCount: number): number {
  if (nodeCount <= 200) return 120;
  if (nodeCount <= LARGE_GRAPH) return 60;
  if (nodeCount <= 1200) return 30;
  return 20;
}

/** Mean node speed (world units per tick) below which the layout counts as settled. */
export const SETTLED_SPEED = 0.05;

/** True when the nodes have (nearly) stopped moving: mean speed under `threshold`. */
export function isSettled(nodes: readonly SimNode[], threshold = SETTLED_SPEED): boolean {
  if (nodes.length === 0) return true;
  let sum = 0;
  for (const n of nodes) sum += Math.abs(n.vx ?? 0) + Math.abs(n.vy ?? 0);
  return sum / nodes.length < threshold;
}

/** A stopped simulation: the caller drives it with `tick()` (one or two per animation frame). */
export function createSimulation(graph: PreparedGraph, seed: number): Simulation<SimNode, SimLink> {
  const rng = mulberry32(seed);
  seedPositions(graph.nodes, rng, graph.center, graph.links);
  const large = graph.nodes.length > LARGE_GRAPH;
  const sim = forceSimulation<SimNode, SimLink>(graph.nodes)
    .randomSource(rng)
    // Seeded near their final shape, graphs settle well before alpha reaches the default minimum:
    // large ones in ~130 ticks, small ones in ~200 (instead of ~300).
    .alphaMin(0.002)
    .alphaDecay(large ? 0.05 : 0.03)
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

// ---- keyboard navigation ----

export type Direction = "left" | "right" | "up" | "down";

/**
 * The node to move to from `from` when pressing an arrow key: the closest one in that direction,
 * favouring nodes straight ahead (off-axis distance counts double). Nodes outside a 60° half-angle
 * cone are only used when nothing is inside it. O(n) per key press.
 */
export function nearestInDirection(nodes: readonly SimNode[], from: SimNode, dir: Direction): SimNode | null {
  const ax = dir === "left" ? -1 : dir === "right" ? 1 : 0;
  const ay = dir === "up" ? -1 : dir === "down" ? 1 : 0;
  let best: SimNode | null = null;
  let bestScore = Infinity;
  let bestInCone = false;
  for (const n of nodes) {
    if (n === from) continue;
    const dx = n.x - from.x;
    const dy = n.y - from.y;
    const along = dx * ax + dy * ay;
    if (along <= 1e-9) continue;
    const across = Math.abs(dx * ay - dy * ax);
    const inCone = across <= along * Math.sqrt(3);
    const score = along + across * 2;
    if ((inCone && !bestInCone) || (inCone === bestInCone && score < bestScore)) {
      best = n;
      bestScore = score;
      bestInCone = inCone;
    }
  }
  return best;
}

/** The node nearest the world point (`x`, `y`): where keyboard navigation starts. */
export function nearestTo(nodes: readonly SimNode[], x: number, y: number): SimNode | null {
  let best: SimNode | null = null;
  let bestD = Infinity;
  for (const n of nodes) {
    const d = (n.x - x) ** 2 + (n.y - y) ** 2;
    if (d < bestD) {
      best = n;
      bestD = d;
    }
  }
  return best;
}

/**
 * Next index when cycling with Tab (`step` 1) or Shift-Tab (-1) from `current`, or `null` when
 * Tab should leave the canvas instead: nothing is focused yet (`current` -1, e.g. after Escape)
 * or the cycle runs off either end. That keeps Tab from trapping keyboard users in a big graph.
 */
export function cycleIndex(current: number, step: 1 | -1, count: number): number | null {
  if (count === 0 || current < 0) return null;
  const next = current + step;
  return next < 0 || next >= count ? null : next;
}
