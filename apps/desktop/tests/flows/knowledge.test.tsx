// @vitest-environment jsdom
/** Knowledge: entity list, detail, text and kind filters, rejecting a fact, empty knowledge. */
import { describe, expect, it } from "vitest";
import type { EntitySummaryView } from "../../src/api";
import { entities, entityDetail } from "../../src/test/fixtures";
import {
  click,
  daemon,
  errors,
  findByRole,
  findByText,
  getByRole,
  queryByText,
  renderApp,
  setupHarness,
  t,
  type,
  waitFor,
  within,
} from "../harness";

setupHarness();

const list = () => getByRole("region", { name: t("knowledge.entitiesLabel") });
const entityButton = (name: string) => within(list()).getByRole("button", { name: new RegExp(`^${name}`) });

/** Filter the fixture entities like the daemon would. */
function filterEntities(args: Record<string, unknown>): EntitySummaryView[] {
  const req = args.request as { text: string | null; kind: string | null };
  return entities.filter(
    (e) => (!req.text || e.name.toLowerCase().includes(req.text.toLowerCase())) && (!req.kind || e.kind === req.kind),
  );
}

describe("knowledge flow", () => {
  it("lists entities and opens one's detail", async () => {
    const d = daemon({ knowledge_entity: entityDetail });
    await renderApp({ page: "knowledge" });

    expect(d.callsTo("knowledge_entities")).toEqual([{ request: { text: null, kind: null, limit: 51, offset: 0 } }]);
    expect(entityButton("Ada Lovelace").textContent).toContain(t("knowledge.rowMeta", { kind: "Person", claims: 4, mentions: 9 }));
    expect(entityButton("Analytical Engine")).toBeTruthy();
    expect(list().textContent).toContain(t("knowledge.rangeOf", { from: 1, to: 2, total: 2 }));
    expect(getByRole("main").textContent).toContain(t("knowledge.chooseTitle"));

    await click(entityButton("Ada Lovelace"));
    expect(entityButton("Ada Lovelace").getAttribute("aria-current")).toBe("true");
    expect(d.callsTo("knowledge_entity")).toEqual([{ request: { id: "e1" } }]);
    const detail = await findByRole("region", { name: "Ada Lovelace" });
    expect(detail.textContent).toContain("Countess of Lovelace");
    expect(detail.textContent).toContain("ada@example.com");
    expect(detail.textContent).toContain("Analytical Engine");
    // Evidence is behind a disclosure.
    expect(detail.textContent).not.toContain("Ada's notes on the engine");
    const why = within(detail).getByRole("button", { name: new RegExp(`^${t("entity.why")}`) });
    await click(why);
    expect(why.getAttribute("aria-expanded")).toBe("true");
    expect(detail.textContent).toContain("Ada's notes on the engine");
  });

  it("rejects a fact and can undo it", async () => {
    const d = daemon({ knowledge_entity: entityDetail, knowledge_claim_status: null });
    await renderApp({ page: "knowledge" });
    await click(entityButton("Ada Lovelace"));
    const detail = await findByRole("region", { name: "Ada Lovelace" });

    await click(within(detail).getByRole("button", { name: /^Reject this fact/ }));
    expect(d.callsTo("knowledge_claim_status")).toEqual([{ request: { claim: 1, status: "rejected" } }]);
    await click(within(detail).getByRole("button", { name: t("entity.undo") }));
    expect(d.last("knowledge_claim_status")).toEqual({ request: { claim: 1, status: "active" } });
  });

  it("filters by text (debounced) and by kind", async () => {
    const d = daemon({ knowledge_entities: filterEntities });
    await renderApp({ page: "knowledge" });

    await type(within(list()).getByRole("searchbox", { name: t("knowledge.searchLabel") }), "engine");
    await waitFor(() => expect(queryByText(/^Ada Lovelace/, { root: list(), selector: "button" })).toBeNull());
    // One request for the settled text, not one per keystroke.
    expect(d.callsTo("knowledge_entities").map((a) => (a.request as { text: string | null }).text)).toEqual([null, "engine"]);
    expect(entityButton("Analytical Engine")).toBeTruthy();

    await type(within(list()).getByRole("searchbox"), "");
    await findByText(/^Ada Lovelace/, { root: list(), selector: "button" });

    const kinds = within(list()).getByRole("group", { name: t("knowledge.filterByKind") });
    await click(within(kinds).getByRole("button", { name: t("knowledge.kindPlural.project") }));
    expect(d.last("knowledge_entities")).toEqual({ request: { text: null, kind: "project", limit: 51, offset: 0 } });
    await waitFor(() => expect(queryByText(/^Ada Lovelace/, { root: list(), selector: "button" })).toBeNull());
    expect(within(kinds).getByRole("button", { name: t("knowledge.kindPlural.project") }).getAttribute("aria-pressed")).toBe("true");

    await click(within(kinds).getByRole("button", { name: t("knowledge.kindPlural.place") }));
    await findByText(t("knowledge.noMatches"));
  });

  it("shows an empty state when nothing has been learned yet", async () => {
    daemon({
      knowledge_entities: [],
      knowledge_status: { entities: 0, claims: 0, mentions: 0, pending_items: 0, last_run: null, extractor_provider: null, vault: { path: null, pages: 0, last_render: null, conflicts: [], invalid: [] } },
    });
    await renderApp({ page: "knowledge" });
    await findByText(t("knowledge.emptyTitle"));
    expect(within(list()).queryAllByRole("listitem")).toEqual([]);
  });

  it("recovers from a failed entity list with Retry", async () => {
    const d = daemon().failOnce("knowledge_entities", errors.rpc("graph store unavailable"));
    await renderApp({ page: "knowledge" });
    const alert = within(list()).getByRole("alert");
    expect(alert.textContent).toContain("graph store unavailable");
    await click(within(alert).getByRole("button", { name: t("common.retry") }));
    await findByText(/^Ada Lovelace/, { root: list(), selector: "button" });
    expect(d.count("knowledge_entities")).toBe(2);
  });

  it("switches to the graph tab", async () => {
    const d = daemon();
    await renderApp({ page: "knowledge" });
    await click(getByRole("tab", { name: t("knowledge.tab.graph") }));
    expect(getByRole("tab", { name: t("knowledge.tab.graph") }).getAttribute("aria-selected")).toBe("true");
    await waitFor(() => expect(d.count("knowledge_graph")).toBeGreaterThan(0));
  });
});
