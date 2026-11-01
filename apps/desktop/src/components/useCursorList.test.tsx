// @vitest-environment jsdom
/** useCursorList (LL1): cursor pages, stale replies, retry after an error, duplicate rows. */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { useCursorList, type CursorList, type CursorPageResult } from "./useCursorList";

type Row = { id: string };
type Call = { cursor: string | null; resolve: (p: CursorPageResult<Row>) => void; reject: (e: unknown) => void };

function fetcher() {
  const calls: Call[] = [];
  const fetchPage = (cursor: string | null) =>
    new Promise<CursorPageResult<Row>>((resolve, reject) => {
      calls.push({ cursor, resolve, reject });
    });
  return { calls, fetchPage };
}
const rows = (...ns: number[]): Row[] => ns.map((n) => ({ id: `r${n}` }));
const ids = (l: CursorList<Row>) => (l.items ?? []).map((r) => r.id);

let root: Root | null = null;
let host: HTMLElement | null = null;
afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
});

async function mount(fetchPage: (c: string | null) => Promise<CursorPageResult<Row>>, key: string) {
  const out: { current: CursorList<Row> } = { current: null as unknown as CursorList<Row> };
  const Probe = ({ k }: { k: string }) => {
    out.current = useCursorList(fetchPage, k, { itemKey: (r) => r.id });
    return null;
  };
  host = document.createElement("div");
  root = createRoot(host);
  await act(async () => root!.render(<Probe k={key} />));
  return { out, rekey: (k: string) => act(async () => root!.render(<Probe k={k} />)) };
}
const reply = (c: Call | undefined, items: Row[], next: string | null) => act(async () => c!.resolve({ items, next }));
const fail = (c: Call | undefined) => act(async () => c!.reject(new Error("boom")));
const call = async (f: () => void) => act(async () => f());

describe("useCursorList", () => {
  it("follows the daemon's cursors to the end, asking each once and merging repeats", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    expect(f.calls.map((c) => c.cursor)).toEqual([null]);
    expect(out.current.loading).toBe(true);
    await reply(f.calls[0], rows(1, 2), "c2");
    expect(ids(out.current)).toEqual(["r1", "r2"]);
    expect(out.current.hasMore).toBe(true);
    await call(out.current.loadMore);
    await call(out.current.loadMore);
    expect(f.calls.map((c) => c.cursor)).toEqual([null, "c2"]);
    expect(out.current.loadingMore).toBe(true);
    await reply(f.calls[1], rows(2, 3), null);
    expect(ids(out.current)).toEqual(["r1", "r2", "r3"]);
    expect(out.current.hasMore).toBe(false);
    await call(out.current.loadMore);
    expect(f.calls).toHaveLength(2);
  });

  it("drops a reply for a key that has gone", async () => {
    const f = fetcher();
    const { out, rekey } = await mount(f.fetchPage, "a");
    await rekey("b");
    await reply(f.calls[0], rows(1), "x");
    expect(out.current.items).toBeUndefined();
    await reply(f.calls[1], rows(9), null);
    expect(ids(out.current)).toEqual(["r9"]);
  });

  it("keeps the rows after a failed page and retries that page", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    await reply(f.calls[0], rows(1), "c1");
    await call(out.current.loadMore);
    await fail(f.calls[1]);
    expect(ids(out.current)).toEqual(["r1"]);
    expect(out.current.error).toBeInstanceOf(Error);
    await call(out.current.loadMore);
    expect(f.calls).toHaveLength(2);
    await call(out.current.retry);
    expect(f.calls.map((c) => c.cursor)).toEqual([null, "c1", "c1"]);
    await reply(f.calls[2], rows(2), null);
    expect(ids(out.current)).toEqual(["r1", "r2"]);
    expect(out.current.error).toBeNull();
  });
});
