// @vitest-environment jsdom
/** usePagedList (II5): stale replies, errors across keys, refresh vs. next page, duplicate rows. */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { usePagedList, type PagedList } from "./usePagedList";

type Row = { id: string };
type Call = { offset: number; limit: number; resolve: (rows: Row[]) => void; reject: (e: unknown) => void };

/** A fetcher whose replies the test sends, in any order. */
function fetcher() {
  const calls: Call[] = [];
  const fetchPage = (offset: number, limit: number) =>
    new Promise<Row[]>((resolve, reject) => {
      calls.push({ offset, limit, resolve, reject });
    });
  return { calls, fetchPage };
}
const rows = (from: number, n: number): Row[] => Array.from({ length: n }, (_, i) => ({ id: `r${from + i}` }));
const ids = (l: PagedList<Row>) => (l.items ?? []).map((r) => r.id);

let root: Root | null = null;
let host: HTMLElement | null = null;
afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
});

async function mount(fetchPage: (o: number, l: number) => Promise<Row[]>, key: string, pageSize = 3) {
  const out: { current: PagedList<Row> } = { current: null as unknown as PagedList<Row> };
  const Probe = ({ k }: { k: string }) => {
    out.current = usePagedList(fetchPage, k, { pageSize, itemKey: (r) => r.id });
    return null;
  };
  host = document.createElement("div");
  root = createRoot(host);
  await act(async () => root!.render(<Probe k={key} />));
  return {
    out,
    rekey: (k: string) => act(async () => root!.render(<Probe k={k} />)),
  };
}
const reply = (c: Call | undefined, r: Row[]) => act(async () => c!.resolve(r));
const fail = (c: Call | undefined) => act(async () => c!.reject(new Error("boom")));
const call = async (f: () => void) => act(async () => f());

describe("usePagedList", () => {
  it("pages by offset, one extra row to learn whether more exist", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    expect(f.calls.map((c) => [c.offset, c.limit])).toEqual([[0, 4]]);
    await reply(f.calls[0], rows(0, 4));
    expect(ids(out.current)).toEqual(["r0", "r1", "r2"]);
    expect(out.current.hasMore).toBe(true);
    await call(out.current.loadMore);
    expect(f.calls.at(-1)!.offset).toBe(3);
    expect(out.current.loadingMore).toBe(true);
    await reply(f.calls[1], rows(3, 2));
    expect(ids(out.current)).toEqual(["r0", "r1", "r2", "r3", "r4"]);
    expect(out.current.hasMore).toBe(false);
  });

  it("drops a reply for a key that has gone", async () => {
    const f = fetcher();
    const { out, rekey } = await mount(f.fetchPage, "a");
    await rekey("b");
    expect(out.current.loading).toBe(true);
    await reply(f.calls[1], [{ id: "b0" }]);
    await reply(f.calls[0], [{ id: "a0" }]);
    expect(ids(out.current)).toEqual(["b0"]);
    expect(out.current.loading).toBe(false);
  });

  it("does not show the old key's rows as the new key's after an error", async () => {
    const f = fetcher();
    const { out, rekey } = await mount(f.fetchPage, "a");
    await reply(f.calls[0], rows(0, 2));
    await rekey("b");
    // While b loads, a's rows stay (marked loading)…
    expect(ids(out.current)).toEqual(["r0", "r1"]);
    await fail(f.calls[1]);
    // …but once b fails they are not passed off as b's.
    expect(out.current.items).toBeUndefined();
    expect(out.current.error).toBeTruthy();
    expect(out.current.loading).toBe(false);
  });

  it("keeps a key's rows when its next page fails, and asks again after reload", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    await reply(f.calls[0], rows(0, 4));
    await call(out.current.loadMore);
    await fail(f.calls[1]);
    expect(ids(out.current)).toEqual(["r0", "r1", "r2"]);
    await call(out.current.loadMore);
    expect(f.calls).toHaveLength(2);
    await call(out.current.reload);
    await reply(f.calls[2], rows(0, 4));
    await call(out.current.loadMore);
    expect(f.calls.at(-1)!.offset).toBe(3);
  });

  it("refreshes in the background without marking the list busy, and loads the next page after", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    await reply(f.calls[0], rows(0, 4));
    await call(out.current.refresh);
    expect(f.calls.at(-1)).toMatchObject({ offset: 0, limit: 4 });
    expect(out.current.refreshing).toBe(true);
    expect(out.current.loading).toBe(false);
    // A next page asked for during the refresh waits for it (no request, nothing consumed)…
    await call(out.current.loadMore);
    expect(f.calls).toHaveLength(2);
    await reply(f.calls[1], rows(0, 4));
    expect(out.current.refreshing).toBe(false);
    // …and goes once it is done.
    await call(out.current.loadMore);
    expect(f.calls).toHaveLength(3);
    expect(f.calls[2]!.offset).toBe(3);
  });

  it("does not refresh over a page in flight", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    await reply(f.calls[0], rows(0, 4));
    await call(out.current.loadMore);
    await call(out.current.refresh);
    expect(f.calls).toHaveLength(2);
    await reply(f.calls[1], rows(3, 4));
    expect(ids(out.current)).toEqual(["r0", "r1", "r2", "r3", "r4", "r5"]);
  });

  it("asks for each offset once until the list starts over", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    await reply(f.calls[0], rows(0, 4));
    await call(out.current.loadMore);
    await fail(f.calls[1]);
    await call(out.current.reload);
    await reply(f.calls[2], rows(0, 4));
    // Same length and first row as before the reload: infinite scroll still goes on.
    await call(out.current.loadMore);
    expect(f.calls[3]!.offset).toBe(3);
  });

  it("never lists a row twice when the list shifts between pages", async () => {
    const f = fetcher();
    const { out } = await mount(f.fetchPage, "a");
    await reply(f.calls[0], rows(0, 4));
    await call(out.current.loadMore);
    // A new row arrived at the top: offset 3 now starts at r2 again.
    await reply(f.calls[1], [{ id: "r2" }, { id: "r3" }, { id: "r4" }, { id: "r5" }]);
    expect(ids(out.current)).toEqual(["r0", "r1", "r2", "r3", "r4"]);
    // The next offset follows what the daemon returned, so a page of duplicates doesn't stall.
    await call(out.current.loadMore);
    expect(f.calls[2]!.offset).toBe(6);
    await reply(f.calls[2], [{ id: "r4" }, { id: "r5" }]);
    expect(ids(out.current)).toEqual(["r0", "r1", "r2", "r3", "r4", "r5"]);
    expect(new Set(ids(out.current)).size).toBe(ids(out.current).length);
  });
});
