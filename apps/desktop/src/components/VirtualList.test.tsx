// @vitest-environment jsdom
/** VirtualList (II5): windowing math, the rendered window, keyboard, ARIA set size, incremental loading. */
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { t } from "../i18n";
import { cleanup, flush, installDomShims, render } from "../test/render";
import { nextIndex, VirtualList } from "./VirtualList";

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
});
