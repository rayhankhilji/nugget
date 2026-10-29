/**
 * Graph label placement: which entity names to draw, and where, so labels never overlap.
 *
 * Every frame the canvas fills a pool of {@link LabelCandidate}s (screen-space anchors and label
 * sizes) and calls {@link LabelPlacer.place}. Placement is greedy over candidates sorted by
 * priority (pinned first: hovered, selected, focused, centre), trying a few positions around each
 * node and keeping the first that does not collide with an already placed label. Collisions are
 * checked against a uniform grid (spatial hash) of placed boxes, so a frame costs about O(n) plus
 * the sort. The placer keeps its buffers between frames to avoid garbage in the draw loop.
 *
 * Nothing here touches the DOM, so it is unit-tested directly.
 */

/** Zoom at which the least connected node's label becomes eligible (more connected: lower). */
export const LABEL_MAX_MIN_ZOOM = 2;
/** Graphs this small are labelled at any zoom (collision avoidance still applies). */
export const SMALL_GRAPH_LABELS = 40;

/**
 * The zoom (`k`) from which a node's label may be drawn: 0 for the best connected node, rising to
 * {@link LABEL_MAX_MIN_ZOOM} for isolated ones. Hovered/selected nodes ignore this.
 */
export function labelMinZoom(degree: number, maxDegree: number, nodeCount: number): number {
  if (nodeCount <= SMALL_GRAPH_LABELS) return 0;
  if (maxDegree <= 0) return LABEL_MAX_MIN_ZOOM;
  const frac = Math.min(1, Math.log1p(Math.max(0, degree)) / Math.log1p(maxDegree));
  const rest = 1 - frac;
  return LABEL_MAX_MIN_ZOOM * rest * rest;
}

/** Whether a label is eligible at zoom `k` (pinned labels always are). */
export function labelEligible(k: number, minZoom: number, pinned: boolean): boolean {
  return pinned || k >= minZoom;
}

/** Placement priority of a non-pinned label: degree first, nearness to the centre second. */
export function labelPriority(degree: number, depth: number, neighbourOfHighlight: boolean): number {
  return (neighbourOfHighlight ? 1e6 : 0) + degree * 16 + Math.max(0, 8 - depth);
}

/**
 * Edge opacity multiplier: dense or zoomed-out views fade edges so structure (and labels) stay
 * readable; with a highlighted node, every edge not touching it fades further.
 */
export function edgeAlpha(visibleEdges: number, k: number, highlight: boolean): number {
  const density = Math.sqrt(300 / Math.max(300, visibleEdges)); // 1 → ~0.3 at 3,000 edges
  const zoom = Math.min(1, 0.45 + k * 0.55);
  const a = Math.max(0.12, Math.min(1, density * zoom));
  return highlight ? a * 0.35 : a;
}

/** Where a label went relative to its node. */
export const Side = { Below: 0, Above: 1, Right: 2, Left: 3 } as const;
export type Side = (typeof Side)[keyof typeof Side];

export interface LabelCandidate {
  /** Index of the node in the graph's node array (the caller's handle). */
  node: number;
  /** Node centre in screen px. */
  x: number;
  y: number;
  /** Node radius in screen px. */
  r: number;
  /** Label size in screen px. */
  w: number;
  h: number;
  priority: number;
  /** Always placed (hovered, selected, focused, centre). */
  pinned: boolean;
  // ---- output ----
  placed: boolean;
  /** Top-left of the placed box, screen px. */
  left: number;
  top: number;
  side: Side;
}

export function newCandidate(): LabelCandidate {
  return { node: 0, x: 0, y: 0, r: 0, w: 0, h: 0, priority: 0, pinned: false, placed: false, left: 0, top: 0, side: Side.Below };
}

/** Gap between a node and its label, and between labels (px). */
const GAP = 2;
const PAD = 1;
const SIDES = [Side.Below, Side.Above, Side.Right, Side.Left] as const;

function boxFor(c: LabelCandidate, side: Side, out: { l: number; t: number }): void {
  switch (side) {
    case Side.Below:
      out.l = c.x - c.w / 2;
      out.t = c.y + c.r + GAP;
      break;
    case Side.Above:
      out.l = c.x - c.w / 2;
      out.t = c.y - c.r - GAP - c.h;
      break;
    case Side.Right:
      out.l = c.x + c.r + GAP + 1;
      out.t = c.y - c.h / 2;
      break;
    case Side.Left:
      out.l = c.x - c.r - GAP - 1 - c.w;
      out.t = c.y - c.h / 2;
      break;
  }
}

/**
 * Greedy, priority-ordered label placement with a spatial hash. Reuse one instance across frames.
 */
export class LabelPlacer {
  /** Grid cell size in px; roughly one short label wide. */
  readonly cell: number;
  private grid = new Map<number, number[]>();
  private usedKeys: number[] = [];
  /** Placed boxes as [l, t, r, b] quads. */
  private boxes = new Float64Array(4 * 256);
  private nBoxes = 0;
  private order: number[] = [];
  private tmp = { l: 0, t: 0 };

