// @vitest-environment jsdom
/**
 * Pages at scale (II5): long lists render a window of rows, not all of them, and Knowledge loads
 * entities a page at a time as the list scrolls.
 */
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { EgressEvent, EntitySummaryView } from "../api";
import { formatNumber, t } from "../i18n";
import { RESPONSES } from "../test/fixtures";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { KnowledgePage } from "./Knowledge";
import { PrivacyPage } from "./Privacy";

beforeEach(installDomShims);
afterEach(cleanup);

const TOTAL = 50_000;

async function press(el: Element, key: string) {
  await act(async () => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  });
  await flush();
}

describe("Privacy ledger at 50,000 rows", () => {
  it("renders only a window of table rows and tells assistive tech the whole size", async () => {
    const rows: EgressEvent[] = Array.from({ length: TOTAL }, (_, i) => ({
      at: new Date(Date.UTC(2026, 9, 1) - i * 60_000).toISOString(),
      host: `api${i % 7}.example.com`,
      feature: "chat",
      bytes_sent: 1000 + i,
      bytes_received: 2000,
      items: [],
    }));
    mockDaemon({ egress_recent: rows });
    const { container } = await render(<PrivacyPage />);
    await flush();
    const table = container.querySelector("table")!;
    expect(table.getAttribute("aria-rowcount")).toBe(String(TOTAL + 1));
    const rendered = table.querySelectorAll("tbody tr[data-index]");
    expect(rendered.length).toBeGreaterThan(0);
    expect(rendered.length).toBeLessThanOrEqual(40);
    expect(rendered[0]!.getAttribute("aria-rowindex")).toBe("2");
    expect(rendered[0]!.textContent).toContain("api0.example.com");
    // The summary still counts every row.
    expect(container.querySelector(".stats")!.textContent).toContain(formatNumber(TOTAL));
    // Rows are not Tab stops: the table's scroll region already is one.
    expect(table.querySelectorAll("tbody tr[tabindex]")).toHaveLength(0);
  });
});

describe("Privacy ledger paging (LL1)", () => {
  const ledger: EgressEvent[] = Array.from({ length: 450 }, (_, i) => ({
    at: new Date(Date.UTC(2026, 9, 1) - i * 60_000).toISOString(),
    host: `host${i}.example.com`,
    feature: "chat",
    bytes_sent: 10,
    bytes_received: 20,
    items: [],
  }));

  it("loads older pages by cursor and says how many rows are loaded", async () => {
    const requests: { limit: number; cursor?: string }[] = [];
    const calls = mockDaemon({
      egress_page: (args: unknown) => {
        const r = (args as { request: { limit: number; cursor?: string } }).request;
        requests.push(r);
        const from = r.cursor ? Number(r.cursor.slice(2)) : 0;
        const items = ledger.slice(from, from + r.limit);
        const next = from + r.limit < ledger.length ? `e1${from + r.limit}` : null;
        return { items, next_cursor: next };
      },
    });
    const { container } = await render(<PrivacyPage />);
    await flush();
    expect(requests).toEqual([{ limit: 200 }]);
    const table = container.querySelector("table")!;
    expect(table.getAttribute("aria-rowcount")).toBe("201");
    const note = () => container.querySelector(".privacy-loaded")!.textContent;
    expect(note()).toBe(t("privacy.loadedSome", { n: 200 }));
    const more = () =>
      Array.from(container.querySelectorAll("button")).find((b) => b.textContent === t("privacy.loadOlder"));

    await click(more());
    await flush();
    expect(requests.at(-1)).toEqual({ limit: 200, cursor: "e1200" });
    expect(table.getAttribute("aria-rowcount")).toBe("401");
    expect(container.querySelector(".stats")!.textContent).toContain(formatNumber(400));
    // Announced politely once the page is in.
    const said = () => Array.from(container.querySelectorAll('[role="status"]'), (e) => e.textContent ?? "").join(" ");
    expect(said()).toContain(t("privacy.loadedSome", { n: 400 }));

    await click(more());
    await flush();
    expect(requests.at(-1)).toEqual({ limit: 200, cursor: "e1400" });
    expect(table.getAttribute("aria-rowcount")).toBe("451");
    expect(note()).toBe(t("privacy.loadedAll", { n: 450 }));
    expect(more()).toBeUndefined();
    expect(said()).toContain(t("privacy.loadedAll", { n: 450 }));
    // Every row once, in order.
    expect(table.querySelector("tbody tr[data-index]")!.textContent).toContain("host0.example.com");
    expect(calls).not.toContain("egress_recent");
  });

  it("falls back to egress.recent on a daemon without egress.page", async () => {
    const calls = mockDaemon({ egress_recent: ledger.slice(0, 50) });
    const { container } = await render(<PrivacyPage />);
    await flush();
    expect(calls).toContain("egress_page");
    expect(calls).toContain("egress_recent");
    expect(container.querySelector("table")!.getAttribute("aria-rowcount")).toBe("51");
    expect(container.querySelector(".privacy-loaded")!.textContent).toBe(t("privacy.loadedAll", { n: 50 }));
  });
});

describe("Knowledge entities at 50,000", () => {
  const all: EntitySummaryView[] = Array.from({ length: TOTAL }, (_, i) => ({
    id: `e${i}`,
    kind: "person",
    name: `Person ${i}`,
    claims: 1,
    mentions: 1,
    updated_at: "2026-10-01T00:00:00Z",
  }));

  it("loads a page at a time as the list reaches its end, rendering only a window", async () => {
    const requests: { limit: number; offset: number }[] = [];
    mockDaemon({
      knowledge_status: { ...(RESPONSES.knowledge_status as object), entities: TOTAL },
      knowledge_entities: (args: unknown) => {
        const r = (args as { request: { limit: number; offset: number } }).request;
        requests.push({ limit: r.limit, offset: r.offset });
        return all.slice(r.offset, r.offset + r.limit);
      },
    });
    const { container } = await render(<KnowledgePage />);
    await flush();
    expect(requests).toEqual([{ limit: 51, offset: 0 }]);
    const count = () => container.querySelector(".kb-count")!.textContent;
    expect(count()).toBe(t("knowledge.rangeOf", { from: 1, to: 50, total: TOTAL }));
    const buttons = () => Array.from(container.querySelectorAll<HTMLButtonElement>("button.entity-row"));
    expect(buttons().length).toBeLessThanOrEqual(30);
    // The whole set's size, not the loaded page's.
    expect(container.querySelector(".entity-list li")!.getAttribute("aria-setsize")).toBe(String(TOTAL));
    expect(buttons().filter((b) => b.tabIndex === 0)).toHaveLength(1);

    act(() => buttons()[0]!.focus());
    await press(buttons()[0]!, "End");
    expect(document.activeElement!.textContent).toContain("Person 49");
    expect(requests).toEqual([
      { limit: 51, offset: 0 },
      { limit: 51, offset: 50 },
    ]);
    expect(count()).toBe(t("knowledge.rangeOf", { from: 1, to: 100, total: TOTAL }));
    await press(document.activeElement!, "End");
    expect(document.activeElement!.textContent).toContain("Person 99");
    expect(requests.at(-1)).toEqual({ limit: 51, offset: 100 });
    expect(buttons().length).toBeLessThanOrEqual(30);
    expect(document.querySelector(".entity-list")!.getAttribute("aria-busy")).toBeNull();
  });
});
