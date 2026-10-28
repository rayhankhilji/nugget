// @vitest-environment jsdom
/** Status: providers (and probing one), ingest queue, local runtime, audit chain, knowledge. */
import { describe, expect, it } from "vitest";
import { EVENT } from "../../src/api";
import { status as fixtureStatus, providers } from "../../src/test/fixtures";
import {
  click,
  daemon,
  emitEvent,
  errors,
  findByText,
  getByRole,
  getByText,
  renderApp,
  setupHarness,
  t,
  waitFor,
  within,
} from "../harness";

setupHarness();

/** The value shown for a `<dt>` label within `root`. */
function stat(label: string, root: ParentNode = document.body): string {
  const dt = getByText(label, { root, selector: "dt" });
  return (dt.nextElementSibling?.textContent ?? "").trim();
}

const providerRow = (id: string) => within(getByRole("region", { name: t("status.providers") })).getByRole("rowheader", { name: new RegExp(`^${id}`) }).closest("tr")!;

describe("status flow", () => {
  it("shows the daemon, providers, ingest queue, runtime and knowledge", async () => {
    const d = daemon();
    await renderApp({ page: "status" });

    for (const cmd of ["status", "ingest_status", "runtime_status", "knowledge_status"]) expect(d.commands).toContain(cmd);
    expect(stat(t("status.daemon"))).toBe(t("status.version", { version: "0.0.1" }));
    expect(stat(t("status.auditLog"))).toBe(t("status.auditVerified"));

    // Providers: one row each, with where they run and what they can do.
    expect(providerRow("local-chat").textContent).toContain("qwen3-4b");
    expect(providerRow("local-chat").textContent).toContain(t("status.notProbed"));
    expect(providerRow("anthropic").textContent).toContain(t("status.cap.vision"));

    const indexing = getByRole("region", { name: t("status.indexing") });
    expect(stat(t("status.queue"), indexing)).toContain("3");
    expect(stat(t("status.embeddings"), indexing)).toBe("local-embed");

    const runtime = getByRole("region", { name: t("status.localRuntime") });
    const rt = within(runtime).getByRole("rowheader", { name: "qwen3-4b-q4" }).closest("tr")!;
    expect(rt.textContent).toContain(t("status.runtime.ready"));
    expect(rt.textContent).toContain("local-chat");

    const knowledge = getByRole("region", { name: t("status.knowledge") });
    expect(stat(t("status.entities"), knowledge)).toBe("2");
    expect(stat(t("status.facts"), knowledge)).toBe("5");
  });

  it("probes a provider by id and refreshes the status", async () => {
    const d = daemon({ providers_probe: { streaming: true } });
    await renderApp({ page: "status" });
    const before = d.count("status");

    const probe = d.hang("providers_probe");
    await click(within(providerRow("local-chat")).getByRole("button", { name: t("status.probe") }));
    expect(d.callsTo("providers_probe")).toEqual([{ provider: "local-chat" }]);
    expect(within(providerRow("local-chat")).getByRole("button", { name: t("status.probing") })).toHaveProperty("disabled", true);

    // The re-fetched status now carries the probed capabilities.
    d.on("status", { ...fixtureStatus, providers: [{ ...providers[0]!, capabilities: { streaming: true, vision: false, tools: false, json_schema: false, embeddings: false, context_tokens: 32768 } }, providers[1]!] });
    await probe.resolve({ streaming: true });
    await waitFor(() => expect(d.count("status")).toBe(before + 1));
    await waitFor(() => expect(within(providerRow("local-chat")).getByRole("button", { name: t("status.reprobe") })).toBeTruthy());
    expect(providerRow("local-chat").textContent).toContain(t("status.cap.streaming"));
  });

  it("shows a failed probe on the provider's row", async () => {
    daemon().fail("providers_probe", errors.rpc("provider timed out"));
    await renderApp({ page: "status" });
    await click(within(providerRow("anthropic")).getByRole("button", { name: t("status.reprobe") }));
    expect(within(providerRow("anthropic")).getByRole("alert").textContent).toContain("provider timed out");
  });

  it("warns loudly when the audit chain fails verification", async () => {
    daemon({ status: { ...fixtureStatus, audit_chain_ok: false } });
    await renderApp({ page: "status" });
    expect(stat(t("status.auditLog"))).toBe(t("status.auditTamperedBadge"));
    await findByText(t("status.auditTampered"));
    expect(getByText(t("status.auditTampered")).closest("[role=alert]")).not.toBeNull();
  });

  it("says when the audit check hasn't finished", async () => {
    daemon({ status: { ...fixtureStatus, audit_chain_ok: null } });
    await renderApp({ page: "status" });
    expect(stat(t("status.auditLog"))).toBe(t("status.auditChecking"));
  });

  it("refreshes the ingest queue on progress events", async () => {
    const d = daemon();
    // The first call answers 3 waiting, later ones 42.
    d.on("ingest_status", (_a, n) => ({ queue_depth: n === 1 ? 3 : 42, freshness_p50_ms: 850, freshness_p95_ms: 4200, embedding_provider: "local-embed", triage_provider: null, sources: [] }));
    await renderApp({ page: "status" });
    const indexing = getByRole("region", { name: t("status.indexing") });
    expect(stat(t("status.queue"), indexing)).toContain("3");

    await emitEvent(EVENT.ingestProgress, { source: "files-1", phase: "indexing", done: 5, total: 10 });
    await waitFor(() => expect(stat(t("status.queue"), indexing)).toContain("42"));
    expect(d.count("ingest_status")).toBe(2);
  });

  it("shows sections the daemon doesn't support as unavailable", async () => {
    daemon().fail("runtime_status", errors.methodNotFound());
    await renderApp({ page: "status" });
    const runtime = getByRole("region", { name: t("status.localRuntime") });
    expect(within(runtime).queryByRole("table")).toBeNull();
    expect(runtime.textContent).toContain(t("common.notAvailableSentence"));
  });
});
