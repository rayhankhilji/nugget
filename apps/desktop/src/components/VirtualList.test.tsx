// @vitest-environment jsdom
/** VirtualList (II5): windowing math, the rendered window, keyboard, ARIA set size, incremental loading. */
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { t } from "../i18n";
import { cleanup, flush, installDomShims, render } from "../test/render";
import { buildOffsets, computeRange, fallbackRange, nextIndex, rowAt, VirtualList } from "./VirtualList";

beforeEach(installDomShims);
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const many = (n: number) => Array.from({ length: n }, (_, i) => ({ id: `r${i}`, label: `Row ${i}` }));
type Row = ReturnType<typeof many>[number];
const key = (r: Row) => r.id;
const rowsOf = (root: ParentNode) => Array.from(root.querySelectorAll<HTMLElement>("[data-index]"));

async function press(el: Element, key: string) {
  await act(async () => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  });
  await flush(2);
}

describe("windowing math", () => {
  it("builds offsets with a gap and finds the row at a position", () => {
    const off = buildOffsets([10, 20, 30], 2);
    expect(Array.from(off)).toEqual([0, 12, 34, 66]);
    expect(rowAt(off, -5)).toBe(0);
    expect(rowAt(off, 0)).toBe(0);
    expect(rowAt(off, 11.9)).toBe(0);
    expect(rowAt(off, 12)).toBe(1);
    expect(rowAt(off, 1000)).toBe(2);
    expect(rowAt(buildOffsets([]), 10)).toBe(0);
  });

  it("windows the visible span plus overscan, clamped to the list", () => {
    const off = buildOffsets(new Array(100_000).fill(50));
    // 500px tall view 10,000px down: rows 200..209, three more each side.
    expect(computeRange(off, 10_000, 10_500, 3)).toEqual({ start: 197, end: 213 });
    expect(computeRange(off, -400, 100, 3)).toEqual({ start: 0, end: 5 });
    expect(computeRange(off, 5_000_000, 5_000_500, 3)).toEqual({ start: 99_996, end: 100_000 });
    expect(computeRange(buildOffsets([]), 0, 500, 3)).toEqual({ start: 0, end: 0 });
  });

  it("falls back to a window around the focused row without layout", () => {
    expect(fallbackRange(1000, 30, 0)).toEqual({ start: 0, end: 30 });
    expect(fallbackRange(1000, 30, 29)).toEqual({ start: 0, end: 30 });
    expect(fallbackRange(1000, 30, 500)).toEqual({ start: 485, end: 515 });
    expect(fallbackRange(1000, 30, 999)).toEqual({ start: 970, end: 1000 });
    expect(fallbackRange(10, 30, 5)).toEqual({ start: 0, end: 10 });
    expect(fallbackRange(0, 30, 0)).toEqual({ start: 0, end: 0 });
  });

  it("maps navigation keys to rows", () => {
    expect(nextIndex("ArrowDown", 5, 10, 3)).toBe(6);
    expect(nextIndex("ArrowDown", 9, 10, 3)).toBe(9);
    expect(nextIndex("ArrowUp", 0, 10, 3)).toBe(0);
    expect(nextIndex("Home", 7, 10, 3)).toBe(0);
    expect(nextIndex("End", 0, 10, 3)).toBe(9);
    expect(nextIndex("PageDown", 2, 10, 3)).toBe(5);
    expect(nextIndex("PageDown", 8, 10, 3)).toBe(9);
    expect(nextIndex("PageUp", 2, 10, 3)).toBe(0);
    expect(nextIndex("Enter", 2, 10, 3)).toBeNull();
    expect(nextIndex("ArrowDown", 0, 0, 3)).toBeNull();
  });
});

