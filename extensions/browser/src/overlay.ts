// "Nugget is acting" banner with a Stop button, in a closed shadow root so the page can neither
// read nor restyle it. Stop engages the kill switch (see background.ts).

const OVERLAY_ATTR = "data-nugget-overlay";

export interface Overlay {
  show(label?: string): void;
  hide(): void;
  readonly visible: boolean;
}

export function createOverlay(doc: Document, onStop: () => void): Overlay {
  let host: HTMLElement | undefined;
  let labelEl: HTMLElement | undefined;

  const build = () => {
    host = doc.createElement("div");
    host.setAttribute(OVERLAY_ATTR, "");
    host.style.cssText = "all:initial;position:fixed;z-index:2147483647;top:12px;right:12px;";
    const root = host.attachShadow({ mode: "closed" });
    const style = doc.createElement("style");
    style.textContent = `
      .bar { font: 13px/1.3 system-ui, sans-serif; display: flex; gap: 10px; align-items: center;
        background: #1f2328; color: #fff; padding: 8px 10px 8px 12px; border-radius: 8px;
        box-shadow: 0 2px 12px rgba(0,0,0,.35); outline: 2px solid #f5b301; }
      .dot { width: 8px; height: 8px; border-radius: 50%; background: #f5b301; }
      button { font: inherit; font-weight: 600; background: #d1242f; color: #fff; border: 0;
        border-radius: 6px; padding: 4px 10px; cursor: pointer; }
      button:focus-visible { outline: 2px solid #fff; outline-offset: 2px; }`;
    const bar = doc.createElement("div");
    bar.className = "bar";
    bar.setAttribute("role", "status");
    const dot = doc.createElement("span");
    dot.className = "dot";
    labelEl = doc.createElement("span");
    const stop = doc.createElement("button");
    stop.type = "button";
    stop.textContent = "Stop";
    stop.setAttribute("aria-label", "Stop Nugget");
    stop.addEventListener("click", (e) => {
      e.stopPropagation();
      onStop();
    });
    bar.append(dot, labelEl, stop);
    root.append(style, bar);
  };

  return {
    show(label = "Nugget is acting on this page") {
      if (!host) build();
      labelEl!.textContent = label;
      if (!host!.isConnected) (doc.body ?? doc.documentElement).appendChild(host!);
    },
    hide() {
      host?.remove();
    },
    get visible() {
      return host?.isConnected ?? false;
    },
  };
}

/** True if `el` belongs to our overlay (snapshots skip it). */
export function isOverlay(el: Element): boolean {
  return el.hasAttribute(OVERLAY_ATTR);
}
