// @vitest-environment jsdom
/**
 * The graph canvas honours `prefers-reduced-motion`: the layout is settled before the first
 * paint and nothing keeps drawing; switching the preference on mid-layout finishes it at once.
 * Frames are queued by a stand-in `requestAnimationFrame` and run by hand.
 */
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { GraphEdgeView, GraphNodeView } from "../api";
import { isSettled, prepareGraph, type PreparedGraph } from "../graph";
import { cleanup, installDomShims, render } from "../test/render";
import { GraphCanvas } from "./GraphCanvas";

let frames: FrameRequestCallback[] = [];
const originalRaf = window.requestAnimationFrame;
const originalCancel = window.cancelAnimationFrame;
const originalMatchMedia = window.matchMedia;

/** A media query list whose `matches` the test flips, notifying its listeners. */
function fakeReducedMotion(initial: boolean) {
  const listeners = new Set<(e: MediaQueryListEvent) => void>();
  const list = {
    matches: initial,
    media: "(prefers-reduced-motion: reduce)",
    addEventListener: (_: string, l: (e: MediaQueryListEvent) => void) => listeners.add(l),
    removeEventListener: (_: string, l: (e: MediaQueryListEvent) => void) => listeners.delete(l),
  };
  window.matchMedia = ((q: string) =>
    q.includes("reduced-motion")
      ? list
      : { matches: false, media: q, addEventListener() {}, removeEventListener() {} }) as unknown as typeof window.matchMedia;
  return {
    set(matches: boolean) {
      list.matches = matches;
      for (const l of listeners) l({ matches } as MediaQueryListEvent);
    },
  };
}

/** Run the frames queued so far (not the ones they queue); returns how many ran. */
function runFrames(): number {
  const now = frames;
  frames = [];
  act(() => {
    for (const f of now) f(performance.now());
  });
  return now.length;
}

function graph(): PreparedGraph {
  const nodes: GraphNodeView[] = Array.from({ length: 300 }, (_, i) => ({ id: `n${i}`, kind: "person", name: `Node ${i}`, depth: i === 0 ? 0 : 1 }));
  const edges: GraphEdgeView[] = [];
  for (let i = 1; i < 300; i++) edges.push({ claim: i, from: `n${i}`, to: `n${(i * 7) % 13}`, predicate: "knows" });
  return prepareGraph({ nodes, edges, truncated: false }, { center: "n0" });
}

function mount(g: PreparedGraph) {
  return render(<GraphCanvas graph={g} seed={1} selected={null} focused={null} onFocusChange={() => {}} onOpen={() => {}} onCenter={() => {}} />);
}

beforeEach(() => {
  installDomShims();
  frames = [];
  window.requestAnimationFrame = (cb) => {
    frames.push(cb);
    return frames.length;
  };
  window.cancelAnimationFrame = () => {};
});
afterEach(() => {
  cleanup();
  window.requestAnimationFrame = originalRaf;
  window.cancelAnimationFrame = originalCancel;
  window.matchMedia = originalMatchMedia;
});

describe("GraphCanvas and prefers-reduced-motion", () => {
  it("settles the layout before the first paint and stops drawing after it", async () => {
    fakeReducedMotion(true);
    const g = graph();
    await mount(g);
    expect(isSettled(g.nodes)).toBe(true);
    const before = g.nodes.map((n) => [n.x, n.y]);
    // One frame for the first paint (and the resize), then nothing more is scheduled.
    for (let i = 0; i < 3 && frames.length; i++) runFrames();
    expect(frames).toHaveLength(0);
    expect(g.nodes.map((n) => [n.x, n.y])).toEqual(before);
  });

  it("animates the layout without the preference", async () => {
    fakeReducedMotion(false);
    const g = graph();
    await mount(g);
    const before = g.nodes.map((n) => [n.x, n.y]);
    runFrames();
    // Still moving: the frame scheduled the next one and nodes moved.
    expect(frames.length).toBeGreaterThan(0);
    expect(g.nodes.map((n) => [n.x, n.y])).not.toEqual(before);
  });

  it("finishes a running layout at once when the preference is switched on", async () => {
    const media = fakeReducedMotion(false);
    const g = graph();
    await mount(g);
    runFrames();
    expect(frames.length).toBeGreaterThan(0);
    act(() => media.set(true));
    expect(isSettled(g.nodes)).toBe(true);
    for (let i = 0; i < 3 && frames.length; i++) runFrames();
    expect(frames).toHaveLength(0);
  });
});