describe("VirtualList", () => {
  it("renders only a window of a 50,000-row list, with the whole size for screen readers", async () => {
    const { container } = await render(
      <VirtualList aria-label="Rows" items={many(50_000)} itemKey={key} renderItem={(r) => r.label} estimateSize={40} initialCount={30} />,
    );
    const list = container.querySelector("ul")!;
    const rows = rowsOf(list);
    expect(rows).toHaveLength(30);
    expect(rows[0]!.tagName).toBe("LI");
    expect(rows[0]!.getAttribute("aria-setsize")).toBe("50000");
    expect(rows[0]!.getAttribute("aria-posinset")).toBe("1");
    expect(rows[29]!.getAttribute("aria-posinset")).toBe("30");
    // The rows not rendered still take their room, so the scrollbar is right.
    expect(list.style.paddingBlockStart).toBe("0px");
    expect(list.style.paddingBlockEnd).toBe(`${(50_000 - 30) * 40}px`);
    // One Tab stop for the whole list.
    expect(rows.filter((r) => r.tabIndex === 0)).toHaveLength(1);
    expect(rows[0]!.tabIndex).toBe(0);
  });

  it("moves focus with the arrow keys, Home, End and the page keys", async () => {
    const { container } = await render(<VirtualList items={many(1000)} itemKey={key} renderItem={(r) => r.label} initialCount={20} />);
    const list = container.querySelector("ul")!;
    const focused = () => (document.activeElement as HTMLElement).getAttribute("aria-posinset");
    act(() => rowsOf(list)[0]!.focus());

    await press(document.activeElement!, "ArrowDown");
    expect(focused()).toBe("2");
    await press(document.activeElement!, "ArrowUp");
    await press(document.activeElement!, "ArrowUp");
    expect(focused()).toBe("1");
    // No layout: a page is half the fallback window.
    await press(document.activeElement!, "PageDown");
    expect(focused()).toBe("11");
    await press(document.activeElement!, "End");
    expect(focused()).toBe("1000");
    expect(rowsOf(list)).toHaveLength(20);
    expect(rowsOf(list).at(-1)!.textContent).toBe("Row 999");
    expect((document.activeElement as HTMLElement).tabIndex).toBe(0);
    await press(document.activeElement!, "PageUp");
    expect(focused()).toBe("990");
    await press(document.activeElement!, "Home");
    expect(focused()).toBe("1");
    expect(list.style.paddingBlockStart).toBe("0px");
  });

  it("leaves keys typed in a field inside a row alone", async () => {
    const { container } = await render(<VirtualList items={many(5)} itemKey={key} renderItem={(r) => <input aria-label={r.label} />} />);
    const input = container.querySelector("input")!;
    act(() => input.focus());
    await press(input, "ArrowDown");
    expect(document.activeElement).toBe(input);
  });

  it("focuses the marked element inside a row with focus=inner", async () => {
    const { container } = await render(
      <VirtualList
        items={many(100)}
        itemKey={key}
        focus="inner"
        renderItem={(r, { tabIndex }) => (
          <button type="button" data-vl-focus="" tabIndex={tabIndex}>
            {r.label}
          </button>
        )}
      />,
    );
    const buttons = () => Array.from(container.querySelectorAll("button"));
    expect(buttons().filter((b) => b.tabIndex === 0)).toHaveLength(1);
    expect(rowsOf(container)[0]!.hasAttribute("tabindex")).toBe(false);
    act(() => buttons()[0]!.focus());
    await press(document.activeElement!, "ArrowDown");
    expect(document.activeElement!.textContent).toBe("Row 1");
    expect((document.activeElement as HTMLElement).tabIndex).toBe(0);
    await press(document.activeElement!, "End");
    expect(document.activeElement!.textContent).toBe("Row 99");
  });

  it("asks for more near the end and announces loading politely", async () => {
    const onEndReached = vi.fn();
    const items = many(200);
    const ui = (loadingMore: boolean, list = items) => (
      <VirtualList items={list} itemKey={key} renderItem={(r) => r.label} hasMore loadingMore={loadingMore} onEndReached={onEndReached} initialCount={30} />
    );
    const { container } = await render(ui(false));
    // Not near the end yet; an unknown total is -1.
    expect(onEndReached).not.toHaveBeenCalled();
    expect(rowsOf(container)[0]!.getAttribute("aria-setsize")).toBe("-1");
    act(() => rowsOf(container)[0]!.focus());
    await press(document.activeElement!, "End");
    expect(onEndReached).toHaveBeenCalledTimes(1);
    // The caller de-duplicates (usePagedList.loadMore); while loading, nothing is asked.

    const { container: loading } = await render(ui(true));
    const status = loading.querySelector('[role="status"]')!;
    expect(status.getAttribute("aria-live")).toBe("polite");
    expect(status.textContent).toBe(t("list.loadingMore"));
  });

  it("asks again after a reset to the same length and first row (retry, refresh, filter round trip)", async () => {
    const first = vi.fn();
    const second = vi.fn();
    const items = many(40);
    const ui = (onEndReached: () => void, list = items) => (
      <VirtualList items={list} itemKey={key} renderItem={(r) => r.label} hasMore onEndReached={onEndReached} initialCount={40} />
    );
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    await act(async () => root.render(ui(first)));
    expect(first).toHaveBeenCalledTimes(1);
    // The caller's state changed (a new callback), the rows are the same: ask again.
    await act(async () => root.render(ui(second, [...items])));
    expect(second).toHaveBeenCalledTimes(1);
    act(() => root.unmount());
    host.remove();
  });

  it("keeps rows out of the Tab order and leaves keys alone with focus=none", async () => {
    const { container } = await render(
      <VirtualList
        items={many(50)}
        itemKey={key}
        focus="none"
        renderItem={(r) => (
          <button type="button">{r.label}</button>
        )}
      />,
    );
    expect(container.querySelectorAll("li[tabindex]")).toHaveLength(0);
    const b = container.querySelector("button")!;
    act(() => b.focus());
    await press(b, "ArrowDown");
    expect(document.activeElement).toBe(b);
  });

  it("does not ask for more when there is no more", async () => {
    const onEndReached = vi.fn();
    await render(<VirtualList items={many(5)} itemKey={key} renderItem={(r) => r.label} onEndReached={onEndReached} />);
    expect(onEndReached).not.toHaveBeenCalled();
  });

  it("renders table rows with row indexes and spacer rows", async () => {
    const { container } = await render(
      <table aria-rowcount={10_001}>
        <thead>
          <tr aria-rowindex={1}>
            <th>a</th>
            <th>b</th>
          </tr>
        </thead>
        <VirtualList
          as="tbody"
          items={many(10_000)}
          itemKey={key}
          columns={2}
          rowIndexBase={2}
          estimateSize={30}
          renderItem={(r) => (
            <>
              <td>{r.label}</td>
              <td>x</td>
            </>
          )}
        />
      </table>,
    );
    const body = container.querySelector("tbody")!;
    const rows = rowsOf(body);
    expect(rows).toHaveLength(30);
    expect(rows[0]!.tagName).toBe("TR");
    expect(rows[0]!.getAttribute("aria-rowindex")).toBe("2");
    const spacer = body.querySelector<HTMLElement>("tr.vl-spacer")!;
    expect(spacer.getAttribute("aria-hidden")).toBe("true");
    expect(spacer.querySelector("td")!.colSpan).toBe(2);
    expect(spacer.querySelector("td")!.style.blockSize).toBe(`${(10_000 - 30) * 30}px`);
  });

  it("follows the scroll position when there is layout", async () => {
    const scroller = document.createElement("div");
    scroller.style.overflowY = "auto";
    document.body.appendChild(scroller);
    let scrollTop = 0;
    Object.defineProperty(scroller, "clientHeight", { configurable: true, get: () => 500 });
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, get: () => 5_000_000 });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (v: number) => {
        scrollTop = v;
      },
    });
    const rect = (top: number, height: number) => ({ top, bottom: top + height, height, left: 0, right: 300, width: 300, x: 0, y: top, toJSON() {} }) as DOMRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this === scroller) return rect(0, 500);
      if (this.tagName === "UL") return rect(-scrollTop, 100_000 * 50);
      return rect(0, 0);
    });
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => window.setTimeout(() => cb(0), 0));

    // Mounted inside the scroller, as a list is inside its page's scroller in the app.
    const host = document.createElement("div");
    scroller.appendChild(host);
    const root = createRoot(host);
    await act(async () => {
      root.render(<VirtualList items={many(100_000)} itemKey={key} renderItem={(r) => r.label} estimateSize={50} fixedSize overscan={3} />);
    });
    await flush(2);
    const list = host.querySelector("ul")!;
    expect(rowsOf(list).map((r) => r.dataset.index)).toEqual(Array.from({ length: 13 }, (_, i) => String(i)));

    scrollTop = 10_000;
    await act(async () => {
      scroller.dispatchEvent(new Event("scroll"));
    });
    await flush(3);
    const idx = rowsOf(list).map((r) => Number(r.dataset.index));
    expect(idx[0]).toBe(197);
    expect(idx.at(-1)).toBe(212);
    expect(list.style.paddingBlockStart).toBe(`${197 * 50}px`);
    expect(list.style.paddingBlockEnd).toBe(`${(100_000 - 213) * 50}px`);

    // A focused row scrolled out of the window stays mounted (out of the layout), so focus
    // doesn't fall to <body>; the window's padding is unchanged.
    scrollTop = 0;
    await act(async () => {
      scroller.dispatchEvent(new Event("scroll"));
    });
    await flush(3);
    const row0 = list.querySelector<HTMLElement>('[data-index="0"]')!;
    act(() => row0.focus());
    scrollTop = 10_000;
    await act(async () => {
      scroller.dispatchEvent(new Event("scroll"));
    });
    await flush(3);
    expect(document.activeElement).toBe(row0);
    expect(row0.isConnected).toBe(true);
    expect(row0.hasAttribute("data-vl-detached")).toBe(true);
    expect(rowsOf(list)[0]).toBe(row0);
    expect(rowsOf(list)[1]!.dataset.index).toBe("197");
    expect(list.style.paddingBlockStart).toBe(`${197 * 50}px`);
    // Still one Tab stop: the focused row.
    expect(rowsOf(list).filter((r) => r.tabIndex === 0)).toEqual([row0]);
    // Arrow keys move on from it, back into the window's place.
    await press(row0, "ArrowDown");
    expect((document.activeElement as HTMLElement).dataset.index).toBe("1");
    expect(list.querySelector("[data-vl-detached]")).toBeNull();
    act(() => root.unmount());
    vi.unstubAllGlobals();
    scroller.remove();
  });
});
