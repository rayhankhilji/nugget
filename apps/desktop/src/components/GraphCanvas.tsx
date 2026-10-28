import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent, type PointerEvent } from "react";
import type { Simulation } from "d3-force";
import {
  createSimulation,
  fitTransform,
  LARGE_GRAPH,
  nodeAt,
  toWorld,
  zoomAt,
  type PreparedGraph,
  type SimLink,
  type SimNode,
  type ViewTransform,
} from "../graph";
import { ENTITY_KINDS } from "../api";
import { useT } from "../i18n";
import { kindLabel } from "../knowledge";

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

function shortName(s: string): string {
  const chars = Array.from(s);
  return chars.length > LABEL_CHARS ? `${chars.slice(0, LABEL_CHARS - 1).join("")}…` : s;
}

type Gesture =
  | { kind: "node"; node: SimNode; startX: number; startY: number; moved: boolean }
  | { kind: "pan"; startX: number; startY: number; t0: ViewTransform; moved: boolean };

/**
 * Force-directed graph on a `<canvas>`. Drag nodes, drag the background to pan, wheel to zoom,
 * click to open, double-click to centre. Keyboard: arrows pan, + / − zoom, 0 fits, N / P move
 * between entities, Enter opens, C centres, Escape clears. The list view is the accessible
 * alternative for everything here.
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
  const settled = useRef(false);
  const [announce, setAnnounce] = useState("");
  const helpId = useId();

  // Latest props for the draw loop, which lives outside React's render cycle.
  const live = useRef({ selected, focused, onOpen, onCenter, onFocusChange });
  live.current = { selected, focused, onOpen, onCenter, onFocusChange };

  const index = useMemo(() => {
    const byId = new Map(graph.nodes.map((n) => [n.id, n]));
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
    for (const n of graph.nodes) {
      const list = byKind.get(n.kind);
      if (list) list.push(n);
      else byKind.set(n.kind, [n]);
    }
    return { byId, adj, byKind };
  }, [graph]);
  const indexRef = useRef(index);
  indexRef.current = index;
  // Listeners registered once (wheel, resize, theme) reach the draw loop through refs only.
  const graphRef = useRef(graph);
  graphRef.current = graph;

  const draw = () => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    if (!paletteRef.current) paletteRef.current = readPalette(canvas);
    const pal = paletteRef.current;
    const { w, h, dpr } = sizeRef.current;
    const tf = tRef.current;
    const { byId, adj, byKind } = indexRef.current;
    const graph = graphRef.current;
    const { selected: sel, focused: foc } = live.current;
    const hl = hoverRef.current ?? foc ?? sel;
    const hlNbrs = hl ? adj.get(hl) : undefined;

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = pal.bg;
    ctx.fillRect(0, 0, w, h);

    if (reducedMotion.current && !settled.current) {
      ctx.fillStyle = pal.muted;
      ctx.font = "13px system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.fillText(t("graph.layingOut"), w / 2, h / 2);
      return;
    }

    ctx.setTransform(dpr * tf.k, 0, 0, dpr * tf.k, dpr * tf.x, dpr * tf.y);
    // Visible world rectangle, for culling.
    const x0 = -tf.x / tf.k;
    const y0 = -tf.y / tf.k;
    const x1 = (w - tf.x) / tf.k;
    const y1 = (h - tf.y) / tf.k;
    const visible = (n: SimNode) => n.x + n.r >= x0 && n.x - n.r <= x1 && n.y + n.r >= y0 && n.y - n.r <= y1;

    // Edges: one path for all, then the highlighted node's edges on top.
    ctx.lineWidth = 1 / tf.k;
    ctx.strokeStyle = pal.edge;
    ctx.beginPath();
    for (const l of graph.links) {
      const s = l.source as SimNode;
      const d = l.target as SimNode;
      if ((s.x < x0 && d.x < x0) || (s.x > x1 && d.x > x1) || (s.y < y0 && d.y < y0) || (s.y > y1 && d.y > y1)) continue;
      ctx.moveTo(s.x, s.y);
      ctx.lineTo(d.x, d.y);
    }
    ctx.stroke();
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

    // Nodes: one fill per kind.
    for (const [kind, nodes] of byKind) {
      ctx.fillStyle = pal.kinds[kind] ?? pal.other;
      ctx.beginPath();
      for (const n of nodes) {
        if (!visible(n)) continue;
        ctx.moveTo(n.x + n.r, n.y);
        ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
      }
      ctx.fill();
    }

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
    ring(hoverRef.current, pal.muted, 1.5, 2);
    ring(foc, pal.focus, 2.5, 6);

    // Labels: everything when few or zoomed in, else just the nodes that matter right now.
    const large = graph.nodes.length > LARGE_GRAPH;
    const showAll = graph.nodes.length <= 150 || tf.k >= (large ? 2.2 : 1.2);
    const labelled: SimNode[] = [];
    const seen = new Set<string>();
    const add = (n: SimNode | undefined) => {
      if (n && !seen.has(n.id) && labelled.length < MAX_LABELS && visible(n)) {
        seen.add(n.id);
        labelled.push(n);
      }
    };
    for (const id of [graph.center, sel, foc, hoverRef.current]) if (id) add(byId.get(id));
    if (hlNbrs) for (const id of hlNbrs) add(byId.get(id));
    if (showAll) for (const n of graph.nodes) add(n);
    else if (graph.nodes.length <= 600) for (const n of graph.nodes) if (n.depth <= 1) add(n);

    ctx.font = `${12 / tf.k}px system-ui, -apple-system, "Segoe UI", sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    ctx.lineJoin = "round";
    ctx.lineWidth = 3 / tf.k;
    ctx.strokeStyle = pal.bg;
    ctx.fillStyle = pal.text;
    for (const n of labelled) {
      const label = shortName(n.name);
      const y = n.y + n.r + 3 / tf.k;
      ctx.strokeText(label, n.x, y);
      ctx.fillText(label, n.x, y);
    }
  };

  const frame = () => {
    rafRef.current = 0;
    const sim = simRef.current;
    const graph = graphRef.current;
    let again = false;
    if (sim && sim.alpha() > sim.alphaMin()) {
      if (reducedMotion.current && !settled.current) {
        // No animation: lay out in time-boxed chunks, draw once at the end.
        const start = performance.now();
        while (sim.alpha() > sim.alphaMin() && performance.now() - start < 12) sim.tick();
      } else {
        sim.tick(graph.nodes.length > 1000 ? 1 : 2);
      }
      again = sim.alpha() > sim.alphaMin() || gestureRef.current?.kind === "node";
      if (!again) settled.current = true;
      // Keep the whole graph in view while it settles, until the user pans or zooms.
      if (!userMoved.current && gestureRef.current === null) {
        const { w, h } = sizeRef.current;
        const target = fitTransform(graph.nodes, w, h);
        const cur = tRef.current;
        const f = reducedMotion.current || !again ? 1 : 0.2;
        tRef.current = { k: cur.k + (target.k - cur.k) * f, x: cur.x + (target.x - cur.x) * f, y: cur.y + (target.y - cur.y) * f };
      }
    }
    draw();
    if (again) schedule();
  };

  const schedule = () => {
    if (!rafRef.current) rafRef.current = requestAnimationFrame(frame);
  };

  // New graph: new simulation.
  useEffect(() => {
    reducedMotion.current = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    settled.current = false;
    userMoved.current = false;
    hoverRef.current = null;
    const sim = createSimulation(graph, seed);
    simRef.current = sim;
    const { w, h } = sizeRef.current;
    tRef.current = { k: 1, x: w / 2, y: h / 2 };
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

  // Theme changes: re-read colours.
  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-color-scheme: dark)");
    if (!mq) return;
    const onChange = () => {
      paletteRef.current = null;
      schedule();
    };
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
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
      if (sim) {
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

  const moveFocus = (step: 1 | -1) => {
    const nodes = graph.nodes;
    if (nodes.length === 0) return;
    const i = focused ? nodes.findIndex((n) => n.id === focused) : -1;
    const next = nodes[(i + step + nodes.length) % nodes.length]!;
    onFocusChange(next.id);
    reveal(next);
    const links = index.adj.get(next.id)?.size ?? 0;
    setAnnounce(t("graph.announceNode", { name: next.name, kind: kindLabel(next.kind), links }));
    schedule();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLCanvasElement>) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const { w, h } = sizeRef.current;
    const tf = tRef.current;
    let handled = true;
    switch (e.key) {
      case "ArrowLeft":
        tRef.current = { ...tf, x: tf.x + PAN_STEP };
        break;
      case "ArrowRight":
        tRef.current = { ...tf, x: tf.x - PAN_STEP };
        break;
      case "ArrowUp":
        tRef.current = { ...tf, y: tf.y + PAN_STEP };
        break;
      case "ArrowDown":
        tRef.current = { ...tf, y: tf.y - PAN_STEP };
        break;
      case "+":
      case "=":
        tRef.current = zoomAt(tf, 1.25, w / 2, h / 2);
        break;
      case "-":
      case "_":
        tRef.current = zoomAt(tf, 0.8, w / 2, h / 2);
        break;
      case "0":
        userMoved.current = false;
        tRef.current = fitTransform(graph.nodes, w, h);
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
    if (handled) {
      e.preventDefault();
      if (e.key.startsWith("Arrow") || "+=-_".includes(e.key)) userMoved.current = true;
      schedule();
    }
  };

  const fit = () => {
    const { w, h } = sizeRef.current;
    userMoved.current = false;
    tRef.current = fitTransform(graph.nodes, w, h);
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
          <button type="button" className="btn btn-small" onClick={fit}>
            {t("graph.fit")}
          </button>
        </div>
      </div>
      <p id={helpId} className="muted small graph-help">
        {t("graph.help")}
      </p>
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>
    </div>
  );
}
