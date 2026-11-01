// @vitest-environment jsdom
/** Knowledge: entity list, detail, text and kind filters, rejecting a fact, empty knowledge. */
import { describe, expect, it } from "vitest";
import { entityDetail } from "../../src/test/fixtures";
import {
  click,
  daemon,
  errors,
  findByRole,
  findByText,
  getByRole,
  renderApp,
  setupHarness,
  t,
  waitFor,
  within,
} from "../harness";

setupHarness();

const list = () => getByRole("region", { name: t("knowledge.entitiesLabel") });
const entityButton = (name: string) => within(list()).getByRole("button", { name: new RegExp(`^${name}`) });


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

  it("shows one error when status and list both fail, and Retry reloads both", async () => {
    const d = daemon().failOnce("knowledge_status", errors.rpc("database is locked")).failOnce("knowledge_entities", errors.rpc("database is locked"));
    await renderApp({ page: "knowledge" });
    await waitFor(() => expect(getByRole("main").querySelectorAll('[role="alert"]').length).toBe(1));
    const alert = getByRole("main").querySelector('[role="alert"]')!;
    const statusCalls = d.count("knowledge_status");
    const listCalls = d.count("knowledge_entities");
    await click(within(alert as HTMLElement).getByRole("button", { name: t("common.retry") }));
    await findByText(/^Ada Lovelace/, { root: list(), selector: "button" });
    expect(d.count("knowledge_status")).toBeGreaterThan(statusCalls);
    expect(d.count("knowledge_entities")).toBeGreaterThan(listCalls);
    expect(getByRole("main").querySelectorAll('[role="alert"]').length).toBe(0);
  });

  it("asks for sensitive entities only when the graph's toggle is on", async () => {
    const d = daemon();
    await renderApp({ page: "knowledge" });
    await click(getByRole("tab", { name: t("knowledge.tab.graph") }));
    await waitFor(() => expect(d.count("knowledge_graph")).toBeGreaterThan(0));
    const sent = () => d.callsTo("knowledge_graph").map((a) => (a as { request: { include_sensitive?: boolean } }).request.include_sensitive);
    expect(sent()).toEqual([false]);
    await click(getByRole("checkbox", { name: t("graph.showSensitive") }));
    await waitFor(() => expect(sent()).toEqual([false, true]));
  });
});