  constructor(cell = 64) {
    this.cell = cell;
  }

  /**
   * Place `cands[0..count)`, writing `placed`, `left`, `top` and `side` on each. Labels outside
   * the `width` × `height` viewport are skipped unless pinned; at most `maxLabels` are placed
   * besides pinned ones. Returns the number placed.
   */
  place(cands: readonly LabelCandidate[], count: number, width: number, height: number, maxLabels = Infinity): number {
    this.reset();
    const order = this.order;
    order.length = count;
    for (let i = 0; i < count; i++) {
      order[i] = i;
      cands[i]!.placed = false;
    }
    order.sort((a, b) => {
      const ca = cands[a]!;
      const cb = cands[b]!;
      return Number(cb.pinned) - Number(ca.pinned) || cb.priority - ca.priority || ca.node - cb.node;
    });

    let placed = 0;
    let free = 0;
    const tmp = this.tmp;
    for (let oi = 0; oi < count; oi++) {
      const c = cands[order[oi]!]!;
      if (!c.pinned && free >= maxLabels) break;
      let chosen: Side | -1 = -1;
      for (let s = 0; s < SIDES.length; s++) {
        boxFor(c, SIDES[s]!, tmp);
        const l = tmp.l;
        const t = tmp.t;
        if (!c.pinned && (l + c.w < 0 || t + c.h < 0 || l > width || t > height)) continue;
        if (!this.collides(l - PAD, t - PAD, l + c.w + PAD, t + c.h + PAD)) {
          chosen = SIDES[s]!;
          break;
        }
      }
      if (chosen === -1 && !c.pinned) continue;
      // Pinned labels always show, below their node if they must overlap.
      const side: Side = chosen === -1 ? Side.Below : chosen;
      boxFor(c, side, tmp);
      c.placed = true;
      c.side = side;
      c.left = tmp.l;
      c.top = tmp.t;
      this.insert(tmp.l, tmp.t, tmp.l + c.w, tmp.t + c.h);
      placed++;
      if (!c.pinned) free++;
    }
    return placed;
  }

  private reset(): void {
    for (const key of this.usedKeys) this.grid.get(key)!.length = 0;
    this.usedKeys.length = 0;
    this.nBoxes = 0;
  }

  private key(cx: number, cy: number): number {
    // Cells fit in 16 bits each for any realistic viewport (offset keeps negatives positive).
    return ((cx + 32768) & 0xffff) * 65536 + ((cy + 32768) & 0xffff);
  }

  private collides(l: number, t: number, r: number, b: number): boolean {
    const cs = this.cell;
    const cx0 = Math.floor(l / cs);
    const cx1 = Math.floor(r / cs);
    const cy0 = Math.floor(t / cs);
    const cy1 = Math.floor(b / cs);
    const boxes = this.boxes;
    for (let cx = cx0; cx <= cx1; cx++) {
      for (let cy = cy0; cy <= cy1; cy++) {
        const list = this.grid.get(this.key(cx, cy));
        if (!list) continue;
        for (let i = 0; i < list.length; i++) {
          const o = list[i]! * 4;
          if (l < boxes[o + 2]! && r > boxes[o]! && t < boxes[o + 3]! && b > boxes[o + 1]!) return true;
        }
      }
    }
    return false;
  }

  private insert(l: number, t: number, r: number, b: number): void {
    const id = this.nBoxes++;
    if (id * 4 + 4 > this.boxes.length) {
      const next = new Float64Array(this.boxes.length * 2);
      next.set(this.boxes);
      this.boxes = next;
    }
    const o = id * 4;
    this.boxes[o] = l;
    this.boxes[o + 1] = t;
    this.boxes[o + 2] = r;
    this.boxes[o + 3] = b;
    const cs = this.cell;
    for (let cx = Math.floor(l / cs); cx <= Math.floor(r / cs); cx++) {
      for (let cy = Math.floor(t / cs); cy <= Math.floor(b / cs); cy++) {
        const k = this.key(cx, cy);
        let list = this.grid.get(k);
        if (!list) {
          list = [];
          this.grid.set(k, list);
        }
        if (list.length === 0) this.usedKeys.push(k);
        list.push(id);
      }
    }
  }
}

/** Text width cache: `measure` is only called once per distinct string (per font). */
export class WidthCache {
  private widths = new Map<string, number>();
  private font = "";

  /** Width of `text` in `font`, measuring with `measure` on a miss. */
  get(text: string, font: string, measure: (s: string) => number): number {
    if (font !== this.font) {
      this.widths.clear();
      this.font = font;
    }
    let w = this.widths.get(text);
    if (w === undefined) {
      w = measure(text);
      if (this.widths.size > 10_000) this.widths.clear();
      this.widths.set(text, w);
    }
    return w;
  }

  get size(): number {
    return this.widths.size;
  }
}
