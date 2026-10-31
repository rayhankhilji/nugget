/**
 * Windowed rendering for long lists (II5): only the rows near the visible part of the page are in
 * the DOM, so a list of 50,000 rows costs about as much as one of 50.
 *
 * - The list scrolls with its nearest scrolling ancestor (`<main>` in the app, else the window):
 *   no nested scroll box, so the page looks the same as an unvirtualised list.
 * - Row heights are estimated, then measured as rows render (`fixedSize` skips measuring).
 *   Rows above and below the window are stood in for by padding (`ul`/`ol`) or a spacer row
 *   (`tbody`), so the scrollbar reflects the whole list.
 * - Keyboard: ArrowUp/ArrowDown, Home/End and PageUp/PageDown move focus between rows (a roving
 *   tabindex: one Tab stop for the whole list) and scroll the focused row into view.
 * - ARIA: list rows carry `aria-setsize`/`aria-posinset` (table rows `aria-rowindex`), so a screen
 *   reader knows the whole size while only a window is rendered.
 * - `onEndReached` fires when the window nears the end and `hasMore` is set (incremental loading);
 *   "Loading more…" is announced politely while `loadingMore`.
 * - Without layout (jsdom, a hidden tab) it renders a window of `initialCount` rows around the
 *   focused row.
 *
 * Logical CSS properties only (`padding-block-*`, `block-size`), so it is right-to-left safe.
 */
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FocusEvent,
  type HTMLAttributes,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useT } from "../i18n";

export interface RowContext {
  /** The row's position in `items`. */
  index: number;
  /** This row holds the list's single Tab stop. */
  active: boolean;
  /**
   * For `focus="inner"`: put this on the row's focus target and mark it `data-vl-focus`.
   * (`focus="row"` puts it on the row element itself.)
   */
  tabIndex: 0 | -1;
}

export interface VirtualListProps<T> extends Omit<HTMLAttributes<HTMLElement>, "children" | "onKeyDown" | "onFocus"> {
  items: readonly T[];
  /** A stable key per item (measured heights are remembered by key). */
  itemKey: (item: T, index: number) => string;
  /** The row's contents (the component renders the `<li>`/`<tr>` itself). */
  renderItem: (item: T, ctx: RowContext) => ReactNode;
  /** The list element: `ul` (default), `ol`, or `tbody` for table rows (`<tr>`). */
  as?: "ul" | "ol" | "tbody";
  /** Estimated row height in px (and the exact one with `fixedSize`). */
  estimateSize?: number;
  /** Every row is exactly `estimateSize` tall: skip measuring. */
  fixedSize?: boolean;
  /** Rows rendered beyond each edge of the visible part. */
  overscan?: number;
  /** Rows rendered when nothing can be measured (jsdom, a hidden container). */
  initialCount?: number;
  /** The size of the whole set when more rows exist than `items` (for `aria-setsize`). */
  total?: number;
  /** More rows can be loaded with `onEndReached`. */
  hasMore?: boolean;
  /** A page of rows is loading (announces "Loading more…"). */
  loadingMore?: boolean;
  /** Called once per length of `items` when the window comes within `endThreshold` rows of the end. */
  onEndReached?: () => void;
  endThreshold?: number;
  /** `row`: the `<li>`/`<tr>` takes focus. `inner`: an element inside it marked `data-vl-focus`. */
  focus?: "row" | "inner";
  rowClassName?: string;
  /** Table rows: `aria-rowindex` of the first row (1 + header rows). */
  rowIndexBase?: number;
  /** Table rows: columns the spacer row spans. */
  columns?: number;
}

export interface Range {
  start: number;
  end: number;
}

/** Cumulative offsets: `offsets[i]` is where row `i` starts; `offsets[n]` is the total height (plus one trailing gap). */
export function buildOffsets(sizes: ArrayLike<number>, gap = 0): Float64Array {
  const n = sizes.length;
  const offsets = new Float64Array(n + 1);
  for (let i = 0; i < n; i++) offsets[i + 1] = offsets[i]! + sizes[i]! + gap;
  return offsets;
}

