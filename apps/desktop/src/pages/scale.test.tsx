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
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
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
