import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent, type PointerEvent } from "react";
import type { Simulation } from "d3-force";
import {
  createSimulation,
  cycleIndex,
  fitTransform,
  isSettled,
  nearestInDirection,
  nearestTo,
  nodeAt,
  preTicks,
  toWorld,
  zoomAt,
  type Direction,
  type PreparedGraph,
  type SimLink,
  type SimNode,
  type ViewTransform,
} from "../graph";
import { edgeAlpha, LabelPlacer, labelEligible, labelMinZoom, labelPriority, newCandidate, WidthCache, type LabelCandidate } from "../graphLabels";
import { ENTITY_KINDS } from "../api";
import { formatNumber, useT } from "../i18n";
import { kindLabel, kindPlural } from "../knowledge";
import "./graph.css";

/** Theme colours, read from CSS custom properties so the canvas follows light/dark mode. */
interface Palette {
  bg: string;
  text: string;
  muted: string;
  edge: string;
  edgeHi: string;
  accent: string;
  focus: string;
  kinds: Record<string, string>;
  other: string;
}

function readPalette(el: Element): Palette {
  const css = getComputedStyle(el);
  const v = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  const kinds: Record<string, string> = {};
  for (const k of ENTITY_KINDS) kinds[k] = v(`--kind-${k}`, "#888");
  return {
    bg: v("--surface", "#fff"),
    text: v("--text", "#000"),
    muted: v("--muted", "#666"),
    edge: v("--graph-edge", "rgba(128,128,128,0.35)"),
    edgeHi: v("--accent", "#c98a0c"),
    accent: v("--accent", "#c98a0c"),
    focus: v("--focus", "#1a66d9"),
    kinds,
    other: v("--muted", "#888"),
  };
}

const MAX_LABELS = 400;
const LABEL_CHARS = 28;
const PAN_STEP = 60;
const LABEL_FONT = `12px system-ui, -apple-system, "Segoe UI", sans-serif`;
const LABEL_FONT_BOLD = `600 12px system-ui, -apple-system, "Segoe UI", sans-serif`;
const LABEL_HEIGHT = 14;
/** Opacity of nodes outside the highlighted neighbourhood. */
const DIM_NODE_ALPHA = 0.35;
/** Upper bound on the ticks of a layout settled in one go (reduced motion). */
const MAX_SETTLE_TICKS = 400;

const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";

/**
 * Run `sim` to rest without drawing: until its alpha runs out, its nodes stop moving or
 * `MAX_SETTLE_TICKS` (counted, not timed, so the result is deterministic). Then stop it.
 */
/**
 * What "Fit" frames: the connected entities when they are nearly all of them, so one or two
 * isolated nodes drifting to the edges don't shrink the main cluster to a corner.
 */
function fitNodes(nodes: readonly SimNode[]): readonly SimNode[] {
  const linked = nodes.filter((n) => n.degree > 0);
  return linked.length >= 3 && linked.length >= nodes.length * 0.85 ? linked : nodes;
}

function settleNow(sim: Simulation<SimNode, SimLink>, nodes: readonly SimNode[]) {
  for (let i = 0; i < MAX_SETTLE_TICKS && sim.alpha() > sim.alphaMin(); i++) {
    sim.tick();
    if (sim.alpha() < 0.3 && isSettled(nodes)) break;
  }
  sim.alpha(0);
}

function shortName(s: string): string {
  const chars = Array.from(s);
  return chars.length > LABEL_CHARS ? `${chars.slice(0, LABEL_CHARS - 1).join("")}…` : s;
}

const ARROWS: Record<string, Direction> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };

type Gesture =
  | { kind: "node"; node: SimNode; startX: number; startY: number; moved: boolean }
  | { kind: "pan"; startX: number; startY: number; t0: ViewTransform; moved: boolean };