/** The last row starting at or before `y` (0 when `y` is above the list, n-1 below it). */
export function rowAt(offsets: Float64Array, y: number): number {
  const n = offsets.length - 1;
  if (n <= 0) return 0;
  let lo = 0;
  let hi = n - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (offsets[mid]! <= y) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/** Rows to render for the visible span `[top, bottom)` (list coordinates) with `overscan` rows each side. */
export function computeRange(offsets: Float64Array, top: number, bottom: number, overscan: number): Range {
  const n = offsets.length - 1;
  if (n <= 0) return { start: 0, end: 0 };
  const first = rowAt(offsets, Math.max(0, top));
  const last = rowAt(offsets, Math.max(top, bottom - 0.5));
  return { start: Math.max(0, first - overscan), end: Math.min(n, last + 1 + overscan) };
}

/** The window shown without layout: `count` rows, including `anchor` when it is set. */
export function fallbackRange(n: number, count: number, anchor: number): Range {
  if (n <= 0) return { start: 0, end: 0 };
  const size = Math.min(n, Math.max(1, count));
  const start = anchor < size ? 0 : Math.min(n - size, anchor - Math.floor(size / 2));
  return { start, end: start + size };
}

/** Where focus goes for `key` from row `index` (null: not a navigation key). */
export function nextIndex(key: string, index: number, n: number, pageRows: number): number | null {
  if (n <= 0) return null;
  switch (key) {
    case "ArrowDown":
      return Math.min(n - 1, index + 1);
    case "ArrowUp":
      return Math.max(0, index - 1);
    case "Home":
      return 0;
    case "End":
      return n - 1;
    case "PageDown":
      return Math.min(n - 1, index + Math.max(1, pageRows));
    case "PageUp":
      return Math.max(0, index - Math.max(1, pageRows));
    default:
      return null;
  }
}

type Scroller = HTMLElement | Window;

const raf = (cb: () => void): number =>
  typeof requestAnimationFrame === "function" ? requestAnimationFrame(cb) : window.setTimeout(cb, 16);
const caf = (id: number) => (typeof cancelAnimationFrame === "function" ? cancelAnimationFrame(id) : window.clearTimeout(id));

/**
 * The element that scrolls the list: the nearest ancestor that scrolls vertically now, else the
 * nearest `<main>` (the app's page scroller), else the window. A box that only scrolls sideways
 * (`overflow-x: auto` computes `overflow-y: auto` too, but it never gets taller than its content)
 * is passed over.
 */
function scrollParent(el: HTMLElement): Scroller {
  for (let p = el.parentElement; p; p = p.parentElement) {
    const y = getComputedStyle(p).overflowY;
    if ((y === "auto" || y === "scroll" || y === "overlay") && p.scrollHeight > p.clientHeight + 1) return p;
  }
  return el.closest("main") ?? window;
}

/** The scroller's visible span in viewport coordinates. */
function viewport(s: Scroller): { top: number; height: number } {
  if (s === window) return { top: 0, height: window.innerHeight };
  const el = s as HTMLElement;
  return { top: el.getBoundingClientRect().top, height: el.clientHeight };
}

function isEditable(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  return el.isContentEditable || el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT";
}

export function VirtualList<T>({
  items,
  itemKey,
  renderItem,
  as = "ul",
  estimateSize = 48,
  fixedSize = false,
  overscan = 8,
  initialCount = 30,
  total,
  hasMore = false,
  loadingMore = false,
  onEndReached,
  endThreshold = 10,
  focus = "row",
  rowClassName,
  rowIndexBase = 1,
  columns = 1,
  style,
  ...rest
}: VirtualListProps<T>) {
  const t = useT();
  const n = items.length;
  const table = as === "tbody";
  const outer = useRef<HTMLElement | null>(null);
  const scroller = useRef<Scroller | null>(null);
  const sizes = useRef(new Map<string, number>());
  const [version, setVersion] = useState(0);
  const [gap, setGap] = useState(0);
  const [layout, setLayout] = useState(false);
  const [active, setActive] = useState(0);
  const [range, setRange] = useState<Range>(() => fallbackRange(n, initialCount, 0));
  const pendingFocus = useRef<number | null>(null);
  const activeIndex = n === 0 ? -1 : Math.min(active, n - 1);

  const keys = useMemo(() => items.map((it, i) => itemKey(it, i)), [items, itemKey]);
  const offsets = useMemo(() => {
    const s = new Float64Array(n);
    for (let i = 0; i < n; i++) s[i] = fixedSize ? estimateSize : (sizes.current.get(keys[i]!) ?? estimateSize);
    return buildOffsets(s, gap);
    // `version` changes when a measured height does.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [keys, n, estimateSize, fixedSize, gap, version]);
  const offsetsRef = useRef(offsets);
  offsetsRef.current = offsets;

  const activeRef = useRef(activeIndex);
  activeRef.current = Math.max(0, activeIndex);

  /** Recompute the window from the scroll position. */
  const update = useCallback(() => {
    const el = outer.current;
    if (!el) return;
    const s = scroller.current ?? (scroller.current = scrollParent(el));
    const rect = el.getBoundingClientRect();
    const view = viewport(s);
    const off = offsetsRef.current;
    const count = off.length - 1;
    const measurable = view.height > 0 && (rect.width > 0 || rect.height > 0);
    setLayout(measurable);
    let next: Range;
    if (!measurable) {
      next = fallbackRange(count, initialCount, pendingFocus.current ?? activeRef.current);
    } else {
      const top = view.top - rect.top;
      next = computeRange(off, top, top + view.height, overscan);
    }
    setRange((r) => (r.start === next.start && r.end === next.end ? r : next));
  }, [initialCount, overscan]);
  // Follow the scroller and the viewport size.
  useEffect(() => {
    const el = outer.current;
    if (!el) return;
    const s = (scroller.current = scrollParent(el));
    let frame = 0;
    const onScroll = () => {
      if (frame) return;
      frame = raf(() => {
        frame = 0;
        update();
      });
    };
    s.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);
    const ro = typeof ResizeObserver === "function" ? new ResizeObserver(onScroll) : null;
    if (ro && s !== window) ro.observe(s as HTMLElement);
    return () => {
      s.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
      ro?.disconnect();
      if (frame) caf(frame);
    };
    // The scroller can change once the list grows taller than its box.
  }, [update, n]);

  // New rows or heights: recompute the window before paint.
  useLayoutEffect(update, [update, offsets]);

  // Measure what rendered; a changed height re-lays out the window.
  useLayoutEffect(() => {
    const el = outer.current;
    if (!el || !layout) return;
    if (!table) {
      const g = parseFloat(getComputedStyle(el).rowGap);
      const next = Number.isFinite(g) ? g : 0;
      if (next !== gap) setGap(next);
    }
    if (fixedSize) return;
    let changed = false;
    for (const child of Array.from(el.children) as HTMLElement[]) {
      const i = child.dataset.index;
      if (i === undefined) continue;
      const key = keys[Number(i)];
      const h = child.getBoundingClientRect().height;
      if (key === undefined || h <= 0) continue;
      if (Math.abs((sizes.current.get(key) ?? estimateSize) - h) > 0.5) {
        sizes.current.set(key, h);
        changed = true;
      }
    }
    if (changed) setVersion((v) => v + 1);
  });

  // Focus the row a key moved to once it has rendered.
  useLayoutEffect(() => {
    const i = pendingFocus.current;
    const el = outer.current;
    if (i === null || !el) return;
    const row = el.querySelector<HTMLElement>(`:scope > [data-index="${i}"]`);
    if (!row) return;
    pendingFocus.current = null;
    const target = focus === "inner" ? (row.querySelector<HTMLElement>("[data-vl-focus]") ?? row) : row;
    target.focus({ preventScroll: true });
    row.scrollIntoView?.({ block: "nearest" });
  });

  // Incremental loading: ask once per list length (and first row) as the window nears the end.
  const asked = useRef("");
  const firstKey = keys[0] ?? "";
  useEffect(() => {
    if (!onEndReached || !hasMore || loadingMore || n === 0) return;
    if (range.end < n - endThreshold) return;
    const mark = `${n}\u0000${firstKey}`;
    if (asked.current === mark) return;
    asked.current = mark;
    onEndReached();
  }, [onEndReached, hasMore, loadingMore, n, range.end, endThreshold, firstKey]);

  /** Scroll so row `i` is in view (by its offsets; it may not be rendered yet). */
  const reveal = (i: number) => {
    const el = outer.current;
    const s = scroller.current;
    if (!el || !s || !layout) return;
    const rect = el.getBoundingClientRect();
    const view = viewport(s);
    const top = rect.top + offsets[i]!;
    const bottom = rect.top + offsets[i + 1]! - (table ? 0 : gap);
    let delta = 0;
    if (top < view.top) delta = top - view.top;
    else if (bottom > view.top + view.height) delta = Math.min(top - view.top, bottom - view.top - view.height);
    if (delta === 0) return;
    if (s === window) window.scrollBy(0, delta);
    else (s as HTMLElement).scrollTop += delta;
  };

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || isEditable(e.target)) return;
    const row = (e.target as HTMLElement).closest<HTMLElement>("[data-index]");
    if (!row || row.parentElement !== outer.current) return;
    const from = Number(row.dataset.index);
    const view = scroller.current ? viewport(scroller.current).height : 0;
    const avg = n > 0 ? (offsets[n]! / n) : estimateSize;
    const pageRows = layout && avg > 0 ? Math.max(1, Math.floor(view / avg) - 1) : Math.max(1, Math.floor(initialCount / 2));
    const to = nextIndex(e.key, from, n, pageRows);
    if (to === null) return;
    e.preventDefault();
    setActive(to);
    pendingFocus.current = to;
    reveal(to);
    update();
  };

  const onFocus = (e: FocusEvent<HTMLElement>) => {
    const row = (e.target as HTMLElement).closest<HTMLElement>("[data-index]");
    if (row && row.parentElement === outer.current) setActive(Number(row.dataset.index));
  };

  const start = Math.min(range.start, n);
  const end = Math.min(Math.max(range.end, start), n);
  // One Tab stop: the active row, or the first rendered one when it has scrolled out.
  const tabbable = activeIndex >= start && activeIndex < end ? activeIndex : start;
  const before = offsets[start] ?? 0;
  const after = Math.max(0, (offsets[n] ?? 0) - (offsets[end] ?? 0));
  const setSize = total ?? (hasMore ? -1 : n);

  const rows: ReactNode[] = [];
  if (table && before > 0) rows.push(<Spacer key="vl-before" size={before} columns={columns} />);
  for (let i = start; i < end; i++) {
    const tabIndex: 0 | -1 = i === tabbable ? 0 : -1;
    const content = renderItem(items[i]!, { index: i, active: i === activeIndex, tabIndex });
    const common = { "data-index": i, className: rowClassName, tabIndex: focus === "row" ? tabIndex : undefined };
    rows.push(
      table ? (
        <tr key={keys[i]} {...common} aria-rowindex={rowIndexBase + i}>
          {content}
        </tr>
      ) : (
        <li key={keys[i]} {...common} aria-setsize={setSize} aria-posinset={i + 1}>
          {content}
        </li>
      ),
    );
  }
  if (table && after > 0) rows.push(<Spacer key="vl-after" size={after} columns={columns} />);

  const Tag = as;
  const listStyle = table ? style : { ...style, paddingBlockStart: before, paddingBlockEnd: after };
  const list = (
    <Tag
      {...rest}
      ref={(el: HTMLElement | null) => {
        outer.current = el;
      }}
      style={listStyle}
      onKeyDown={onKeyDown}
      onFocus={onFocus}
      data-virtual-list=""
    >
      {rows}
    </Tag>
  );
  if (table || !onEndReached) return list;
  return (
    <>
      {list}
      <LoadingMore loading={loadingMore} label={t("list.loadingMore")} />
    </>
  );
}

function Spacer({ size, columns }: { size: number; columns: number }) {
  return (
    <tr aria-hidden="true" className="vl-spacer">
      <td colSpan={columns} style={{ blockSize: size, padding: 0, border: 0 }} />
    </tr>
  );
}

/** "Loading more…", announced politely; an empty status region otherwise. Tables place it after the table. */
export function LoadingMore({ loading, label }: { loading: boolean; label?: string }) {
  const t = useT();
  return (
    <p className={loading ? "muted small vl-status" : "visually-hidden"} role="status" aria-live="polite">
      {loading ? (label ?? t("list.loadingMore")) : ""}
    </p>
  );
}
