// @vitest-environment jsdom
/**
 * Secondary-page polish: Status reports one daemon failure once, Safety names built-in rules and
 * folds long pattern lists, Models groups by role and badges one "best" pick, and Apps tells
 * same-named connections apart.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { FitReport, ModelFit, ModelInfo, PolicyRule, PolicyState } from "../api";
import { t } from "../i18n";
import { RESPONSES } from "../test/fixtures";
import { cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { AppsPage } from "./Apps";
import { bestChatModel, modelName, modelNames, modelRole, ModelsPage } from "./Models";
import { defaultRuleName, SafetyPage } from "./Safety";
import { StatusPage } from "./Status";

beforeEach(installDomShims);
afterEach(cleanup);

const busy = () => {
  throw { kind: "rpc", message: "database is locked", code: -32000 };
};

describe("Status: one daemon failure, one error", () => {
  it("shows a single page-level error whose Retry reloads every section", async () => {
    let failing = true;
    const answer = (cmd: string) => () => {
      if (failing) busy();
      return RESPONSES[cmd as keyof typeof RESPONSES];
    };
    const calls = mockDaemon({
      status: answer("status"),
      ingest_status: answer("ingest_status"),
      runtime_status: answer("runtime_status"),
      knowledge_status: answer("knowledge_status"),
      updates_status: null,
    });
    const { container } = await render(<StatusPage />);
    await flush();
    const errors = container.querySelectorAll(".note-error");
    expect(errors).toHaveLength(1);
    // The sections with nothing to show are left out while the page reports the failure.
    expect(container.querySelector("#indexing-title")).toBeNull();
    const retry = Array.from(errors[0]!.querySelectorAll("button")).find((b) => b.textContent === "Retry");
    expect(retry).toBeDefined();
    const count = (cmd: string) => calls.filter((c) => c === cmd).length;
    const before = ["status", "ingest_status", "runtime_status", "knowledge_status"].map(count);
    failing = false;
    await click(retry);
    await flush();
    expect(["status", "ingest_status", "runtime_status", "knowledge_status"].map(count)).toEqual(before.map((n) => n + 1));
    expect(container.querySelectorAll(".note-error")).toHaveLength(0);
    expect(container.querySelector("#indexing-title")).not.toBeNull();
  });
});

describe("Safety: built-in rules", () => {
  it("names known rules from the catalog, else from the daemon's description, else the id", () => {
    const rule = (id: string, description?: string): PolicyRule => ({ id, description, effect: "deny" });
    expect(defaultRuleName(rule("default:deny-financial", "banking apps"), t)).toBe("Banking, brokerage and payment apps and sites");
    expect(defaultRuleName(rule("default:new-rule", "password vaults"), t)).toBe("Password vaults");
    expect(defaultRuleName(rule("default:bare"), t)).toBe("default:bare");
  });

  it("shows a few patterns and folds the rest behind Show all", async () => {
    const targets = Array.from({ length: 12 }, (_, i) => `*.bank${i}.*`);
    const policy: PolicyState = {
      config: { version: 1, rules: [], disabled_defaults: [], rate_limits: {}, confirm_timeout_secs: 120 },
      defaults: [{ rule: { id: "default:deny-financial", description: "banking", targets, effect: "deny" }, enabled: true }],
      halted: false,
      log: { entries: 0, head: null, chain_ok: true },
    };
    mockDaemon({ policy_get: policy, policy_approvals: { pending: [], task: [] }, browser_status: { connected: false, origin: null, host_version: null, tools: [] } });
    const { container } = await render(<SafetyPage />);
    await flush();
    const row = container.querySelector("table tbody tr")!;
    expect(row.querySelector("th")?.textContent).toBe("Banking, brokerage and payment apps and sites");
    const preview = row.querySelector("bdi.pattern-inline")!;
    expect(preview.getAttribute("dir")).toBe("ltr");
    expect(preview.textContent).toBe(targets.slice(0, 4).join(", "));
    expect(row.textContent).toContain("and 8 more");
    const details = row.querySelector("details")!;
    expect(details.querySelector("summary")?.textContent).toBe("Show all 12 patterns");
    expect(details.querySelectorAll("li")).toHaveLength(12);
    expect(row.querySelector('input[role="switch"]')?.getAttribute("aria-label")).toBe("Use built-in rule Banking, brokerage and payment apps and sites");
  });
});

describe("Models: roles, names and the best pick", () => {
  const model = (id: string, over: Partial<ModelInfo> = {}): ModelInfo => ({ id, size_bytes: 1e9, installed: false, recommended: true, ...over });
  const fitFor = (id: string, reasons: ModelFit["reasons"] = ["ram_fits"]): ModelFit => ({
    id,
    recommended: true,
    tiers: [],
    size_bytes: 1e9,
    min_ram_gb: 8,
    disk_needed_bytes: 2e9,
    fits_ram: true,
    fits_disk: true,
    machine_ram_gb: 16,
    gpu: "unknown",
    accel: "cpu",
    reasons,
  });
  const report = (models: ModelFit[]): FitReport => ({
    hardware: { total_ram: 16e9, available_ram: 8e9, cpu_cores: 8, gpu: "unknown" },
    ram_gb: 16,
    free_disk_bytes: 100e9,
    models,
  });

  it("classes search models by tiers, and by known families without tiers", () => {
    expect(modelRole(model("qwen3-4b-q4km", { tiers: ["light", "synthesis"] }))).toBe("chat");
    expect(modelRole(model("x", { tiers: ["embedding"] }))).toBe("embedding");
    expect(modelRole(model("x", { tiers: ["rerank"] }))).toBe("rerank");
    for (const id of ["nomic-embed-text-v1.5-q8", "bge-small-en-v1.5", "bge-m3", "multilingual-e5-small", "gte-base"]) {
      expect(modelRole(model(id)), id).toBe("embedding");
    }
    expect(modelRole(model("bge-reranker-v2-m3-q4km"))).toBe("rerank");
    expect(modelRole(model("llama-3.2-3b-q4km"))).toBe("chat");
  });

  it("reads ids as names, and keeps quantisations apart when names would collide", () => {
    expect(modelName("qwen3-4b-q4km")).toBe("Qwen3 4B");
    expect(modelName("bge-reranker-v2-m3-q4km")).toBe("BGE Reranker v2 M3");
    const names = modelNames(["qwen3-4b-q4km", "qwen3-4b-q8", "qwen3-8b-q4km"]);
    expect(names.get("qwen3-4b-q4km")).toBe("Qwen3 4B Q4_K_M");
    expect(names.get("qwen3-4b-q8")).toBe("Qwen3 4B Q8");
    expect(names.get("qwen3-8b-q4km")).toBe("Qwen3 8B");
  });

  it("picks the daemon's largest fitting chat model as best, and none without fit data", () => {
    const chat = [model("a-1b"), model("b-4b"), model("c-8b", { recommended: false })];
    expect(bestChatModel(chat, report([fitFor("a-1b"), fitFor("b-4b", ["ram_fits_largest"])]))?.id).toBe("b-4b");
    expect(bestChatModel(chat, null)).toBeUndefined();
    expect(bestChatModel(chat, report([fitFor("a-1b")]))).toBeUndefined();
  });

  it("groups chat and search models and badges one best pick", async () => {
    const list = [model("qwen3-4b-q4km"), model("qwen3-1.7b-q8"), model("bge-small-en-v1.5")];
    mockDaemon({ models_list: list, models_fit: report([fitFor("qwen3-4b-q4km", ["ram_fits_largest"]), fitFor("qwen3-1.7b-q8"), fitFor("bge-small-en-v1.5")]) });
    const { container } = await render(<ModelsPage />);
    await flush();
    const group = (title: string) => Array.from(container.querySelectorAll("section.models-group")).find((s) => s.querySelector("h2")?.textContent === title)!;
    const ids = (s: Element) => Array.from(s.querySelectorAll("li.card")).map((li) => li.getAttribute("data-model-id"));
    expect(ids(group("Chat and drafting"))).toEqual(["qwen3-1.7b-q8", "qwen3-4b-q4km"]);
    expect(ids(group("Search"))).toEqual(["bge-small-en-v1.5"]);
    const badges = Array.from(container.querySelectorAll(".badge")).map((b) => b.textContent);
    expect(badges.filter((b) => b === "Best for this computer")).toHaveLength(1);
    expect(container.querySelector('li[data-model-id="qwen3-4b-q4km"] .badge')?.textContent).toBe("Best for this computer");
    expect(container.querySelector('li[data-model-id="qwen3-1.7b-q8"] .badge')).toBeNull();
  });

  it("without fit data, badges no best pick and keeps Recommended", async () => {
    mockDaemon({ models_list: [model("qwen3-4b-q4km"), model("qwen3-1.7b-q8")], models_fit: () => busy() });
    const { container } = await render(<ModelsPage />);
    await flush();
    const badges = Array.from(container.querySelectorAll(".badge")).map((b) => b.textContent);
    expect(badges).not.toContain("Best for this computer");
    expect(badges.filter((b) => b === "Recommended")).toHaveLength(2);
  });
});

describe("Apps: same-named connections", () => {
  it("shows a short id and puts it in the Revoke name when two apps share a name", async () => {
    const at = "2026-10-28T11:00:00Z";
    const client = (id: string) => ({ id, name: "Claude Desktop", scopes: { sources: null, tools: [], include_sensitive: false }, created_at: at, last_used: null });
    mockDaemon({ mcp_clients_list: [client("aaaaaaaa1111"), client("bbbbbbbb2222")] });
    const { container } = await render(<AppsPage />);
    await flush();
    const labels = Array.from(container.querySelectorAll("tbody button")).map((b) => b.getAttribute("aria-label"));
    expect(labels).toEqual(["Revoke Claude Desktop (aaaaaaaa)", "Revoke Claude Desktop (bbbbbbbb)"]);
  });
});