/**
 * Force-directed graph on a `<canvas>`. Drag nodes, drag the background to pan, wheel to zoom,
 * click to open, double-click to centre. Keyboard: arrows move to the nearest entity in that
 * direction (Shift+arrows pan), Tab / Shift+Tab and N / P step through entities, Enter opens,
 * C centres, Escape clears, + / − zoom, F or 0 fits. Labels are placed without overlaps (most
 * connected first; hovered, selected and focused always). The list view is the accessible
 * alternative for everything here.
 *
 * With `prefers-reduced-motion: reduce` (followed live) nothing moves on its own: the layout is
 * settled before the first paint, the view jumps instead of easing, dragging a node moves only
 * that node, and a frame is drawn only when something changed.
 */
export function GraphCanvas({
  graph,
  seed,
  selected,
  focused,
  onFocusChange,
  onOpen,
  onCenter,
}: {
  graph: PreparedGraph;
  seed: number;
  selected: string | null;
  focused: string | null;
  onFocusChange: (id: string | null) => void;
  onOpen: (id: string) => void;
  onCenter: (id: string) => void;
}) {
  const t = useT();
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const simRef = useRef<Simulation<SimNode, SimLink> | null>(null);
  const tRef = useRef<ViewTransform>({ k: 1, x: 0, y: 0 });
  const sizeRef = useRef({ w: 0, h: 0, dpr: 1 });
  const userMoved = useRef(false);
  const hoverRef = useRef<string | null>(null);
  const gestureRef = useRef<Gesture | null>(null);
  const paletteRef = useRef<Palette | null>(null);
  const rafRef = useRef(0);
  const reducedMotion = useRef(false);
  // Label placement state, reused across frames (no allocations in the draw loop).
  const placerRef = useRef<LabelPlacer | null>(null);
  const widthsRef = useRef<WidthCache | null>(null);
  const boldWidthsRef = useRef<WidthCache | null>(null);
  const candsRef = useRef<LabelCandidate[]>([]);
  const [announce, setAnnounce] = useState("");
  const helpId = useId();

  // Latest props for the draw loop, which lives outside React's render cycle.
  const live = useRef({ selected, focused, onOpen, onCenter, onFocusChange });
  live.current = { selected, focused, onOpen, onCenter, onFocusChange };

  const index = useMemo(() => {
    const byId = new Map(graph.nodes.map((n) => [n.id, n]));
    const pos = new Map(graph.nodes.map((n, i) => [n.id, i]));
    const adj = new Map<string, Set<string>>();
    for (const l of graph.links) {
      const a = typeof l.source === "string" ? l.source : l.source.id;
      const b = typeof l.target === "string" ? l.target : l.target.id;
      if (!adj.has(a)) adj.set(a, new Set());
      if (!adj.has(b)) adj.set(b, new Set());
      adj.get(a)!.add(b);
      adj.get(b)!.add(a);
    }
    const byKind = new Map<string, SimNode[]>();
    let maxDegree = 0;
    for (const n of graph.nodes) {
      const list = byKind.get(n.kind);
      if (list) list.push(n);
      else byKind.set(n.kind, [n]);
      maxDegree = Math.max(maxDegree, n.degree);
    }
    const labels = graph.nodes.map((n) => shortName(n.name));
    const minZoom = Float32Array.from(graph.nodes, (n) => labelMinZoom(n.degree, maxDegree, graph.nodes.length));
    return { byId, pos, adj, byKind, labels, minZoom };
  }, [graph]);
  const indexRef = useRef(index);
  indexRef.current = index;
  // Listeners registered once (wheel, resize, theme) reach the draw loop through refs only.
  const graphRef = useRef(graph);
  graphRef.current = graph;

  // Open where there's room; a closed legend doesn't cover the canvas on small windows.
  const [legendOpen] = useState(() => typeof window === "undefined" || window.innerWidth >= 1200);
  const legend = useMemo(() => {
    const known = ENTITY_KINDS.filter((k) => index.byKind.has(k)).map((k) => ({ kind: k, count: index.byKind.get(k)!.length }));
    const extra = [...index.byKind.keys()]
      .filter((k) => !(ENTITY_KINDS as readonly string[]).includes(k))
      .sort()
      .map((k) => ({ kind: k, count: index.byKind.get(k)!.length }));
    return [...known, ...extra];
  }, [index]);

  const draw = () => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    if (!paletteRef.current) paletteRef.current = readPalette(canvas);
    const pal = paletteRef.current;
    const { w, h, dpr } = sizeRef.current;
    const tf = tRef.current;
    const { byId, adj, byKind, labels, minZoom } = indexRef.current;
    const graph = graphRef.current;
    const { selected: sel, focused: foc } = live.current;
    const hover = hoverRef.current;
    const hl = hover ?? foc ?? sel;
    const hlNbrs = hl ? adj.get(hl) : undefined;
    const lit = (n: SimNode) => n.id === hl || (hlNbrs !== undefined && hlNbrs.has(n.id));

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.globalAlpha = 1;
    ctx.fillStyle = pal.bg;
    ctx.fillRect(0, 0, w, h);

    ctx.setTransform(dpr * tf.k, 0, 0, dpr * tf.k, dpr * tf.x, dpr * tf.y);
    // Visible world rectangle, for culling.
    const x0 = -tf.x / tf.k;
    const y0 = -tf.y / tf.k;
    const x1 = (w - tf.x) / tf.k;
    const y1 = (h - tf.y) / tf.k;
    const visible = (n: SimNode) => n.x + n.r >= x0 && n.x - n.r <= x1 && n.y + n.r >= y0 && n.y - n.r <= y1;

    // Edges: one faded path for all (fainter when dense, zoomed out or something is highlighted),
    // then the highlighted node's edges on top.
    ctx.lineWidth = 1 / tf.k;
    ctx.strokeStyle = pal.edge;
    ctx.beginPath();
    let drawn = 0;
    for (const l of graph.links) {
      const s = l.source as SimNode;
      const d = l.target as SimNode;
      if ((s.x < x0 && d.x < x0) || (s.x > x1 && d.x > x1) || (s.y < y0 && d.y < y0) || (s.y > y1 && d.y > y1)) continue;
      if (hl && (s.id === hl || d.id === hl)) continue;
      ctx.moveTo(s.x, s.y);
      ctx.lineTo(d.x, d.y);
      drawn++;
    }
    ctx.globalAlpha = edgeAlpha(drawn, tf.k, hl !== null);
    ctx.stroke();
    ctx.globalAlpha = 1;
    if (hl) {
      ctx.lineWidth = 2 / tf.k;
      ctx.strokeStyle = pal.edgeHi;
      ctx.beginPath();
      for (const l of graph.links) {
        const s = l.source as SimNode;
        const d = l.target as SimNode;
        if (s.id !== hl && d.id !== hl) continue;
        ctx.moveTo(s.x, s.y);
        ctx.lineTo(d.x, d.y);
      }
      ctx.stroke();
    }

    // Nodes: one fill per kind; with a highlight, everything outside its neighbourhood is dimmed.
    for (let pass = hl ? 0 : 1; pass < 2; pass++) {
      ctx.globalAlpha = pass === 0 ? DIM_NODE_ALPHA : 1;
      for (const [kind, nodes] of byKind) {
        ctx.fillStyle = pal.kinds[kind] ?? pal.other;
        ctx.beginPath();
        for (const n of nodes) {
          if (!visible(n)) continue;
          if (hl && (pass === 1) !== lit(n)) continue;
          ctx.moveTo(n.x + n.r, n.y);
          ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
        }
        ctx.fill();
      }
    }
    ctx.globalAlpha = 1;

    const ring = (id: string | null, color: string, width: number, gap: number) => {
      const n = id ? byId.get(id) : undefined;
      if (!n) return;
      ctx.strokeStyle = color;
      ctx.lineWidth = width / tf.k;
      ctx.beginPath();
      ctx.arc(n.x, n.y, n.r + gap / tf.k, 0, Math.PI * 2);
      ctx.stroke();
    };
    ring(graph.center, pal.accent, 2.5, 2);
    ring(sel, pal.text, 2, 3.5);
    ring(hover, pal.muted, 1.5, 2);
    ring(foc, pal.focus, 2.5, 6);

    // Labels, in screen space at a constant size: candidates are the visible nodes whose zoom
    // threshold (by degree) is met, plus the highlighted neighbourhood; pinned ones always.
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.font = LABEL_FONT;
    const widths = (widthsRef.current ??= new WidthCache());
    const boldWidths = (boldWidthsRef.current ??= new WidthCache());
    const placer = (placerRef.current ??= new LabelPlacer());
    const pool = candsRef.current;
    // Only called on a cache miss; the font is restored after the loop.
    const measure = (s: string) => {
      ctx.font = LABEL_FONT;
      return ctx.measureText(s).width;
    };
    const measureBold = (s: string) => {
      ctx.font = LABEL_FONT_BOLD;
      return ctx.measureText(s).width;
    };
    let count = 0;
    const nodes = graph.nodes;
    for (let i = 0; i < nodes.length; i++) {
      const n = nodes[i]!;
      if (!visible(n)) continue;
      const pinned = n.id === hover || n.id === sel || n.id === foc || n.id === graph.center;
      const nbr = hlNbrs !== undefined && hlNbrs.has(n.id);
      if (!nbr && !labelEligible(tf.k, minZoom[i]!, pinned)) continue;
      if (count === pool.length) pool.push(newCandidate());
      const c = pool[count++]!;
      c.node = i;
      c.x = n.x * tf.k + tf.x;
      c.y = n.y * tf.k + tf.y;
      c.r = n.r * tf.k;
      c.w = pinned ? boldWidths.get(labels[i]!, LABEL_FONT_BOLD, measureBold) : widths.get(labels[i]!, LABEL_FONT, measure);
      c.h = LABEL_HEIGHT;
      c.pinned = pinned;
      c.priority = labelPriority(n.degree, n.depth, nbr);
    }
    placer.place(pool, count, w, h, MAX_LABELS);

    ctx.font = LABEL_FONT;
    ctx.textAlign = "left";
    ctx.textBaseline = "top";
    ctx.lineJoin = "round";
    ctx.lineWidth = 3;
    ctx.strokeStyle = pal.bg;
    ctx.fillStyle = pal.text;
    for (let pass = 0; pass < 2; pass++) {
      if (pass === 1) ctx.font = LABEL_FONT_BOLD;
      for (let i = 0; i < count; i++) {
        const c = pool[i]!;
        if (!c.placed || c.pinned !== (pass === 1)) continue;
        const label = labels[c.node]!;
        ctx.strokeText(label, c.left, c.top + 1);
        ctx.fillText(label, c.left, c.top + 1);
      }
    }
  };

  /** Fit the whole graph in view (until the user pans or zooms again). */
  const fitNow = () => {
    const { w, h } = sizeRef.current;
    userMoved.current = false;
    tRef.current = fitTransform(fitNodes(graphRef.current.nodes), w, h);
  };

  const frame = () => {
    rafRef.current = 0;
    const sim = simRef.current;
    const graph = graphRef.current;
    let again = false;
    if (sim && sim.alpha() > sim.alphaMin()) {
      sim.tick(graph.nodes.length > 1000 ? 1 : 2);
      const dragging = gestureRef.current?.kind === "node";
      // Stop as soon as nodes stop moving, not only when alpha runs out.
      if (!dragging && sim.alpha() < 0.3 && isSettled(graph.nodes)) sim.alpha(0);
      again = sim.alpha() > sim.alphaMin() || dragging;
      // Keep the whole graph in view while it settles, until the user pans or zooms.
      if (!userMoved.current && gestureRef.current === null) {
        const { w, h } = sizeRef.current;
        const target = fitTransform(fitNodes(graph.nodes), w, h);
        const cur = tRef.current;
        const f = !again ? 1 : 0.2;
        tRef.current = { k: cur.k + (target.k - cur.k) * f, x: cur.x + (target.x - cur.x) * f, y: cur.y + (target.y - cur.y) * f };
      }
    }
    draw();
    if (again) schedule();
  };

  const schedule = () => {
    if (!rafRef.current) rafRef.current = requestAnimationFrame(frame);
  };

  // New graph: new simulation, pre-ticked off-screen so the first paint is already near its
  // final shape (with reduced motion: in its final shape), then fitted to the view.
  useEffect(() => {
    reducedMotion.current = window.matchMedia?.(REDUCED_MOTION).matches ?? false;
    userMoved.current = false;
    hoverRef.current = null;
    const sim = createSimulation(graph, seed);
    if (reducedMotion.current) settleNow(sim, graph.nodes);
    else sim.tick(preTicks(graph.nodes.length));
    simRef.current = sim;
    fitNow();
    schedule();
    return () => {
      sim.stop();
      simRef.current = null;
      cancelAnimationFrame(rafRef.current);
      rafRef.current = 0;
    };
  }, [graph, seed]);

  // Redraw when highlight props change.
  useEffect(() => {
    schedule();
  }, [selected, focused]);

  // Size the canvas to its box, at device resolution.
  useEffect(() => {
    const wrap = wrapRef.current;
    const canvas = canvasRef.current;
    if (!wrap || !canvas) return;
    const resize = () => {
      const r = wrap.getBoundingClientRect();
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.max(1, Math.floor(r.width));
      const h = Math.max(1, Math.floor(r.height));
      sizeRef.current = { w, h, dpr };
      canvas.width = Math.floor(w * dpr);
      canvas.height = Math.floor(h * dpr);
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
      if (!userMoved.current) tRef.current = fitTransform(graphRef.current.nodes, w, h);
      schedule();
    };
    resize();
    const ro = new ResizeObserver(resize);
    ro.observe(wrap);
    return () => ro.disconnect();
  }, []);

  // Reduced motion switched on while the layout still moves: finish it at once.
  useEffect(() => {
    const mq = window.matchMedia?.(REDUCED_MOTION);
    if (!mq) return;
    const onChange = () => {
      reducedMotion.current = mq.matches;
      const sim = simRef.current;
      if (!mq.matches || !sim || gestureRef.current?.kind === "node") return;
      sim.alphaTarget(0);
      settleNow(sim, graphRef.current.nodes);
      if (!userMoved.current) fitNow();
      schedule();
    };
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  // Theme and contrast changes: re-read colours.
  useEffect(() => {
    const queries = ["(prefers-color-scheme: dark)", "(prefers-contrast: more)", "(forced-colors: active)"]
      .map((q) => window.matchMedia?.(q))
      .filter((mq): mq is MediaQueryList => !!mq);
    const onChange = () => {
      paletteRef.current = null;
      schedule();
    };
    for (const mq of queries) mq.addEventListener("change", onChange);
    return () => {
      for (const mq of queries) mq.removeEventListener("change", onChange);
    };
  }, []);

  // Wheel zoom needs a non-passive listener to stop the page scrolling.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const r = canvas.getBoundingClientRect();
      const unit = e.deltaMode === 1 ? 0.05 : e.deltaMode === 2 ? 1 : 0.0015;
      tRef.current = zoomAt(tRef.current, Math.exp(-e.deltaY * unit), e.clientX - r.left, e.clientY - r.top);
      userMoved.current = true;
      schedule();
    };
    canvas.addEventListener("wheel", onWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", onWheel);
  }, []);

  const local = (e: PointerEvent<HTMLCanvasElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    if (e.button !== 0) return;
    const p = local(e);
    const hit = nodeAt(graph.nodes, tRef.current, p.x, p.y);
    e.currentTarget.setPointerCapture(e.pointerId);
    gestureRef.current = hit
      ? { kind: "node", node: hit, startX: p.x, startY: p.y, moved: false }
      : { kind: "pan", startX: p.x, startY: p.y, t0: tRef.current, moved: false };
  };

  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const p = local(e);
    const g = gestureRef.current;
    if (!g) {
      const hit = nodeAt(graph.nodes, tRef.current, p.x, p.y);
      const id = hit?.id ?? null;
      if (id !== hoverRef.current) {
        hoverRef.current = id;
        e.currentTarget.style.cursor = id ? "pointer" : "grab";
        schedule();
      }
      return;
    }
    const dx = p.x - g.startX;
    const dy = p.y - g.startY;
    if (!g.moved && Math.hypot(dx, dy) < 4) return;
    g.moved = true;
    if (g.kind === "node") {
      const wpt = toWorld(tRef.current, p.x, p.y);
      g.node.fx = wpt.x;
      g.node.fy = wpt.y;
      const sim = simRef.current;
      if (reducedMotion.current) {
        // Only the dragged node moves; the rest of the layout stays put.
        g.node.x = wpt.x;
        g.node.y = wpt.y;
        g.node.vx = 0;
        g.node.vy = 0;
      } else if (sim) {
        sim.alphaTarget(0.3);
        if (sim.alpha() < 0.3) sim.alpha(0.3);
      }
      userMoved.current = true;
    } else {
      tRef.current = { k: g.t0.k, x: g.t0.x + dx, y: g.t0.y + dy };
      userMoved.current = true;
      e.currentTarget.style.cursor = "grabbing";
    }
    schedule();
  };

  const onPointerUp = (e: PointerEvent<HTMLCanvasElement>) => {
    const g = gestureRef.current;
    gestureRef.current = null;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    e.currentTarget.style.cursor = hoverRef.current ? "pointer" : "grab";
    if (!g) return;
    if (g.kind === "node") {
      if (g.node.id !== graph.center) {
        g.node.fx = null;
        g.node.fy = null;
      }
      simRef.current?.alphaTarget(0);
      if (!g.moved) {
        live.current.onFocusChange(g.node.id);
        live.current.onOpen(g.node.id);
      }
    }
    schedule();
  };

  const onDoubleClick = (e: MouseEvent<HTMLCanvasElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const hit = nodeAt(graph.nodes, tRef.current, e.clientX - r.left, e.clientY - r.top);
    if (hit) live.current.onCenter(hit.id);
  };

  /** Pan so `n` is on screen (centred if it was off). */
  const reveal = (n: SimNode) => {
    const { w, h } = sizeRef.current;
    const tf = tRef.current;
    const sx = n.x * tf.k + tf.x;
    const sy = n.y * tf.k + tf.y;
    if (sx < 40 || sx > w - 40 || sy < 40 || sy > h - 40) {
      tRef.current = { k: tf.k, x: w / 2 - n.x * tf.k, y: h / 2 - n.y * tf.k };
      userMoved.current = true;
    }
  };

  /** Move keyboard focus to `next`, keep it on screen and announce it. */
  const focusNode = (next: SimNode) => {
    onFocusChange(next.id);
    reveal(next);
    const links = index.adj.get(next.id)?.size ?? 0;
    setAnnounce(t("graph.announceNode", { name: next.name, kind: kindLabel(next.kind), links }));
    schedule();
  };

  const focusedIndex = () => (focused ? (index.pos.get(focused) ?? -1) : -1);

  const moveFocus = (step: 1 | -1) => {
    const nodes = graph.nodes;
    if (nodes.length === 0) return;
    const i = focusedIndex();
    focusNode(nodes[(i + step + nodes.length) % nodes.length]!);
  };

  /** Arrow keys: the nearest entity that way, or (nothing focused yet) the one nearest the middle. */
  const moveDirection = (dir: Direction) => {
    const from = focused ? index.byId.get(focused) : undefined;
    if (!from) {
      const { w, h } = sizeRef.current;
      const mid = toWorld(tRef.current, w / 2, h / 2);
      const n = nearestTo(graph.nodes, mid.x, mid.y);
      if (n) focusNode(n);
      return;
    }
    const next = nearestInDirection(graph.nodes, from, dir);
    if (next) focusNode(next);
    else setAnnounce(t("graph.noneThatWay"));
  };

  const onKeyDown = (e: KeyboardEvent<HTMLCanvasElement>) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const { w, h } = sizeRef.current;
    const tf = tRef.current;
    let handled = true;
    const dir = ARROWS[e.key];
    if (dir && e.shiftKey) {
      // Shift+arrows pan.
      const dx = dir === "left" ? PAN_STEP : dir === "right" ? -PAN_STEP : 0;
      const dy = dir === "up" ? PAN_STEP : dir === "down" ? -PAN_STEP : 0;
      tRef.current = { ...tf, x: tf.x + dx, y: tf.y + dy };
      userMoved.current = true;
    } else if (dir) {
      moveDirection(dir);
    } else {
      switch (e.key) {
        case "Tab": {
          // Steps through entities once one is focused; leaves the canvas at either end or when
          // nothing is focused (Escape first), so Tab never traps.
          const next = cycleIndex(focusedIndex(), e.shiftKey ? -1 : 1, graph.nodes.length);
          if (next === null) handled = false;
          else focusNode(graph.nodes[next]!);
          break;
        }
        case "+":
        case "=":
          tRef.current = zoomAt(tf, 1.25, w / 2, h / 2);
          userMoved.current = true;
          break;
        case "-":
        case "_":
          tRef.current = zoomAt(tf, 0.8, w / 2, h / 2);
          userMoved.current = true;
          break;
        case "0":
        case "f":
        case "F":
          fitNow();
          break;
        case "n":
        case "N":
          moveFocus(e.shiftKey ? -1 : 1);
          break;
        case "p":
        case "P":
          moveFocus(-1);
          break;
        case "Enter":
        case " ":
          if (focused) onOpen(focused);
          else handled = false;
          break;
        case "c":
        case "C":
          if (focused) onCenter(focused);
          else handled = false;
          break;
        case "Escape":
          if (focused) onFocusChange(null);
          else handled = false;
          break;
        default:
          handled = false;
      }
    }
    if (handled) {
      e.preventDefault();
      schedule();
    }
  };

  const fit = () => {
    fitNow();
    schedule();
  };

  return (
    <div className="graph-canvas-wrap">
      <div ref={wrapRef} className="graph-canvas-box">
        <canvas
          ref={canvasRef}
          className="graph-canvas"
          tabIndex={0}
          role="application"
          aria-roledescription={t("graph.roleDescription")}
          aria-label={t("graph.canvasLabel", { nodes: graph.nodes.length, links: graph.links.length })}
          aria-describedby={helpId}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onPointerLeave={() => {
            if (!gestureRef.current && hoverRef.current) {
              hoverRef.current = null;
              schedule();
            }
          }}
          onDoubleClick={onDoubleClick}
          onKeyDown={onKeyDown}
          onFocus={() => {
            if (!focused && graph.nodes[0]) setAnnounce(t("graph.pressN"));
          }}
        />
        <div className="graph-overlay">
          <button type="button" className="btn btn-small" onClick={fit} title={t("graph.fitTitle")} aria-keyshortcuts="F 0">
            {t("graph.fit")}
          </button>
        </div>
        {legend.length > 0 && (
          <details className="graph-legend" open={legendOpen}>
            <summary>{t("graph.legend")}</summary>
            <ul>
              {legend.map(({ kind, count }) => (
                <li key={kind}>
                  <span className={`kind-dot kind-${kind}`} aria-hidden="true" />
                  <span>{kindPlural(kind)}</span>
                  <span className="graph-legend-count">{formatNumber(count)}</span>
                </li>
              ))}
            </ul>
          </details>
        )}
      </div>
      <div className="graph-help">
        <span className="muted small">{t("graph.hint")}</span>
        <details className="graph-keys">
          <summary className="small">{t("graph.keysTitle")}</summary>
          <p id={helpId} className="muted small">
            {t("graph.help")}
          </p>
        </details>
      </div>
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>
    </div>
  );
}
