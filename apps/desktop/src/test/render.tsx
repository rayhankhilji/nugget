/** Minimal jsdom rendering helpers (no testing-library): mount, act, query, unmount. */
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { RESPONSES } from "./fixtures";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

export interface Rendered {
  container: HTMLElement;
  unmount: () => void;
}

const mounted: Root[] = [];

export async function render(ui: ReactNode): Promise<Rendered> {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  mounted.push(root);
  await act(async () => {
    root.render(ui);
  });
  await flush();
  return {
    container,
    unmount: () => {
      act(() => root.unmount());
      container.remove();
    },
  };
}

/** Let pending promises (mocked IPC) and the renders they cause settle. */
export async function flush(times = 5): Promise<void> {
  for (let i = 0; i < times; i++) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

export async function click(el: Element | null | undefined): Promise<void> {
  if (!el) throw new Error("click: element not found");
  await act(async () => {
    (el as HTMLElement).click();
  });
  await flush(2);
}

export function byText(root: ParentNode, selector: string, text: string | RegExp): HTMLElement | undefined {
  return Array.from(root.querySelectorAll<HTMLElement>(selector)).find((e) =>
    typeof text === "string" ? e.textContent?.trim() === text : text.test(e.textContent ?? ""),
  );
}

/**
 * Pretend to be inside the Tauri shell. Commands answer from `responses` (falling back to the
 * shared fixtures); unknown commands fail like an older daemon ("method not found").
 */
export function mockDaemon(responses: Record<string, unknown | ((args: unknown) => unknown)> = {}): string[] {
  const calls: string[] = [];
  mockIPC(
    (cmd, args) => {
      calls.push(cmd);
      const all = { ...RESPONSES, ...responses };
      if (cmd in all) {
        const r = all[cmd];
        return typeof r === "function" ? (r as (a: unknown) => unknown)(args) : r;
      }
      throw { kind: "rpc", message: "method not found", code: -32601 };
    },
    { shouldMockEvents: true },
  );
  return calls;
}

export function cleanup(): void {
  while (mounted.length) {
    const r = mounted.pop()!;
    act(() => r.unmount());
  }
  document.body.innerHTML = "";
  clearMocks();
  try {
    localStorage.clear();
  } catch {
    /* ignore */
  }
}

// jsdom lacks these; components touch them.
export function installDomShims(): void {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
      this.setAttribute("open", "");
    };
    HTMLDialogElement.prototype.close = function (this: HTMLDialogElement) {
      this.removeAttribute("open");
    };
  }
  if (!window.matchMedia) {
    window.matchMedia = (q: string) =>
      ({ matches: false, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {}, onchange: null, dispatchEvent: () => false }) as MediaQueryList;
  }
  if (!("ResizeObserver" in window)) {
    (window as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  HTMLCanvasElement.prototype.getContext = (() => null) as unknown as HTMLCanvasElement["getContext"];
}
